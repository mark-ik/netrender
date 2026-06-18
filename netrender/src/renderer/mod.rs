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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use netrender_device::compositor::{Compositor, PresentedFrame};
use netrender_device::WgpuDevice;

use crate::external_texture::{
    ExternalTextureComposite, ExternalTexturePipeline, ExternalTexturePlacement,
};
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
    /// Per-target-format pipeline cache for zero-copy external
    /// texture overlays.
    pub(crate) external_texture_pipelines:
        Mutex<HashMap<wgpu::TextureFormat, ExternalTexturePipeline>>,
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

fn scene_tail_fragment(scene: &Scene, scene_op_boundary: usize) -> Scene {
    let mut fragment = scene.clone();
    fragment.ops = scene.ops[scene_op_boundary.min(scene.ops.len())..].to_vec();
    fragment.compositor_surfaces.clear();
    fragment
}

fn make_external_tail_target(
    device: &wgpu::Device,
    viewport_width: u32,
    viewport_height: u32,
    format: wgpu::TextureFormat,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("netrender external texture ordered tail"),
        size: wgpu::Extent3d {
            width: viewport_width,
            height: viewport_height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
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

/// Roadmap D1 — does this scene have any layer with a
/// `backdrop_filter` set? Used by `render_vello` to decide whether
/// to take the no-backdrop fast path or the multi-pass path.
fn has_backdrop_filter(scene: &Scene) -> bool {
    scene
        .ops
        .iter()
        .any(|op| matches!(op, crate::scene::SceneOp::PushLayer(l) if l.backdrop_filter.is_some()))
}

/// Roadmap D1 — build a "prefix scene" containing every op before
/// the layer at `cutoff_idx`, with any unclosed `PushLayer` scopes
/// closed by appending `PopLayer` ops so the prefix is balanced.
/// Reuses the parent scene's transforms, fonts, and image_sources
/// (cheap; image data is `peniko::Blob` Arc-shares).
fn build_prefix_scene(scene: &Scene, cutoff_idx: usize) -> Scene {
    use crate::scene::SceneOp;
    let mut prefix = Scene::new(scene.viewport_width, scene.viewport_height);
    prefix.transforms = scene.transforms.clone();
    prefix.fonts = scene.fonts.clone();
    prefix.image_sources = scene.image_sources.clone();
    prefix.root_alpha = scene.root_alpha;
    prefix.root_blend_mode = scene.root_blend_mode;
    prefix.ops = scene.ops[..cutoff_idx].to_vec();
    // Strip any backdrop_filter from prefix layers — D1 first-cut
    // doesn't recurse into nested filters; later filters processed
    // independently see the unfiltered prefix.
    for op in &mut prefix.ops {
        if let SceneOp::PushLayer(l) = op {
            l.backdrop_filter = None;
        }
    }
    // Balance unclosed PushLayer scopes by appending PopLayer ops.
    let mut depth: i32 = 0;
    for op in &prefix.ops {
        match op {
            SceneOp::PushLayer(_) => depth += 1,
            SceneOp::PopLayer => depth -= 1,
            _ => {}
        }
    }
    for _ in 0..depth.max(0) {
        prefix.ops.push(SceneOp::PopLayer);
    }
    prefix
}

/// Sentinel `ImageKey` region for backdrop-filter results — the top of the u64
/// space (same convention as the `u64::MAX` font sentinel), counting down.
const BACKDROP_FILTER_KEY_BASE: ImageKey = u64::MAX - 1;
/// Sentinel `ImageKey` region for element-filter results, counting down from the
/// midpoint — disjoint from [`BACKDROP_FILTER_KEY_BASE`] so the two passes can't
/// collide, and a key above this marks a backdrop image (used to keep element
/// `filter` from filtering the backdrop).
const ELEMENT_FILTER_KEY_BASE: ImageKey = u64::MAX / 2;

/// True if any layer carries a non-empty CSS `filter` chain (element filter).
fn has_element_filter(scene: &Scene) -> bool {
    use crate::scene::SceneOp;
    scene
        .ops
        .iter()
        .any(|op| matches!(op, SceneOp::PushLayer(l) if !l.filters.is_empty()))
}

/// Index of the `PopLayer` that matches the `PushLayer` at `push_idx` (depth
/// counting). `None` if the scene is unbalanced (a consumer bug).
fn matching_pop(ops: &[crate::scene::SceneOp], push_idx: usize) -> Option<usize> {
    use crate::scene::SceneOp;
    let mut depth: i32 = 0;
    for (i, op) in ops.iter().enumerate().skip(push_idx) {
        match op {
            SceneOp::PushLayer(_) => depth += 1, // counts push_idx itself -> depth 1
            SceneOp::PopLayer => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            },
            _ => {},
        }
    }
    None
}

/// The layer's *own content* — the ops strictly between `push_idx` and
/// `pop_idx` — as a flat sub-scene (the same palette clones as
/// [`build_prefix_scene`], so inner `transform_id`/`font_id`/`key` indices stay
/// valid). The outer `PushLayer`/`PopLayer` are excluded: the layer's
/// alpha/blend/clip get re-applied when the filtered image composites back.
/// Nested filters/backdrops are stripped so this first cut does not recurse.
fn build_layer_content_scene(scene: &Scene, push_idx: usize, pop_idx: usize) -> Scene {
    use crate::scene::SceneOp;
    let mut content = Scene::new(scene.viewport_width, scene.viewport_height);
    content.transforms = scene.transforms.clone();
    content.fonts = scene.fonts.clone();
    content.image_sources = scene.image_sources.clone();
    content.root_alpha = scene.root_alpha;
    content.root_blend_mode = scene.root_blend_mode;
    // The layer's own content, minus any backdrop-filter image the backdrop pass
    // injected as the first content op: CSS `filter` must affect only the
    // element, never the backdrop. Backdrop sentinel keys sit above the element
    // region, so a high key marks the injected backdrop image. (It stays in the
    // outer scene, behind the element's filtered result — see the splice below.)
    content.ops = scene.ops[push_idx + 1..pop_idx]
        .iter()
        .filter(|op| !matches!(op, SceneOp::Image(im) if im.key > ELEMENT_FILTER_KEY_BASE))
        .cloned()
        .collect();
    for op in &mut content.ops {
        if let SceneOp::PushLayer(l) = op {
            l.backdrop_filter = None;
            l.filters.clear();
        }
    }
    // The interior is already balanced; this is a defensive no-op.
    let mut depth: i32 = 0;
    for op in &content.ops {
        match op {
            SceneOp::PushLayer(_) => depth += 1,
            SceneOp::PopLayer => depth -= 1,
            _ => {},
        }
    }
    for _ in 0..depth.max(0) {
        content.ops.push(SceneOp::PopLayer);
    }
    content
}

/// Identity color matrix (row-major 4x5; out RGBA = M * [r,g,b,a,1]).
const FILTER_IDENTITY: [f32; 20] = [
    1.0, 0.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 0.0, 1.0, 0.0,
];

/// CSS Filter Effects L1 shorthand color functions as a 4x5 color matrix
/// (row-major; out RGBA = M * [r,g,b,a,1]). Operates on **sRGB-encoded**,
/// **straight** (non-premultiplied) color — the `cs_color_matrix` shader does
/// the unpremultiply/clamp/re-premultiply around it. `Blur` is a spatial pass
/// (not a matrix); the caller dispatches it separately, so it maps to identity
/// here. Coefficients per the spec: saturate/hue-rotate use 0.213/0.715/0.072;
/// grayscale uses the BT.709 0.2126/0.7152/0.0722; identity amounts are 1.0 for
/// brightness/contrast/saturate and 0.0 for grayscale/sepia/invert/hue-rotate.
fn scene_filter_to_matrix(f: crate::scene::SceneFilter) -> [f32; 20] {
    use crate::scene::SceneFilter as F;
    match f {
        F::Blur(_) => FILTER_IDENTITY,
        F::Grayscale(a) => {
            let s = 1.0 - a.clamp(0.0, 1.0);
            [
                0.2126 + 0.7874 * s, 0.7152 - 0.7152 * s, 0.0722 - 0.0722 * s, 0.0, 0.0, //
                0.2126 - 0.2126 * s, 0.7152 + 0.2848 * s, 0.0722 - 0.0722 * s, 0.0, 0.0, //
                0.2126 - 0.2126 * s, 0.7152 - 0.7152 * s, 0.0722 + 0.9278 * s, 0.0, 0.0, //
                0.0, 0.0, 0.0, 1.0, 0.0,
            ]
        },
        F::Sepia(a) => {
            let s = 1.0 - a.clamp(0.0, 1.0);
            [
                0.393 + 0.607 * s, 0.769 - 0.769 * s, 0.189 - 0.189 * s, 0.0, 0.0, //
                0.349 - 0.349 * s, 0.686 + 0.314 * s, 0.168 - 0.168 * s, 0.0, 0.0, //
                0.272 - 0.272 * s, 0.534 - 0.534 * s, 0.131 + 0.869 * s, 0.0, 0.0, //
                0.0, 0.0, 0.0, 1.0, 0.0,
            ]
        },
        F::Saturate(a) => {
            let s = a.max(0.0);
            [
                0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s, 0.0, 0.0, //
                0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s, 0.0, 0.0, //
                0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s, 0.0, 0.0, //
                0.0, 0.0, 0.0, 1.0, 0.0,
            ]
        },
        F::HueRotate(deg) => {
            let t = deg.to_radians();
            let (c, n) = (t.cos(), t.sin());
            [
                0.213 + c * 0.787 - n * 0.213, 0.715 - c * 0.715 - n * 0.715, 0.072 - c * 0.072 + n * 0.928, 0.0, 0.0, //
                0.213 - c * 0.213 + n * 0.143, 0.715 + c * 0.285 + n * 0.140, 0.072 - c * 0.072 - n * 0.283, 0.0, 0.0, //
                0.213 - c * 0.213 - n * 0.787, 0.715 - c * 0.715 + n * 0.715, 0.072 + c * 0.928 + n * 0.072, 0.0, 0.0, //
                0.0, 0.0, 0.0, 1.0, 0.0,
            ]
        },
        F::Brightness(a) => {
            let m = a.max(0.0);
            [
                m, 0.0, 0.0, 0.0, 0.0, //
                0.0, m, 0.0, 0.0, 0.0, //
                0.0, 0.0, m, 0.0, 0.0, //
                0.0, 0.0, 0.0, 1.0, 0.0,
            ]
        },
        F::Contrast(a) => {
            let m = a.max(0.0);
            let b = 0.5 * (1.0 - m);
            [
                m, 0.0, 0.0, 0.0, b, //
                0.0, m, 0.0, 0.0, b, //
                0.0, 0.0, m, 0.0, b, //
                0.0, 0.0, 0.0, 1.0, 0.0,
            ]
        },
        F::Invert(a) => {
            let a = a.clamp(0.0, 1.0);
            let d = 1.0 - 2.0 * a;
            [
                d, 0.0, 0.0, 0.0, a, //
                0.0, d, 0.0, 0.0, a, //
                0.0, 0.0, d, 0.0, a, //
                0.0, 0.0, 0.0, 1.0, 0.0,
            ]
        },
    }
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
            label,
            actual,
            expected,
            diff,
            tol,
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
        invert: bool,
    ) {
        use crate::filter::{blur_pass_callback, clip_rectangle_callback, make_bilinear_sampler};
        use crate::render_graph::{RenderGraph, Task, TaskId};

        let device = self.wgpu_device.core.device.clone();
        let queue = self.wgpu_device.core.queue.clone();

        let mask_format = wgpu::TextureFormat::Rgba8Unorm;
        // `invert` builds a `1 - coverage` mask (the inset-shadow primitive); blur
        // is linear so the blurred inverted mask equals `1 - blurred coverage`.
        let clip_pipe = self.wgpu_device.ensure_clip_rectangle(mask_format, true, invert);
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
        let Some(rast_mutex) = self.vello_rasterizer.as_ref() else {
            return;
        };
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
    /// Apply backdrop + element `filter` preprocessing to `scene` if any filter
    /// is present, returning the rewritten scene; `None` when there is nothing to
    /// do (the fast path). Backdrop filters run first (they read the unfiltered
    /// prefix), then element CSS `filter` on the result (the layer's own output).
    /// Shared by every render entry so filters apply on all paths.
    ///
    /// Note on the per-frame `register_texture`: the filter passes mint a fresh
    /// GPU texture under a deterministic sentinel key each frame and never
    /// `unregister` it. This is deliberate, matching the backdrop pass: the tile
    /// cache bakes the vello image handle into cached per-tile Scenes, so a clean
    /// (reused) tile must still resolve its filter image next frame — which
    /// requires the prior handle to stay alive. Freeing it would break tile reuse
    /// for static filtered content. The cost is a slow growth of vello's
    /// paint-texture table on heavily-animated filtered pages; a tile-cache-aware
    /// key/handle reuse scheme is the proper fix and a shared follow-up with
    /// backdrop-filter.
    fn preprocess_filters(
        &self,
        scene: &Scene,
        rast: &mut crate::vello_tile_rasterizer::VelloTileRasterizer,
        tc: &mut std::sync::MutexGuard<'_, TileCache>,
    ) -> Option<Scene> {
        if !has_backdrop_filter(scene) && !has_element_filter(scene) {
            return None;
        }
        let mut pre = if has_backdrop_filter(scene) {
            self.preprocess_backdrop_filters(scene, rast, tc)
        } else {
            scene.clone()
        };
        if has_element_filter(&pre) {
            pre = self.preprocess_element_filters(&pre, rast, tc);
        }
        Some(pre)
    }

    /// # Panics
    ///
    /// - If `enable_vello` was false at construction.
    /// - If `tile_cache_size` was `None` at construction.
    /// - If a vello render error occurs (mirrors the existing
    ///   `render()` shape, which doesn't return a Result).
    pub fn render_vello(&self, scene: &Scene, target_view: &wgpu::TextureView, clear: ColorLoad) {
        let rast_mutex = self
            .vello_rasterizer
            .as_ref()
            .expect("Renderer::render_vello requires NetrenderOptions::enable_vello = true");
        let tc_mutex = self
            .tile_cache
            .as_ref()
            .expect("Renderer::render_vello requires NetrenderOptions::tile_cache_size = Some(_)");

        let base = match clear {
            ColorLoad::Clear(c) => {
                vello::peniko::Color::new([c.r as f32, c.g as f32, c.b as f32, c.a as f32])
            }
            ColorLoad::Load => vello::peniko::Color::new([0.0, 0.0, 0.0, 0.0]),
        };

        let mut rast = rast_mutex.lock().expect("vello_rasterizer lock");
        let mut tc = tc_mutex.lock().expect("tile_cache lock");

        // Roadmap D1 — if any layer carries a `backdrop_filter`,
        // pre-render the scene-prefix to a texture, blur it, and
        // inject a SceneImage covering the layer's bounds so the
        // layer paints over the blurred backdrop. Falls through to
        // the no-backdrop fast path when no filters are present.
        let processed = self.preprocess_filters(scene, &mut rast, &mut tc);
        let scene_to_render = processed.as_ref().unwrap_or(scene);

        rast.render(scene_to_render, &mut tc, target_view, base)
            .unwrap_or_else(|e| panic!("vello render_to_texture failed: {:?}", e));
    }

    /// Compose a same-device external texture directly into an
    /// already-rendered target view.
    ///
    /// This is the zero-copy path for WebGL canvas / video / embedder
    /// textures that already live on this renderer's `wgpu::Device`.
    /// The source texture is sampled directly from `source_view` and
    /// blended over `target_view`; unlike `insert_image_vello`, the
    /// source texture does not need `COPY_SRC` usage and is not copied
    /// into vello's image atlas.
    ///
    /// First-slice limitation: this helper is an overlay pass. It
    /// preserves correct composition for external content that is
    /// topmost relative to the vello-rendered scene. Fully interleaved
    /// painter order requires splitting the scene around external
    /// texture ops or moving this pass into the scene compositor.
    pub fn compose_external_texture(
        &self,
        source_view: &wgpu::TextureView,
        target_view: &wgpu::TextureView,
        target_format: wgpu::TextureFormat,
        viewport_width: u32,
        viewport_height: u32,
        placement: ExternalTexturePlacement,
    ) {
        let pipe = {
            let mut pipelines = self
                .external_texture_pipelines
                .lock()
                .expect("external_texture_pipelines lock");
            pipelines
                .entry(target_format)
                .or_insert_with(|| {
                    crate::external_texture::build_external_texture_pipeline(
                        &self.wgpu_device.core.device,
                        target_format,
                    )
                })
                .clone()
        };

        crate::external_texture::compose_external_texture(
            &self.wgpu_device.core.device,
            &self.wgpu_device.core.queue,
            &pipe,
            source_view,
            target_view,
            viewport_width,
            viewport_height,
            placement,
        );
    }

    /// Roadmap D1 — pre-process backdrop filters: for each layer
    /// carrying a [`SceneFilter`], render the scene-prefix to an
    /// intermediate texture, blur it, register as an `ImageKey`,
    /// and inject a `SceneImage` covering the layer's bounds at the
    /// start of the layer's scope. Returns the augmented Scene with
    /// `backdrop_filter` cleared (the work has been done).
    ///
    /// First-cut scope: handles every backdrop-filter layer in the
    /// scene's op order, but each prefix is rendered independently
    /// (no sharing). For typical UI usage (one or two backdrop
    /// elements) this is fine; heavier consumers can revisit the
    /// caching story when profiles surface it.
    fn preprocess_backdrop_filters(
        &self,
        scene: &Scene,
        rast: &mut crate::vello_tile_rasterizer::VelloTileRasterizer,
        tc: &mut std::sync::MutexGuard<'_, TileCache>,
    ) -> Scene {
        use crate::scene::{SceneClip, SceneFilter, SceneImage, SceneOp, NO_CLIP, SHARP_CLIP};

        let mut processed = scene.clone();

        // Collect (orig_op_index, filter, bounds) for every
        // backdrop-filter layer in painter order.
        let backdrops: Vec<(usize, SceneFilter, [f32; 4])> = scene
            .ops
            .iter()
            .enumerate()
            .filter_map(|(i, op)| match op {
                SceneOp::PushLayer(l) => l.backdrop_filter.map(|f| {
                    let bounds = match &l.clip {
                        SceneClip::None => [
                            0.0,
                            0.0,
                            scene.viewport_width as f32,
                            scene.viewport_height as f32,
                        ],
                        SceneClip::Rect { rect, .. } => *rect,
                        SceneClip::Path(path) => path.local_aabb().unwrap_or([
                            0.0,
                            0.0,
                            scene.viewport_width as f32,
                            scene.viewport_height as f32,
                        ]),
                    };
                    (i, f, bounds)
                }),
                _ => None,
            })
            .collect();

        // ImageKey region unlikely to collide with consumer keys (top of the u64
        // space, same shape as the `u64::MAX` font sentinel), disjoint from the
        // element-filter region.
        let mut next_key: ImageKey = BACKDROP_FILTER_KEY_BASE;

        // Each backdrop filter shifts subsequent op indices by +1
        // (the injected SceneImage). Track the running offset.
        let mut offset = 0_usize;

        for (orig_idx, filter, bounds) in backdrops {
            // Build the prefix scene: ops up to (but not including)
            // this PushLayer. Balance any unclosed PushLayer scopes
            // by appending PopLayer ops to the prefix.
            let prefix = build_prefix_scene(scene, orig_idx);

            // Render prefix to an intermediate texture at viewport
            // dimensions.
            let prefix_tex = self.render_scene_to_texture(rast, tc, &prefix);

            // Blur it. `backdrop-filter` only supports blur today; the color
            // ops exist for element `filter` (`SceneLayer::filters`), so skip a
            // non-blur backdrop defensively rather than mis-render it.
            let SceneFilter::Blur(radius) = filter else {
                continue;
            };
            let blurred = self.build_blurred_image(prefix_tex, scene.viewport_width, radius);

            // Register as an ImageKey on the rasterizer's
            // `image_overrides` (Path B).
            rast.register_texture(next_key, blurred);

            // Compute UV: the blurred texture is the FULL viewport;
            // we sample the bounds region.
            let vw = scene.viewport_width as f32;
            let vh = scene.viewport_height as f32;
            let uv = [
                bounds[0] / vw,
                bounds[1] / vh,
                bounds[2] / vw,
                bounds[3] / vh,
            ];

            // Inject the SceneImage right after the PushLayer in
            // `processed`. The PushLayer's index in `processed` is
            // `orig_idx + offset` (because earlier injections shifted
            // it). The SceneImage goes at `orig_idx + offset + 1`.
            let inject_idx = orig_idx + offset + 1;
            processed.ops.insert(
                inject_idx,
                SceneOp::Image(SceneImage {
                    x0: bounds[0],
                    y0: bounds[1],
                    x1: bounds[2],
                    y1: bounds[3],
                    uv,
                    color: [1.0, 1.0, 1.0, 1.0],
                    key: next_key,
                    transform_id: 0,
                    clip_rect: NO_CLIP,
                    clip_corner_radii: SHARP_CLIP,
                    clamp_to_uv: false,
                }),
            );

            // Strip the backdrop_filter from the processed
            // PushLayer so the no-backdrop fast path renders it.
            if let SceneOp::PushLayer(l) = &mut processed.ops[orig_idx + offset] {
                l.backdrop_filter = None;
            }

            offset += 1;
            next_key = next_key.wrapping_sub(1);
        }

        processed
    }

    /// Render the given Scene into a fresh `Rgba8Unorm` texture at
    /// the scene's viewport dimensions. Used by D1's backdrop
    /// preprocessing for the prefix render.
    fn render_scene_to_texture(
        &self,
        rast: &mut crate::vello_tile_rasterizer::VelloTileRasterizer,
        tc: &mut std::sync::MutexGuard<'_, TileCache>,
        scene: &Scene,
    ) -> wgpu::Texture {
        let device = &self.wgpu_device.core.device;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("netrender D1 backdrop prefix"),
            size: wgpu::Extent3d {
                width: scene.viewport_width,
                height: scene.viewport_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let transparent = vello::peniko::Color::new([0.0, 0.0, 0.0, 0.0]);
        rast.render(scene, tc, &view, transparent)
            .unwrap_or_else(|e| panic!("D1 prefix render failed: {:?}", e));
        texture
    }

    /// Roadmap D1 — blur an arbitrary input texture using the
    /// existing render-graph cascade machinery (and R5's downscale
    /// path for large radii). Returns a fresh `Rgba8Unorm` texture
    /// of size `dim × dim`.
    fn build_blurred_image(
        &self,
        input: wgpu::Texture,
        dim: u32,
        blur_radius_px: f32,
    ) -> wgpu::Texture {
        use crate::filter::{blur_pass_callback, make_bilinear_sampler};
        use crate::render_graph::{RenderGraph, Task, TaskId};
        use std::collections::HashMap;

        let device = self.wgpu_device.core.device.clone();
        let queue = self.wgpu_device.core.queue.clone();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let blur_pipe = self.wgpu_device.ensure_brush_blur(format);
        let sampler = make_bilinear_sampler(&device);

        let (level, passes, step_px) = blur_kernel_plan_with_downscale(blur_radius_px);
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

        const INPUT: TaskId = 1;
        let mut graph = RenderGraph::new();
        let mut prev: TaskId = INPUT;
        let mut next_id: TaskId = INPUT + 1;

        if level > 1 {
            let down_id = next_id;
            graph.push(Task {
                id: down_id,
                extent: scaled_extent,
                format,
                inputs: vec![prev],
                encode: blur_pass_callback(blur_pipe.clone(), Arc::clone(&sampler), 0.0, 0.0),
            });
            prev = down_id;
            next_id += 1;
        }

        for _ in 0..passes {
            let h_id = next_id;
            graph.push(Task {
                id: h_id,
                extent: scaled_extent,
                format,
                inputs: vec![prev],
                encode: blur_pass_callback(blur_pipe.clone(), Arc::clone(&sampler), step_uv, 0.0),
            });
            let v_id = h_id + 1;
            graph.push(Task {
                id: v_id,
                extent: scaled_extent,
                format,
                inputs: vec![h_id],
                encode: blur_pass_callback(blur_pipe.clone(), Arc::clone(&sampler), 0.0, step_uv),
            });
            prev = v_id;
            next_id = v_id + 1;
        }

        if level > 1 {
            let up_id = next_id;
            graph.push(Task {
                id: up_id,
                extent: full_extent,
                format,
                inputs: vec![prev],
                encode: blur_pass_callback(blur_pipe.clone(), Arc::clone(&sampler), 0.0, 0.0),
            });
            prev = up_id;
        }

        let mut externals = HashMap::new();
        externals.insert(INPUT, input);

        let mut outputs = graph.execute(&device, &queue, externals);
        outputs.remove(&prev).expect("D1 final blur output")
    }

    /// Apply one CSS color-matrix filter to `input` (a premultiplied
    /// `Rgba8Unorm` texture), returning a fresh `dim x dim` texture via a single
    /// `cs_color_matrix` pass. `dim` is the square work size (matches
    /// `build_blurred_image`; a non-square viewport is a shared limitation).
    fn build_color_matrix_image(
        &self,
        input: wgpu::Texture,
        dim: u32,
        matrix: [f32; 20],
    ) -> wgpu::Texture {
        use crate::filter::{color_matrix_callback, make_bilinear_sampler};
        use crate::render_graph::{RenderGraph, Task, TaskId};
        use std::collections::HashMap;

        let device = self.wgpu_device.core.device.clone();
        let queue = self.wgpu_device.core.queue.clone();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let pipe = self.wgpu_device.ensure_color_matrix(format);
        let sampler = make_bilinear_sampler(&device);
        let extent = wgpu::Extent3d { width: dim, height: dim, depth_or_array_layers: 1 };

        const INPUT: TaskId = 1;
        const CM: TaskId = 2;
        let mut graph = RenderGraph::new();
        graph.push(Task {
            id: CM,
            extent,
            format,
            inputs: vec![INPUT],
            encode: color_matrix_callback(pipe, sampler, matrix),
        });
        let mut externals = HashMap::new();
        externals.insert(INPUT, input);
        let mut outputs = graph.execute(&device, &queue, externals);
        outputs.remove(&CM).expect("color_matrix output")
    }

    /// Apply a CSS `filter` chain to a rendered layer-content texture, in order:
    /// `Blur` via the separable blur cascade, the color functions via
    /// `cs_color_matrix`. Each pass feeds the next; returns the final texture.
    fn apply_filter_chain(
        &self,
        input: wgpu::Texture,
        dim: u32,
        filters: &[crate::scene::SceneFilter],
    ) -> wgpu::Texture {
        use crate::scene::SceneFilter;
        let mut tex = input;
        for f in filters {
            tex = match f {
                SceneFilter::Blur(r) => self.build_blurred_image(tex, dim, *r),
                other => self.build_color_matrix_image(tex, dim, scene_filter_to_matrix(*other)),
            };
        }
        tex
    }

    /// Element CSS `filter`: for every outermost layer carrying a non-empty
    /// `filters` chain, render that layer's own content to a texture, apply the
    /// chain, and replace the content ops with one image so the surviving
    /// `PushLayer`/`PopLayer` composite the filtered result with the layer's
    /// alpha/blend/clip. Mirrors [`Self::preprocess_backdrop_filters`]. Nested
    /// element filters are deferred (outermost-only), and the image-key region is
    /// disjoint from the backdrop pass's.
    fn preprocess_element_filters(
        &self,
        scene: &Scene,
        rast: &mut crate::vello_tile_rasterizer::VelloTileRasterizer,
        tc: &mut std::sync::MutexGuard<'_, TileCache>,
    ) -> Scene {
        use crate::scene::{SceneClip, SceneFilter, SceneImage, SceneOp, NO_CLIP, SHARP_CLIP};

        let mut processed = scene.clone();

        // Collect (push_idx, pop_idx, filters, bounds) for outermost filtered
        // layers in painter order; skip any layer nested inside an
        // already-collected one (nested element filters are a follow-up).
        let mut covered_until = 0usize;
        let mut elements: Vec<(usize, usize, Vec<SceneFilter>, [f32; 4])> = Vec::new();
        for (i, op) in scene.ops.iter().enumerate() {
            if let SceneOp::PushLayer(l) = op {
                if l.filters.is_empty() || i < covered_until {
                    continue;
                }
                let Some(pop_idx) = matching_pop(&scene.ops, i) else {
                    continue;
                };
                let bounds = match &l.clip {
                    SceneClip::None => {
                        [0.0, 0.0, scene.viewport_width as f32, scene.viewport_height as f32]
                    },
                    SceneClip::Rect { rect, .. } => *rect,
                    SceneClip::Path(path) => path.local_aabb().unwrap_or([
                        0.0,
                        0.0,
                        scene.viewport_width as f32,
                        scene.viewport_height as f32,
                    ]),
                };
                elements.push((i, pop_idx, l.filters.clone(), bounds));
                covered_until = pop_idx;
            }
        }

        // Image-key region disjoint from the backdrop pass; both count down from
        // their bases (see ELEMENT_FILTER_KEY_BASE).
        let mut next_key: ImageKey = ELEMENT_FILTER_KEY_BASE;

        // Replacing an interior of `inner_len` ops with one image shifts later
        // indices by `1 - inner_len`. Track the running (signed) offset.
        let mut offset: isize = 0;

        for (push_idx, pop_idx, filters, bounds) in elements {
            // Render the layer's own content (flat) to a viewport texture, then
            // run the filter chain over it.
            let content = build_layer_content_scene(scene, push_idx, pop_idx);
            let content_tex = self.render_scene_to_texture(rast, tc, &content);
            let filtered = self.apply_filter_chain(content_tex, scene.viewport_width, &filters);
            rast.register_texture(next_key, filtered);

            let vw = scene.viewport_width as f32;
            let vh = scene.viewport_height as f32;
            let uv = [bounds[0] / vw, bounds[1] / vh, bounds[2] / vw, bounds[3] / vh];

            let cur_push = (push_idx as isize + offset) as usize;
            let cur_pop = (pop_idx as isize + offset) as usize;
            // Keep a backdrop-filter image (the layer's first content op, marked
            // by a sentinel key above the element region) behind the filtered
            // result, so `backdrop-filter` + `filter` on one layer composites the
            // unfiltered backdrop under the element's filtered content.
            let content_start = match processed.ops.get(cur_push + 1) {
                Some(SceneOp::Image(im)) if im.key > ELEMENT_FILTER_KEY_BASE => cur_push + 2,
                _ => cur_push + 1,
            };
            let inner_len = cur_pop - content_start;

            let image = SceneOp::Image(SceneImage {
                x0: bounds[0],
                y0: bounds[1],
                x1: bounds[2],
                y1: bounds[3],
                uv,
                color: [1.0, 1.0, 1.0, 1.0],
                key: next_key,
                transform_id: 0,
                clip_rect: NO_CLIP,
                clip_corner_radii: SHARP_CLIP,
                // `false` like the backdrop path: the filtered result is a
                // GPU-texture override (no CPU `ImageData`), so the `clamp_to_uv`
                // CPU crop path can't run on it.
                clamp_to_uv: false,
            });
            // Replace the (non-backdrop) interior with the single filtered image.
            processed.ops.splice(content_start..cur_pop, std::iter::once(image));
            // Clear the layer's filters so it re-enters the no-filter fast path,
            // applying alpha/blend/clip over the filtered image.
            if let SceneOp::PushLayer(l) = &mut processed.ops[cur_push] {
                l.filters.clear();
            }

            offset += 1 - inner_len as isize;
            next_key = next_key.wrapping_sub(1);
        }

        processed
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
        self.render_with_compositor_and_external_textures(
            scene,
            master_format,
            compositor,
            base_color,
            &[],
        );
    }

    /// Render a scene into the compositor master texture, then blend
    /// same-device external textures into that master before handing the
    /// frame to the consumer compositor.
    ///
    /// `ExternalTextureComposite::scene_op_boundary` preserves painter
    /// order without routing producer textures through Vello's atlas:
    /// ordinary scene content paints once into the master, each external
    /// texture composites at its boundary, and the ordinary scene tail
    /// that should remain above that texture is redrawn into a transparent
    /// scratch target and blended back over the master. Callers that keep
    /// the default `usize::MAX` boundary retain the topmost-overlay fast
    /// path and pay no tail redraw.
    pub fn render_with_compositor_and_external_textures(
        &self,
        scene: &Scene,
        master_format: wgpu::TextureFormat,
        compositor: &mut dyn Compositor,
        base_color: vello::peniko::Color,
        external_textures: &[ExternalTextureComposite<'_>],
    ) {
        let rast_mutex = self.vello_rasterizer.as_ref().expect(
            "Renderer::render_with_compositor requires NetrenderOptions::enable_vello = true",
        );
        let tc_mutex = self.tile_cache.as_ref().expect(
            "Renderer::render_with_compositor requires NetrenderOptions::tile_cache_size = Some(_)",
        );

        let mut rast = rast_mutex.lock().expect("vello_rasterizer lock");
        let mut tc = tc_mutex.lock().expect("tile_cache lock");

        // Apply backdrop + element CSS `filter` preprocessing (same as the
        // `render_vello` path). External-texture boundaries below still index the
        // original `scene` (filters + interleaved external textures is a
        // follow-up); the common no-external-texture path is unaffected.
        let processed = self.preprocess_filters(scene, &mut rast, &mut tc);
        let render_scene = processed.as_ref().unwrap_or(scene);

        // 1. Render the scene into the rasterizer's pool-allocated master.
        rast.render_to_internal_master(render_scene, &mut tc, master_format, base_color)
            .unwrap_or_else(|e| panic!("vello render_to_texture failed: {:?}", e));

        if !external_textures.is_empty() {
            let master_texture = rast
                .master_texture()
                .expect("master_pool guaranteed by render_to_internal_master above");
            let master_view = master_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let mut tail_target: Option<(wgpu::Texture, wgpu::TextureView)> = None;
            let mut previous_boundary = 0usize;
            for external in external_textures {
                let boundary = external.scene_op_boundary.min(scene.ops.len());
                debug_assert!(
                    boundary >= previous_boundary,
                    "external textures must be supplied in nondecreasing scene-op order",
                );
                previous_boundary = boundary;

                self.compose_external_texture(
                    external.source_view,
                    &master_view,
                    master_format,
                    scene.viewport_width,
                    scene.viewport_height,
                    external.placement,
                );

                if boundary >= scene.ops.len() {
                    continue;
                }

                let tail_scene = scene_tail_fragment(scene, boundary);
                if tail_scene.ops.is_empty() {
                    continue;
                }

                let (_, tail_view) = tail_target.get_or_insert_with(|| {
                    make_external_tail_target(
                        &self.wgpu_device.core.device,
                        scene.viewport_width,
                        scene.viewport_height,
                        master_format,
                    )
                });
                rast.render_overlay_fragment(
                    &tail_scene,
                    tail_view,
                    vello::peniko::Color::new([0.0, 0.0, 0.0, 0.0]),
                )
                .unwrap_or_else(|e| panic!("vello overlay tail render failed: {:?}", e));
                self.compose_external_texture(
                    tail_view,
                    &master_view,
                    master_format,
                    scene.viewport_width,
                    scene.viewport_height,
                    ExternalTexturePlacement::new([
                        0.0,
                        0.0,
                        scene.viewport_width as f32,
                        scene.viewport_height as f32,
                    ]),
                );
            }
        }

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
