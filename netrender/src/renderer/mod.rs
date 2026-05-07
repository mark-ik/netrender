/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `Renderer` shell — vello-backed.
//!
//! Public entry point: [`Renderer::render_vello`]. The renderer
//! owns a [`crate::vello_tile_rasterizer::VelloTileRasterizer`]
//! (constructed at init when `NetrenderOptions::enable_vello` is
//! true) and a [`TileCache`] (constructed when
//! `NetrenderOptions::tile_cache_size` is `Some(_)`). Both must be
//! present for `render_vello` to succeed.
//!
//! The Renderer used to host a parallel batched-WGSL rasterizer
//! (`prepare()` / `render()` returning `PreparedFrame`); that path
//! was retired in favor of a single vello pipeline. The brush
//! pipeline factories on `WgpuDevice` (brush_blur, clip_rectangle)
//! are still used by render-graph tasks, but the rasterizer-side
//! brush_solid / brush_rect_solid / brush_image / brush_gradient
//! factories are now unreachable from netrender; they're slated for
//! removal from `netrender_device` in a follow-up.
//!
//! Phase mapping after the cleanup:
//!
//! - **Phase 6** (render-graph for filters / blur / clip-mask
//!   tasks) lives on, intersecting with the rasterizer via
//!   [`Renderer::insert_image_vello`] — render-graph outputs
//!   become image sources for vello scenes.
//! - **Phase 7** picture caching is now the [`TileCache`]
//!   algorithm + the per-tile `vello::Scene` cache inside
//!   `VelloTileRasterizer`.
//! - **Phase 8** gradients (linear / radial / conic / N-stop) are
//!   `peniko::Gradient` mapped from `SceneGradient`; see
//!   `vello_rasterizer::scene_to_vello`.
//! - **Phase 9** clips are vello `push_layer` shapes (axis-aligned
//!   today; arbitrary path on Phase 9' completion).

pub(crate) mod init;

use std::sync::{Arc, Mutex};

use netrender_device::compositor::{Compositor, PresentedFrame};
use netrender_device::WgpuDevice;

use crate::scene::{ImageKey, Scene};
use crate::tile_cache::TileCache;

pub struct Renderer {
    pub wgpu_device: WgpuDevice,
    /// Phase 7: tile-cache invalidation algorithm. Configured via
    /// `NetrenderOptions::tile_cache_size`. Required for
    /// `render_vello` (the vello rasterizer holds its per-tile
    /// `vello::Scene` cache against this tile cache's coords).
    pub(crate) tile_cache: Option<Mutex<TileCache>>,
    /// Phase 7' — vello-backed tile rasterizer. Constructed at init
    /// when `NetrenderOptions::enable_vello` is true.
    pub(crate) vello_rasterizer: Option<Mutex<crate::vello_tile_rasterizer::VelloTileRasterizer>>,
}

/// Per-frame load policy on the color attachment for `render_vello`.
/// `Clear(c)` maps to vello's `RenderParams::base_color`. `Load` is
/// not supported on the vello path (vello always overwrites the
/// entire target); it's accepted for API compatibility and treated
/// as `Clear(transparent)`.
pub enum ColorLoad {
    Clear(wgpu::Color),
    Load,
}

impl Default for ColorLoad {
    fn default() -> Self {
        Self::Clear(wgpu::Color::TRANSPARENT)
    }
}

/// Pick a (pass count, per-pass step in pixels) for `brush_blur`
/// such that the cascaded 5-tap binomial kernel approximates a
/// Gaussian with σ = `blur_radius_px / 2` (the conventional CSS
/// blur-radius → σ relation).
///
/// One binomial 5-tap pass with `step = k` pixels has σ ≈ k.
/// N cascaded H+V passes accumulate: σ_total = k · √N.
///
/// We cap the per-pass step at 2 px so each pass keeps a tight tap
/// spread (avoids the visible "5-tap quantization" you get when one
/// pass's step is large relative to the feature size). Larger
/// blurs absorb the budget by running more passes.
///
/// Pass count is capped at `MAX_PASSES` for sanity — at the cap a
/// blur radius of ~28 px is achievable. Roadmap R5 lifts this cap
/// for large blurs via [`blur_kernel_plan_with_downscale`], which
/// picks a downscale level and runs the cascade at the smaller
/// resolution before upscaling back.
fn blur_kernel_plan(blur_radius_px: f32) -> (usize, f32) {
    const MAX_STEP_PX: f32 = 2.0;
    const MAX_PASSES: usize = 50;

    let target_sigma = (blur_radius_px * 0.5).max(0.5);
    if target_sigma <= MAX_STEP_PX {
        // One pass suffices; pick step = σ so the kernel covers σ.
        return (1, target_sigma);
    }
    let passes = ((target_sigma / MAX_STEP_PX).powi(2)).ceil().max(1.0) as usize;
    (passes.min(MAX_PASSES), MAX_STEP_PX)
}

/// Roadmap R5 — upgraded planner that introduces a downscale level
/// for blurs beyond what the cascade alone can reach.
///
/// Returns `(level, passes, step_px)` where `level ∈ {1, 2, 4, 8}`
/// is the resolution divisor for the blur intermediate. A `level`
/// of 1 keeps everything at native resolution (existing behavior);
/// higher levels halve the work-resolution `level` times, so the
/// effective blur radius in source-pixel units becomes
/// `step_px · √passes · level`.
///
/// Heuristic: at native resolution the cascade caps at
/// `SINGLE_LEVEL_MAX_RADIUS ≈ 28` px (50 passes at MAX_STEP_PX = 2,
/// 2σ → blur_radius). Beyond that we step down by powers of 2 so
/// the *scaled* radius stays under the cap.
///
/// `passes` and `step_px` are then `blur_kernel_plan(blur_radius_px
/// / level)` — the cascade plan for the radius as it appears at
/// the scaled resolution.
pub(crate) fn blur_kernel_plan_with_downscale(blur_radius_px: f32) -> (u32, usize, f32) {
    const SINGLE_LEVEL_MAX_RADIUS: f32 = 28.0;
    const MAX_LEVEL: u32 = 8; // Stops the level chain at quarter-quarter res.

    let level: u32 = if blur_radius_px <= SINGLE_LEVEL_MAX_RADIUS {
        1
    } else {
        let raw = (blur_radius_px / SINGLE_LEVEL_MAX_RADIUS).ceil() as u32;
        raw.next_power_of_two().min(MAX_LEVEL)
    };
    let scaled_radius = blur_radius_px / level as f32;
    let (passes, step_px) = blur_kernel_plan(scaled_radius);
    (level, passes, step_px)
}

#[cfg(test)]
mod blur_plan_tests {
    use super::blur_kernel_plan;

    #[track_caller]
    fn assert_close(actual: f32, expected: f32, tol: f32, label: &str) {
        let diff = (actual - expected).abs();
        assert!(
            diff <= tol,
            "{}: actual {}, expected {} (diff = {}, tol = {})",
            label, actual, expected, diff, tol,
        );
    }

    #[test]
    fn zero_radius_collapses_to_single_tight_pass() {
        let (passes, step) = blur_kernel_plan(0.0);
        assert_eq!(passes, 1);
        assert_close(step, 0.5, 0.01, "step at radius 0 floors to 0.5");
    }

    #[test]
    fn small_radius_uses_one_pass_with_step_eq_sigma() {
        let (passes, step) = blur_kernel_plan(2.0); // σ_target = 1.0
        assert_eq!(passes, 1);
        assert_close(step, 1.0, 0.01, "step matches target σ when σ ≤ 2");
    }

    #[test]
    fn radius_at_step_cap_still_one_pass() {
        let (passes, step) = blur_kernel_plan(4.0); // σ_target = 2.0
        assert_eq!(passes, 1);
        assert_close(step, 2.0, 0.01, "σ at MAX_STEP_PX still single-pass");
    }

    #[test]
    fn large_radius_cascades() {
        // σ_target = 5, MAX_STEP_PX = 2 → passes = ceil((5/2)²) = 7
        let (passes, step) = blur_kernel_plan(10.0);
        assert_eq!(passes, 7);
        assert_close(step, 2.0, 0.01, "step pinned at MAX_STEP_PX for cascaded");

        // σ_total = step·√passes ≈ 2·√7 ≈ 5.29 ≥ target 5.0
        let actual_sigma = step * (passes as f32).sqrt();
        assert!(
            actual_sigma >= 5.0,
            "cascaded σ {} should reach target 5.0",
            actual_sigma,
        );
    }

    #[test]
    fn pass_count_capped() {
        let (passes, _) = blur_kernel_plan(1000.0);
        assert!(
            passes <= 50,
            "MAX_PASSES = 50 should cap unbounded radii; got {}",
            passes,
        );
    }

    use super::blur_kernel_plan_with_downscale;

    #[test]
    fn pr5_small_radius_keeps_level_one() {
        let (level, _, _) = blur_kernel_plan_with_downscale(8.0);
        assert_eq!(level, 1, "small radii skip downscale");
        let (level, _, _) = blur_kernel_plan_with_downscale(28.0);
        assert_eq!(level, 1, "exactly at the cap stays at level 1");
    }

    #[test]
    fn pr5_medium_radius_picks_level_two() {
        // Radii between 28 and 56 round up to level 2 (next power
        // of 2 above ceil(radius / 28)).
        let (level, _, _) = blur_kernel_plan_with_downscale(40.0);
        assert_eq!(level, 2, "radius 40 picks level 2");
    }

    #[test]
    fn pr5_large_radius_picks_higher_level() {
        let (level, _, _) = blur_kernel_plan_with_downscale(100.0);
        assert!(level >= 4, "radius 100 picks level ≥ 4, got {level}");
        let (level, _, _) = blur_kernel_plan_with_downscale(1000.0);
        assert!(level <= 8, "level capped at 8, got {level}");
    }

    #[test]
    fn pr5_passes_stay_unclipped_for_realistic_radii() {
        // At every level chosen by the heuristic, for radii up to
        // MAX_LEVEL * SINGLE_LEVEL_MAX_RADIUS = 8 * 28 = 224, the
        // scaled cascade should stay within the 50-pass cap. Beyond
        // 224 the downscale heuristic clamps at level 8 and σ-clip
        // returns; documenting that as a known limit.
        for &r in &[8.0_f32, 28.0, 40.0, 64.0, 100.0, 200.0, 224.0] {
            let (level, passes, _) = blur_kernel_plan_with_downscale(r);
            assert!(
                passes <= 50,
                "radius {r}: at level {level}, passes = {passes} exceeds MAX_PASSES"
            );
            // For radii in this range the cascade should not be at
            // the cap (it has headroom).
            if r <= 200.0 {
                assert!(
                    passes < 50,
                    "radius {r}: at level {level}, passes = {passes} should be below the 50-pass cap (downscale path has headroom)"
                );
            }
        }
    }
}

impl Renderer {
    /// Borrow the tile cache mutex (used by tests for invalidation
    /// inspection). Returns `None` if `tile_cache_size` was `None`.
    pub fn tile_cache(&self) -> Option<&Mutex<TileCache>> {
        self.tile_cache.as_ref()
    }

    /// Phase 11c' — build a blurred rounded-rect coverage texture
    /// suitable for use as a CSS-style box-shadow mask, register
    /// it under `key`, and make it addressable from subsequent
    /// `render_vello` calls.
    ///
    /// The caller composites by referencing `key` in a
    /// [`Scene::push_image_full`] (or `_rounded`) call with a
    /// chromatic tint matching the desired shadow color. The
    /// shadow's "spread" is encoded via the size of `bounds`; its
    /// "blur" is encoded via the blur step (typically `1 / DIM`
    /// for a 5-tap effective radius); its "offset" is encoded by
    /// where the user composites the mask.
    ///
    /// # Internals
    ///
    /// Runs a (1 + 2N)-task render graph:
    ///   1. `cs_clip_rectangle` writes a coverage mask matching
    ///      `bounds` + `corner_radius` into a fresh
    ///      `Rgba8Unorm` `dim × dim` texture.
    ///   2. N pairs of separable `brush_blur` passes (H then V),
    ///      each pass running the 5-tap binomial kernel. N and the
    ///      per-pass step are chosen by `blur_kernel_plan` so the
    ///      cumulative Gaussian σ matches `blur_radius_px / 2`
    ///      (the standard CSS blur-radius → σ relation).
    ///
    /// `blur_radius_px` is in target-pixel units: `0.0` is no
    /// blur (single tight pass), `8.0` matches a CSS
    /// `box-shadow: 0 0 8px` shadow's spread, and so on.
    ///
    /// The final texture is registered with the vello rasterizer
    /// via `insert_image_vello`.
    ///
    /// # Panics
    ///
    /// If `enable_vello` was false at construction.
    pub fn build_box_shadow_mask(
        &self,
        key: ImageKey,
        dim: u32,
        bounds: [f32; 4],
        corner_radius: f32,
        blur_radius_px: f32,
    ) {
        use crate::filter::{blur_pass_callback, clip_rectangle_callback, make_bilinear_sampler};
        use crate::render_graph::{RenderGraph, Task, TaskId};

        let device = self.wgpu_device.core.device.clone();
        let queue = self.wgpu_device.core.queue.clone();

        let mask_format = wgpu::TextureFormat::Rgba8Unorm;
        let clip_pipe = self.wgpu_device.ensure_clip_rectangle(mask_format, true);
        let blur_pipe = self.wgpu_device.ensure_brush_blur(mask_format);
        let sampler = make_bilinear_sampler(&device);

        let (level, passes, step_px) = blur_kernel_plan_with_downscale(blur_radius_px);
        // Roadmap R5 — large blurs run at a downscaled work
        // resolution, then upscale to the target. The cascade runs
        // at `scaled_dim`; a `step_px` pixel at scaled_dim is
        // `level * step_px` pixels at full dim, so the effective
        // σ scales accordingly.
        let scaled_dim = (dim / level).max(1);
        let scaled_extent = wgpu::Extent3d {
            width: scaled_dim,
            height: scaled_dim,
            depth_or_array_layers: 1,
        };
        let full_extent = wgpu::Extent3d {
            width: dim,
            height: dim,
            depth_or_array_layers: 1,
        };
        let step_uv = step_px / scaled_dim as f32;

        const MASK: TaskId = 1;
        let mut graph = RenderGraph::new();
        graph.push(Task {
            id: MASK,
            extent: full_extent,
            format: mask_format,
            inputs: vec![],
            encode: clip_rectangle_callback(clip_pipe, bounds, corner_radius),
        });

        // R5 — when level > 1, prepend a downscale task that reads
        // the full-resolution mask and writes it at scaled_dim. We
        // implement the downscale as a brush_blur pass with step=0:
        // five taps at the same UV → effectively a bilinear sample
        // of the source at the target's resolution. The bilinear
        // filter on the input texture acts as the box-filter
        // pre-AA expected of a 2x downscale.
        let mut prev: TaskId = MASK;
        let mut next_id: TaskId = MASK + 1;
        if level > 1 {
            let down_id = next_id;
            graph.push(Task {
                id: down_id,
                extent: scaled_extent,
                format: mask_format,
                inputs: vec![prev],
                encode: blur_pass_callback(blur_pipe.clone(), Arc::clone(&sampler), 0.0, 0.0),
            });
            prev = down_id;
            next_id += 1;
        }

        // Chain N H+V blur pairs at scaled_extent. The first H pass
        // reads the (downscaled) mask.
        for _ in 0..passes {
            let h_id = next_id;
            graph.push(Task {
                id: h_id,
                extent: scaled_extent,
                format: mask_format,
                inputs: vec![prev],
                encode: blur_pass_callback(blur_pipe.clone(), Arc::clone(&sampler), step_uv, 0.0),
            });
            let v_id = h_id + 1;
            graph.push(Task {
                id: v_id,
                extent: scaled_extent,
                format: mask_format,
                inputs: vec![h_id],
                encode: blur_pass_callback(blur_pipe.clone(), Arc::clone(&sampler), 0.0, step_uv),
            });
            prev = v_id;
            next_id = v_id + 1;
        }

        // R5 — when level > 1, append an upscale task that reads
        // the blurred scaled-resolution texture and writes at full
        // dim. Same brush_blur(step=0) trick; bilinear filter
        // smooths the upsample.
        if level > 1 {
            let up_id = next_id;
            graph.push(Task {
                id: up_id,
                extent: full_extent,
                format: mask_format,
                inputs: vec![prev],
                encode: blur_pass_callback(blur_pipe.clone(), Arc::clone(&sampler), 0.0, 0.0),
            });
            prev = up_id;
        }

        let mut outputs = graph.execute(&device, &queue, std::collections::HashMap::new());
        let blurred = outputs.remove(&prev).expect("final blur-pass output");
        self.insert_image_vello(key, Arc::new(blurred));
    }

    /// Register a GPU-resident wgpu texture as an image source for
    /// subsequent `render_vello` calls under the given `ImageKey`.
    /// Render-graph outputs (blur results, mask coverage textures,
    /// etc.) become addressable from within a vello scene's
    /// `SceneImage` primitives via this entry point.
    ///
    /// The texture is cloned (cheap — `wgpu::Texture` is internally
    /// Arc-shared) and handed to `vello::Renderer::register_texture`
    /// (Path B from rasterizer plan §3.5). Entries persist across
    /// `render_vello` calls until `unregister_image_vello` is
    /// called or the renderer is dropped. Overrides win over
    /// `scene.image_sources` entries with the same `ImageKey`.
    ///
    /// # Panics
    ///
    /// If `enable_vello` was false at construction.
    pub fn insert_image_vello(&self, key: ImageKey, texture: Arc<wgpu::Texture>) {
        let rast_mutex = self
            .vello_rasterizer
            .as_ref()
            .expect("Renderer::insert_image_vello requires enable_vello = true");
        let mut rast = rast_mutex.lock().expect("vello_rasterizer lock");
        rast.register_texture(key, (*texture).clone());
    }

    /// Drop a previously-registered `insert_image_vello` entry.
    /// No-op if `key` was never registered or `enable_vello` is
    /// false.
    pub fn unregister_image_vello(&self, key: ImageKey) {
        let Some(rast_mutex) = self.vello_rasterizer.as_ref() else { return };
        let mut rast = rast_mutex.lock().expect("vello_rasterizer lock");
        rast.unregister_texture(key);
    }

    /// Number of tiles whose `vello::Scene`s were rebuilt during
    /// the most recent `render_vello` call. `0` after a no-op
    /// frame (unchanged scene). Returns `None` if `enable_vello`
    /// was false.
    pub fn vello_last_dirty_count(&self) -> Option<usize> {
        let rast_mutex = self.vello_rasterizer.as_ref()?;
        let rast = rast_mutex.lock().expect("vello_rasterizer lock");
        Some(rast.last_dirty_count())
    }

    /// Number of tile-Scenes currently held in the vello
    /// rasterizer's cache. Returns `None` if `enable_vello` was
    /// false.
    pub fn vello_cached_tile_count(&self) -> Option<usize> {
        let rast_mutex = self.vello_rasterizer.as_ref()?;
        let rast = rast_mutex.lock().expect("vello_rasterizer lock");
        Some(rast.cached_tile_count())
    }

    /// Render `scene` into `target_view` via the vello-backed tile
    /// rasterizer.
    ///
    /// Steps (all internal):
    /// 1. `tile_cache.invalidate(scene)` → list of dirty tile coords.
    /// 2. For each dirty tile, build a filtered `vello::Scene`
    ///    containing only the primitives whose AABB intersects the
    ///    tile's world rect.
    /// 3. Compose all cached tile-Scenes into a master Scene with
    ///    per-tile clip layers.
    /// 4. One `vello::Renderer::render_to_texture` call.
    ///
    /// `clear` controls the base color. `Clear(c)` is the typical
    /// case; `Load` is not supported by vello's compute pipeline
    /// (which always overwrites the entire target) and is treated
    /// as `Clear(transparent)` for API compatibility.
    ///
    /// # Panics
    ///
    /// - If `enable_vello` was false at construction.
    /// - If `tile_cache_size` was `None` at construction.
    /// - If a vello render error occurs (mirrors the existing
    ///   `render()` shape, which doesn't return a Result).
    pub fn render_vello(
        &self,
        scene: &Scene,
        target_view: &wgpu::TextureView,
        clear: ColorLoad,
    ) {
        let rast_mutex = self
            .vello_rasterizer
            .as_ref()
            .expect("Renderer::render_vello requires NetrenderOptions::enable_vello = true");
        let tc_mutex = self
            .tile_cache
            .as_ref()
            .expect("Renderer::render_vello requires NetrenderOptions::tile_cache_size = Some(_)");

        let base = match clear {
            ColorLoad::Clear(c) => vello::peniko::Color::new([
                c.r as f32, c.g as f32, c.b as f32, c.a as f32,
            ]),
            ColorLoad::Load => vello::peniko::Color::new([0.0, 0.0, 0.0, 0.0]),
        };

        let mut rast = rast_mutex.lock().expect("vello_rasterizer lock");
        let mut tc = tc_mutex.lock().expect("tile_cache lock");
        rast.render(scene, &mut tc, target_view, base)
            .unwrap_or_else(|e| panic!("vello render_to_texture failed: {:?}", e));
    }

    /// Number of times the path-(b′) master-texture pool has
    /// allocated a fresh `wgpu::Texture` over this Renderer's
    /// lifetime. Returns `None` if `enable_vello` was false.
    ///
    /// Test signal: stable across consecutive `render_with_compositor`
    /// calls at the same viewport / format; increments on resize or
    /// format change.
    pub fn vello_master_allocations(&self) -> Option<usize> {
        let rast_mutex = self.vello_rasterizer.as_ref()?;
        let rast = rast_mutex.lock().expect("vello_rasterizer lock");
        Some(rast.master_allocations())
    }

    /// Roadmap A4 — return the per-phase timings captured by the
    /// most recent `render_vello` / `render_with_compositor` /
    /// `compose_into` call. Returns `None` if `enable_vello` was
    /// false or no timed render has run yet.
    ///
    /// Spans currently captured:
    ///
    /// - `refresh_image_data` — Path A image cache refresh.
    /// - `tile_invalidate` — `TileCache::invalidate(scene)`.
    /// - `dirty_tile_rebuild` — per-dirty-tile filter + WGSL-style
    ///   translation into per-tile vello scenes.
    /// - `master_compose` — building the master `vello::Scene`.
    /// - `vello_render` (only on render paths that submit to GPU)
    ///   — `vello::Renderer::render_to_texture`.
    /// - `master_append` (only on `compose_into`) —
    ///   `vello::Scene::append`.
    ///
    /// Plus a `total` wall-clock duration on `FrameTimings` itself.
    pub fn last_frame_timings(&self) -> Option<crate::profiling::FrameTimings> {
        let rast_mutex = self.vello_rasterizer.as_ref()?;
        let rast = rast_mutex.lock().expect("vello_rasterizer lock");
        rast.last_timings().cloned()
    }

    /// Path (b′) entry point — render `scene` into an internal
    /// master texture (pool-allocated by `(width, height,
    /// master_format)` on the rasterizer), forward declare/destroy
    /// surface lifecycle events to `compositor`, then hand the
    /// master texture and per-surface `LayerPresent` payload to the
    /// consumer via [`Compositor::present_frame`].
    ///
    /// Per-frame ordering:
    /// 1. Render scene to internal master.
    /// 2. Compute the surface diff against last frame's state.
    /// 3. Emit `destroy_surface` for keys present last frame but
    ///    absent now; emit `declare_surface` for new + bounds-
    ///    changed keys (idempotent per the trait contract).
    /// 4. Build per-surface `LayerPresent` with the four-source
    ///    dirty OR (tile-intersection / absent-last-frame /
    ///    bounds-changed) computed inline.
    /// 5. Call `present_frame` so the consumer can blit dirty
    ///    surface regions and route native textures to the OS.
    /// 6. Commit the frame's surface state to the rasterizer for
    ///    next-frame diff.
    ///
    /// `master_format` must match the format of the consumer-owned
    /// destination textures: `copy_texture_to_texture` requires
    /// identical formats. `wgpu::TextureFormat::Rgba8Unorm` is the
    /// graphshell-shaped consumer default. See design doc §8(1) for
    /// the BGRA-storage caveat on native-compositor paths.
    ///
    /// See
    /// [`netrender-notes/2026-05-05_compositor_handoff_path_b_prime.md`](../../netrender-notes/2026-05-05_compositor_handoff_path_b_prime.md)
    /// for the design.
    ///
    /// # Panics
    ///
    /// - If `enable_vello` was false at construction.
    /// - If `tile_cache_size` was `None` at construction.
    /// - If a vello render error occurs.
    pub fn render_with_compositor(
        &self,
        scene: &Scene,
        master_format: wgpu::TextureFormat,
        compositor: &mut dyn Compositor,
        base_color: vello::peniko::Color,
    ) {
        let rast_mutex = self.vello_rasterizer.as_ref().expect(
            "Renderer::render_with_compositor requires NetrenderOptions::enable_vello = true",
        );
        let tc_mutex = self.tile_cache.as_ref().expect(
            "Renderer::render_with_compositor requires NetrenderOptions::tile_cache_size = Some(_)",
        );

        let mut rast = rast_mutex.lock().expect("vello_rasterizer lock");
        let mut tc = tc_mutex.lock().expect("tile_cache lock");

        // 1. Render the scene into the rasterizer's pool-allocated master.
        rast.render_to_internal_master(scene, &mut tc, master_format, base_color)
            .unwrap_or_else(|e| panic!("vello render_to_texture failed: {:?}", e));

        // 2. Diff surface lifecycle against last frame.
        let (declares, destroys) = rast.diff_compositor_surfaces(scene);

        // 3. Forward lifecycle events. Destroys first so consumer can
        // free old destination textures before any new declares
        // potentially reuse keys (re-declare with same key after
        // destroy is a valid pattern, though the diff doesn't currently
        // emit that case — declares only fire when bounds differ).
        for key in &destroys {
            compositor.destroy_surface(*key);
        }
        for (key, bounds) in &declares {
            compositor.declare_surface(*key, *bounds);
        }

        // 4. Build LayerPresent vec.
        let layers = rast.build_layer_presents(scene, &tc);

        // 5. Hand off. Re-borrow master + handles after lifecycle calls
        // (which used &self) so the &mut self borrow for the present
        // payload is fresh.
        let master_texture = rast
            .master_texture()
            .expect("master_pool guaranteed by render_to_internal_master above");
        let handles = rast.handles_ref();
        compositor.present_frame(PresentedFrame {
            master: master_texture,
            handles,
            layers: &layers,
        });

        // 6. Persist surface state for next frame.
        rast.commit_compositor_state(scene);
    }
}

#[derive(Debug)]
pub enum RendererError {
    WgpuFeaturesMissing(wgpu::Features),
    /// `NetrenderOptions::enable_vello = true` requires
    /// `tile_cache_size = Some(_)`. The vello rasterizer holds the
    /// per-tile `vello::Scene` cache against the tile cache's
    /// coords; without a tile cache there's nothing for it to cache
    /// against.
    VelloRequiresTileCache,
    /// `vello::Renderer` construction failed during
    /// `create_netrender_instance`. The wrapped string is vello's
    /// error formatted via `{:?}` (vello::Error doesn't implement
    /// `std::error::Error` in 0.8 — the string is informational
    /// only).
    VelloInit(String),
}
