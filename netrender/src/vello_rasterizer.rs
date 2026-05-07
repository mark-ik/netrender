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

use vello::kurbo::{Affine, BezPath, Cap, Join, Point, Rect, RoundedRect, RoundedRectRadii, Stroke};
use vello::peniko::{
    self, BlendMode, Color, ColorStop, Compose, Extend, Fill, FontData, Gradient, ImageAlphaType,
    ImageBrush, ImageData, ImageFormat, Mix,
};

use crate::scene::{
    FontBlob, GradientKind, ImageKey, NO_CLIP, PathOp, Scene, SceneBlendMode, SceneClip,
    SceneGlyphRun, SceneGradient, SceneImage, SceneLayer, SceneOp, ScenePattern, SceneRect,
    SceneShape, SceneStroke, SceneStrokeCap, SceneStrokeJoin, Transform,
};

/// Map a netrender [`SceneStrokeCap`] to a kurbo [`Cap`].
fn map_stroke_cap(c: SceneStrokeCap) -> Cap {
    match c {
        SceneStrokeCap::Butt => Cap::Butt,
        SceneStrokeCap::Round => Cap::Round,
        SceneStrokeCap::Square => Cap::Square,
    }
}

/// Map a netrender [`SceneStrokeJoin`] to a kurbo [`Join`].
fn map_stroke_join(j: SceneStrokeJoin) -> Join {
    match j {
        SceneStrokeJoin::Bevel => Join::Bevel,
        SceneStrokeJoin::Miter => Join::Miter,
        SceneStrokeJoin::Round => Join::Round,
    }
}

/// Map a netrender [`SceneBlendMode`] to a vello [`BlendMode`].
pub(crate) fn map_blend_mode(b: SceneBlendMode) -> peniko::BlendMode {
    let mix = match b {
        SceneBlendMode::Normal => peniko::Mix::Normal,
        SceneBlendMode::Multiply => peniko::Mix::Multiply,
        SceneBlendMode::Screen => peniko::Mix::Screen,
        SceneBlendMode::Overlay => peniko::Mix::Overlay,
        SceneBlendMode::Darken => peniko::Mix::Darken,
        SceneBlendMode::Lighten => peniko::Mix::Lighten,
    };
    peniko::BlendMode::new(mix, peniko::Compose::SrcOver)
}

/// Translate a netrender [`Scene`] into a [`vello::Scene`] suitable
/// for [`vello::Renderer::render_to_texture`].
///
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
            SceneOp::Pattern(pattern) => emit_pattern(&mut vscene, pattern, &scene.transforms, images),
            SceneOp::Shape(shape) => emit_shape(&mut vscene, shape, &scene.transforms),
            SceneOp::GlyphRun(run) => {
                emit_glyph_run(&mut vscene, run, &scene.fonts, &scene.transforms)
            }
            SceneOp::PushLayer(layer) => {
                emit_push_layer(&mut vscene, layer, scene);
                layer_depth += 1;
            }
            SceneOp::PopLayer => {
                debug_assert!(layer_depth > 0, "SceneOp::PopLayer with no matching PushLayer");
                if layer_depth > 0 {
                    vscene.pop_layer();
                    layer_depth -= 1;
                }
            }
        }
    }
    debug_assert_eq!(
        layer_depth, 0,
        "Scene ended with {} unclosed PushLayer(s)", layer_depth,
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

fn emit_rect(vscene: &mut vello::Scene, rect: &SceneRect, transforms: &[Transform]) {
    let affine = transform_to_affine(&transforms[rect.transform_id as usize]);
    let shape = Rect::new(
        rect.x0 as f64,
        rect.y0 as f64,
        rect.x1 as f64,
        rect.y1 as f64,
    );
    let color = unpremultiply_color(rect.color);

    let needs_clip = rect.clip_rect != NO_CLIP;
    if needs_clip {
        push_clip_layer(vscene, rect.clip_rect, rect.clip_corner_radii);
    }
    vscene.fill(Fill::NonZero, affine, color, None, &shape);
    if needs_clip {
        vscene.pop_layer();
    }
}

fn emit_glyph_run(
    vscene: &mut vello::Scene,
    run: &SceneGlyphRun,
    fonts: &[FontBlob],
    transforms: &[Transform],
) {
    if run.font_id == 0 || run.glyphs.is_empty() {
        return;
    }
    let blob = &fonts[run.font_id as usize];
    // `FontBlob.data` is already a `peniko::Blob<u8>` with a stable
    // id across frames (post-FontBlob unification); cloning it is
    // an Arc bump + id copy, not a fresh atlas slot.
    let font_data = FontData {
        data: blob.data.clone(),
        index: blob.index,
    };
    let world = transform_to_affine(&transforms[run.transform_id as usize]);
    let color = unpremultiply_color(run.color);

    let needs_clip = run.clip_rect != NO_CLIP;
    if needs_clip {
        push_clip_layer(vscene, run.clip_rect, run.clip_corner_radii);
    }

    let glyphs_iter = run.glyphs.iter().map(|g| vello::Glyph {
        id: g.id,
        x: g.x,
        y: g.y,
    });

    // Roadmap C4 — variable-font axis values. When non-empty,
    // resolve user-space settings to normalized coords via skrifa
    // and pass to vello via `normalized_coords`. Empty axis values
    // (the common case) keep the font at its default location.
    let normalized_coords: Vec<vello::NormalizedCoord> = if run.font_axis_values.is_empty() {
        Vec::new()
    } else {
        compute_normalized_coords(blob, &run.font_axis_values)
    };

    let mut draw = vscene
        .draw_glyphs(&font_data)
        .font_size(run.font_size)
        .transform(world)
        .brush(color);
    if !normalized_coords.is_empty() {
        draw = draw.normalized_coords(&normalized_coords);
    }
    draw.draw(Fill::NonZero, glyphs_iter);

    if needs_clip {
        vscene.pop_layer();
    }
}

/// Roadmap C4 — convert user-space variable-font axis settings to
/// the normalized i16 coords vello consumes. Reads the font's axis
/// table via skrifa; tags that don't match an axis are silently
/// ignored (matches skrifa's `location_to_slice` semantics).
/// Returns an empty Vec on font-parse failure (caller treats this
/// as "default location").
fn compute_normalized_coords(
    blob: &FontBlob,
    user_settings: &[(crate::scene::SceneFontAxisTag, f32)],
) -> Vec<vello::NormalizedCoord> {
    use skrifa::MetadataProvider;
    let Ok(font) = skrifa::FontRef::from_index(blob.data.data(), blob.index) else {
        return Vec::new();
    };
    let axes = font.axes();
    if axes.len() == 0 {
        return Vec::new();
    }
    // Build (&str, f32) tuples for skrifa. ASCII tag bytes only;
    // non-UTF-8 tag bytes (consumer error) are skipped.
    let settings: Vec<(&str, f32)> = user_settings
        .iter()
        .filter_map(|(tag, value)| {
            std::str::from_utf8(tag).ok().map(|s| (s, *value))
        })
        .collect();
    // skrifa returns coords as F2Dot14; vello wants raw i16 of the
    // same fixed-point representation. F2Dot14 wraps i16 directly.
    axes.location(settings)
        .coords()
        .iter()
        .map(|c| c.to_bits())
        .collect()
}

pub(crate) fn build_bez_path(path: &crate::scene::ScenePath) -> BezPath {
    let mut bp = BezPath::new();
    for op in &path.ops {
        match *op {
            PathOp::MoveTo(x, y) => bp.move_to(Point::new(x as f64, y as f64)),
            PathOp::LineTo(x, y) => bp.line_to(Point::new(x as f64, y as f64)),
            PathOp::QuadTo(cx, cy, x, y) => bp.quad_to(
                Point::new(cx as f64, cy as f64),
                Point::new(x as f64, y as f64),
            ),
            PathOp::CubicTo(c1x, c1y, c2x, c2y, x, y) => bp.curve_to(
                Point::new(c1x as f64, c1y as f64),
                Point::new(c2x as f64, c2y as f64),
                Point::new(x as f64, y as f64),
            ),
            PathOp::Close => bp.close_path(),
        }
    }
    bp
}

fn emit_shape(vscene: &mut vello::Scene, shape: &SceneShape, transforms: &[Transform]) {
    if shape.fill_color.is_none() && shape.stroke.is_none() {
        return; // Nothing to paint.
    }
    let bp = build_bez_path(&shape.path);
    let affine = transform_to_affine(&transforms[shape.transform_id as usize]);

    let needs_clip = shape.clip_rect != NO_CLIP;
    if needs_clip {
        push_clip_layer(vscene, shape.clip_rect, shape.clip_corner_radii);
    }

    if let Some(color) = shape.fill_color {
        let fill = unpremultiply_color(color);
        vscene.fill(Fill::NonZero, affine, fill, None, &bp);
    }
    if let Some(stroke) = shape.stroke {
        let style = Stroke::new(stroke.width as f64);
        let color = unpremultiply_color(stroke.color);
        vscene.stroke(&style, affine, color, None, &bp);
    }

    if needs_clip {
        vscene.pop_layer();
    }
}

fn emit_stroke(vscene: &mut vello::Scene, stroke: &SceneStroke, transforms: &[Transform]) {
    let affine = transform_to_affine(&transforms[stroke.transform_id as usize]);
    let rect = Rect::new(
        stroke.x0 as f64,
        stroke.y0 as f64,
        stroke.x1 as f64,
        stroke.y1 as f64,
    );
    let color = unpremultiply_color(stroke.color);
    // Roadmap C1 — apply cap / join / dash decorations.
    let mut style = Stroke::new(stroke.stroke_width as f64)
        .with_caps(map_stroke_cap(stroke.cap))
        .with_join(map_stroke_join(stroke.join));
    if !stroke.dash_pattern.is_empty() {
        style = style.with_dashes(
            stroke.dash_offset as f64,
            stroke.dash_pattern.iter().map(|&v| v as f64),
        );
    }

    let needs_clip = stroke.clip_rect != NO_CLIP;
    if needs_clip {
        push_clip_layer(vscene, stroke.clip_rect, stroke.clip_corner_radii);
    }

    let any_radii = stroke.stroke_corner_radii.iter().any(|&r| r > 0.0);
    if any_radii {
        let rrect = RoundedRect::from_rect(
            rect,
            RoundedRectRadii::new(
                stroke.stroke_corner_radii[0] as f64,
                stroke.stroke_corner_radii[1] as f64,
                stroke.stroke_corner_radii[2] as f64,
                stroke.stroke_corner_radii[3] as f64,
            ),
        );
        vscene.stroke(&style, affine, color, None, &rrect);
    } else {
        vscene.stroke(&style, affine, color, None, &rect);
    }

    if needs_clip {
        vscene.pop_layer();
    }
}

/// Phase 12b' — emit a `vscene.push_layer` for a [`SceneLayer`] op.
/// The matching `pop_layer` is emitted by the `SceneOp::PopLayer`
/// arm of `scene_to_vello_with_overrides`.
fn emit_push_layer(vscene: &mut vello::Scene, layer: &SceneLayer, scene: &Scene) {
    let blend = map_blend_mode(layer.blend_mode);
    let alpha = layer.alpha.clamp(0.0, 1.0);
    let world = transform_to_affine(&scene.transforms[layer.transform_id as usize]);

    match &layer.clip {
        SceneClip::None => {
            // No clip → use the viewport rect so vello has a shape
            // to clip against; the layer is logically unbounded but
            // pixels outside the viewport never get sampled anyway.
            let viewport = Rect::new(
                0.0,
                0.0,
                scene.viewport_width as f64,
                scene.viewport_height as f64,
            );
            vscene.push_layer(Fill::NonZero, blend, alpha, world, &viewport);
        }
        SceneClip::Rect { rect, radii } => {
            let r = Rect::new(
                rect[0] as f64,
                rect[1] as f64,
                rect[2] as f64,
                rect[3] as f64,
            );
            if radii.iter().any(|&v| v > 0.0) {
                let rrect = RoundedRect::from_rect(
                    r,
                    RoundedRectRadii::new(
                        radii[0] as f64,
                        radii[1] as f64,
                        radii[2] as f64,
                        radii[3] as f64,
                    ),
                );
                vscene.push_layer(Fill::NonZero, blend, alpha, world, &rrect);
            } else {
                vscene.push_layer(Fill::NonZero, blend, alpha, world, &r);
            }
        }
        SceneClip::Path(path) => {
            // Phase 9b' — arbitrary `kurbo::BezPath` clip. Same
            // path-build pipeline as `SceneShape`.
            let bez = build_bez_path(path);
            vscene.push_layer(Fill::NonZero, blend, alpha, world, &bez);
        }
    }
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

fn emit_gradient(
    vscene: &mut vello::Scene,
    grad: &SceneGradient,
    transforms: &[Transform],
) {
    let target = Rect::new(
        grad.x0 as f64,
        grad.y0 as f64,
        grad.x1 as f64,
        grad.y1 as f64,
    );
    let world = transform_to_affine(&transforms[grad.transform_id as usize]);

    let stops: Vec<ColorStop> = grad
        .stops
        .iter()
        .map(|s| ColorStop::from((s.offset, unpremultiply_color(s.color))))
        .collect();

    // Per Phase 1' p1prime_03: the GPU compute path ignores
    // `interpolation_cs`, so leave it at default (Srgb) — matches the
    // existing Phase 8 batched receipts which lerp in sRGB-encoded
    // component space.
    let (peniko_grad, brush_xform) = match grad.kind {
        GradientKind::Linear => {
            let [sx, sy, ex, ey] = grad.params;
            let g = Gradient::new_linear(
                Point::new(sx as f64, sy as f64),
                Point::new(ex as f64, ey as f64),
            )
            .with_stops(stops.as_slice());
            (g, None)
        }
        GradientKind::Radial => {
            let [cx, cy, rx, ry] = grad.params;
            let circular = (rx - ry).abs() < 1e-3;
            if circular {
                let g = Gradient::new_radial(Point::new(cx as f64, cy as f64), rx)
                    .with_stops(stops.as_slice());
                (g, None)
            } else {
                // Build a unit-circle radial at origin, then warp into
                // the desired ellipse via the brush transform. Vello
                // composes brush as `transform * brush_transform`, so
                // brush_transform maps brush-space → device-space.
                // We want brush-origin (0, 0) → (cx, cy) and brush-x
                // unit (1, 0) → (cx + rx, cy):
                //   brush_transform = translate(cx, cy) * scale(rx, ry).
                let g = Gradient::new_radial(Point::ORIGIN, 1.0)
                    .with_stops(stops.as_slice());
                let bx = Affine::translate((cx as f64, cy as f64))
                    * Affine::scale_non_uniform(rx as f64, ry as f64);
                (g, Some(bx))
            }
        }
        GradientKind::Conic => {
            let [cx, cy, start_angle, _pad] = grad.params;
            let g = Gradient::new_sweep(
                Point::new(cx as f64, cy as f64),
                start_angle,
                start_angle + std::f32::consts::TAU,
            )
            .with_stops(stops.as_slice());
            (g, None)
        }
    };

    let needs_clip = grad.clip_rect != NO_CLIP;
    if needs_clip {
        push_clip_layer(vscene, grad.clip_rect, grad.clip_corner_radii);
    }
    vscene.fill(Fill::NonZero, world, &peniko_grad, brush_xform, &target);
    if needs_clip {
        vscene.pop_layer();
    }
}

fn emit_image(
    vscene: &mut vello::Scene,
    image: &SceneImage,
    transforms: &[Transform],
    cache: &HashMap<ImageKey, ImageData>,
) {
    let img = cache
        .get(&image.key)
        .expect("scene_to_vello: SceneImage references unknown ImageKey");

    let (alpha, chromatic) = split_tint(image.color);
    let brush = ImageBrush::new(img.clone()).with_alpha(alpha);

    let target = Rect::new(
        image.x0 as f64,
        image.y0 as f64,
        image.x1 as f64,
        image.y1 as f64,
    );
    let world = transform_to_affine(&transforms[image.transform_id as usize]);
    let brush_xform = uv_to_target_affine(image.uv, target, img.width, img.height);

    let needs_clip = image.clip_rect != NO_CLIP;
    if needs_clip {
        push_clip_layer(vscene, image.clip_rect, image.clip_corner_radii);
    }

    if let Some(chromatic_color) = chromatic {
        // Wrap image + multiply step in a layer so the multiply
        // composes with the *image*, not with anything painted
        // before this primitive. SrcAtop on the inner Multiply
        // layer keeps transparent regions of the image transparent.
        vscene.push_layer(
            Fill::NonZero,
            Mix::Normal,
            1.0,
            Affine::IDENTITY,
            &target,
        );
        vscene.fill(Fill::NonZero, world, &brush, Some(brush_xform), &target);
        vscene.push_layer(
            Fill::NonZero,
            BlendMode::new(Mix::Multiply, Compose::SrcAtop),
            1.0,
            Affine::IDENTITY,
            &target,
        );
        vscene.fill(Fill::NonZero, world, chromatic_color, None, &target);
        vscene.pop_layer();
        vscene.pop_layer();
    } else {
        vscene.fill(Fill::NonZero, world, &brush, Some(brush_xform), &target);
    }

    if needs_clip {
        vscene.pop_layer();
    }
}

/// Roadmap C2 — emit a tiling [`ScenePattern`] op. Repeats the
/// `tile` image (extended via `Extend::Repeat` on both axes) across
/// the `extent` rectangle. `scale` shapes the tile size: native
/// image pixels span `image_size * scale` in scene-local space.
fn emit_pattern(
    vscene: &mut vello::Scene,
    pattern: &ScenePattern,
    transforms: &[Transform],
    cache: &HashMap<ImageKey, ImageData>,
) {
    let img = cache
        .get(&pattern.tile)
        .expect("scene_to_vello: ScenePattern references unknown ImageKey");

    // Negative or zero scale gets clamped to 1.0 (the API contract
    // says "treated as 1.0"); avoid divide-by-zero in the brush
    // transform.
    let scale = if pattern.scale > 0.0 { pattern.scale as f64 } else { 1.0 };

    let brush = ImageBrush::new(img.clone()).with_extend(Extend::Repeat);
    // Brush-space → scene-space: scale by `scale` so a unit step in
    // the image's pixel space becomes `scale` units in scene-local
    // space (a tile is `image_size * scale` wide).
    let brush_xform = Affine::scale(scale);

    let target = Rect::new(
        pattern.extent[0] as f64,
        pattern.extent[1] as f64,
        pattern.extent[2] as f64,
        pattern.extent[3] as f64,
    );
    let world = transform_to_affine(&transforms[pattern.transform_id as usize]);

    let needs_clip = pattern.clip_rect != NO_CLIP;
    if needs_clip {
        push_clip_layer(vscene, pattern.clip_rect, pattern.clip_corner_radii);
    }
    vscene.fill(Fill::NonZero, world, &brush, Some(brush_xform), &target);
    if needs_clip {
        vscene.pop_layer();
    }
}

/// Map UV `[u0, v0, u1, v1]` (normalized to `[0, 1]`) of a `(W, H)`
/// image onto a target `Rect`. The returned affine is the brush
/// transform passed to `vello::Scene::fill`: it maps brush-local
/// coordinates (= image pixel coordinates) onto target-rect
/// coordinates so that the UV sub-region lands on the rect's bounds.
fn uv_to_target_affine(uv: [f32; 4], target: Rect, image_w: u32, image_h: u32) -> Affine {
    let (u0, v0, u1, v1) = (uv[0] as f64, uv[1] as f64, uv[2] as f64, uv[3] as f64);
    let w = image_w as f64;
    let h = image_h as f64;
    // Source pixel range covered by the UV slice.
    let src_x0 = u0 * w;
    let src_y0 = v0 * h;
    let src_w = (u1 - u0) * w;
    let src_h = (v1 - v0) * h;
    let tgt_w = target.width();
    let tgt_h = target.height();
    let sx = if src_w.abs() > 0.0 { tgt_w / src_w } else { 1.0 };
    let sy = if src_h.abs() > 0.0 { tgt_h / src_h } else { 1.0 };
    // brush_xform * src_pixel = target_pixel, i.e. translate then scale.
    Affine::translate((target.x0 - src_x0 * sx, target.y0 - src_y0 * sy))
        * Affine::scale_non_uniform(sx, sy)
}

/// Decompose a premultiplied tint `[r, g, b, a]` into an alpha
/// multiplier (applied to the image brush via `with_alpha`) and an
/// optional chromatic factor (applied via a `Mix::Multiply` layer
/// per §3.2). Returns `(a, None)` when the tint is achromatic
/// (white-with-alpha — straight RGB equals 1).
fn split_tint(color: [f32; 4]) -> (f32, Option<Color>) {
    let [r, g, b, a] = color;
    let a_clamped = a.clamp(0.0, 1.0);
    if a_clamped <= 0.0 {
        return (0.0, None);
    }
    // Premultiplied → straight: each channel divided by alpha.
    let sr = (r / a_clamped).clamp(0.0, 1.0);
    let sg = (g / a_clamped).clamp(0.0, 1.0);
    let sb = (b / a_clamped).clamp(0.0, 1.0);
    let achromatic = (sr - 1.0).abs() < 1e-3
        && (sg - 1.0).abs() < 1e-3
        && (sb - 1.0).abs() < 1e-3;
    if achromatic {
        (a_clamped, None)
    } else {
        let chromatic = Color::from_rgba8(
            (sr * 255.0).round() as u8,
            (sg * 255.0).round() as u8,
            (sb * 255.0).round() as u8,
            255,
        );
        (a_clamped, Some(chromatic))
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
