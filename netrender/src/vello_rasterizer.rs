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
use std::sync::Arc;

use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, RoundedRectRadii, Stroke};
use vello::peniko::{
    self, BlendMode, Blob, Color, ColorStop, Compose, Fill, Gradient, ImageAlphaType, ImageBrush,
    ImageData, ImageFormat, Mix,
};

use crate::scene::{
    GradientKind, ImageKey, NO_CLIP, PathOp, Scene, SceneGradient, SceneImage, SceneRect,
    SceneShape, SceneStroke, Transform,
};

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
    let mut vscene = vello::Scene::new();

    let images = build_image_cache(scene, image_overrides);

    for rect in &scene.rects {
        emit_rect(&mut vscene, rect, &scene.transforms);
    }
    for stroke in &scene.strokes {
        emit_stroke(&mut vscene, stroke, &scene.transforms);
    }
    for gradient in &scene.gradients {
        emit_gradient(&mut vscene, gradient, &scene.transforms);
    }
    for image in &scene.images {
        emit_image(&mut vscene, image, &scene.transforms, &images);
    }
    for shape in &scene.shapes {
        emit_shape(&mut vscene, shape, &scene.transforms);
    }

    vscene
}

fn build_image_cache(
    scene: &Scene,
    overrides: &HashMap<ImageKey, ImageData>,
) -> HashMap<ImageKey, ImageData> {
    let mut cache = HashMap::with_capacity(scene.image_sources.len() + overrides.len());
    // Path A — CPU bytes from scene.image_sources.
    for (key, data) in &scene.image_sources {
        let blob = Blob::new(Arc::new(data.bytes.clone()));
        cache.insert(
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

fn build_bez_path(path: &crate::scene::ScenePath) -> BezPath {
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
    let style = Stroke::new(stroke.stroke_width as f64);

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

fn transform_to_affine(t: &Transform) -> Affine {
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
