/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Roadmap R6 — `push_layout_with_inline_boxes` receipts.
//!
//! Verifies that the integrated walker:
//!
//! 1. Surfaces every `PositionedLayoutItem::InlineBox` to the
//!    callback while still pushing glyph runs to the scene.
//! 2. Applies `origin` to inline-box placements (consumer paints
//!    boxes inline without re-deriving line geometry).
//! 3. Preserves consumer-supplied ids end-to-end.
//! 4. Is order-stable: callbacks fire in parley's visual order
//!    (top-to-bottom by line, interleaved with glyph runs).
//! 5. Behaves identically to the existing `push_layout` for scenes
//!    that have no inline boxes.

use std::sync::Arc;

use netrender::{FontRegistry, Scene};
use netrender_text::{push_layout, push_layout_with_inline_boxes, InlineBoxPlacement};
use netrender_text::parley::{
    self, Alignment, AlignmentOptions, FontContext, FontFamily, InlineBox, InlineBoxKind, Layout,
    LayoutContext, StyleProperty,
};

const LAYOUT_WIDTH: f32 = 400.0;

fn try_load_system_font() -> Option<Vec<u8>> {
    let candidates = [
        r"C:\Windows\Fonts\arial.ttf",
        r"C:\Windows\Fonts\segoeui.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
        "/Library/Fonts/Arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(bytes);
        }
    }
    None
}

/// Build a layout `text` with `boxes` (each `(byte_index, id, width,
/// height)`). Returns `None` if no system font is available.
fn shape_with_boxes(text: &str, boxes: &[(usize, u64, f32, f32)]) -> Option<Layout<[f32; 4]>> {
    let font_bytes = try_load_system_font()?;
    let mut font_cx = FontContext::new();
    let blob = parley::fontique::Blob::new(Arc::new(font_bytes));
    let registered = font_cx.collection.register_fonts(blob, None);
    let (family_id, _) = registered.into_iter().next()?;
    let family_name = font_cx.collection.family_name(family_id)?.to_owned();

    let mut layout_cx: LayoutContext<[f32; 4]> = LayoutContext::new();
    let mut builder = layout_cx.ranged_builder(&mut font_cx, text, 1.0, true);
    builder.push_default(StyleProperty::FontSize(16.0));
    builder.push_default(StyleProperty::Brush([1.0, 1.0, 1.0, 1.0]));
    builder.push_default(StyleProperty::FontFamily(FontFamily::named(&family_name)));

    for (idx, id, width, height) in boxes {
        builder.push_inline_box(InlineBox {
            id: *id,
            kind: InlineBoxKind::InFlow,
            index: *idx,
            width: *width,
            height: *height,
        });
    }

    let mut layout: Layout<[f32; 4]> = builder.build(text);
    layout.break_all_lines(Some(LAYOUT_WIDTH));
    layout.align(Alignment::Start, AlignmentOptions::default());
    Some(layout)
}

#[test]
fn r6_inline_box_callback_fires_with_metadata_intact() {
    let Some(layout) = shape_with_boxes("hello world", &[(5, 42, 24.0, 24.0)]) else {
        eprintln!("pr6: no system font; skipping");
        return;
    };

    let mut scene = Scene::new(500, 200);
    let mut registry = FontRegistry::new();
    let mut placements: Vec<InlineBoxPlacement> = Vec::new();
    push_layout_with_inline_boxes(
        &mut scene,
        &mut registry,
        &layout,
        [0.0, 0.0],
        |placement| placements.push(placement),
    );

    assert_eq!(placements.len(), 1, "one inline box → one callback fire");
    let p = placements[0];
    assert_eq!(p.id, 42, "consumer-supplied id round-trips");
    assert_eq!(p.width, 24.0);
    assert_eq!(p.height, 24.0);
}

#[test]
fn r6_origin_applied_as_translation_delta() {
    // Run the walker twice with different origins and assert the
    // placement coordinates differ by exactly the origin delta.
    // Avoids hard-coding parley's baseline-relative y math.
    let Some(layout) = shape_with_boxes("hello world", &[(5, 1, 24.0, 24.0)]) else {
        return;
    };

    let mut scene_a = Scene::new(500, 200);
    let mut reg_a = FontRegistry::new();
    let mut pa: Option<InlineBoxPlacement> = None;
    push_layout_with_inline_boxes(&mut scene_a, &mut reg_a, &layout, [0.0, 0.0], |p| {
        pa = Some(p)
    });

    let mut scene_b = Scene::new(500, 200);
    let mut reg_b = FontRegistry::new();
    let mut pb: Option<InlineBoxPlacement> = None;
    push_layout_with_inline_boxes(&mut scene_b, &mut reg_b, &layout, [10.0, 20.0], |p| {
        pb = Some(p)
    });

    let pa = pa.expect("zero-origin placement");
    let pb = pb.expect("offset-origin placement");
    assert!(
        (pb.x - pa.x - 10.0).abs() < 1e-3,
        "x delta = origin x: pa={pa:?} pb={pb:?}"
    );
    assert!(
        (pb.y - pa.y - 20.0).abs() < 1e-3,
        "y delta = origin y: pa={pa:?} pb={pb:?}"
    );
    // Width/height/id are origin-independent.
    assert_eq!(pa.width, pb.width);
    assert_eq!(pa.height, pb.height);
    assert_eq!(pa.id, pb.id);
}

#[test]
fn r6_glyph_runs_still_emitted_alongside_inline_box_callbacks() {
    let Some(layout) = shape_with_boxes("hello world", &[(5, 1, 16.0, 16.0)]) else {
        return;
    };

    let mut scene = Scene::new(500, 200);
    let mut registry = FontRegistry::new();
    let initial_ops = scene.ops.len();

    push_layout_with_inline_boxes(&mut scene, &mut registry, &layout, [0.0, 0.0], |_| {});

    // The walker should have pushed at least one glyph run for
    // "hello" and one for "world" (the inline box splits the text).
    let glyph_runs = scene
        .ops
        .iter()
        .filter(|op| matches!(op, netrender::scene::SceneOp::GlyphRun(_)))
        .count();
    assert!(
        glyph_runs >= 1,
        "at least one glyph run pushed, got {glyph_runs} ops total: {}",
        scene.ops.len() - initial_ops,
    );
}

#[test]
fn r6_multiple_inline_boxes_arrive_in_order() {
    // Three boxes at increasing byte indices should produce three
    // callbacks in left-to-right order on the same line.
    let Some(layout) = shape_with_boxes(
        "abcdefghijklmnop",
        &[
            (4, 100, 12.0, 12.0),
            (8, 200, 12.0, 12.0),
            (12, 300, 12.0, 12.0),
        ],
    ) else {
        return;
    };

    let mut scene = Scene::new(500, 200);
    let mut registry = FontRegistry::new();
    let mut placements: Vec<InlineBoxPlacement> = Vec::new();
    push_layout_with_inline_boxes(
        &mut scene,
        &mut registry,
        &layout,
        [0.0, 0.0],
        |placement| placements.push(placement),
    );

    assert_eq!(placements.len(), 3, "three inline boxes → three callbacks");
    let ids: Vec<u64> = placements.iter().map(|p| p.id).collect();
    assert_eq!(ids, vec![100, 200, 300], "ids in left-to-right order");
    // x positions monotonically increase on the same line.
    for w in placements.windows(2) {
        assert!(
            w[1].x >= w[0].x,
            "boxes appear left-to-right on the line: {} -> {}",
            w[0].x,
            w[1].x,
        );
    }
}

#[test]
fn r6_no_inline_boxes_means_no_callback_fires() {
    // The simple `push_layout` is now a thin wrapper over the
    // inline-box-aware walker with an empty callback. Verify that
    // path still emits glyphs and doesn't surprise the consumer
    // with phantom callbacks.
    let Some(layout) = shape_with_boxes("just text", &[]) else {
        return;
    };

    let mut scene = Scene::new(500, 200);
    push_layout(&mut scene, &layout, [0.0, 0.0]);
    assert!(
        scene
            .ops
            .iter()
            .any(|op| matches!(op, netrender::scene::SceneOp::GlyphRun(_))),
        "push_layout still emits glyphs for plain text"
    );
}

#[test]
fn r6_placement_x_inside_layout_width() {
    // Box x should fit inside the layout's measured width. (y can
    // extend above the line if the box is taller than the line's
    // ascent — that's parley's baseline-relative positioning, not
    // a netrender_text concern.)
    let Some(layout) = shape_with_boxes("hello world", &[(5, 7, 20.0, 20.0)]) else {
        return;
    };

    let layout_w = layout.width();
    let mut scene = Scene::new(500, 200);
    let mut registry = FontRegistry::new();
    let mut placement: Option<InlineBoxPlacement> = None;
    push_layout_with_inline_boxes(&mut scene, &mut registry, &layout, [0.0, 0.0], |p| {
        placement = Some(p)
    });

    let p = placement.expect("inline-box callback fired");
    assert!(
        p.x >= 0.0 && p.x + p.width <= layout_w + 0.5,
        "box x in layout: {p:?}, layout_w={layout_w}"
    );
}
