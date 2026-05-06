//! Roadmap A1 — `Scene::dump_ops()` op-list inspector receipt.
//!
//! Verifies that the inspector:
//! 1. emits a header line with the right counts,
//! 2. produces one body line per op in painter order,
//! 3. nests indentation across `PushLayer` / `PopLayer` pairs,
//! 4. surfaces non-default `transform_id` / `clip_rect` modifiers.
//!
//! Output format is **not stable**; these tests check structural
//! invariants (line count, ordering, indentation, kind tags), not
//! exact strings.

use netrender::scene::{
    NO_CLIP, Scene, SceneBlendMode, SceneClip, SceneLayer, SceneOp, Transform,
};

#[test]
fn header_line_reports_counts_and_viewport() {
    let mut scene = Scene::new(800, 600);
    scene.push_rect(0.0, 0.0, 10.0, 10.0, [1.0, 0.0, 0.0, 1.0]);
    scene.push_rect(20.0, 20.0, 30.0, 30.0, [0.0, 1.0, 0.0, 1.0]);

    let dump = scene.dump_ops();
    let header = dump.lines().next().expect("header present");

    assert!(header.contains("800x600"), "viewport in header: {:?}", header);
    assert!(header.contains("ops=2"), "op count in header: {:?}", header);
    assert!(header.contains("transforms=1"), "identity-only palette: {:?}", header);
    assert!(header.contains("fonts=1"), "sentinel font palette: {:?}", header);
}

#[test]
fn body_has_one_line_per_op_with_index_and_kind() {
    let mut scene = Scene::new(100, 100);
    scene.push_rect(0.0, 0.0, 10.0, 10.0, [1.0, 0.0, 0.0, 1.0]);
    scene.push_rect(0.0, 0.0, 10.0, 10.0, [0.0, 1.0, 0.0, 1.0]);
    scene.push_rect(0.0, 0.0, 10.0, 10.0, [0.0, 0.0, 1.0, 1.0]);

    let dump = scene.dump_ops();
    let body: Vec<&str> = dump.lines().skip(1).collect();

    assert_eq!(body.len(), 3, "one line per op: {}", dump);
    for (i, line) in body.iter().enumerate() {
        assert!(line.contains(&format!("{:04}", i)), "line {i} has zero-padded index: {line:?}");
        assert!(line.contains("Rect"), "line {i} kind tag: {line:?}");
    }
}

#[test]
fn modifiers_only_appear_when_non_default() {
    let mut scene = Scene::new(100, 100);
    // Identity transform, no clip — modifiers should NOT show.
    scene.push_rect(0.0, 0.0, 10.0, 10.0, [1.0, 0.0, 0.0, 1.0]);
    // Custom transform + custom clip — modifiers SHOULD show.
    let xf = scene.push_transform(Transform::translate_2d(50.0, 50.0));
    scene.push_rect_clipped(0.0, 0.0, 10.0, 10.0, [0.0, 1.0, 0.0, 1.0], xf, [5.0, 5.0, 95.0, 95.0]);

    let dump = scene.dump_ops();
    let body: Vec<&str> = dump.lines().skip(1).collect();

    assert!(!body[0].contains("transform="), "identity transform omitted: {:?}", body[0]);
    assert!(!body[0].contains("clip="), "no-clip omitted: {:?}", body[0]);
    assert!(body[1].contains("transform=1"), "custom transform shown: {:?}", body[1]);
    assert!(body[1].contains("clip="), "custom clip shown: {:?}", body[1]);
}

/// Inspector body lines are formatted `"  {index:04}{indent}{kind} ..."`,
/// so the meaningful nest depth is the run of spaces *between* the
/// index and the kind tag, not the leading prefix.
fn indent_after_index(line: &str) -> usize {
    // Skip "  " prefix (2) + 4-digit index (4) = 6 chars, then count
    // ASCII space bytes until the kind alpha begins.
    line.as_bytes()
        .iter()
        .skip(6)
        .take_while(|b| **b == b' ')
        .count()
}

#[test]
fn push_layer_nests_indentation() {
    let mut scene = Scene::new(100, 100);
    scene.push_rect(0.0, 0.0, 10.0, 10.0, [1.0, 0.0, 0.0, 1.0]);
    scene.ops.push(SceneOp::PushLayer(SceneLayer::alpha(0.5)));
    scene.push_rect(0.0, 0.0, 10.0, 10.0, [0.0, 1.0, 0.0, 1.0]);
    scene.push_rect(0.0, 0.0, 10.0, 10.0, [0.0, 0.0, 1.0, 1.0]);
    scene.ops.push(SceneOp::PopLayer);
    scene.push_rect(0.0, 0.0, 10.0, 10.0, [1.0, 1.0, 0.0, 1.0]);

    let dump = scene.dump_ops();
    let body: Vec<&str> = dump.lines().skip(1).collect();

    assert_eq!(body.len(), 6, "one line per op: {dump}");

    // The two rects inside the PushLayer/PopLayer scope should be
    // indented further than the bare ones outside.
    let outer_pad = indent_after_index(body[0]);
    let push_pad = indent_after_index(body[1]);
    let inside_a_pad = indent_after_index(body[2]);
    let inside_b_pad = indent_after_index(body[3]);
    let pop_pad = indent_after_index(body[4]);
    let after_pop_pad = indent_after_index(body[5]);

    assert_eq!(outer_pad, push_pad, "outer rect and PushLayer at same depth: {dump}");
    assert!(inside_a_pad > push_pad, "inside ops indented past PushLayer: {dump}");
    assert_eq!(inside_a_pad, inside_b_pad, "siblings inside layer share depth: {dump}");
    assert_eq!(pop_pad, push_pad, "PopLayer line dedents back to outer depth: {dump}");
    assert_eq!(after_pop_pad, outer_pad, "rect after pop returns to outer depth: {dump}");

    assert!(dump.contains("PushLayer alpha=0.5"));
    assert!(dump.contains("PopLayer"));
}

#[test]
fn nested_layers_increase_depth_monotonically() {
    let mut scene = Scene::new(100, 100);
    scene.ops.push(SceneOp::PushLayer(SceneLayer::alpha(0.8)));
    scene.ops.push(SceneOp::PushLayer(SceneLayer::clip(SceneClip::Rect {
        rect: [0.0, 0.0, 50.0, 50.0],
        radii: [0.0; 4],
    })));
    scene.push_rect(0.0, 0.0, 10.0, 10.0, [1.0, 0.0, 0.0, 1.0]);
    scene.ops.push(SceneOp::PopLayer);
    scene.ops.push(SceneOp::PopLayer);

    let dump = scene.dump_ops();
    let body: Vec<&str> = dump.lines().skip(1).collect();

    let outer_push = indent_after_index(body[0]);
    let inner_push = indent_after_index(body[1]);
    let inner_rect = indent_after_index(body[2]);
    let inner_pop = indent_after_index(body[3]);
    let outer_pop = indent_after_index(body[4]);

    assert!(inner_push > outer_push, "inner push deeper than outer push: {dump}");
    assert!(inner_rect > inner_push, "innermost rect deepest: {dump}");
    assert_eq!(inner_pop, inner_push, "inner pop dedents to inner push depth: {dump}");
    assert_eq!(outer_pop, outer_push, "outer pop dedents to outer push depth: {dump}");
}

#[test]
fn root_alpha_and_blend_surface_when_non_default() {
    let mut at_default = Scene::new(100, 100);
    at_default.push_rect(0.0, 0.0, 10.0, 10.0, [1.0, 0.0, 0.0, 1.0]);
    let dump_default = at_default.dump_ops();
    assert!(!dump_default.contains("root_alpha"), "defaults omitted: {dump_default}");
    assert!(!dump_default.contains("root_blend_mode"));

    let mut tweaked = Scene::new(100, 100);
    tweaked.root_alpha = 0.75;
    tweaked.root_blend_mode = SceneBlendMode::Multiply;
    tweaked.push_rect(0.0, 0.0, 10.0, 10.0, [1.0, 0.0, 0.0, 1.0]);
    let dump_tweaked = tweaked.dump_ops();
    assert!(dump_tweaked.contains("root_alpha=0.75"), "tweaked alpha shown: {dump_tweaked}");
    assert!(dump_tweaked.contains("Multiply"), "tweaked blend shown: {dump_tweaked}");
}

#[test]
fn empty_scene_emits_only_header() {
    let scene = Scene::new(50, 50);
    let dump = scene.dump_ops();
    assert_eq!(dump.lines().count(), 1, "header only: {dump:?}");
    assert!(dump.contains("ops=0"));
    // unused import suppression in case NO_CLIP isn't otherwise referenced
    let _ = NO_CLIP;
}
