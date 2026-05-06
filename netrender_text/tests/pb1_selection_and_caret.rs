/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Roadmap B1 — `selection_rects` + `caret_rect` receipts.
//!
//! These helpers wrap `parley::Selection::geometry` and
//! `parley::Cursor::geometry` so consumers (nematic's Gemini /
//! Gopher / Scroll viewers, Markdown editors, feed readers) can ask
//! "where do I paint the selection band / caret?" without re-doing
//! shaping math.
//!
//! Pure CPU; no GPU needed. Skipped vacuously on hosts without a
//! known system font path (same pattern as `shape_and_paint`).

use std::sync::Arc;

use netrender_text::{
    caret_rect, selection_rects,
    parley::{
        self, Affinity, FontContext, FontFamily, Layout, LayoutContext, StyleProperty,
    },
};

const LAYOUT_WIDTH: f32 = 200.0;

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

/// Build a parley layout for `text` at 16px, broken to `LAYOUT_WIDTH`.
fn shape(text: &str) -> Option<Layout<[f32; 4]>> {
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

    let mut layout: Layout<[f32; 4]> = builder.build(text);
    layout.break_all_lines(Some(LAYOUT_WIDTH));
    layout.align(
        parley::Alignment::Start,
        parley::AlignmentOptions::default(),
    );
    Some(layout)
}

#[test]
fn selection_rects_collapsed_range_is_empty() {
    let Some(layout) = shape("hello world") else {
        eprintln!("pb1: no system font; skipping");
        return;
    };
    assert!(selection_rects(&layout, 0..0).is_empty());
    assert!(selection_rects(&layout, 5..5).is_empty());
    // Reversed range — defensive: helper short-circuits.
    assert!(selection_rects(&layout, 5..3).is_empty());
}

#[test]
fn selection_rects_single_line_returns_one_band() {
    let Some(layout) = shape("hello world") else {
        return;
    };
    // "hello" — entirely within the first visual line.
    let rects = selection_rects(&layout, 0..5);
    assert_eq!(
        rects.len(),
        1,
        "single-line selection produces one rect: {rects:?}"
    );
    let r = rects[0];
    assert!(r[0] >= 0.0, "x0 inside layout: {r:?}");
    assert!(r[2] > r[0], "rect has positive width: {r:?}");
    assert!(r[3] > r[1], "rect has positive height: {r:?}");
    // Selecting "hello" shouldn't span the full layout width.
    assert!(
        r[2] < LAYOUT_WIDTH,
        "single word selection narrower than layout: {r:?}"
    );
}

#[test]
fn selection_rects_multiline_returns_multiple_bands() {
    // Long text guaranteed to wrap at 200px / 16px font.
    let text = "the quick brown fox jumps over the lazy dog repeatedly under the moonlight";
    let Some(layout) = shape(text) else {
        return;
    };
    assert!(
        layout.len() > 1,
        "test premise: layout must wrap to >1 line"
    );

    // Whole-text selection touches every line.
    let rects = selection_rects(&layout, 0..text.len());
    assert!(
        rects.len() >= 2,
        "multi-line selection produces multiple rects: {rects:?}"
    );

    // Rects should be y-ordered (top-to-bottom): each subsequent
    // rect's y0 >= previous rect's y0.
    for w in rects.windows(2) {
        assert!(
            w[1][1] >= w[0][1],
            "rects ordered top-to-bottom: {:?} → {:?}",
            w[0],
            w[1]
        );
    }
}

#[test]
fn caret_rect_at_start_is_at_layout_left_edge() {
    let Some(layout) = shape("hello world") else {
        return;
    };
    let r = caret_rect(&layout, 0, Affinity::Downstream, 1.5);
    assert!(r[0].abs() < 2.0, "caret near x=0 at start: {r:?}");
    assert!(r[3] > r[1], "caret has positive height: {r:?}");
    // Width was 1.5 — the rect width may differ slightly because
    // parley snaps caret bounds, but should be in the right ballpark.
    let width = r[2] - r[0];
    assert!(
        (0.0..=4.0).contains(&width),
        "caret width near 1.5: got {width}"
    );
}

#[test]
fn caret_rect_advances_through_text() {
    let Some(layout) = shape("hello world") else {
        return;
    };
    // A few cursor positions through the first line; x should
    // monotonically increase.
    let positions: Vec<f32> = [0, 1, 3, 5, 7]
        .iter()
        .map(|i| caret_rect(&layout, *i, Affinity::Downstream, 1.0)[0])
        .collect();
    for w in positions.windows(2) {
        assert!(
            w[1] >= w[0] - 0.001,
            "caret advances or stays put: {w:?}"
        );
    }
    // First and last positions must differ — otherwise we measured
    // five copies of the same point.
    assert!(
        positions.last().unwrap() > positions.first().unwrap(),
        "caret moved across the line: {positions:?}"
    );
}

#[test]
fn caret_rect_height_matches_line_height() {
    let Some(layout) = shape("hello world") else {
        return;
    };
    let r0 = caret_rect(&layout, 0, Affinity::Downstream, 1.0);
    let r3 = caret_rect(&layout, 3, Affinity::Downstream, 1.0);
    let h0 = r0[3] - r0[1];
    let h3 = r3[3] - r3[1];
    // Same line → identical caret height.
    assert!(
        (h0 - h3).abs() < 0.001,
        "caret height stable on the same line: {h0} vs {h3}"
    );
    assert!(h0 > 8.0, "16px font produces caret taller than 8px: {h0}");
}

#[test]
fn selection_rects_partial_line_narrower_than_full_line() {
    let Some(layout) = shape("hello world") else {
        return;
    };
    let partial = selection_rects(&layout, 0..5)[0]; // "hello"
    let full = selection_rects(&layout, 0..11)[0]; // "hello world"
    assert!(
        full[2] - full[0] > partial[2] - partial[0],
        "selecting more text produces a wider band: partial={partial:?} full={full:?}"
    );
}
