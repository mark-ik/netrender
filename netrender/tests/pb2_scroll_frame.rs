//! Roadmap B2 — `Scene::push_scroll_frame` / `pop_scroll_frame`
//! receipts.
//!
//! Pure CPU. Verifies:
//!
//! 1. One `push_scroll_frame` call appends both a translate-by-minus-
//!    offset transform and a `PushLayer` with the rect clip — no
//!    architectural change required, just a bundled setup.
//! 2. The returned transform_id can be threaded through inner
//!    primitives via the `_transformed` helpers.
//! 3. `pop_scroll_frame` appends a single `PopLayer` op.
//! 4. Scroll frames can nest; each level gets its own transform id.
//! 5. The "demo" use case — a scrolling card list — assembles in
//!    the expected number of method calls.

use netrender::scene::{Scene, SceneClip, SceneOp};

#[test]
fn push_scroll_frame_bundles_transform_and_clip_layer() {
    let mut scene = Scene::new(400, 300);
    let transforms_before = scene.transforms.len();
    let ops_before = scene.ops.len();

    let xf = scene.push_scroll_frame([10.0, 20.0, 200.0, 250.0], [0.0, 50.0]);

    // One new transform appended; returned id points at it.
    assert_eq!(scene.transforms.len(), transforms_before + 1);
    assert_eq!(xf as usize, transforms_before);

    // The new transform should translate by (-0, -50).
    let t = &scene.transforms[xf as usize];
    assert!((t.m[12] - 0.0).abs() < 1e-6, "tx mismatch: {:?}", t.m[12]);
    assert!((t.m[13] - -50.0).abs() < 1e-6, "ty mismatch: {:?}", t.m[13]);

    // One new op appended (the PushLayer); the clip is the rect we
    // passed and the layer's own transform_id is identity (clip is
    // in parent space, not subject to the scroll transform).
    assert_eq!(scene.ops.len(), ops_before + 1);
    match scene.ops.last().unwrap() {
        SceneOp::PushLayer(layer) => {
            assert_eq!(layer.transform_id, 0, "scroll-frame clip is parent-space");
            assert!((layer.alpha - 1.0).abs() < f32::EPSILON);
            match &layer.clip {
                SceneClip::Rect { rect, radii } => {
                    assert_eq!(*rect, [10.0, 20.0, 200.0, 250.0]);
                    assert_eq!(*radii, [0.0; 4], "sharp clip by default");
                }
                other => panic!("expected SceneClip::Rect, got {other:?}"),
            }
        }
        other => panic!("expected PushLayer, got {other:?}"),
    }
}

#[test]
fn pop_scroll_frame_appends_a_single_pop_layer() {
    let mut scene = Scene::new(100, 100);
    scene.push_scroll_frame([0.0, 0.0, 100.0, 100.0], [0.0, 0.0]);
    let ops_before_pop = scene.ops.len();

    scene.pop_scroll_frame();

    assert_eq!(scene.ops.len(), ops_before_pop + 1);
    assert!(
        matches!(scene.ops.last().unwrap(), SceneOp::PopLayer),
        "pop_scroll_frame appends PopLayer"
    );
}

#[test]
fn returned_transform_threads_into_primitives() {
    let mut scene = Scene::new(400, 300);

    // Set up a scroll frame and push a rect using the returned
    // transform id. The rect should carry that transform_id; the
    // dump_ops listing should mark the rect as transformed.
    let scroll_xf = scene.push_scroll_frame([0.0, 0.0, 200.0, 200.0], [0.0, 100.0]);
    scene.push_rect_transformed(0.0, 0.0, 50.0, 50.0, [1.0, 0.0, 0.0, 1.0], scroll_xf);
    scene.push_rect_transformed(0.0, 60.0, 50.0, 110.0, [0.0, 1.0, 0.0, 1.0], scroll_xf);
    scene.pop_scroll_frame();

    // Find the two rect ops and check both used scroll_xf.
    let rects: Vec<u32> = scene
        .ops
        .iter()
        .filter_map(|op| match op {
            SceneOp::Rect(r) => Some(r.transform_id),
            _ => None,
        })
        .collect();
    assert_eq!(rects, vec![scroll_xf, scroll_xf]);
}

#[test]
fn nested_scroll_frames_get_independent_transforms() {
    let mut scene = Scene::new(800, 600);
    let outer_xf = scene.push_scroll_frame([0.0, 0.0, 800.0, 600.0], [0.0, 100.0]);
    let inner_xf = scene.push_scroll_frame([100.0, 100.0, 400.0, 300.0], [50.0, 0.0]);

    assert_ne!(outer_xf, inner_xf, "each scroll frame mints a fresh transform id");

    // Outer scroll: ty = -100. Inner scroll: tx = -50.
    assert!((scene.transforms[outer_xf as usize].m[13] - -100.0).abs() < 1e-6);
    assert!((scene.transforms[inner_xf as usize].m[12] - -50.0).abs() < 1e-6);

    scene.pop_scroll_frame();
    scene.pop_scroll_frame();

    // Two PushLayer + two PopLayer in the op list.
    let push_count = scene.ops.iter().filter(|op| matches!(op, SceneOp::PushLayer(_))).count();
    let pop_count = scene.ops.iter().filter(|op| matches!(op, SceneOp::PopLayer)).count();
    assert_eq!(push_count, 2);
    assert_eq!(pop_count, 2);
}

#[test]
fn demo_scrolling_card_list_in_one_setup_call() {
    // The roadmap B2 done condition: "the demo gains a scrolling
    // card list under one method call instead of three." This
    // assembles a 3-card list inside a scroll frame. The setup
    // (clip + scroll transform) is one push_scroll_frame call;
    // before B2 it would have been push_transform + push_layer
    // (two explicit calls).
    let mut scene = Scene::new(400, 300);
    let card_w = 380.0;
    let card_h = 80.0;
    let gap = 10.0;

    let scroll_xf = scene.push_scroll_frame([10.0, 10.0, 390.0, 290.0], [0.0, 60.0]);
    for (i, color) in [
        [1.0, 0.5, 0.5, 1.0],
        [0.5, 1.0, 0.5, 1.0],
        [0.5, 0.5, 1.0, 1.0],
    ]
    .iter()
    .enumerate()
    {
        let y0 = i as f32 * (card_h + gap);
        scene.push_rect_transformed(0.0, y0, card_w, y0 + card_h, *color, scroll_xf);
    }
    scene.pop_scroll_frame();

    // Op shape: PushLayer + 3 Rects + PopLayer = 5 ops.
    assert_eq!(scene.ops.len(), 5);
    assert!(matches!(scene.ops.first().unwrap(), SceneOp::PushLayer(_)));
    assert!(matches!(scene.ops.last().unwrap(), SceneOp::PopLayer));

    // Three middle ops are scrolled rects.
    for i in 1..=3 {
        match &scene.ops[i] {
            SceneOp::Rect(r) => assert_eq!(r.transform_id, scroll_xf),
            other => panic!("expected Rect at index {i}, got {other:?}"),
        }
    }
}

#[test]
fn scroll_frame_with_zero_offset_is_pure_clip() {
    // Edge case: zero scroll offset. The translate transform is
    // identity-equivalent (translate by 0,0); it still gets a fresh
    // transform_id (we don't fold it into id 0) but is observably a
    // no-op. The clip layer is the only effect.
    let mut scene = Scene::new(200, 200);
    let xf = scene.push_scroll_frame([0.0, 0.0, 100.0, 100.0], [0.0, 0.0]);
    let t = &scene.transforms[xf as usize];
    assert!((t.m[12]).abs() < 1e-6, "tx is zero");
    assert!((t.m[13]).abs() < 1e-6, "ty is zero");
    // Identity except the translation columns are explicitly zero.
    assert_eq!(t.m[0], 1.0);
    assert_eq!(t.m[5], 1.0);
}
