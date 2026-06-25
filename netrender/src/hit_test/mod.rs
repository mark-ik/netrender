/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Point queries against a [`Scene`]'s op list.
//!
//! [`hit_test`] walks `Scene::ops` in reverse painter order and
//! returns the stack of primitives covering `point` — index 0 of
//! the returned `Vec` is the top-most hit (the primitive painted
//! last), index `len - 1` is the bottom-most.
//!
//! For the common case of "what did the user click on," call
//! `.first()` on the result.
//!
//! ## Precision
//!
//! AABB-level only:
//!
//! - Rect / image / gradient: world-space axis-aligned bounding
//!   box of the primitive's local rect (after applying the
//!   transform from `scene.transforms`).
//! - Stroke: AABB inflated by `stroke_width / 2`. A point inside
//!   the inflated AABB but inside the *deflated* core counts as a
//!   hit too — i.e., the stroke's interior is treated as part of
//!   its hit area, which is usually what UI consumers want.
//! - Shape: bounding box of the path. Per-segment point-in-polygon
//!   is a future addition; today every point inside the path's
//!   AABB hits.
//! - Glyph run: combined AABB of glyph origins, inflated by
//!   `font_size`. Per-glyph bounds need real font metrics.
//!
//! `clip_rect` (when set) gates inclusion: a point outside the
//! clip's AABB does not hit, even if the primitive's AABB covers
//! it. Rounded-corner clips are tested against their AABB —
//! future work, if needed, can refine the corner regions.
//!
//! ## What this gives a consumer
//!
//! A graphshell-shaped consumer typically:
//! 1. Maps mouse coords to scene-space (apply scroll / zoom).
//! 2. Calls `hit_test(scene, point)`.
//! 3. Walks the stack from top to bottom, dispatching to whatever
//!    interactivity model lives one layer up (event bubbling,
//!    selection cascade, hover targeting, etc.). The top hit is
//!    enough for "click this," but transparent overlays and
//!    badges-over-thumbnails want the stack.

use vello::kurbo::{Rect, RoundedRect, RoundedRectRadii, Shape};

use crate::scene::{Scene, SceneClip, SceneOp, SHARP_CLIP};
use crate::vello_rasterizer::build_bez_path;

mod geometry;
use geometry::{
    aabb_contains, glyph_run_per_glyph_hit, hittable_kind, op_contains_point, world_point_to_local,
};
/// One primitive that covered the queried point. Returned in
/// top-most-first order from [`hit_test`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitResult {
    /// Index into [`Scene::ops`]. Stable for the lifetime of the
    /// scene; if the consumer holds onto a `HitResult` past
    /// further mutations to `Scene::ops` (push or remove), the
    /// index may refer to a different op.
    pub op_index: usize,
    /// The kind of op hit, mirroring [`SceneOp`]'s variants.
    pub kind: HitOpKind,
    /// For [`HitOpKind::GlyphRun`] hits, the index of the specific
    /// glyph whose per-glyph AABB contains the point, or `None`
    /// if the point is in the run's overall AABB but not on any
    /// individual glyph (e.g., trailing whitespace or inter-glyph
    /// gap). Always `None` for other kinds.
    pub glyph_index: Option<usize>,
}

/// Tag identifying which [`SceneOp`] variant a [`HitResult`]
/// refers to. Useful for filtering hits by primitive type without
/// re-matching against `Scene::ops`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitOpKind {
    Rect,
    Stroke,
    Gradient,
    Image,
    /// Roadmap C2 — repeated-tile fill (`SceneOp::Pattern`).
    Pattern,
    Shape,
    GlyphRun,
}

/// Return every primitive whose hit area contains `point`, ordered
/// top-most first (last-pushed first). An empty `Vec` means the
/// point hit nothing.
///
/// `point` is in scene-space (the same coordinate system the push
/// helpers use). Consumers that operate in window-space need to
/// apply their own scroll / zoom inverse first.
///
/// Layer clips *are* honored: a hittable op inside a
/// `PushLayer`/`PopLayer` scope only registers as a hit if the
/// point falls inside every active clip on the layer stack. Clip
/// containment is AABB-level (rounded-rect corners and the inside
/// of arbitrary `SceneClip::Path` clips count as hit areas — same
/// conservative AABB tradeoff as elsewhere in this module).
pub fn hit_test(scene: &Scene, point: [f32; 2]) -> Vec<HitResult> {
    let visibility = precompute_clip_visibility(scene, point);
    let mut hits = Vec::new();
    for (idx, op) in scene.ops.iter().enumerate().rev() {
        if !visibility[idx] {
            continue;
        }
        if let Some(kind) = hittable_kind(op) {
            if op_contains_point(op, point, scene) {
                let glyph_index = match op {
                    SceneOp::GlyphRun(run) => glyph_run_per_glyph_hit(run, point, scene),
                    _ => None,
                };
                hits.push(HitResult {
                    op_index: idx,
                    kind,
                    glyph_index,
                });
            }
        }
    }
    hits
}

/// Convenience: top-most hit only, or `None` if the point hit
/// nothing. Layer-clip-aware (same semantics as [`hit_test`]).
pub fn hit_test_topmost(scene: &Scene, point: [f32; 2]) -> Option<HitResult> {
    let visibility = precompute_clip_visibility(scene, point);
    for (idx, op) in scene.ops.iter().enumerate().rev() {
        if !visibility[idx] {
            continue;
        }
        if let Some(kind) = hittable_kind(op) {
            if op_contains_point(op, point, scene) {
                let glyph_index = match op {
                    SceneOp::GlyphRun(run) => glyph_run_per_glyph_hit(run, point, scene),
                    _ => None,
                };
                return Some(HitResult {
                    op_index: idx,
                    kind,
                    glyph_index,
                });
            }
        }
    }
    None
}

/// Forward-pass: for each op index, compute whether `point` is
/// inside every active layer clip at that op. Used by [`hit_test`]
/// and [`hit_test_topmost`] so the reverse traversal can short-
/// circuit on the first visible-and-hit op.
///
/// Clip containment: see [`clip_aabb_contains_point`]. Layer-clip
/// AABB matches the rasterizer's conservative bound; same caveat
/// (rounded-rect corners and arbitrary path interiors are AABB-
/// only — points in the corner/outside-the-true-path region
/// register as visible).
fn precompute_clip_visibility(scene: &Scene, point: [f32; 2]) -> Vec<bool> {
    let mut visibility = Vec::with_capacity(scene.ops.len());
    // Stack entry = "is `point` inside this layer's clip AABB?"
    // The op is visible iff every entry on the stack is true.
    let mut stack: Vec<bool> = Vec::new();
    let mut all_clips_inside = true;
    for op in &scene.ops {
        match op {
            SceneOp::PushLayer(layer) => {
                let inside =
                    clip_aabb_contains_point(&layer.clip, layer.transform_id, scene, point);
                stack.push(inside);
                all_clips_inside = all_clips_inside && inside;
            }
            SceneOp::PopLayer => {
                if stack.pop().is_some() {
                    all_clips_inside = stack.iter().all(|&b| b);
                }
            }
            _ => {}
        }
        visibility.push(all_clips_inside);
    }
    visibility
}

/// "Is `point` inside this clip" predicate used during hit-test
/// clip-stack tracking.
///
/// Roadmap R3 — path-precise containment for `SceneClip::Path` and
/// rounded-rect clips:
///
/// - `SceneClip::None`: trivially true.
/// - `SceneClip::Rect { rect, radii }`: AABB containment for sharp
///   rects (radii all zero); for non-zero radii, transform `point`
///   to clip-local space and test against `kurbo::RoundedRect`.
/// - `SceneClip::Path`: transform `point` to local and test against
///   the `kurbo::BezPath` interior (non-zero winding rule, matching
///   the rasterizer's `Fill::NonZero`).
///
/// AABB pre-pass guards the path-precise check so the common
/// outside-the-bounding-box case stays cheap.
fn clip_aabb_contains_point(
    clip: &SceneClip,
    transform_id: u32,
    scene: &Scene,
    point: [f32; 2],
) -> bool {
    use crate::tile_cache::world_aabb;
    match clip {
        SceneClip::None => true,
        SceneClip::Rect { rect, radii } => {
            let world_box = world_aabb(*rect, transform_id, scene);
            if !aabb_contains(world_box, point) {
                return false;
            }
            if *radii == SHARP_CLIP {
                return true;
            }
            // Rounded-rect: precise test in clip-local space.
            let Some(local) = world_point_to_local(point, transform_id, scene) else {
                // Non-invertible transform — keep AABB-conservative.
                return true;
            };
            let r = Rect::new(
                rect[0] as f64,
                rect[1] as f64,
                rect[2] as f64,
                rect[3] as f64,
            );
            let rrect = RoundedRect::from_rect(
                r,
                RoundedRectRadii::new(
                    radii[0] as f64,
                    radii[1] as f64,
                    radii[2] as f64,
                    radii[3] as f64,
                ),
            );
            rrect.contains(local)
        }
        SceneClip::Path(path) => {
            let Some(local_aabb) = path.local_aabb() else {
                // Empty path — nothing inside can be hit.
                return false;
            };
            let world_box = world_aabb(local_aabb, transform_id, scene);
            if !aabb_contains(world_box, point) {
                return false;
            }
            let Some(local) = world_point_to_local(point, transform_id, scene) else {
                return true;
            };
            let bez = build_bez_path(path);
            bez.contains(local)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Scene;

    #[test]
    fn empty_scene_hits_nothing() {
        let scene = Scene::new(64, 64);
        assert!(hit_test(&scene, [10.0, 10.0]).is_empty());
        assert!(hit_test_topmost(&scene, [10.0, 10.0]).is_none());
    }

    #[test]
    fn point_inside_rect_hits() {
        let mut scene = Scene::new(64, 64);
        scene.push_rect(10.0, 10.0, 30.0, 30.0, [1.0, 0.0, 0.0, 1.0]);
        let hits = hit_test(&scene, [20.0, 20.0]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, HitOpKind::Rect);
        assert_eq!(hits[0].op_index, 0);
    }

    #[test]
    fn point_outside_rect_misses() {
        let mut scene = Scene::new(64, 64);
        scene.push_rect(10.0, 10.0, 30.0, 30.0, [1.0, 0.0, 0.0, 1.0]);
        assert!(hit_test(&scene, [5.0, 5.0]).is_empty());
        assert!(hit_test(&scene, [50.0, 50.0]).is_empty());
    }

    #[test]
    fn stack_returns_top_first() {
        let mut scene = Scene::new(64, 64);
        // Three full-frame rects pushed in order. Hit at any
        // interior point gives a 3-deep stack with index 2 on top.
        scene.push_rect(0.0, 0.0, 64.0, 64.0, [1.0, 0.0, 0.0, 1.0]);
        scene.push_rect(0.0, 0.0, 64.0, 64.0, [0.0, 1.0, 0.0, 1.0]);
        scene.push_rect(0.0, 0.0, 64.0, 64.0, [0.0, 0.0, 1.0, 1.0]);

        let hits = hit_test(&scene, [32.0, 32.0]);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].op_index, 2, "top is the last-pushed (index 2)");
        assert_eq!(hits[1].op_index, 1);
        assert_eq!(hits[2].op_index, 0);
    }

    #[test]
    fn topmost_short_circuits() {
        let mut scene = Scene::new(64, 64);
        scene.push_rect(0.0, 0.0, 64.0, 64.0, [1.0, 0.0, 0.0, 1.0]);
        scene.push_rect(0.0, 0.0, 64.0, 64.0, [0.0, 1.0, 0.0, 1.0]);
        let top = hit_test_topmost(&scene, [32.0, 32.0]).unwrap();
        assert_eq!(top.op_index, 1);
        assert_eq!(top.kind, HitOpKind::Rect);
    }

    #[test]
    fn clip_rect_excludes_outside_points() {
        // Rect covers (0..64, 0..64) but clipped to (32..64, 32..64).
        // A point at (10, 10) is inside the rect's AABB but outside
        // the clip — must not hit.
        let mut scene = Scene::new(64, 64);
        scene.push_rect_clipped(
            0.0,
            0.0,
            64.0,
            64.0,
            [1.0, 0.0, 0.0, 1.0],
            0,
            [32.0, 32.0, 64.0, 64.0],
        );
        assert!(
            hit_test(&scene, [10.0, 10.0]).is_empty(),
            "point inside primitive AABB but outside clip should miss"
        );
        assert_eq!(
            hit_test(&scene, [40.0, 40.0]).len(),
            1,
            "point inside both should hit"
        );
    }

    #[test]
    fn per_glyph_hit_returns_glyph_index() {
        use crate::scene::Glyph;

        let mut scene = Scene::new(128, 64);
        // Three glyphs at known x positions, baseline y = 32.
        // font_size = 16 → each glyph's hit box is roughly
        // (x, 16) — (x + advance, 36).
        let glyphs = vec![
            Glyph {
                id: 1,
                x: 10.0,
                y: 32.0,
            }, // box: x in [10..30] (advance to next)
            Glyph {
                id: 2,
                x: 30.0,
                y: 32.0,
            }, // box: x in [30..50]
            Glyph {
                id: 3,
                x: 50.0,
                y: 32.0,
            }, // box: x in [50..66] (last; advance = font_size = 16)
        ];
        scene.push_glyph_run(
            0, /* font_id 0 = no-font sentinel ok for hit-test only */
            16.0,
            glyphs,
            [1.0, 1.0, 1.0, 1.0],
        );

        // Point inside glyph[1]'s box — y=24 is within (16, 36),
        // x=40 is within (30, 50).
        let hit = hit_test_topmost(&scene, [40.0, 24.0]).unwrap();
        assert_eq!(hit.kind, HitOpKind::GlyphRun);
        assert_eq!(
            hit.glyph_index,
            Some(1),
            "expected glyph index 1; got {:?}",
            hit
        );

        // Point inside glyph[2]'s box.
        let hit = hit_test_topmost(&scene, [55.0, 24.0]).unwrap();
        assert_eq!(hit.glyph_index, Some(2));

        // Point in the run's outer AABB (font_size pad inflates
        // beyond the real glyph boxes) but past glyph[2]'s right
        // edge — no per-glyph hit.
        // Glyph[2] right edge = 50 + 16 = 66; run AABB extends to
        // 50 + font_size = 66 too on this side, so we need to test
        // at a y that's inside run AABB but no glyph box. y=4 is
        // above all glyph boxes (which start at y=16) but inside
        // run AABB (font_size pad makes top = 32 - 16 = 16 — same).
        // Easier: pick y just outside any glyph's vertical box but
        // still inside run AABB. Glyph y_min = 16, so y=14 is
        // outside boxes; run AABB y_min = 32 - font_size = 16,
        // so y=14 is also outside the run AABB → would miss whole
        // run. Skip this case; the existence of `None` for
        // run-AABB-but-no-glyph hits is documented.

        // Verify a non-glyph-run hit has glyph_index = None.
        scene.push_rect(0.0, 0.0, 128.0, 64.0, [1.0, 0.0, 0.0, 1.0]);
        let hit = hit_test_topmost(&scene, [4.0, 4.0]).unwrap();
        assert_eq!(hit.kind, HitOpKind::Rect);
        assert_eq!(
            hit.glyph_index, None,
            "non-glyph-run hits must have glyph_index = None"
        );
    }

    #[test]
    fn layer_clip_culls_inner_op_outside_clip() {
        use crate::scene::SceneClip;

        // Outer rect-clip layer covers (10..50, 10..50). A
        // full-frame red rect inside the layer is "drawn" but
        // pixels outside the clip are culled. Hit at (4, 4) is
        // outside the clip → must not register a hit on the rect.
        let mut scene = Scene::new(64, 64);
        scene.push_layer_clip(SceneClip::Rect {
            rect: [10.0, 10.0, 50.0, 50.0],
            radii: [0.0; 4],
        });
        scene.push_rect(0.0, 0.0, 64.0, 64.0, [1.0, 0.0, 0.0, 1.0]);
        scene.pop_layer();

        // Outside the clip: rect's AABB still covers the point,
        // but the layer clip culls it.
        assert!(
            hit_test(&scene, [4.0, 4.0]).is_empty(),
            "point outside layer clip should not hit inner rect",
        );
        // Inside the clip: rect registers.
        let hits = hit_test(&scene, [32.0, 32.0]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, HitOpKind::Rect);
    }

    #[test]
    fn nested_layer_clips_intersect() {
        use crate::scene::SceneClip;

        // Outer clip: (0..32, 0..64). Inner clip: (16..48, 0..64).
        // Their intersection is (16..32, 0..64). A point at (24, 32)
        // is inside both → hit. (40, 32) is outside outer → no hit.
        // (8, 32) is outside inner → no hit.
        let mut scene = Scene::new(64, 64);
        scene.push_layer_clip(SceneClip::Rect {
            rect: [0.0, 0.0, 32.0, 64.0],
            radii: [0.0; 4],
        });
        scene.push_layer_clip(SceneClip::Rect {
            rect: [16.0, 0.0, 48.0, 64.0],
            radii: [0.0; 4],
        });
        scene.push_rect(0.0, 0.0, 64.0, 64.0, [1.0, 0.0, 0.0, 1.0]);
        scene.pop_layer();
        scene.pop_layer();

        assert_eq!(hit_test(&scene, [24.0, 32.0]).len(), 1, "in intersection");
        assert!(hit_test(&scene, [40.0, 32.0]).is_empty(), "outside outer");
        assert!(hit_test(&scene, [8.0, 32.0]).is_empty(), "outside inner");
    }

    #[test]
    fn layer_ops_skipped_in_hit_walk() {
        use crate::scene::SceneClip;

        let mut scene = Scene::new(64, 64);
        scene.push_layer_alpha(0.5);
        scene.push_rect(0.0, 0.0, 64.0, 64.0, [1.0, 0.0, 0.0, 1.0]);
        scene.pop_layer();

        // Stack contains the rect only (op_index 1, between push at
        // 0 and pop at 2). Layer push/pop ops aren't hits.
        let hits = hit_test(&scene, [32.0, 32.0]);
        assert_eq!(hits.len(), 1, "only the rect should hit, not layer ops");
        assert_eq!(hits[0].kind, HitOpKind::Rect);
        assert_eq!(hits[0].op_index, 1);

        // Sanity: a clip-only layer with a path also doesn't itself
        // produce a hit, even when the point is on the path.
        let mut scene2 = Scene::new(64, 64);
        scene2.push_layer_clip(SceneClip::Rect {
            rect: [10.0, 10.0, 50.0, 50.0],
            radii: [0.0; 4],
        });
        scene2.pop_layer();
        assert!(
            hit_test(&scene2, [32.0, 32.0]).is_empty(),
            "clip-only layer with no inner content shouldn't hit"
        );
    }

    #[test]
    fn mixed_kinds_in_stack() {
        let mut scene = Scene::new(64, 64);
        scene.push_rect(0.0, 0.0, 64.0, 64.0, [1.0, 0.0, 0.0, 1.0]);
        scene.push_stroke(0.0, 0.0, 64.0, 64.0, [0.0, 1.0, 0.0, 1.0], 4.0);
        let hits = hit_test(&scene, [32.0, 32.0]);
        // Stroke (last pushed) is the top hit; rect is under.
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].kind, HitOpKind::Stroke);
        assert_eq!(hits[1].kind, HitOpKind::Rect);
    }
}
