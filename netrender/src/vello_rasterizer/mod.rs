/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phases 2' / 5' / 8' — netrender Scene → vello::Scene translator.
//!
//! Phase 2': rects with per-primitive transform and axis-aligned
//! clip. Phase 5': image ingestion with per-image transform, clip,
//! UV sub-region, and alpha tint. Phase 8': linear / circular-radial
//! / conic gradients with N-stop ramps. Output is suitable for
//! `Renderer::render_to_texture`; receipts are at
//! `tests/p2prime_vello_rects.rs`, `tests/p5prime_vello_image.rs`,
//! and `tests/p8prime_vello_gradients.rs`.
//!
//! ## Image-tint encoding (Phase 5a + 5b)
//!
//! `SceneImage.color` is a premultiplied RGBA tint, decomposed into
//! `alpha_factor = a` and `chromatic_factor = (r/a, g/a, b/a)`:
//!
//! - **Phase 5a — alpha factor.** Applied via
//!   `ImageBrush::with_alpha(a)`. Sufficient for achromatic tints
//!   (white-with-alpha, the tile-cache composite case).
//! - **Phase 5b — chromatic factor.** When `chromatic_factor` is
//!   not `(1, 1, 1)`, paint the alpha-modulated image and then
//!   apply a `BlendMode::new(Mix::Multiply, Compose::SrcAtop)`
//!   layer that fills the image rect with the chromatic factor as
//!   a solid color (alpha 1.0). `SrcAtop` constrains the multiply
//!   to where the image already painted, so transparent regions of
//!   the image stay transparent. Used by 9A's mask-as-tinted-image
//!   case and any drop-shadow style with a colored shadow.
//!
//! ## Boundary conventions (verified Phase 1' p1prime_02 / p1prime_03)
//!
//! - `SceneRect.color` is **premultiplied** RGBA. `peniko::Color`
//!   expects **straight-alpha**. We unpremultiply at the boundary:
//!   `(r/a, g/a, b/a, a)` for `a > 0`, `(0, 0, 0, 0)` for `a == 0`.
//! - Vello stores straight-alpha sRGB-encoded values in its output
//!   target. The compositor (downstream sample stage) is responsible
//!   for premultiplying after the hardware sRGB→linear decode; that
//!   contract is unchanged from §6.1.
//! - `interpolation_cs` is not threaded through gradients (no-op on
//!   the GPU compute path; see §3.3 / p1prime_03).
//!
//! ## Coordinate conventions
//!
//! `Transform.m` is a column-major 4×4 with the 2D affine in
//! `(m[0], m[1], m[4], m[5], m[12], m[13])` = `(a, b, c, d, e, f)`,
//! matching `kurbo::Affine::new([a, b, c, d, e, f])`.

use std::collections::HashMap;

use vello::kurbo::{Affine, Rect, RoundedRect, RoundedRectRadii};
use vello::peniko::{self, Color, Fill, ImageAlphaType, ImageData, ImageFormat};

use crate::scene::{ImageKey, Scene, SceneOp, Transform};

mod emit;
mod emit_paint;
pub(crate) use emit::build_bez_path;
use emit::{emit_glyph_run, emit_push_layer, emit_rect, emit_shape, emit_stroke};
use emit_paint::{emit_gradient, emit_image, emit_pattern};
/// Phase 2' / 5' scope: rects + images, with per-primitive transform
/// and clip. Gradients in `scene` are silently ignored (Phase 8').
/// Painter order matches the parent scene: rects first, then images
/// painted over them — the same ordering the existing netrender
/// pipeline uses.
pub fn scene_to_vello(scene: &Scene) -> vello::Scene {
    scene_to_vello_with_overrides(scene, &HashMap::new())
}

/// Translate a netrender [`Scene`] into a [`vello::Scene`] with
/// caller-supplied [`peniko::ImageData`] overrides for selected
/// [`ImageKey`]s.
///
/// `image_overrides` lets callers pre-register GPU-resident textures
/// via [`vello::Renderer::register_texture`] (Path B from rasterizer
/// plan §3.5) and pass the resulting [`ImageData`] in. Keys absent
/// from the overrides map fall back to building from
/// `scene.image_sources` CPU bytes (Path A — the default).
///
/// Use this entry point when image data lives as a render-graph
/// output (already a `wgpu::Texture`, no CPU bytes), e.g., the blur
/// task's output texture feeding into a vello-rasterized scene.
pub fn scene_to_vello_with_overrides(
    scene: &Scene,
    image_overrides: &HashMap<ImageKey, ImageData>,
) -> vello::Scene {
    let images = build_image_cache(scene, image_overrides);
    scene_to_vello_with_cache(scene, &images)
}

/// Translate a [`Scene`] into a [`vello::Scene`] using a
/// caller-supplied pre-built image cache. Same body as
/// [`scene_to_vello_with_overrides`] minus the
/// [`build_image_cache`] step; exposed for [`VelloRasterizer`] to
/// reuse a persistent image cache across calls.
pub fn scene_to_vello_with_cache(
    scene: &Scene,
    images: &HashMap<ImageKey, ImageData>,
) -> vello::Scene {
    let mut vscene = vello::Scene::new();

    // Single pass over the unified op list — painter order = consumer
    // push order. (Pre-2026-05-04 op-list refactor this dispatched
    // through six per-type Vec passes with a fixed cross-type order;
    // see plan §11.11 for context.)
    // Layer-balance counter so debug builds catch unbalanced
    // PushLayer/PopLayer pairs at scene-translation time. In release
    // an unbalanced PopLayer with no live layer is silently skipped
    // (vello would panic on underflow).
    let mut layer_depth: u32 = 0;
    for op in &scene.ops {
        match op {
            SceneOp::Rect(rect) => emit_rect(&mut vscene, rect, &scene.transforms),
            SceneOp::Stroke(stroke) => emit_stroke(&mut vscene, stroke, &scene.transforms),
            SceneOp::Gradient(gradient) => emit_gradient(&mut vscene, gradient, &scene.transforms),
            SceneOp::Image(image) => emit_image(&mut vscene, image, &scene.transforms, images),
            SceneOp::Pattern(pattern) => {
                emit_pattern(&mut vscene, pattern, &scene.transforms, images)
            }
            SceneOp::Shape(shape) => emit_shape(&mut vscene, shape, &scene.transforms),
            SceneOp::GlyphRun(run) => {
                emit_glyph_run(&mut vscene, run, &scene.fonts, &scene.transforms)
            }
            SceneOp::PushLayer(layer) => {
                emit_push_layer(&mut vscene, layer, scene);
                layer_depth += 1;
            }
            SceneOp::PopLayer => {
                debug_assert!(
                    layer_depth > 0,
                    "SceneOp::PopLayer with no matching PushLayer"
                );
                if layer_depth > 0 {
                    vscene.pop_layer();
                    layer_depth -= 1;
                }
            }
        }
    }
    debug_assert_eq!(
        layer_depth, 0,
        "Scene ended with {} unclosed PushLayer(s)",
        layer_depth,
    );

    vscene
}

/// Roadmap R4 — stateful wrapper around the simple
/// (non-tile) translator that caches the per-frame image map
/// across calls. Mirror of [`crate::vello_tile_rasterizer::VelloTileRasterizer`]'s
/// `image_data` field for the simple-path consumer.
///
/// A streaming consumer that drives [`scene_to_vello`] once per
/// frame pays an O(N_image_sources) HashMap rebuild every call —
/// each entry constructs a fresh `peniko::ImageData` struct around
/// the shared `peniko::Blob`. With `VelloRasterizer`, cached
/// entries persist across frames and only the diff (newly added or
/// dropped keys) touches the cache. Path B (`register_texture`)
/// uses the same interface as the tile rasterizer.
pub struct VelloRasterizer {
    image_data: HashMap<ImageKey, ImageData>,
    image_overrides: HashMap<ImageKey, ImageData>,
}

impl VelloRasterizer {
    /// Construct an empty rasterizer. Cache fills on the first
    /// `scene_to_vello` call.
    pub fn new() -> Self {
        Self {
            image_data: HashMap::new(),
            image_overrides: HashMap::new(),
        }
    }

    /// Number of CPU-side `ImageData` entries currently held in the
    /// cache (one per `ImageKey` present in the most recent
    /// scene's `image_sources`). Stable across consecutive
    /// `scene_to_vello` calls on the same scene; updates as the
    /// consumer adds or removes image sources.
    pub fn cached_image_count(&self) -> usize {
        self.image_data.len()
    }

    /// Register a Path B [`peniko::ImageData`] (typically the
    /// result of `vello::Renderer::register_texture`) under the
    /// given key. Path B overrides win over `scene.image_sources`
    /// on key collision.
    pub fn register_texture(&mut self, key: ImageKey, image: ImageData) {
        self.image_overrides.insert(key, image);
    }

    /// Drop a previously-registered Path B entry. Returns the
    /// dropped value if present, `None` otherwise.
    pub fn unregister_texture(&mut self, key: ImageKey) -> Option<ImageData> {
        self.image_overrides.remove(&key)
    }

    /// Translate `scene` into a fresh [`vello::Scene`], using and
    /// updating the cached image map.
    ///
    /// Cache invariant: after this call, `self.image_data` contains
    /// exactly the keys present in `scene.image_sources` (unchanged
    /// keys keep their existing `ImageData`; new keys are inserted;
    /// keys absent from the scene are evicted).
    pub fn scene_to_vello(&mut self, scene: &Scene) -> vello::Scene {
        self.refresh_image_data(scene);
        // Merged cache: Path A (cached) + Path B (overrides win).
        // The clone is per-entry Arc bumps + HashMap insert; cheap.
        let mut merged = self.image_data.clone();
        for (key, image) in &self.image_overrides {
            merged.insert(*key, image.clone());
        }
        scene_to_vello_with_cache(scene, &merged)
    }

    fn refresh_image_data(&mut self, scene: &Scene) {
        for (key, data) in &scene.image_sources {
            self.image_data.entry(*key).or_insert_with(|| ImageData {
                data: data.data.clone(),
                format: ImageFormat::Rgba8,
                alpha_type: ImageAlphaType::Alpha,
                width: data.width,
                height: data.height,
            });
        }
        // Evict keys that disappeared from the scene.
        self.image_data
            .retain(|key, _| scene.image_sources.contains_key(key));
    }
}

impl Default for VelloRasterizer {
    fn default() -> Self {
        Self::new()
    }
}

fn build_image_cache(
    scene: &Scene,
    overrides: &HashMap<ImageKey, ImageData>,
) -> HashMap<ImageKey, ImageData> {
    let mut cache = HashMap::with_capacity(scene.image_sources.len() + overrides.len());
    // Path A — Arc-shared bytes from scene.image_sources. Cloning a
    // peniko::Blob is Arc-bump + id copy; vello dedups atlas slots
    // by Blob::id() so the same source bytes share one upload.
    for (key, data) in &scene.image_sources {
        cache.insert(
            *key,
            ImageData {
                data: data.data.clone(),
                format: ImageFormat::Rgba8,
                alpha_type: ImageAlphaType::Alpha,
                width: data.width,
                height: data.height,
            },
        );
    }
    // Path B — caller-supplied overrides win on key collision.
    for (key, image) in overrides {
        cache.insert(*key, image.clone());
    }
    cache
}


/// Push a clip layer for the given clip rect + per-corner radii.
/// Zero radii produce a sharp axis-aligned rect clip (legacy behavior);
/// non-zero radii produce a `kurbo::RoundedRect` clip (Phase 9'). The
/// caller is responsible for matching this with `vscene.pop_layer()`.
fn push_clip_layer(vscene: &mut vello::Scene, clip_rect: [f32; 4], radii: [f32; 4]) {
    let rect = Rect::new(
        clip_rect[0] as f64,
        clip_rect[1] as f64,
        clip_rect[2] as f64,
        clip_rect[3] as f64,
    );
    let any_radius = radii.iter().any(|&r| r > 0.0);
    if any_radius {
        // RoundedRectRadii::new takes (top_leading, top_trailing,
        // bottom_trailing, bottom_leading) which under our Y-down screen
        // coordinates maps to (top_left, top_right, bottom_right,
        // bottom_left) — the same order our SceneRect.clip_corner_radii
        // documents.
        let rrect = RoundedRect::from_rect(
            rect,
            RoundedRectRadii::new(
                radii[0] as f64,
                radii[1] as f64,
                radii[2] as f64,
                radii[3] as f64,
            ),
        );
        vscene.push_layer(
            Fill::NonZero,
            peniko::Mix::Normal,
            1.0,
            Affine::IDENTITY,
            &rrect,
        );
    } else {
        vscene.push_layer(
            Fill::NonZero,
            peniko::Mix::Normal,
            1.0,
            Affine::IDENTITY,
            &rect,
        );
    }
}

pub(crate) fn transform_to_affine(t: &Transform) -> Affine {
    Affine::new([
        t.m[0] as f64,
        t.m[1] as f64,
        t.m[4] as f64,
        t.m[5] as f64,
        t.m[12] as f64,
        t.m[13] as f64,
    ])
}

fn unpremultiply_color(c: [f32; 4]) -> Color {
    let a = c[3];
    if a > 0.0 {
        Color::from_rgba8(
            (c[0] / a * 255.0).round().clamp(0.0, 255.0) as u8,
            (c[1] / a * 255.0).round().clamp(0.0, 255.0) as u8,
            (c[2] / a * 255.0).round().clamp(0.0, 255.0) as u8,
            (a * 255.0).round().clamp(0.0, 255.0) as u8,
        )
    } else {
        Color::from_rgba8(0, 0, 0, 0)
    }
}
