/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Per-command emitters: clips, layers, nine-patch borders, and gradients,
//! plus the box-shadow key base and transform/matrix helpers.

use std::collections::HashMap;

use log::warn;
use netrender::{
    GradientKind, ImageKey as NrImageKey, NO_CLIP, SHARP_CLIP, Scene,
    SceneBlendMode, SceneClip, SceneLayer, SceneStroke, SceneStrokeCap, SceneStrokeJoin, Transform,
};
use paint_list_api::{
    self as ple, ColorF, ImageKey,
};
use crate::convert::*;

pub(crate) const BOX_SHADOW_MASK_KEY_BASE: u64 = 0xFFFF_0000_0000_0000;

/// Apply a column-major 4×4 transform to a 2D point `(x, y, 0, 1)`,
/// returning the transformed `(x', y')`. Used to lift a shadow box
/// from its element-local coords into absolute scene coords for the
/// GPU mask pass (the mask is built in scene space, not local space).
pub(crate) fn apply_transform_2d(m: &[f32; 16], x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[4] * y + m[12], m[1] * x + m[5] * y + m[13])
}

/// The `Transform` at a palette index; identity for index 0.
pub(crate) fn transform_at(scene: &Scene, tid: u32) -> Transform {
    scene
        .transforms
        .get(tid as usize)
        .copied()
        .unwrap_or(Transform::IDENTITY)
}

/// Column-major 4×4 matrix multiply: `a ∘ b` (apply `b` first, then
/// `a`). Used to compose a child transform with its parent so nested
/// `PushTransform`s accumulate.
pub(crate) fn mat_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    for col in 0..4 {
        for row in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[k * 4 + row] * b[col * 4 + k];
            }
            out[col * 4 + row] = s;
        }
    }
    out
}

// =============================================================================
// PaintCmd per-variant emit helpers
// =============================================================================

pub(crate) fn emit_push_clip(scene: &mut Scene, spec: &ple::ClipSpec, tid: u32) {
    let clip = match &spec.kind {
        ple::ClipKind::Rect(rect) => {
            let (x0, y0, x1, y1) = rect_corners(rect);
            SceneClip::Rect {
                rect: [x0, y0, x1, y1],
                radii: [0.0, 0.0, 0.0, 0.0],
            }
        },
        ple::ClipKind::RoundedRect { rect, radius, .. } => {
            let (x0, y0, x1, y1) = rect_corners(rect);
            SceneClip::Rect {
                rect: [x0, y0, x1, y1],
                radii: [
                    radius.top_left.width,
                    radius.top_right.width,
                    radius.bottom_right.width,
                    radius.bottom_left.width,
                ],
            }
        },
        // Arbitrary-path clip (CSS clip-path basic shapes). The scene + vello
        // rasterizer already lower `SceneClip::Path` to a kurbo BezPath clip
        // (Phase 9b'); reuse the same path reconstruction as DrawPath.
        ple::ClipKind::Path(pd) => SceneClip::Path(path_data_to_scene_path(pd)),
    };
    // The clip layer carries the active transform so its geometry is
    // resolved in the same coordinate space as the clipped content.
    scene.push_layer(SceneLayer {
        clip,
        alpha: 1.0,
        blend_mode: SceneBlendMode::Normal,
        compose: netrender::SceneCompose::SrcOver,
        transform_id: tid,
        backdrop_filter: None,
        filters: Vec::new(),
    });
}

/// Map a paint-list `FilterOp` to a netrender `SceneFilter`. `Opacity` returns
/// `None` — it folds into the layer alpha, not the output-filter chain.
fn filter_op_to_scene(f: &ple::FilterOp) -> Option<netrender::SceneFilter> {
    use netrender::SceneFilter as F;
    use ple::FilterOp as O;
    Some(match *f {
        O::Blur(r) => F::Blur(r),
        O::Brightness(v) => F::Brightness(v),
        O::Contrast(v) => F::Contrast(v),
        O::Grayscale(v) => F::Grayscale(v),
        O::HueRotate(v) => F::HueRotate(v),
        O::Invert(v) => F::Invert(v),
        O::Saturate(v) => F::Saturate(v),
        O::Sepia(v) => F::Sepia(v),
        O::Opacity(_) => return None,
        // `drop-shadow()` is its own increment (an element-alpha shadow, like
        // box-shadow); `color-matrix()` (arbitrary 4x5) is a follow-up.
        O::DropShadow { .. } | O::ColorMatrix(_) => return None,
    })
}

pub(crate) fn compose_with_origin(t: &Transform, ox: f32, oy: f32) -> Transform {
    // The netrender Transform is a flat 16-float array. "translate by
    // (ox, oy) then apply t" sets the translation columns: [12] += ox,
    // [13] += oy.
    let mut out = t.m;
    out[12] += ox;
    out[13] += oy;
    Transform { m: out }
}

pub(crate) fn emit_push_layer(scene: &mut Scene, spec: &ple::LayerSpec, tid: u32) {
    let blend_mode = mix_blend_mode_to_scene(spec.mix_blend_mode);
    let mut alpha = spec.opacity;
    // `opacity()` in the chain folds into the layer alpha; the other filter
    // functions apply to the layer's own rasterized output (SceneLayer::filters,
    // rendered offscreen + filtered + composited by netrender).
    for filter in &spec.filters {
        if let ple::FilterOp::Opacity(a) = filter {
            alpha *= *a;
        }
    }
    let filters: Vec<netrender::SceneFilter> = spec.filters.iter().filter_map(filter_op_to_scene).collect();
    let _ = spec.raster_space; // Local vs Screen — deferred
    let _ = spec.flags;        // BLEND_CONTAINER etc. — deferred
    let _ = &spec.mask;        // alpha-mask layer — deferred
    scene.push_layer(SceneLayer {
        clip: SceneClip::None,
        alpha,
        blend_mode,
        compose: netrender::SceneCompose::SrcOver,
        transform_id: tid,
        backdrop_filter: None,
        filters,
    });
}

/// Lower a nine-patch (`border-image`) border. The source image is sliced into
/// four corners, four edges, and an optional fill center; each region is sampled
/// from the source by a UV sub-rect and drawn to its destination — corners
/// scaled, edges stretched or tiled per `repeat_horizontal` / `repeat_vertical`,
/// the center filled when `fill`. One source image, UV-sampled per region (no
/// producer-side pre-slicing), and partial `repeat` tiles are UV-cropped rather
/// than clipped — so no clip-layer edge AA.
///
/// `border.placement.bounds` is the border-image area; `border.widths` the
/// destination border widths; `np.slice` the source slice insets (px); `np.width`
/// / `np.height` the source intrinsic size.
pub(crate) fn emit_nine_patch(
    scene: &mut Scene,
    border: &ple::BorderItem,
    np: &ple::NinePatchBorder,
    image_map: &HashMap<ImageKey, NrImageKey>,
    tid: u32,
) {
    let key = match np.source {
        ple::NinePatchSource::Image(k, _rendering) => match image_map.get(&k) {
            Some(&nr) => nr,
            None => {
                warn!("[paint translator] nine-patch source image {:?} unregistered; skipping", k);
                return;
            },
        },
        // Gradient sources emit directly elsewhere; only image sources slice here.
        _ => {
            warn!("[paint translator] nine-patch gradient source deferred");
            return;
        },
    };
    let (w, h) = (np.width as f32, np.height as f32);
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let (dx0, dy0, dx1, dy1) = rect_corners(&border.placement.bounds);
    let (wt, wr, wb, wl) =
        (border.widths.top, border.widths.right, border.widths.bottom, border.widths.left);
    let (st, sr, sb, sl) =
        (np.slice.top as f32, np.slice.right as f32, np.slice.bottom as f32, np.slice.left as f32);

    // Sample source px-rect (sx0,sy0)-(sx1,sy1) onto dest rect (a,b)-(c,d), scaled.
    // `push_image_clamped` crops to the UV sub-rect so a slice region never bleeds
    // its neighbour's source pixels across the seam.
    let img = |scene: &mut Scene, a, b, c, d, sx0: f32, sy0: f32, sx1: f32, sy1: f32| {
        if c - a <= 0.0 || d - b <= 0.0 || sx1 - sx0 <= 0.0 || sy1 - sy0 <= 0.0 {
            return;
        }
        scene.push_image_clamped(
            a,
            b,
            c,
            d,
            [sx0 / w, sy0 / h, sx1 / w, sy1 / h],
            [1.0, 1.0, 1.0, 1.0],
            key,
            tid,
            NO_CLIP,
        );
    };

    // Corners — always scaled.
    img(scene, dx0, dy0, dx0 + wl, dy0 + wt, 0.0, 0.0, sl, st);
    img(scene, dx1 - wr, dy0, dx1, dy0 + wt, w - sr, 0.0, w, st);
    img(scene, dx0, dy1 - wb, dx0 + wl, dy1, 0.0, h - sb, sl, h);
    img(scene, dx1 - wr, dy1 - wb, dx1, dy1, w - sr, h - sb, w, h);

    // One edge: lay the source strip [s_along_lo..s_along_hi] × [s_cross_lo..hi]
    // along the dest [d_along_lo..hi] at cross [d_cross_lo..hi], per `repeat`. For
    // a horizontal edge `along` is x and `cross` is y; for a vertical edge they
    // swap. The cross axis fills the border width; the tile's along-length scales
    // proportionally.
    #[allow(clippy::too_many_arguments)]
    fn tile_edge(
        scene: &mut Scene,
        img: &dyn Fn(&mut Scene, f32, f32, f32, f32, f32, f32, f32, f32),
        horizontal: bool,
        d_along_lo: f32,
        d_along_hi: f32,
        d_cross_lo: f32,
        d_cross_hi: f32,
        s_along_lo: f32,
        s_along_hi: f32,
        s_cross_lo: f32,
        s_cross_hi: f32,
        repeat: ple::RepeatMode,
    ) {
        let d_len = d_along_hi - d_along_lo;
        let d_cross = d_cross_hi - d_cross_lo;
        let s_along = s_along_hi - s_along_lo;
        let s_cross = s_cross_hi - s_cross_lo;
        if d_len <= 0.0 || d_cross <= 0.0 || s_along <= 0.0 || s_cross <= 0.0 {
            return;
        }
        // One tile covering dest along [a0,a1] sampling source along [su0,su1].
        let put = |scene: &mut Scene, a0: f32, a1: f32, su0: f32, su1: f32| {
            if horizontal {
                img(scene, a0, d_cross_lo, a1, d_cross_hi, su0, s_cross_lo, su1, s_cross_hi);
            } else {
                img(scene, d_cross_lo, a0, d_cross_hi, a1, s_cross_lo, su0, s_cross_hi, su1);
            }
        };
        // Natural tile along-length: the source strip scaled so its cross fills the
        // border width.
        let natural = (s_along * (d_cross / s_cross)).max(1.0);
        match repeat {
            ple::RepeatMode::Stretch => put(scene, d_along_lo, d_along_hi, s_along_lo, s_along_hi),
            ple::RepeatMode::Round => {
                // Rescale so a whole number of tiles fills the edge exactly.
                let n = (d_len / natural).round().max(1.0);
                let t = d_len / n;
                for i in 0..(n as usize) {
                    let a0 = d_along_lo + i as f32 * t;
                    put(scene, a0, a0 + t, s_along_lo, s_along_hi);
                }
            },
            ple::RepeatMode::Space => {
                // Whole tiles at natural size with equal gaps; first/last at the
                // edges. None if not even one fits.
                let n = (d_len / natural).floor();
                if n < 1.0 {
                    return;
                }
                let gap = if n > 1.0 { (d_len - n * natural) / (n - 1.0) } else { 0.0 };
                // A lone tile (no room for two) centers in the edge.
                let base = if n == 1.0 { d_along_lo + (d_len - natural) / 2.0 } else { d_along_lo };
                for i in 0..(n as usize) {
                    let a0 = base + i as f32 * (natural + gap);
                    put(scene, a0, a0 + natural, s_along_lo, s_along_hi);
                }
            },
            ple::RepeatMode::Repeat => {
                // Whole tiles from the start, then a UV-cropped partial.
                let mut a0 = d_along_lo;
                let mut guard = 0;
                while a0 < d_along_hi - 0.01 && guard < 4096 {
                    let a1 = (a0 + natural).min(d_along_hi);
                    let su1 = s_along_lo + s_along * ((a1 - a0) / natural);
                    put(scene, a0, a1, s_along_lo, su1);
                    a0 += natural;
                    guard += 1;
                }
            },
        }
    }

    // Top + bottom edges (horizontal), then left + right (vertical).
    tile_edge(scene, &img, true, dx0 + wl, dx1 - wr, dy0, dy0 + wt, sl, w - sr, 0.0, st, np.repeat_horizontal);
    tile_edge(scene, &img, true, dx0 + wl, dx1 - wr, dy1 - wb, dy1, sl, w - sr, h - sb, h, np.repeat_horizontal);
    tile_edge(scene, &img, false, dy0 + wt, dy1 - wb, dx0, dx0 + wl, st, h - sb, 0.0, sl, np.repeat_vertical);
    tile_edge(scene, &img, false, dy0 + wt, dy1 - wb, dx1 - wr, dx1, st, h - sb, w - sr, w, np.repeat_vertical);

    // Center — only with `fill`; stretched (tiled fill is a refinement).
    if np.fill {
        img(scene, dx0 + wl, dy0 + wt, dx1 - wr, dy1 - wb, sl, st, w - sr, h - sb);
    }
}

pub(crate) fn emit_border_first_cut(scene: &mut Scene, border: &ple::BorderItem, tid: u32) {
    use ple::BorderStyle as BS;
    let rect = &border.placement.bounds;
    let widths = &border.widths;
    let sides = match &border.details {
        ple::BorderDetails::Normal(n) => n,
        ple::BorderDetails::NinePatch(_) => {
            warn!("[paint translator] nine-patch border deferred");
            return;
        },
    };

    // Uniform border (all four sides identical width / style / color) is the
    // common case and the only one a single stroked path can render — strokes
    // can't carry per-side colors. When it's also rounded or dashed/dotted, take
    // the stroke fast path; the 4-rect fallback below handles the rest (and
    // per-side borders, always square).
    let r = &sides.radius;
    let has_radius = r.top_left.width > 0.0 || r.top_right.width > 0.0
        || r.bottom_right.width > 0.0 || r.bottom_left.width > 0.0;
    let w = widths.top;
    let uniform_width = (widths.right - w).abs() < 0.01
        && (widths.bottom - w).abs() < 0.01
        && (widths.left - w).abs() < 0.01;
    let s = sides.top.style;
    let uniform_style = sides.right.style == s && sides.bottom.style == s && sides.left.style == s;
    let c = sides.top.color;
    let col_eq = |a: &ColorF, b: &ColorF| (a.r - b.r).abs() < 0.004 && (a.g - b.g).abs() < 0.004
        && (a.b - b.b).abs() < 0.004 && (a.a - b.a).abs() < 0.004;
    let uniform_color = col_eq(&sides.right.color, &c)
        && col_eq(&sides.bottom.color, &c) && col_eq(&sides.left.color, &c);
    // dotted/dashed need a stroke (dash pattern); solid needs a stroke only to
    // round. `double`/`groove`/etc. fall through to the (square, solid) fallback.
    let dash = match s {
        BS::Dotted => Some(vec![w.max(1.0), w.max(1.0)]),
        BS::Dashed => Some(vec![(w * 3.0).max(1.0), w.max(1.0)]),
        _ => None,
    };
    let strokeable = matches!(s, BS::Solid | BS::Dotted | BS::Dashed);

    if w > 0.01 && uniform_width && uniform_style && uniform_color && strokeable
        && (has_radius || dash.is_some())
    {
        // Stroke is centered on its path; a CSS border sits inside the
        // border-box, so inset the path by w/2 and shrink the corner radii to
        // match (radius is measured at the border-box edge).
        let h = w * 0.5;
        let inset = |r: f32| (r - h).max(0.0);
        scene.push_stroke_op(SceneStroke {
            x0: rect.min.x + h,
            y0: rect.min.y + h,
            x1: rect.max.x - h,
            y1: rect.max.y - h,
            color: color_to_array(&c),
            stroke_width: w,
            stroke_corner_radii: [
                inset(r.top_left.width),
                inset(r.top_right.width),
                inset(r.bottom_right.width),
                inset(r.bottom_left.width),
            ],
            transform_id: tid,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
            cap: SceneStrokeCap::Butt,
            join: SceneStrokeJoin::Miter,
            dash_pattern: dash.unwrap_or_default(),
            dash_offset: 0.0,
        });
        return;
    }

    // Fallback: per-side filled rects (square corners). Handles non-uniform
    // borders and styles the stroke path doesn't model yet (double/groove/...).
    if widths.top > 0.0 {
        scene.push_rect_transformed(
            rect.min.x,
            rect.min.y,
            rect.max.x,
            rect.min.y + widths.top,
            color_to_array(&sides.top.color),
            tid,
        );
    }
    if widths.bottom > 0.0 {
        scene.push_rect_transformed(
            rect.min.x,
            rect.max.y - widths.bottom,
            rect.max.x,
            rect.max.y,
            color_to_array(&sides.bottom.color),
            tid,
        );
    }
    if widths.left > 0.0 {
        scene.push_rect_transformed(
            rect.min.x,
            rect.min.y,
            rect.min.x + widths.left,
            rect.max.y,
            color_to_array(&sides.left.color),
            tid,
        );
    }
    if widths.right > 0.0 {
        scene.push_rect_transformed(
            rect.max.x - widths.right,
            rect.min.y,
            rect.max.x,
            rect.max.y,
            color_to_array(&sides.right.color),
            tid,
        );
    }
    let _ = sides.radius;
    let _ = sides.do_aa;
}

pub(crate) fn emit_linear_gradient(scene: &mut Scene, item: &ple::LinearGradientItem, tid: u32) {
    let rect = &item.placement.bounds;
    let g = &item.gradient;
    scene.push_gradient(netrender::SceneGradient {
        x0: rect.min.x,
        y0: rect.min.y,
        x1: rect.max.x,
        y1: rect.max.y,
        kind: GradientKind::Linear,
        repeat: matches!(g.extend_mode, ple::ExtendMode::Repeat),
        params: [
            g.start_point.x,
            g.start_point.y,
            g.end_point.x,
            g.end_point.y,
        ],
        stops: gradient_stops(&g.stops),
        transform_id: tid,
        clip_rect: NO_CLIP,
        clip_corner_radii: [0.0; 4],
    });
}

pub(crate) fn emit_radial_gradient(scene: &mut Scene, item: &ple::RadialGradientItem, tid: u32) {
    let rect = &item.placement.bounds;
    let g = &item.gradient;
    scene.push_gradient(netrender::SceneGradient {
        x0: rect.min.x,
        y0: rect.min.y,
        x1: rect.max.x,
        y1: rect.max.y,
        kind: GradientKind::Radial,
        repeat: matches!(g.extend_mode, ple::ExtendMode::Repeat),
        params: [g.center.x, g.center.y, g.radius.width, g.radius.height],
        stops: gradient_stops(&g.stops),
        transform_id: tid,
        clip_rect: NO_CLIP,
        clip_corner_radii: [0.0; 4],
    });
}

pub(crate) fn emit_conic_gradient(scene: &mut Scene, item: &ple::ConicGradientItem, tid: u32) {
    let rect = &item.placement.bounds;
    let g = &item.gradient;
    scene.push_gradient(netrender::SceneGradient {
        x0: rect.min.x,
        y0: rect.min.y,
        x1: rect.max.x,
        y1: rect.max.y,
        kind: GradientKind::Conic,
        repeat: matches!(g.extend_mode, ple::ExtendMode::Repeat),
        params: [g.center.x, g.center.y, g.angle, 0.0],
        stops: gradient_stops(&g.stops),
        transform_id: tid,
        clip_rect: NO_CLIP,
        clip_corner_radii: [0.0; 4],
    });
}

