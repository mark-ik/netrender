/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Geometry op emitters: rects, glyph runs, paths/shapes, strokes, and layer
//! pushes (plus their stroke-style / path helpers). See [`super`].

use vello::kurbo::{BezPath, Cap, Join, Point, Rect, RoundedRect, RoundedRectRadii, Stroke};
use vello::peniko::{self, Fill, FontData};

use crate::scene::{
    FontBlob, NO_CLIP, PathOp, Scene, SceneBlendMode, SceneClip, SceneGlyphRun, SceneLayer,
    SceneRect, SceneShape, SceneStroke, SceneStrokeCap, SceneStrokeJoin, Transform,
};

use super::{push_clip_layer, transform_to_affine, unpremultiply_color};
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

/// Roadmap C3 — Map a netrender (`SceneBlendMode`, `SceneCompose`)
/// pair to a vello [`BlendMode`]. The previous standalone
/// `map_blend_mode(SceneBlendMode)` is now an `SrcOver` shortcut
/// inlined at use sites.
fn map_layer_blend(b: SceneBlendMode, c: crate::scene::SceneCompose) -> peniko::BlendMode {
    let mix = match b {
        SceneBlendMode::Normal => peniko::Mix::Normal,
        SceneBlendMode::Multiply => peniko::Mix::Multiply,
        SceneBlendMode::Screen => peniko::Mix::Screen,
        SceneBlendMode::Overlay => peniko::Mix::Overlay,
        SceneBlendMode::Darken => peniko::Mix::Darken,
        SceneBlendMode::Lighten => peniko::Mix::Lighten,
    };
    let compose = match c {
        crate::scene::SceneCompose::SrcOver => peniko::Compose::SrcOver,
        crate::scene::SceneCompose::DestIn => peniko::Compose::DestIn,
    };
    peniko::BlendMode::new(mix, compose)
}

/// Translate a netrender [`Scene`] into a [`vello::Scene`] suitable
/// for [`vello::Renderer::render_to_texture`].

pub(super) fn emit_rect(vscene: &mut vello::Scene, rect: &SceneRect, transforms: &[Transform]) {
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

pub(super) fn emit_glyph_run(
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
        .filter_map(|(tag, value)| std::str::from_utf8(tag).ok().map(|s| (s, *value)))
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

pub(super) fn emit_shape(vscene: &mut vello::Scene, shape: &SceneShape, transforms: &[Transform]) {
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

pub(super) fn emit_stroke(vscene: &mut vello::Scene, stroke: &SceneStroke, transforms: &[Transform]) {
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
pub(super) fn emit_push_layer(vscene: &mut vello::Scene, layer: &SceneLayer, scene: &Scene) {
    // Roadmap C3 — thread the layer's compose mode through so
    // alpha-mask layers (`SceneCompose::DestIn`) get their special
    // composite at pop time.
    let blend = map_layer_blend(layer.blend_mode, layer.compose);
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

