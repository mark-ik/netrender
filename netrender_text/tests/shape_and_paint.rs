/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! End-to-end smoke for the parley → netrender adapter.
//!
//! Builds a parley::Layout for a short ASCII string with a system
//! font, runs `netrender_text::push_layout` to drop SceneGlyphRuns
//! into a Scene, renders via the netrender vello path, and verifies
//! pixels were painted.
//!
//! Skipped vacuously on hosts with no known system font path (same
//! pattern as `netrender/tests/p10prime_b_glyph_render.rs`).

use std::sync::Arc;

use netrender::{ColorLoad, NetrenderOptions, Scene, boot, create_netrender_instance};
use netrender_text::parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, Layout, LayoutContext, StyleProperty,
};

const DIM: u32 = 256;
const TILE: u32 = 64;

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
            eprintln!(
                "netrender_text test: loaded {} ({} bytes)",
                path,
                bytes.len()
            );
            return Some(bytes);
        }
    }
    None
}

fn make_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("netrender_text target"),
        size: wgpu::Extent3d {
            width: DIM,
            height: DIM,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("netrender_text view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, view)
}

/// Shape "Hello, world!" via parley, push to a Scene via the
/// adapter, render, and assert visible glyph pixels are painted.
/// This exercises font registration, glyph positioning from real
/// shaping, and end-to-end rasterization.
#[test]
fn netrender_text_01_shaped_paragraph_paints() {
    let Some(font_bytes) = try_load_system_font() else {
        eprintln!("netrender_text_01: no system font found; skipping");
        return;
    };

    // Boot wgpu / netrender renderer.
    let handles = boot().expect("wgpu boot");
    let renderer = create_netrender_instance(
        handles.clone(),
        NetrenderOptions {
            tile_cache_size: Some(TILE),
            enable_vello: true,
            ..Default::default()
        },
    )
    .expect("create_netrender_instance");

    // Build parley FontContext with our font registered manually.
    // Using register_fonts gives us a stable family name regardless
    // of how the host catalogs the file (Windows lists "Arial",
    // Linux lists "DejaVu Sans", etc.).
    let mut font_cx = FontContext::new();
    let blob = netrender_text::parley::fontique::Blob::new(Arc::new(font_bytes));
    let registered = font_cx.collection.register_fonts(blob, None);
    let (family_id, _) = registered
        .into_iter()
        .next()
        .expect("register_fonts returned no families");
    let family_name = font_cx
        .collection
        .family_name(family_id)
        .expect("registered family has a name")
        .to_owned();
    eprintln!("netrender_text_01: registered family '{}'", family_name);

    let mut layout_cx: LayoutContext<[f32; 4]> = LayoutContext::new();
    let text = "Hello, world!";
    let mut builder = layout_cx.ranged_builder(&mut font_cx, text, 1.0, true);
    builder.push_default(StyleProperty::FontSize(32.0));
    builder.push_default(StyleProperty::Brush([1.0, 1.0, 1.0, 1.0])); // opaque white
    builder.push_default(StyleProperty::FontFamily(FontFamily::named(&family_name)));

    let mut layout: Layout<[f32; 4]> = builder.build(text);
    layout.break_all_lines(Some(DIM as f32));
    layout.align(Alignment::Start, AlignmentOptions::default());
    let layout_height = layout.height();
    assert!(layout_height > 0.0, "parley laid out zero height");

    // Push at (16, 16) so the layout sits in the upper-left of the
    // frame with margin around it.
    let mut scene = Scene::new(DIM, DIM);
    netrender_text::push_layout(&mut scene, &layout, [16.0, 16.0]);

    // The adapter should have registered the font with the scene
    // (slot 1; index 0 is the no-font sentinel).
    assert!(
        scene.fonts.len() >= 2,
        "adapter should register at least one font; got {}",
        scene.fonts.len(),
    );
    assert!(
        scene.iter_glyph_runs().next().is_some(),
        "adapter should produce at least one glyph run for non-empty text",
    );

    // Render onto a black background and count painted pixels.
    let (target, view) = make_target(&handles.device);
    renderer.render_vello(&scene, &view, ColorLoad::Clear(wgpu::Color::BLACK));
    let bytes = renderer.wgpu_device.read_rgba8_texture(&target, DIM, DIM);

    let mut painted = 0usize;
    for chunk in bytes.chunks_exact(4) {
        if chunk[0] > 16 || chunk[1] > 16 || chunk[2] > 16 {
            painted += 1;
        }
    }
    eprintln!(
        "netrender_text_01: painted {} non-background pixels (of {})",
        painted,
        DIM * DIM,
    );
    assert!(
        painted > 200,
        "shaped 13-char paragraph at size 32 should paint many pixels; got {}",
        painted,
    );
}

/// Single-font dedup invariant: a layout that references the same
/// font in N glyph runs should register that font exactly once on
/// the Scene side. Otherwise the cross-frame Blob-id cache (and the
/// scene.fonts vec) would bloat with duplicates.
#[test]
fn netrender_text_02_font_deduped_within_layout() {
    let Some(font_bytes) = try_load_system_font() else {
        eprintln!("netrender_text_02: no system font found; skipping");
        return;
    };

    let mut font_cx = FontContext::new();
    let blob = netrender_text::parley::fontique::Blob::new(Arc::new(font_bytes));
    let (family_id, _) = font_cx
        .collection
        .register_fonts(blob, None)
        .into_iter()
        .next()
        .expect("register_fonts");
    let family_name = font_cx
        .collection
        .family_name(family_id)
        .expect("family_name")
        .to_owned();

    let mut layout_cx: LayoutContext<[f32; 4]> = LayoutContext::new();
    // Multi-line forces parley to emit at least one run per line —
    // good enough to confirm same-font dedup.
    let text = "first line\nsecond line\nthird line\nfourth line";
    let mut builder = layout_cx.ranged_builder(&mut font_cx, text, 1.0, true);
    builder.push_default(StyleProperty::FontSize(20.0));
    builder.push_default(StyleProperty::Brush([1.0, 1.0, 1.0, 1.0]));
    builder.push_default(StyleProperty::FontFamily(FontFamily::named(&family_name)));

    let mut layout: Layout<[f32; 4]> = builder.build(text);
    layout.break_all_lines(Some(DIM as f32));
    layout.align(Alignment::Start, AlignmentOptions::default());

    let mut scene = Scene::new(DIM, DIM);
    netrender_text::push_layout(&mut scene, &layout, [16.0, 16.0]);

    // 4 lines of text, 1 font → exactly one user-registered FontBlob
    // (slot 1; slot 0 is the sentinel placeholder).
    let runs: Vec<_> = scene.iter_glyph_runs().collect();
    assert!(
        runs.len() >= 4,
        "expected at least 4 glyph runs (one per line); got {}",
        runs.len(),
    );
    assert_eq!(
        scene.fonts.len(),
        2,
        "single-font layout should register exactly one font on top of the index-0 sentinel; got {} fonts",
        scene.fonts.len(),
    );
    // Every glyph run should reference font_id 1.
    for run in runs {
        assert_eq!(run.font_id, 1, "all runs should share font slot 1");
    }
}

/// Decoration painting: underline + strikethrough on a styled span
/// emit filled rects above + below the baseline. The adapter
/// pushes underline before the glyph run and strikethrough after,
/// matching the CSS text-decoration painting order.
#[test]
fn netrender_text_03_decorations_emit_rects() {
    use netrender_text::parley::StyleProperty;

    let Some(font_bytes) = try_load_system_font() else {
        eprintln!("netrender_text_03: no system font found; skipping");
        return;
    };

    let mut font_cx = FontContext::new();
    let blob = netrender_text::parley::fontique::Blob::new(Arc::new(font_bytes));
    let (family_id, _) = font_cx
        .collection
        .register_fonts(blob, None)
        .into_iter()
        .next()
        .expect("register_fonts");
    let family_name = font_cx
        .collection
        .family_name(family_id)
        .expect("family_name")
        .to_owned();

    let mut layout_cx: LayoutContext<[f32; 4]> = LayoutContext::new();
    let text = "Underlined";
    let mut builder = layout_cx.ranged_builder(&mut font_cx, text, 1.0, true);
    builder.push_default(StyleProperty::FontSize(24.0));
    builder.push_default(StyleProperty::Brush([1.0, 1.0, 1.0, 1.0]));
    builder.push_default(StyleProperty::FontFamily(FontFamily::named(&family_name)));
    // Underline + strikethrough across the whole text.
    builder.push_default(StyleProperty::Underline(true));
    builder.push_default(StyleProperty::UnderlineBrush(Some([0.0, 1.0, 1.0, 1.0])));
    builder.push_default(StyleProperty::Strikethrough(true));
    builder.push_default(StyleProperty::StrikethroughBrush(Some([
        1.0, 0.0, 1.0, 1.0,
    ])));

    let mut layout: Layout<[f32; 4]> = builder.build(text);
    layout.break_all_lines(Some(DIM as f32));
    layout.align(Alignment::Start, AlignmentOptions::default());

    let mut scene = Scene::new(DIM, DIM);
    netrender_text::push_layout(&mut scene, &layout, [16.0, 32.0]);

    // Expect at least: 1 underline rect + 1 glyph run + 1 strikethrough rect.
    let rects: Vec<_> = scene.iter_rects().collect();
    let runs: Vec<_> = scene.iter_glyph_runs().collect();
    assert!(
        rects.len() >= 2,
        "expected ≥2 rect ops (underline + strikethrough); got {}",
        rects.len(),
    );
    assert!(!runs.is_empty(), "expected at least one glyph run");

    // Sanity: at least one rect carries the cyan underline color and
    // one carries the magenta strikethrough color.
    let has_cyan = rects.iter().any(|r| r.color == [0.0, 1.0, 1.0, 1.0]);
    let has_magenta = rects.iter().any(|r| r.color == [1.0, 0.0, 1.0, 1.0]);
    assert!(has_cyan, "underline color rect missing");
    assert!(has_magenta, "strikethrough color rect missing");

    // Painter order: underline pushed before glyph run, strikethrough after.
    // Find op indices.
    let mut idx_underline = None;
    let mut idx_glyph = None;
    let mut idx_strikethrough = None;
    for (i, op) in scene.ops.iter().enumerate() {
        match op {
            netrender::SceneOp::Rect(r) if r.color == [0.0, 1.0, 1.0, 1.0] => {
                idx_underline.get_or_insert(i);
            }
            netrender::SceneOp::Rect(r) if r.color == [1.0, 0.0, 1.0, 1.0] => {
                idx_strikethrough.get_or_insert(i);
            }
            netrender::SceneOp::GlyphRun(_) => {
                idx_glyph.get_or_insert(i);
            }
            _ => {}
        }
    }
    let u = idx_underline.expect("underline rect found");
    let g = idx_glyph.expect("glyph run found");
    let s = idx_strikethrough.expect("strikethrough rect found");
    assert!(u < g, "underline must paint before glyphs (u={u}, g={g})");
    assert!(
        g < s,
        "strikethrough must paint after glyphs (g={g}, s={s})"
    );
}
