/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Per-op point-containment geometry for [`super::hit_test`]: hittable-kind
//! classification, per-op AABB / path containment, per-glyph hit metrics, and
//! world->local point mapping.

use vello::kurbo::{Affine, Point, Shape};

use crate::scene::{
    NO_CLIP, Scene, SceneGradient, SceneImage, SceneOp, SceneRect, SceneShape, SceneStroke,
};
use crate::vello_rasterizer::{build_bez_path, transform_to_affine};

use super::HitOpKind;

/// Per-glyph hit test inside a [`SceneGlyphRun`]. Returns the index
/// of the glyph whose AABB contains `point`, or `None` if the
/// point is in the run's overall AABB but doesn't land on any
/// individual glyph.
///
/// Roadmap R1 — uses real font-supplied glyph bounds via
/// `skrifa::metrics::GlyphMetrics` when the font parses cleanly.
/// Falls back to an em-box approximation when:
///
/// - the font palette entry is the sentinel (`font_id == 0`),
/// - skrifa can't parse the font bytes (empty / corrupt),
/// - the glyph id has no outline bounds in the font (e.g., COLR
///   emoji glyphs where the outline table is empty — the glyph
///   still rasterizes via color layers, but skrifa's bounds
///   query returns `None`).
///
/// The em-box fallback sketches `(x, y - font_size) → (x +
/// advance, y + font_size * 0.25)` — ascender to shallow
/// descender — which is conservative (over-includes) rather than
/// tight. UI use cases (clicking a character) prefer
/// over-inclusive misses to under-inclusive ones.
pub(super) fn glyph_run_per_glyph_hit(
    run: &crate::scene::SceneGlyphRun,
    point: [f32; 2],
    scene: &Scene,
) -> Option<usize> {
    use crate::tile_cache::world_aabb;
    if run.glyphs.is_empty() {
        return None;
    }

    // Try to load real metrics. `font_id == 0` is the no-font
    // sentinel; skip directly to em-box. For real fonts, parse the
    // blob via skrifa and build a GlyphMetrics at the run's font
    // size. If anything fails, `metrics` stays None and every
    // glyph falls back to em-box.
    use skrifa::MetadataProvider;
    let metrics: Option<skrifa::metrics::GlyphMetrics<'_>> = if run.font_id == 0 {
        None
    } else {
        let blob = &scene.fonts[run.font_id as usize];
        skrifa::FontRef::from_index(blob.data.data(), blob.index)
            .ok()
            .map(|font| {
                font.glyph_metrics(
                    skrifa::instance::Size::new(run.font_size),
                    skrifa::instance::LocationRef::default(),
                )
            })
    };

    for (i, g) in run.glyphs.iter().enumerate() {
        let local = glyph_local_aabb(run, i, g, metrics.as_ref());
        let world = world_aabb(local, run.transform_id, scene);
        if world[0] <= point[0]
            && point[0] <= world[2]
            && world[1] <= point[1]
            && point[1] <= world[3]
        {
            return Some(i);
        }
    }
    None
}

/// Compute the local-space AABB for a single glyph in the run.
/// Real font metrics when available, em-box approximation when
/// skrifa can't supply bounds for this glyph.
fn glyph_local_aabb(
    run: &crate::scene::SceneGlyphRun,
    i: usize,
    g: &crate::scene::Glyph,
    metrics: Option<&skrifa::metrics::GlyphMetrics<'_>>,
) -> [f32; 4] {
    if let Some(metrics) = metrics {
        if let Some(b) = metrics.bounds(skrifa::GlyphId::new(g.id)) {
            // Skrifa returns bounds in font (y-up) space at the
            // glyph origin; convert to scene (y-down) by mirroring
            // around g.y.
            return [g.x + b.x_min, g.y - b.y_max, g.x + b.x_max, g.y - b.y_min];
        }
    }
    // Em-box fallback.
    let n = run.glyphs.len();
    let advance = if i + 1 < n {
        (run.glyphs[i + 1].x - g.x).max(0.0)
    } else {
        run.font_size
    };
    let advance = advance.max(run.font_size * 0.25);
    [
        g.x,
        g.y - run.font_size,
        g.x + advance,
        g.y + run.font_size * 0.25,
    ]
}

/// Returns the [`HitOpKind`] for hittable ops, or `None` for
/// scope-only ops (push/pop layer) that have no visible body of
/// their own.
pub(super) fn hittable_kind(op: &SceneOp) -> Option<HitOpKind> {
    match op {
        SceneOp::Rect(_) => Some(HitOpKind::Rect),
        SceneOp::Stroke(_) => Some(HitOpKind::Stroke),
        SceneOp::Gradient(_) => Some(HitOpKind::Gradient),
        SceneOp::Image(_) => Some(HitOpKind::Image),
        SceneOp::Pattern(_) => Some(HitOpKind::Pattern),
        SceneOp::Shape(_) => Some(HitOpKind::Shape),
        SceneOp::GlyphRun(_) => Some(HitOpKind::GlyphRun),
        SceneOp::PushLayer(_) | SceneOp::PopLayer => None,
    }
}

pub(super) fn op_contains_point(op: &SceneOp, p: [f32; 2], scene: &Scene) -> bool {
    use crate::tile_cache::world_aabb_glyph_run;

    // Roadmap R2 — `SceneOp::Shape` gets a path-precise
    // point-in-polygon check after the AABB pre-pass. Other ops
    // remain AABB-only (rect/image/gradient/stroke/glyph-run aren't
    // path-shaped at this layer).
    if let SceneOp::Shape(s) = op {
        return shape_contains_point(s, p, scene);
    }

    let (world_box, clip_rect) = match op {
        SceneOp::Rect(r) => primitive_box_rect(r, scene),
        SceneOp::Stroke(s) => primitive_box_stroke(s, scene),
        SceneOp::Gradient(g) => primitive_box_gradient(g, scene),
        SceneOp::Image(i) => primitive_box_image(i, scene),
        SceneOp::Pattern(p) => {
            use crate::tile_cache::world_aabb;
            (world_aabb(p.extent, p.transform_id, scene), p.clip_rect)
        }
        SceneOp::Shape(_) => unreachable!("handled above"),
        SceneOp::GlyphRun(r) => match world_aabb_glyph_run(r, scene) {
            Some(aabb) => (aabb, r.clip_rect),
            None => return false,
        },
        // Layer ops are filtered out by `hittable_kind` before
        // reaching this fn; defensive return.
        SceneOp::PushLayer(_) | SceneOp::PopLayer => return false,
    };

    aabb_contains(world_box, p) && clip_allows(clip_rect, p)
}

/// Roadmap R2 — path-precise hit test for [`SceneOp::Shape`].
/// AABB pre-pass keeps the cheap outside-the-bounding-box case
/// fast; the BezPath::contains call only runs when the world AABB
/// covers the point.
///
/// Strokes-only (no fill) shapes are still treated as inside-the-
/// path for hit purposes: clicking the path interior counts as a hit
/// even if the painted region is just the outline. UI use cases
/// (clicking on a stroked node) typically want this. If a future
/// consumer needs "stroke-only" hit semantics, a fill_color-aware
/// branch can be added.
fn shape_contains_point(s: &SceneShape, p: [f32; 2], scene: &Scene) -> bool {
    use crate::tile_cache::world_aabb_shape;

    let Some(world_box) = world_aabb_shape(s, scene) else {
        return false;
    };
    if !aabb_contains(world_box, p) {
        return false;
    }
    if !clip_allows(s.clip_rect, p) {
        return false;
    }
    let Some(local) = world_point_to_local(p, s.transform_id, scene) else {
        // Non-invertible transform — keep AABB-conservative.
        return true;
    };
    let bez = build_bez_path(&s.path);
    bez.contains(local)
}

/// Apply the inverse of `transforms[transform_id]` to `world_point`,
/// returning the local-space point. `None` if the transform is
/// non-invertible (degenerate scale, reflection-degenerate, …) — in
/// that case the caller should fall back to the AABB-conservative
/// answer.
pub(super) fn world_point_to_local(world_point: [f32; 2], transform_id: u32, scene: &Scene) -> Option<Point> {
    let pt = Point::new(world_point[0] as f64, world_point[1] as f64);
    if transform_id == 0 {
        return Some(pt);
    }
    let affine: Affine = transform_to_affine(&scene.transforms[transform_id as usize]);
    // kurbo::Affine::inverse panics on non-invertible matrices; check
    // determinant first. det = a*d - b*c with column-major
    // [a, b, c, d, tx, ty].
    let coeffs = affine.as_coeffs();
    let det = coeffs[0] * coeffs[3] - coeffs[1] * coeffs[2];
    if det.abs() < 1e-12 {
        return None;
    }
    Some(affine.inverse() * pt)
}

fn primitive_box_rect(r: &SceneRect, scene: &Scene) -> ([f32; 4], [f32; 4]) {
    use crate::tile_cache::world_aabb;
    (
        world_aabb([r.x0, r.y0, r.x1, r.y1], r.transform_id, scene),
        r.clip_rect,
    )
}

fn primitive_box_image(i: &SceneImage, scene: &Scene) -> ([f32; 4], [f32; 4]) {
    use crate::tile_cache::world_aabb;
    (
        world_aabb([i.x0, i.y0, i.x1, i.y1], i.transform_id, scene),
        i.clip_rect,
    )
}

fn primitive_box_gradient(g: &SceneGradient, scene: &Scene) -> ([f32; 4], [f32; 4]) {
    use crate::tile_cache::world_aabb;
    (
        world_aabb([g.x0, g.y0, g.x1, g.y1], g.transform_id, scene),
        g.clip_rect,
    )
}

fn primitive_box_stroke(s: &SceneStroke, scene: &Scene) -> ([f32; 4], [f32; 4]) {
    use crate::tile_cache::world_aabb;
    let half = s.stroke_width * 0.5;
    (
        world_aabb(
            [s.x0 - half, s.y0 - half, s.x1 + half, s.y1 + half],
            s.transform_id,
            scene,
        ),
        s.clip_rect,
    )
}

pub(super) fn aabb_contains(a: [f32; 4], p: [f32; 2]) -> bool {
    p[0] >= a[0] && p[0] <= a[2] && p[1] >= a[1] && p[1] <= a[3]
}

fn clip_allows(clip_rect: [f32; 4], p: [f32; 2]) -> bool {
    clip_rect == NO_CLIP || aabb_contains(clip_rect, p)
}
