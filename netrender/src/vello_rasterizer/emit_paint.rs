/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Paint op emitters: linear/radial/conic gradients, images (UV crop, alpha +
//! chromatic tint), and repeated-tile patterns. See [`super`].

use std::collections::HashMap;

use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{
    self, BlendMode, Color, ColorStop, Compose, Extend, Fill, Gradient, ImageBrush,
    ImageData, ImageFormat, Mix,
};

use crate::scene::{
    GradientKind, ImageKey, NO_CLIP, SceneGradient, SceneImage, ScenePattern, Transform,
};

use super::{push_clip_layer, transform_to_affine, unpremultiply_color};
pub(super) fn emit_gradient(vscene: &mut vello::Scene, grad: &SceneGradient, transforms: &[Transform]) {
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
    let (mut peniko_grad, brush_xform) = match grad.kind {
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
                let g = Gradient::new_radial(Point::ORIGIN, 1.0).with_stops(stops.as_slice());
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

    // `repeating-*-gradient`: tile the colorstop range instead of clamping. The
    // producer sized the gradient to one repeat period, so Repeat reproduces the
    // CSS pattern across the fill.
    if grad.repeat {
        peniko_grad.extend = Extend::Repeat;
    }

    let needs_clip = grad.clip_rect != NO_CLIP;
    if needs_clip {
        push_clip_layer(vscene, grad.clip_rect, grad.clip_corner_radii);
    }
    vscene.fill(Fill::NonZero, world, &peniko_grad, brush_xform, &target);
    if needs_clip {
        vscene.pop_layer();
    }
}

/// Crop `img` to the normalized `uv` sub-rect, returning a tightly-packed RGBA8
/// sub-image. A clamp-to-uv draw samples this crop (padded at its edges) instead
/// of a sub-region of the full source, so the sampler cannot read past the
/// sub-rect into adjacent source pixels.
fn crop_to_uv(img: &ImageData, uv: [f32; 4]) -> ImageData {
    let (iw, ih) = (img.width, img.height);
    let px = |u: f32, dim: u32| (u * dim as f32).round().clamp(0.0, dim as f32) as u32;
    let sx0 = px(uv[0], iw);
    let sy0 = px(uv[1], ih);
    let sx1 = px(uv[2], iw).max(sx0 + 1).min(iw);
    let sy1 = px(uv[3], ih).max(sy0 + 1).min(ih);
    let (sw, sh) = (sx1 - sx0, sy1 - sy0);
    let bytes: &[u8] = img.data.as_ref();
    let mut out = Vec::with_capacity((sw * sh * 4) as usize);
    for row in 0..sh {
        let y = sy0 + row;
        let start = (((y * iw) + sx0) * 4) as usize;
        out.extend_from_slice(&bytes[start..start + (sw * 4) as usize]);
    }
    ImageData {
        data: peniko::Blob::new(std::sync::Arc::new(out)),
        format: ImageFormat::Rgba8,
        alpha_type: img.alpha_type,
        width: sw,
        height: sh,
    }
}

pub(super) fn emit_image(
    vscene: &mut vello::Scene,
    image: &SceneImage,
    transforms: &[Transform],
    cache: &HashMap<ImageKey, ImageData>,
) -> Option<ImageKey> {
    let Some(img) = cache.get(&image.key) else {
        // A content scene (a fetched web page) must never crash the whole renderer:
        // skip an image whose source is missing rather than panic (a real page,
        // ycombinator.com, tripped the old `.expect()`). Return the missing key so
        // the caller reports one aggregated warn per rasterize — a systemic drop
        // (e.g. unbuilt box-shadow masks) is thousands of ops, and a per-op warn
        // floods the log into noise instead of a signal. (Diagnostics — aggregated.)
        return Some(image.key);
    };

    let (alpha, chromatic) = split_tint(image.color);
    let target = Rect::new(
        image.x0 as f64,
        image.y0 as f64,
        image.x1 as f64,
        image.y1 as f64,
    );
    let world = transform_to_affine(&transforms[image.transform_id as usize]);

    // Clamp-to-uv crops the source to the `uv` sub-rect and pads at its edges, so
    // bilinear filtering cannot bleed in neighbouring source pixels at a seam
    // (nine-patch slices, sprite cells). Otherwise sample the whole image, mapping
    // the `uv` sub-region onto the target via the brush transform.
    let (brush, brush_xform) = if image.clamp_to_uv {
        let sub = crop_to_uv(img, image.uv);
        let (sw, sh) = (sub.width, sub.height);
        (
            ImageBrush::new(sub).with_alpha(alpha).with_extend(Extend::Pad),
            uv_to_target_affine([0.0, 0.0, 1.0, 1.0], target, sw, sh),
        )
    } else {
        (
            ImageBrush::new(img.clone()).with_alpha(alpha),
            uv_to_target_affine(image.uv, target, img.width, img.height),
        )
    };

    let needs_clip = image.clip_rect != NO_CLIP;
    if needs_clip {
        push_clip_layer(vscene, image.clip_rect, image.clip_corner_radii);
    }

    if let Some(chromatic_color) = chromatic {
        // Wrap image + multiply step in a layer so the multiply
        // composes with the *image*, not with anything painted
        // before this primitive. SrcAtop on the inner Multiply
        // layer keeps transparent regions of the image transparent.
        vscene.push_layer(Fill::NonZero, Mix::Normal, 1.0, Affine::IDENTITY, &target);
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
    None
}

/// Roadmap C2 — emit a tiling [`ScenePattern`] op. Repeats the
/// `tile` image (extended via `Extend::Repeat` on both axes) across
/// the `extent` rectangle. `scale` shapes the tile size: native
/// image pixels span `image_size * scale` in scene-local space.
pub(super) fn emit_pattern(
    vscene: &mut vello::Scene,
    pattern: &ScenePattern,
    transforms: &[Transform],
    cache: &HashMap<ImageKey, ImageData>,
) -> Option<ImageKey> {
    let Some(img) = cache.get(&pattern.tile) else {
        // Same content-robustness as `emit_image`: skip a tiling pattern (a CSS
        // background) whose source is missing rather than panic. Return the key so
        // the caller aggregates the report. (Diagnostics — aggregated.)
        return Some(pattern.tile);
    };

    // Per-axis tile scale (CSS background-size). Non-positive on an axis is
    // clamped to 1.0 (the API contract; avoids a degenerate brush transform).
    let sx = if pattern.scale[0] > 0.0 { pattern.scale[0] as f64 } else { 1.0 };
    let sy = if pattern.scale[1] > 0.0 { pattern.scale[1] as f64 } else { 1.0 };

    let target = Rect::new(
        pattern.extent[0] as f64,
        pattern.extent[1] as f64,
        pattern.extent[2] as f64,
        pattern.extent[3] as f64,
    );

    let brush = ImageBrush::new(img.clone()).with_extend(Extend::Repeat);
    // Brush-space (image pixels) → scene-space: scale each axis (a tile spans
    // `image_w * sx` by `image_h * sy`) AND translate so the first tile's origin
    // is the extent's top-left — otherwise the repeat phase is anchored at the
    // scene origin, shifting the tiling (mirrors `uv_to_target_affine`).
    let brush_xform =
        Affine::translate((target.x0, target.y0)) * Affine::scale_non_uniform(sx, sy);
    let world = transform_to_affine(&transforms[pattern.transform_id as usize]);

    let needs_clip = pattern.clip_rect != NO_CLIP;
    if needs_clip {
        push_clip_layer(vscene, pattern.clip_rect, pattern.clip_corner_radii);
    }
    vscene.fill(Fill::NonZero, world, &brush, Some(brush_xform), &target);
    if needs_clip {
        vscene.pop_layer();
    }
    None
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
    let sx = if src_w.abs() > 0.0 {
        tgt_w / src_w
    } else {
        1.0
    };
    let sy = if src_h.abs() > 0.0 {
        tgt_h / src_h
    } else {
        1.0
    };
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
    let achromatic = (sr - 1.0).abs() < 1e-3 && (sg - 1.0).abs() < 1e-3 && (sb - 1.0).abs() < 1e-3;
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
