//! Roadmap R1 — per-glyph hit testing receipts.
//!
//! Verifies that hit testing inside `SceneOp::GlyphRun` uses real
//! font-supplied glyph bounds via `skrifa::metrics::GlyphMetrics`
//! instead of the em-box approximation. The decisive case: glyph
//! `g` (lowercase) has a descender that extends well below the
//! `font_size * 0.25` shallow-descender em-box approximation. A
//! click at the descender's tail hits with real metrics; under
//! the old em-box approximation it would miss.
//!
//! Skipped vacuously on hosts without a known Latin font path
//! (same pattern as `p10prime_b_glyph_render`).

use std::sync::Arc;

use netrender::{hit_test_topmost, FontBlob, Glyph, Scene};
use netrender::peniko::Blob;

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

/// Look up a glyph id by character via skrifa's charmap. Returns
/// the (`u32`) glyph id or `None` if the codepoint isn't in the
/// font's cmap.
fn glyph_id_for_char(bytes: &[u8], ch: char) -> Option<u32> {
    use skrifa::MetadataProvider;
    let font = skrifa::FontRef::new(bytes).ok()?;
    let cmap = font.charmap();
    Some(cmap.map(ch as u32)?.to_u32())
}

#[test]
fn pr1_lowercase_g_descender_hits_under_real_metrics() {
    let Some(font_bytes) = try_load_system_font() else {
        eprintln!("pr1: no system font; skipping");
        return;
    };

    let Some(g_id) = glyph_id_for_char(&font_bytes, 'g') else {
        eprintln!("pr1: 'g' not in font's cmap; skipping");
        return;
    };

    let mut scene = Scene::new(200, 100);
    let font_id = scene.push_font(FontBlob {
        data: Blob::new(Arc::new(font_bytes.clone())),
        index: 0,
    });

    // Glyph at baseline y=50, font_size=32. With em-box, descender
    // ends at y = 50 + 32 * 0.25 = 58. With real metrics for 'g',
    // descender extends much further (typically y_min ≈ -0.2 to
    // -0.25 of font_size for Latin descenders) — for Arial / DejaVu
    // / Liberation this puts the descender bottom around y = 50 +
    // ~7 = 57 to ~10 = 60+ in screen-down coords. We pick a y
    // that's safely past the em-box but still inside real metrics.
    //
    // Pick y = 60 (10 px below baseline). At font_size=32, em-box
    // shallow descender ends at 50 + 8 = 58, so y=60 misses the
    // em-box. Real-metric descender for most Latin 'g' glyphs at
    // 32px reaches at least ~7-9 px below baseline; the sample
    // point at y=60 sits inside the descender column for a good
    // selection of fonts.
    scene.push_glyph_run(
        font_id,
        32.0,
        vec![Glyph { id: g_id, x: 50.0, y: 50.0 }],
        [1.0, 1.0, 1.0, 1.0],
    );

    // Probe 1: dead-center of the glyph (always hits).
    let center = hit_test_topmost(&scene, [55.0, 40.0]);
    assert!(
        center.and_then(|h| h.glyph_index) == Some(0),
        "center of 'g' hits glyph 0 (got {center:?})"
    );

    // Probe 2: deep descender — the discriminating hit. With
    // skrifa metrics this should hit; with em-box only it would
    // miss because the box ends above this y. Allow the test to
    // pass vacuously if the host font's 'g' has an unusually
    // shallow descender (some condensed faces); we log and skip.
    use skrifa::MetadataProvider;
    let font = skrifa::FontRef::new(&font_bytes).expect("font parses");
    let metrics = font.glyph_metrics(
        skrifa::instance::Size::new(32.0),
        skrifa::instance::LocationRef::default(),
    );
    let bounds = metrics
        .bounds(skrifa::GlyphId::new(g_id))
        .expect("'g' has bounds in this font");
    let descender_screen_y = 50.0 - bounds.y_min;
    let em_box_descender_y = 50.0 + 32.0 * 0.25;
    if descender_screen_y <= em_box_descender_y + 1.0 {
        eprintln!(
            "pr1: this font's 'g' descender ({:.1}) doesn't reach past em-box ({:.1}); skipping discriminator",
            descender_screen_y, em_box_descender_y
        );
        return;
    }
    // Pick a probe y that's just above the real-metric descender
    // bottom but well past the em-box bottom.
    let probe_y = (descender_screen_y - 1.0).max(em_box_descender_y + 1.0);
    let probe_x = 50.0 + (bounds.x_min + bounds.x_max) * 0.5; // middle of glyph horizontally
    let descender = hit_test_topmost(&scene, [probe_x, probe_y]);
    assert!(
        descender.and_then(|h| h.glyph_index) == Some(0),
        "deep-descender point ({:.1}, {:.1}) hits 'g' under real metrics \
         (em-box bottom was {:.1}, real descender at {:.1}); got {descender:?}",
        probe_x, probe_y, em_box_descender_y, descender_screen_y,
    );
}

#[test]
fn pr1_above_glyph_top_misses() {
    // A point well above the glyph (y << ascender) should miss
    // regardless of metrics path. Guards against R1 over-extending
    // the bounding box upward.
    let Some(font_bytes) = try_load_system_font() else {
        return;
    };
    let Some(g_id) = glyph_id_for_char(&font_bytes, 'A') else {
        return;
    };

    let mut scene = Scene::new(200, 100);
    let font_id = scene.push_font(FontBlob {
        data: Blob::new(Arc::new(font_bytes)),
        index: 0,
    });
    scene.push_glyph_run(
        font_id,
        32.0,
        vec![Glyph { id: g_id, x: 50.0, y: 50.0 }],
        [1.0, 1.0, 1.0, 1.0],
    );

    // Way above the ascender — y=5 is well outside any reasonable
    // bounding box for a 32px glyph at baseline y=50.
    let above = hit_test_topmost(&scene, [55.0, 5.0]);
    assert!(
        above.is_none() || above.and_then(|h| h.glyph_index).is_none(),
        "point above the glyph misses (got {above:?})"
    );
}

#[test]
fn pr1_sentinel_font_falls_back_to_em_box() {
    // Sentinel font (font_id 0) → can't parse, so fall back to
    // em-box. The hit test should still work via the em-box path.
    // We construct a SceneGlyphRun with font_id 0 (which Scene's
    // push_glyph_run normally wouldn't allow, but we go around it
    // for this test).
    use netrender::scene::{SceneGlyphRun, SceneOp, NO_CLIP, SHARP_CLIP};

    let mut scene = Scene::new(200, 100);
    scene.ops.push(SceneOp::GlyphRun(SceneGlyphRun {
        font_id: 0, // sentinel
        font_size: 32.0,
        glyphs: vec![Glyph { id: 0, x: 50.0, y: 50.0 }],
        color: [1.0, 1.0, 1.0, 1.0],
        transform_id: 0,
        clip_rect: NO_CLIP,
        clip_corner_radii: SHARP_CLIP,
        font_axis_values: Vec::new(),
    }));

    // Em-box: x ∈ [50, 50+font_size]=[50, 82], y ∈ [50-32, 50+8]=
    // [18, 58]. Center of em-box is around (66, 38).
    let hit = hit_test_topmost(&scene, [60.0, 40.0]);
    assert!(
        hit.is_some(),
        "sentinel font falls back to em-box path; (60, 40) hits"
    );

    let miss = hit_test_topmost(&scene, [60.0, 70.0]);
    assert!(
        miss.is_none() || miss.and_then(|h| h.glyph_index).is_none(),
        "sentinel font em-box does not extend below y=58 (got {miss:?})"
    );
}
