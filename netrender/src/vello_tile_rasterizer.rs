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
//! GPU-side cache; tile_cache stays rasterizer-agnostic. The
//! `Tile.texture` field on the existing `Tile` struct is unused on
//! this path (kept for now so the existing batched tile cache stays
//! working in parallel; cleanup is a future commit).
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
//! `peniko::ImageData` once per frame and shared across all
//! tile-Scenes via `scene_to_vello_with_overrides`. Vello's
//! internal image atlas dedups by `Blob.id()`, so re-handing the
//! same `Arc<Vec<u8>>` across frames is one upload, not N.

use std::collections::HashMap;
use std::sync::Arc;

use vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions,
    kurbo::{Affine, Rect},
    peniko::{BlendMode, Blob, Color, Compose, Fill, ImageAlphaType, ImageData, ImageFormat, Mix},
};

use netrender_device::WgpuHandles;

use crate::scene::{ImageKey, Scene, SceneBlendMode};
use crate::tile_cache::{TileCache, TileCoord};
use crate::vello_rasterizer::scene_to_vello_with_overrides;

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
    /// Per-frame image data built from `scene.image_sources` (Path A,
    /// CPU bytes). Cleared and refreshed at every `render` call.
    image_data: HashMap<ImageKey, ImageData>,
    /// Caller-registered GPU textures via `register_texture` (Path B).
    /// Persists across frames; entries survive until the texture is
    /// explicitly unregistered or the rasterizer is dropped.
    image_overrides: HashMap<ImageKey, ImageData>,
    last_dirty_count: usize,
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
        })
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
        self.refresh_image_data(scene);

        // Build the merged Path A + Path B image map once per frame
        // (Path B overrides win on key collision). Previously this
        // ran inside build_tile_scene and re-merged for every dirty
        // tile — O(N_images × N_dirty_tiles) instead of O(N_images).
        let mut merged_images = self.image_data.clone();
        for (key, image) in &self.image_overrides {
            merged_images.insert(*key, image.clone());
        }

        let dirty = tile_cache.invalidate(scene);
        self.last_dirty_count = dirty.len();

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

        let master = self.compose_master(tile_cache, scene);

        self.vello_renderer.render_to_texture(
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
        )
    }

    fn refresh_image_data(&mut self, scene: &Scene) {
        // For Path A blobs we hand peniko an Arc<Vec<u8>>. Vello
        // dedups across frames by Blob::id() — but only when the
        // *same* Arc is handed in. Building a fresh Arc per frame
        // (as today) defeats that dedup; revisit when image-heavy
        // scenes show up in profiles.
        self.image_data.clear();
        self.image_data.reserve(scene.image_sources.len());
        for (key, data) in &scene.image_sources {
            let blob = Blob::new(Arc::new(data.bytes.clone()));
            self.image_data.insert(
                *key,
                ImageData {
                    data: blob,
                    format: ImageFormat::Rgba8,
                    alpha_type: ImageAlphaType::Alpha,
                    width: data.width,
                    height: data.height,
                },
            );
        }
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

        master
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
/// returning a new `Scene` with only the intersecting primitives.
/// Transforms and image_sources are shallow-cloned (cheap for
/// transforms; for large image-source HashMaps this is a known
/// inefficiency, see module docs).
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

    for rect in &scene.rects {
        let aabb = world_aabb(
            [rect.x0, rect.y0, rect.x1, rect.y1],
            rect.transform_id,
            scene,
        );
        if aabb_intersects(aabb, tile_rect) {
            filtered.rects.push(rect.clone());
        }
    }
    for grad in &scene.gradients {
        let aabb = world_aabb([grad.x0, grad.y0, grad.x1, grad.y1], grad.transform_id, scene);
        if aabb_intersects(aabb, tile_rect) {
            filtered.gradients.push(grad.clone());
        }
    }
    for image in &scene.images {
        let aabb = world_aabb(
            [image.x0, image.y0, image.x1, image.y1],
            image.transform_id,
            scene,
        );
        if aabb_intersects(aabb, tile_rect) {
            filtered.images.push(image.clone());
        }
    }
    for stroke in &scene.strokes {
        // Inflate by half stroke width so strokes whose pen reaches
        // a tile aren't filtered out when their path bounds don't.
        let half = stroke.stroke_width * 0.5;
        let aabb = world_aabb(
            [stroke.x0 - half, stroke.y0 - half, stroke.x1 + half, stroke.y1 + half],
            stroke.transform_id,
            scene,
        );
        if aabb_intersects(aabb, tile_rect) {
            filtered.strokes.push(stroke.clone());
        }
    }
    for shape in &scene.shapes {
        if let Some(aabb) = crate::tile_cache::world_aabb_shape(shape, scene) {
            if aabb_intersects(aabb, tile_rect) {
                filtered.shapes.push(shape.clone());
            }
        }
    }
    for run in &scene.glyph_runs {
        if let Some(aabb) = crate::tile_cache::world_aabb_glyph_run(run, scene) {
            if aabb_intersects(aabb, tile_rect) {
                filtered.glyph_runs.push(run.clone());
            }
        }
    }

    filtered
}

