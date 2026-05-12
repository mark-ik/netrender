/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 7' — vello-backed tile cache (Masonry pattern per
//! rasterizer plan §2.1-§2.4).
//!
//! Replaces the parent-plan Phase 7 architecture
//! (`Tile.texture: Option<Arc<wgpu::Texture>>` per tile, one
//! `brush_image_alpha` composite draw per tile) with:
//!
//! - One [`vello::Renderer`] for the lifetime of this struct.
//! - A per-tile [`vello::Scene`] cache keyed by [`TileCoord`].
//! - One `Scene::append` of every cached tile-Scene into a master
//!   frame Scene per render.
//! - One [`vello::Renderer::render_to_texture`] per frame, one submit.
//!
//! `TileCache` keeps its existing job — frame-stamp invalidation,
//! dependency hashing, retain heuristic. The rasterizer holds the
//! GPU-side cache; tile_cache stays rasterizer-agnostic.
//!
//! ## Per-tile clipping at compose time
//!
//! A primitive whose AABB intersects multiple tiles ends up in each
//! tile's filtered Scene. Without clipping, vello would rasterize
//! the same primitive once per tile and the overlapping pixels
//! would be drawn N times — wasteful and incorrect for non-opaque
//! primitives. We solve this by wrapping each tile-Scene's
//! `Scene::append` in `push_layer(tile_world_rect)` /
//! `pop_layer` at compose time. Each tile's draws are clipped to
//! its own world rect; spanning primitives draw correctly without
//! over-rendering.
//!
//! ## Image cache
//!
//! Images uploaded via `scene.image_sources` are converted to
//! `peniko::ImageData` once per `ImageKey` and reused across
//! frames. Vello's internal image atlas dedups by `Blob.id()`, so
//! the same `Arc<Vec<u8>>` re-handed each frame is one upload,
//! not N. New keys are built on first sight; keys that disappear
//! from `scene.image_sources` are evicted.

use std::collections::{HashMap, HashSet};
use vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions,
    kurbo::{Affine, Rect},
    peniko::{BlendMode, Brush, Color, Compose, Fill, ImageAlphaType, ImageData, ImageFormat, Mix},
};

use netrender_device::compositor::{LayerPresent, SurfaceKey};
use netrender_device::WgpuHandles;

use crate::scene::{ImageKey, Scene, SceneBlendMode, SceneOp};
use crate::tile_cache::{TileCache, TileCoord, aabb_intersects};
use crate::vello_rasterizer::scene_to_vello_with_overrides;

/// Path (b′) per-frame state held across `render_with_compositor`
/// calls. Used to compute the four-source dirty OR for declared
/// compositor surfaces (tile-intersection / newly-declared /
/// bounds-changed / absent-last-frame).
#[derive(Default)]
struct CompositorState {
    seen_last_frame: HashSet<SurfaceKey>,
    prev_bounds: HashMap<SurfaceKey, [f32; 4]>,
}

fn map_blend_mode(b: SceneBlendMode) -> BlendMode {
    let mix = match b {
        SceneBlendMode::Normal => Mix::Normal,
        SceneBlendMode::Multiply => Mix::Multiply,
        SceneBlendMode::Screen => Mix::Screen,
        SceneBlendMode::Overlay => Mix::Overlay,
        SceneBlendMode::Darken => Mix::Darken,
        SceneBlendMode::Lighten => Mix::Lighten,
    };
    BlendMode::new(mix, Compose::SrcOver)
}

/// One vello-backed tile rasterizer. Owns the vello::Renderer, the
/// per-tile vello::Scene cache, and the per-frame peniko image data
/// cache. See module docs.
pub struct VelloTileRasterizer {
    handles: WgpuHandles,
    vello_renderer: Renderer,
    tile_scenes: HashMap<TileCoord, vello::Scene>,
    /// Persistent image data built from `scene.image_sources` (Path
    /// A, CPU bytes). Each entry holds an `Arc<Vec<u8>>` (via
    /// `peniko::Blob`) that lives across frames so vello's
    /// `Blob::id()` dedup keeps the GPU upload to once per
    /// `ImageKey`. Entries are added on first sight of a key and
    /// evicted when the key disappears from `scene.image_sources`.
    image_data: HashMap<ImageKey, ImageData>,
    /// Caller-registered GPU textures via `register_texture` (Path B).
    /// Persists across frames; entries survive until the texture is
    /// explicitly unregistered or the rasterizer is dropped.
    image_overrides: HashMap<ImageKey, ImageData>,
    last_dirty_count: usize,
    /// Retained from the most recent `tile_cache.invalidate(scene)`
    /// call, used by `build_layer_presents` to compute per-surface
    /// tile-intersection dirty bits. Cleared back to empty by
    /// `build_master_scene_timed` on each frame before being repopulated.
    last_dirty_tiles: Vec<TileCoord>,
    /// Path (b′) compositor handoff: cached internal master texture,
    /// reused frame-to-frame when `(width, height, format)` matches.
    /// Reallocated on viewport resize or format change. `None` until
    /// the first `render_to_internal_master` call.
    master_pool: Option<MasterEntry>,
    /// Allocation counter for the master texture pool (test signal).
    /// Increments on each fresh allocation; stable when the pool
    /// reuses the cached texture across frames.
    master_allocations: usize,
    /// Per-surface state across frames for the four-source dirty OR.
    compositor_state: CompositorState,
    /// Roadmap A3 — when true, `compose_master` appends a translucent
    /// red wash for tiles dirtied within `dirty_overlay_window_frames`.
    dirty_overlay_enabled: bool,
    /// Roadmap A3 — fade window in frames; opacity decays linearly
    /// from `OVERLAY_PEAK_ALPHA` at `age = 0` to `0` at `age = window`.
    dirty_overlay_window_frames: u32,
    /// Roadmap A4 — most recent frame's per-phase timings, captured
    /// in `render` / `render_to_internal_master` / `compose_into`.
    /// Cleared back to `None` on `clear_last_timings`.
    last_timings: Option<crate::profiling::FrameTimings>,
}

struct MasterEntry {
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    texture: wgpu::Texture,
}

impl VelloTileRasterizer {
    /// Construct a rasterizer over the given wgpu device. Boots a
    /// fresh `vello::Renderer` immediately; subsequent renders reuse
    /// it. Returns an error if vello pipeline construction fails.
    pub fn new(handles: WgpuHandles) -> Result<Self, vello::Error> {
        let vello_renderer = Renderer::new(
            &handles.device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )?;
        Ok(Self {
            handles,
            vello_renderer,
            tile_scenes: HashMap::new(),
            image_data: HashMap::new(),
            image_overrides: HashMap::new(),
            last_dirty_count: 0,
            last_dirty_tiles: Vec::new(),
            master_pool: None,
            master_allocations: 0,
            compositor_state: CompositorState::default(),
            dirty_overlay_enabled: false,
            dirty_overlay_window_frames: 30,
            last_timings: None,
        })
    }

    /// Roadmap A4 — return the per-phase timings captured by the most
    /// recent render call (`render` / `render_to_internal_master` /
    /// `compose_into`). `None` until the first render call returns.
    pub fn last_timings(&self) -> Option<&crate::profiling::FrameTimings> {
        self.last_timings.as_ref()
    }

    /// Roadmap A3 — toggle the tile-dirty overlay. When `enabled`,
    /// `compose_master` appends a translucent red wash on top of every
    /// tile that's been reported dirty within the last `window_frames`.
    /// `window_frames` is clamped to `>= 1` (zero would never paint).
    pub fn set_dirty_overlay(&mut self, enabled: bool, window_frames: u32) {
        self.dirty_overlay_enabled = enabled;
        self.dirty_overlay_window_frames = window_frames.max(1);
    }

    /// Roadmap A3 — read the current overlay flag (introspection helper).
    pub fn dirty_overlay_enabled(&self) -> bool {
        self.dirty_overlay_enabled
    }

    /// Roadmap A3 — read the current fade window in frames.
    pub fn dirty_overlay_window_frames(&self) -> u32 {
        self.dirty_overlay_window_frames
    }

    /// Number of times the master-texture pool allocated a fresh
    /// `wgpu::Texture` over the rasterizer's lifetime. Stays constant
    /// across consecutive `render_to_internal_master` calls at the
    /// same `(width, height, format)`; increments on viewport resize
    /// or format change.
    pub fn master_allocations(&self) -> usize {
        self.master_allocations
    }

    /// Borrow the cached master texture from the path-(b′) pool, if
    /// any. `None` until the first `render_to_internal_master` call.
    pub fn master_texture(&self) -> Option<&wgpu::Texture> {
        self.master_pool.as_ref().map(|e| &e.texture)
    }

    /// Borrow the underlying `WgpuHandles`. Used by
    /// `Renderer::render_with_compositor` to populate
    /// `PresentedFrame.handles` so the consumer can encode + submit
    /// its own GPU copies during `present_frame`.
    pub fn handles_ref(&self) -> &WgpuHandles {
        &self.handles
    }

    /// Diff `scene.compositor_surfaces` against last frame's seen
    /// state. Returns `(declares, destroys)` where:
    ///
    /// - `declares` lists `(key, bounds)` for surfaces newly added
    ///   this frame OR whose bounds changed since last frame. The
    ///   caller forwards each as a `Compositor::declare_surface`
    ///   call (idempotent on repeat keys per the trait contract).
    /// - `destroys` lists keys present last frame but absent this
    ///   frame.
    ///
    /// Pure query — does not mutate `compositor_state`. Persistence
    /// happens in [`Self::commit_compositor_state`] *after*
    /// `present_frame` returns.
    pub fn diff_compositor_surfaces(
        &self,
        scene: &Scene,
    ) -> (Vec<(SurfaceKey, [f32; 4])>, Vec<SurfaceKey>) {
        let mut declares = Vec::new();
        let mut destroys = Vec::new();

        let current_keys: HashSet<SurfaceKey> =
            scene.compositor_surfaces.iter().map(|s| s.key).collect();

        for s in &scene.compositor_surfaces {
            let prev = self.compositor_state.prev_bounds.get(&s.key).copied();
            if prev != Some(s.bounds) {
                declares.push((s.key, s.bounds));
            }
        }
        for key in &self.compositor_state.seen_last_frame {
            if !current_keys.contains(key) {
                destroys.push(*key);
            }
        }

        (declares, destroys)
    }

    /// Build the per-frame `LayerPresent` vec for `scene.compositor_surfaces`,
    /// in declaration order (vec position = z-order).
    ///
    /// `LayerPresent.dirty` ORs four sources per design doc §4:
    /// - tile-intersection: any tile in `last_dirty_tiles` intersects
    ///   the surface's bounds;
    /// - newly-declared / absent-last-frame: surface key was not in
    ///   the previous frame's seen-set;
    /// - bounds-changed: previous-frame bounds differ from current.
    ///
    /// `source_rect_in_master` clamps `surface.bounds` to the master
    /// pixel space `[0..viewport_width, 0..viewport_height)`.
    pub fn build_layer_presents(&self, scene: &Scene, tile_cache: &TileCache) -> Vec<LayerPresent> {
        let mw = scene.viewport_width as f32;
        let mh = scene.viewport_height as f32;
        scene
            .compositor_surfaces
            .iter()
            .map(|s| {
                let absent = !self.compositor_state.seen_last_frame.contains(&s.key);
                let bounds_changed =
                    self.compositor_state.prev_bounds.get(&s.key).copied() != Some(s.bounds);
                let tile_dirty = self.last_dirty_tiles.iter().any(|c| {
                    tile_cache
                        .tile_world_rect(*c)
                        .is_some_and(|tr| aabb_intersects(tr, s.bounds))
                });
                let dirty = absent || bounds_changed || tile_dirty;

                // Clamp to master pixel space; ensures x0 <= x1 and y0 <= y1
                // even if surface bounds are out-of-order (defensive).
                let clamp = |v: f32, lo: f32, hi: f32| v.max(lo).min(hi);
                let mut x0 = clamp(s.bounds[0], 0.0, mw) as u32;
                let mut y0 = clamp(s.bounds[1], 0.0, mh) as u32;
                let mut x1 = clamp(s.bounds[2], 0.0, mw) as u32;
                let mut y1 = clamp(s.bounds[3], 0.0, mh) as u32;
                if x1 < x0 {
                    std::mem::swap(&mut x0, &mut x1);
                }
                if y1 < y0 {
                    std::mem::swap(&mut y0, &mut y1);
                }

                // Compose `bounds.origin` into `world_transform` so the
                // consumer gets one transform that already places the
                // surface at its declared world position. The
                // user-supplied `s.transform` is column-major
                // `[a, b, c, d, tx, ty]` representing
                // `| a c tx |`
                // `| b d ty |`
                // `| 0 0  1 |`
                // and pre-composing a translation by
                // `(bounds.origin.x, bounds.origin.y)` yields
                // `[a, b, c, d, tx + origin.x, ty + origin.y]` —
                // the linear part is unchanged; only the translation
                // column shifts.
                //
                // Without this, every consumer that holds the surface
                // at layer-local origin (e.g. macOS CALayer) would
                // need to remember `bounds.origin` from declare and
                // re-apply it in present, which the
                // `OsCompositorBackend` trait surface doesn't even
                // carry today (declare gets dims, present gets
                // transform/clip/opacity — neither passes origin).
                // Cleaner to compose here and hand the consumer one
                // transform that's complete on its own.
                //
                // Use the original `s.bounds[0]` / `s.bounds[1]`
                // (not the clamped/swapped `x0` / `y0` above) — those
                // were normalized for `source_rect_in_master`'s
                // master-pixel-space contract; world_transform stays
                // in the user's coordinate space.
                let origin_x = s.bounds[0];
                let origin_y = s.bounds[1];
                let world_transform = [
                    s.transform[0],
                    s.transform[1],
                    s.transform[2],
                    s.transform[3],
                    s.transform[4] + origin_x,
                    s.transform[5] + origin_y,
                ];
                LayerPresent {
                    key: s.key,
                    source_rect_in_master: [x0, y0, x1, y1],
                    world_transform,
                    clip: s.clip,
                    opacity: s.opacity,
                    dirty,
                }
            })
            .collect()
    }

    /// Persist the current frame's compositor-surface state for
    /// next-frame dirty/diff computation. Call after the consumer's
    /// `present_frame` returns.
    pub fn commit_compositor_state(&mut self, scene: &Scene) {
        self.compositor_state.seen_last_frame =
            scene.compositor_surfaces.iter().map(|s| s.key).collect();
        self.compositor_state.prev_bounds = scene
            .compositor_surfaces
            .iter()
            .map(|s| (s.key, s.bounds))
            .collect();
    }

    /// Register a GPU-resident wgpu texture as an image source for
    /// subsequent `render` calls under the given `ImageKey`. The
    /// texture is handed to vello via
    /// `vello::Renderer::register_texture` (Path B from rasterizer
    /// plan §3.5); vello copies into its internal atlas every frame
    /// the image is referenced by a scene.
    ///
    /// Use this when an image source is a render-graph output (blur
    /// result, mask coverage texture, etc.) that exists only on the
    /// GPU and has no CPU-side `ImageData`. Overrides win over
    /// `scene.image_sources` entries with the same `ImageKey`.
    pub fn register_texture(&mut self, key: ImageKey, texture: wgpu::Texture) {
        let image = self.vello_renderer.register_texture(texture);
        self.image_overrides.insert(key, image);
    }

    /// Drop a previously-registered `register_texture` entry.
    /// No-op if `key` was never registered.
    pub fn unregister_texture(&mut self, key: ImageKey) {
        if let Some(image) = self.image_overrides.remove(&key) {
            self.vello_renderer.unregister_texture(image);
        }
    }

    /// Number of tiles whose Scenes were rebuilt by the last
    /// `render` call. Useful for tile-cache hit-rate assertions.
    pub fn last_dirty_count(&self) -> usize {
        self.last_dirty_count
    }

    /// Number of tile-Scenes currently held in the rasterizer's
    /// cache (one per tile present in `TileCache` at last render).
    pub fn cached_tile_count(&self) -> usize {
        self.tile_scenes.len()
    }

    /// Render `scene` into `target_view` via the tile-cache path.
    ///
    /// Steps:
    /// 1. Refresh peniko image data from `scene.image_sources` (Path
    ///    A blobs, dedup by `Blob.id()` if Arc-shared across frames).
    /// 2. `tile_cache.invalidate(scene)` → list of dirty tile coords.
    /// 3. For each dirty tile, build a filtered `vello::Scene`
    ///    containing only the primitives whose AABB intersects the
    ///    tile's world rect.
    /// 4. Evict tile-Scenes whose coords no longer appear in
    ///    `tile_cache` (handled by the tile cache's RETAIN_FRAMES
    ///    eviction).
    /// 5. Compose all cached tile-Scenes into a master Scene with
    ///    per-tile clip layers, render once.
    pub fn render(
        &mut self,
        scene: &Scene,
        tile_cache: &mut TileCache,
        target_view: &wgpu::TextureView,
        base_color: Color,
    ) -> Result<(), vello::Error> {
        use crate::profiling::{FrameTimings, Span};
        let total_span = Span::start("total");
        let mut timings = FrameTimings::empty();

        let master = self.build_master_scene_timed(scene, tile_cache, &mut timings);

        let vello_span = Span::start("vello_render");
        let result = self.vello_renderer.render_to_texture(
            &self.handles.device,
            &self.handles.queue,
            &master,
            target_view,
            &RenderParams {
                base_color,
                width: scene.viewport_width,
                height: scene.viewport_height,
                antialiasing_method: AaConfig::Area,
            },
        );
        vello_span.stop_recording(&mut timings);

        timings.total = total_span.stop();
        self.last_timings = Some(timings);
        result
    }

    /// Path (b′) entry point — render `scene` into an internal
    /// master texture pool-allocated by `(width, height, format)`,
    /// returning a reference to it. The caller (typically
    /// `Renderer::render_with_compositor`) hands this reference
    /// onward to a `Compositor::present_frame` call.
    ///
    /// The master texture is owned by the rasterizer and reused
    /// across frames at the same dimensions / format. Viewport
    /// resize or format change reallocates (visible via
    /// [`Self::master_allocations`]).
    ///
    /// `master_format` is the texture format only; the pool always
    /// allocates with `STORAGE_BINDING | TEXTURE_BINDING | COPY_SRC`
    /// usage so the consumer can use the result as a copy source.
    ///
    /// Returns `(master_texture, handles)` — both borrowed from the
    /// rasterizer. The caller uses these to construct a
    /// `PresentedFrame` for the consumer's `Compositor::present_frame`.
    /// Returning both via one `&mut self` call avoids a second borrow
    /// after the master is rendered.
    pub fn render_to_internal_master(
        &mut self,
        scene: &Scene,
        tile_cache: &mut TileCache,
        master_format: wgpu::TextureFormat,
        base_color: Color,
    ) -> Result<(&wgpu::Texture, &WgpuHandles), vello::Error> {
        use crate::profiling::{FrameTimings, Span};
        let total_span = Span::start("total");
        let mut timings = FrameTimings::empty();

        self.ensure_master_texture(scene.viewport_width, scene.viewport_height, master_format);

        let master_scene = self.build_master_scene_timed(scene, tile_cache, &mut timings);

        // The master_pool entry is guaranteed by ensure_master_texture above.
        let entry = self
            .master_pool
            .as_ref()
            .expect("master_pool guaranteed by ensure_master_texture");
        let view = entry
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let vello_span = Span::start("vello_render");
        let result = self.vello_renderer.render_to_texture(
            &self.handles.device,
            &self.handles.queue,
            &master_scene,
            &view,
            &RenderParams {
                base_color,
                width: scene.viewport_width,
                height: scene.viewport_height,
                antialiasing_method: AaConfig::Area,
            },
        );
        vello_span.stop_recording(&mut timings);
        result?;

        timings.total = total_span.stop();
        self.last_timings = Some(timings);

        Ok((&self.master_pool.as_ref().unwrap().texture, &self.handles))
    }

    fn ensure_master_texture(&mut self, width: u32, height: u32, format: wgpu::TextureFormat) {
        let needs_realloc = match &self.master_pool {
            Some(e) => e.width != width || e.height != height || e.format != format,
            None => true,
        };
        if !needs_realloc {
            return;
        }

        let texture = self
            .handles
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("netrender path-b' master"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
        self.master_pool = Some(MasterEntry {
            width,
            height,
            format,
            texture,
        });
        self.master_allocations += 1;
    }

    /// Run the same tile-cache update and master-scene composition
    /// as [`Self::render`], but append the result into a caller-
    /// provided `vello::Scene` with the given transform — instead
    /// of rendering to a texture.
    ///
    /// This is the C-architecture entry point: a caller (graphshell
    /// workbench, app-level compositor) holds a master `vello::Scene`
    /// for the whole frame and asks each consumer to compose its
    /// content into it. The caller does the single
    /// `vello::Renderer::render_to_texture` at end-of-frame; vello
    /// dedups font / image atlas slots across the appended sub-
    /// scenes via `Blob::id()`.
    ///
    /// Per `vello::Scene::append`: the operation is bytewise-cheap
    /// (per-encoding-element O(N), no GPU work), and `transform` is
    /// applied to every transform inside this rasterizer's master.
    /// Pass `Affine::IDENTITY` to compose at scene-space origin.
    ///
    /// `last_dirty_count` and `cached_tile_count` reflect the work
    /// done by this call exactly as they would for `render`.
    pub fn compose_into(
        &mut self,
        scene: &Scene,
        tile_cache: &mut TileCache,
        master: &mut vello::Scene,
        transform: Affine,
    ) {
        use crate::profiling::{FrameTimings, Span};
        let total_span = Span::start("total");
        let mut timings = FrameTimings::empty();

        let local_master = self.build_master_scene_timed(scene, tile_cache, &mut timings);

        let append_span = Span::start("master_append");
        let xform = if transform == Affine::IDENTITY {
            None
        } else {
            Some(transform)
        };
        master.append(&local_master, xform);
        append_span.stop_recording(&mut timings);

        timings.total = total_span.stop();
        self.last_timings = Some(timings);
    }

    /// Internal: tile-cache update + master-scene composition with
    /// A4 timing instrumentation. Shared by [`Self::render`],
    /// [`Self::render_to_internal_master`], and
    /// [`Self::compose_into`]; each caller wraps this in its own
    /// outer total + per-format spans (vello_render, master_append,
    /// etc.) and finalises `self.last_timings`.
    fn build_master_scene_timed(
        &mut self,
        scene: &Scene,
        tile_cache: &mut TileCache,
        timings: &mut crate::profiling::FrameTimings,
    ) -> vello::Scene {
        use crate::profiling::Span;

        let refresh_span = Span::start("refresh_image_data");
        self.refresh_image_data(scene);
        refresh_span.stop_recording(timings);

        // Build the merged Path A + Path B image map once per frame
        // (Path B overrides win on key collision). Previously this
        // ran inside build_tile_scene and re-merged for every dirty
        // tile — O(N_images × N_dirty_tiles) instead of O(N_images).
        let mut merged_images = self.image_data.clone();
        for (key, image) in &self.image_overrides {
            merged_images.insert(*key, image.clone());
        }

        let invalidate_span = Span::start("tile_invalidate");
        let dirty = tile_cache.invalidate(scene);
        invalidate_span.stop_recording(timings);
        self.last_dirty_count = dirty.len();
        self.last_dirty_tiles = dirty.clone();

        let rebuild_span = Span::start("dirty_tile_rebuild");
        for &coord in &dirty {
            let world_rect = tile_cache
                .tile_world_rect(coord)
                .expect("dirty tile must be in tile_cache");
            let filtered = filter_scene_to_tile(scene, world_rect);
            let tile_scene = scene_to_vello_with_overrides(&filtered, &merged_images);
            self.tile_scenes.insert(coord, tile_scene);
        }

        // Drop tile-Scenes whose coords were evicted from the tile
        // cache (e.g., scrolled out of viewport for RETAIN_FRAMES
        // frames).
        self.tile_scenes
            .retain(|coord, _| tile_cache.tile_world_rect(*coord).is_some());
        rebuild_span.stop_recording(timings);

        let compose_span = Span::start("master_compose");
        let master = self.compose_master(tile_cache, scene);
        compose_span.stop_recording(timings);
        master
    }

    fn refresh_image_data(&mut self, scene: &Scene) {
        // Path A blobs are Arc<Vec<u8>> wrapped in peniko::Blob.
        // Vello dedups uploads by Blob::id(), so we keep each
        // entry alive across frames — same Arc, same id, one
        // upload per ImageKey for the life of the rasterizer (or
        // until the consumer drops the key from the scene).
        for (key, data) in &scene.image_sources {
            self.image_data.entry(*key).or_insert_with(|| ImageData {
                data: data.data.clone(),
                format: ImageFormat::Rgba8,
                alpha_type: ImageAlphaType::Alpha,
                width: data.width,
                height: data.height,
            });
            // ImageKey is contractually a unique identifier for
            // its bytes (Scene::set_image_source is or_insert).
            // A size mismatch on re-encounter means the consumer
            // reused a key for different data; flag it in debug.
            debug_assert_eq!(
                (
                    self.image_data[key].width,
                    self.image_data[key].height,
                    self.image_data[key].data.len(),
                ),
                (data.width, data.height, data.data.len()),
                "ImageKey {key:#x} reused with different dimensions or byte length",
            );
        }
        // Evict cache entries whose keys disappeared from the
        // scene (e.g., scene was rebuilt and a key retired).
        self.image_data
            .retain(|key, _| scene.image_sources.contains_key(key));
    }

    /// Return the `peniko::Blob` id for the cached Path A image
    /// data under `key`, if any. Stable across frames as long as
    /// the key remains in `scene.image_sources` — used by tests
    /// to verify the cross-frame cache invariant.
    pub fn cached_image_blob_id(&self, key: ImageKey) -> Option<u64> {
        self.image_data.get(&key).map(|img| img.data.id())
    }

    fn compose_master(&self, tile_cache: &TileCache, scene: &Scene) -> vello::Scene {
        let mut master = vello::Scene::new();

        // Phase 12a' scene-level alpha + blend mode wrap. Skip the
        // outer layer when settings are at their defaults
        // (alpha = 1.0 and blend = Normal) so simple scenes don't
        // pay an extra layer.
        let scene_alpha = scene.root_alpha.clamp(0.0, 1.0);
        let scene_blend = map_blend_mode(scene.root_blend_mode);
        let needs_root_layer = scene_alpha < 1.0 || scene_blend.mix != Mix::Normal;
        if needs_root_layer {
            let viewport = Rect::new(
                0.0,
                0.0,
                scene.viewport_width as f64,
                scene.viewport_height as f64,
            );
            master.push_layer(
                Fill::NonZero,
                scene_blend,
                scene_alpha,
                Affine::IDENTITY,
                &viewport,
            );
        }

        for (coord, tile_scene) in &self.tile_scenes {
            // Get the world rect from the tile cache. If it's not
            // present (race with eviction), skip — the retain pass
            // above should have already pruned, so this is purely
            // defensive.
            let Some(world_rect) = tile_cache.tile_world_rect(*coord) else {
                continue;
            };
            let clip = Rect::new(
                world_rect[0] as f64,
                world_rect[1] as f64,
                world_rect[2] as f64,
                world_rect[3] as f64,
            );
            master.push_layer(Fill::NonZero, Mix::Normal, 1.0, Affine::IDENTITY, &clip);
            master.append(tile_scene, None);
            master.pop_layer();
        }

        if needs_root_layer {
            master.pop_layer();
        }

        // Roadmap A3 — translucent red wash on tiles dirtied within
        // the configured fade window. Painted *after* the root layer
        // pop so the overlay is not subject to scene-level alpha or
        // blend-mode wraps.
        if self.dirty_overlay_enabled {
            self.paint_dirty_overlay(&mut master, tile_cache);
        }

        master
    }

    /// Roadmap A3 — append a translucent red wash on top of every
    /// tile dirtied within `dirty_overlay_window_frames`. Opacity
    /// fades linearly with age. Caller decides whether to call this
    /// (gated on `dirty_overlay_enabled`).
    fn paint_dirty_overlay(&self, master: &mut vello::Scene, tile_cache: &TileCache) {
        // Peak alpha at age 0; decays to 0 at age = window. 0.4 is a
        // bright-enough wash to be visible over typical content
        // without obscuring it. Tune via fork if profiles surface a
        // legibility concern.
        const OVERLAY_PEAK_ALPHA: f32 = 0.4;

        let recent = tile_cache.recent_dirty_tiles(self.dirty_overlay_window_frames as u64);
        if recent.is_empty() {
            return;
        }
        for (rect, age_frac) in recent {
            let alpha = OVERLAY_PEAK_ALPHA * (1.0 - age_frac);
            let alpha_u8 = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
            if alpha_u8 == 0 {
                continue;
            }
            let color = Color::from_rgba8(255, 0, 0, alpha_u8);
            let shape = Rect::new(
                rect[0] as f64,
                rect[1] as f64,
                rect[2] as f64,
                rect[3] as f64,
            );
            master.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                &Brush::Solid(color),
                None,
                &shape,
            );
        }
    }

    /// Borrow the underlying vello::Renderer for advanced uses
    /// (e.g., `register_texture` to convert a wgpu::Texture into a
    /// peniko::ImageData usable as a scene image source). The
    /// resulting ImageData lives until `unregister_texture` is
    /// called or the rasterizer is dropped.
    pub fn vello_renderer_mut(&mut self) -> &mut Renderer {
        &mut self.vello_renderer
    }
}

/// Filter `scene`'s primitives by AABB intersection with `tile_rect`,
/// returning a new `Scene` with only the intersecting ops in their
/// original painter order. Transforms and image_sources are
/// shallow-cloned (cheap for transforms; for large image-source
/// HashMaps this is a known inefficiency, see module docs).
fn filter_scene_to_tile(scene: &Scene, tile_rect: [f32; 4]) -> Scene {
    use crate::tile_cache::{aabb_intersects, world_aabb};

    let mut filtered = Scene::new(scene.viewport_width, scene.viewport_height);
    filtered.transforms = scene.transforms.clone();
    // Fonts are cloned (Arc-shared payload — clone is cheap).
    // Resolved by font_id in emit_glyph_run; the filtered Scene
    // needs the same palette as the source.
    filtered.fonts = scene.fonts.clone();
    // Image cache is supplied by the rasterizer's image_data via
    // overrides at scene_to_vello time, so we can leave
    // image_sources empty here — saves a HashMap clone.
    debug_assert!(filtered.image_sources.is_empty());

    for op in &scene.ops {
        let intersects = match op {
            SceneOp::Rect(rect) => aabb_intersects(
                world_aabb(
                    [rect.x0, rect.y0, rect.x1, rect.y1],
                    rect.transform_id,
                    scene,
                ),
                tile_rect,
            ),
            SceneOp::Gradient(grad) => aabb_intersects(
                world_aabb(
                    [grad.x0, grad.y0, grad.x1, grad.y1],
                    grad.transform_id,
                    scene,
                ),
                tile_rect,
            ),
            SceneOp::Image(image) => aabb_intersects(
                world_aabb(
                    [image.x0, image.y0, image.x1, image.y1],
                    image.transform_id,
                    scene,
                ),
                tile_rect,
            ),
            SceneOp::Pattern(pattern) => aabb_intersects(
                world_aabb(pattern.extent, pattern.transform_id, scene),
                tile_rect,
            ),
            SceneOp::Stroke(stroke) => {
                // Inflate by half stroke width so strokes whose pen
                // reaches a tile aren't filtered out when their path
                // bounds don't.
                let half = stroke.stroke_width * 0.5;
                aabb_intersects(
                    world_aabb(
                        [
                            stroke.x0 - half,
                            stroke.y0 - half,
                            stroke.x1 + half,
                            stroke.y1 + half,
                        ],
                        stroke.transform_id,
                        scene,
                    ),
                    tile_rect,
                )
            }
            SceneOp::Shape(shape) => crate::tile_cache::world_aabb_shape(shape, scene)
                .is_some_and(|aabb| aabb_intersects(aabb, tile_rect)),
            SceneOp::GlyphRun(run) => crate::tile_cache::world_aabb_glyph_run(run, scene)
                .is_some_and(|aabb| aabb_intersects(aabb, tile_rect)),
            // Layer push/pop ops carry no visible content of their
            // own — they wrap inner ops. Always include them so the
            // filtered scene stays balanced (every PushLayer has its
            // matching PopLayer). The layer's clip narrows what
            // pixels can be touched anyway, so passing the wrap
            // through to vello is correct.
            SceneOp::PushLayer(_) | SceneOp::PopLayer => true,
        };
        if intersects {
            filtered.ops.push(op.clone());
        }
    }

    filtered
}
