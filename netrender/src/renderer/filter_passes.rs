/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Box-shadow mask synthesis and the backdrop-filter pass (the filter
//! dispatcher + prefix render). See [`super`].

use std::sync::Arc;

use crate::scene::{ImageKey, Scene};
use crate::tile_cache::TileCache;

use super::filters::{
    blur_kernel_plan_with_downscale, build_prefix_scene, has_backdrop_filter, has_element_filter,
    BACKDROP_FILTER_KEY_BASE,
};
use super::Renderer;

impl Renderer {
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
    pub(super) fn preprocess_filters(
        &self,
        scene: &Scene,
        rast: &mut crate::vello_tile_rasterizer::VelloTileRasterizer,
        tc: &mut TileCache,
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
        tc: &mut TileCache,
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
                    nearest: false,
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
    pub(super) fn render_scene_to_texture(
        &self,
        rast: &mut crate::vello_tile_rasterizer::VelloTileRasterizer,
        tc: &mut TileCache,
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
}
