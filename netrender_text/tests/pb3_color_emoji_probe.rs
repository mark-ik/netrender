/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Roadmap B3 — color-emoji / COLR-font verification probe.
//!
//! Loads a system emoji font (Segoe UI Emoji on Windows, Apple Color
//! Emoji on macOS, Noto Color Emoji on Linux), shapes an emoji
//! string via parley, renders through netrender's vello path, and
//! reads back the pixels to check whether the painted glyphs are
//! **chromatic** (color emoji) or **achromatic silhouettes**.
//!
//! Two outcomes per the roadmap:
//!
//! - **Verified (color present)**: vello + skrifa render COLR layers
//!   for free; the assertion passes and the probe is a one-line
//!   maintenance signal — re-run on text-stack changes.
//! - **Regression (only achromatic painted pixels)**: the assertion
//!   fails and the probe demotes B3 to a real work item against
//!   whichever upstream piece is missing.
//!
//! Skipped vacuously on hosts with no known emoji font path. CI
//! that wants to enforce this should bundle Noto Color Emoji under
//! `tests/data/` (license-permissive) and point the loader at it.

use std::sync::Arc;

use netrender::{ColorLoad, NetrenderOptions, Scene, boot, create_netrender_instance};
use netrender_text::parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, Layout, LayoutContext, StyleProperty,
};

const DIM: u32 = 256;
const TILE: u32 = 64;

/// Try a known list of system color-emoji font paths. Returns the
/// bytes of the first one that exists, or `None` if nothing matched.
fn try_load_color_emoji_font() -> Option<(Vec<u8>, &'static str)> {
    let candidates: &[&str] = &[
        // Windows
        r"C:\Windows\Fonts\seguiemj.ttf",
        // macOS
        "/System/Library/Fonts/Apple Color Emoji.ttc",
        // Linux (most distros bundle Noto Color Emoji somewhere)
        "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
        "/usr/share/fonts/noto/NotoColorEmoji.ttf",
        "/usr/share/fonts/google-noto-color-emoji/NotoColorEmoji.ttf",
        "/usr/share/fonts/TTF/NotoColorEmoji.ttf",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            eprintln!("pb3: loaded {} ({} bytes)", path, bytes.len());
            return Some((bytes, *path));
        }
    }
    None
}

fn make_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pb3 target"),
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
        label: Some("pb3 view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, view)
}

/// Treat a pixel as chromatic if its R, G, B channels differ by more
/// than `threshold`. A grayscale silhouette has R == G == B
/// (within rounding); a color emoji rendering has channel divergence
/// at the colored regions of the glyph (yellows, reds, blues).
fn is_chromatic(rgba: &[u8], threshold: u8) -> bool {
    let r = rgba[0];
    let g = rgba[1];
    let b = rgba[2];
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    max.saturating_sub(min) > threshold
}

#[test]
fn pb3_color_emoji_renders_chromatically() {
    let Some((font_bytes, src_path)) = try_load_color_emoji_font() else {
        eprintln!("pb3: no system color-emoji font found; skipping");
        return;
    };

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

    // Register the emoji font with parley's font context so the
    // shaper can resolve it by family name.
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
    eprintln!("pb3: registered emoji family '{}' from {}", family_name, src_path);

    // Build a layout containing a few common emoji. Mixing several
    // gives us multiple shots at landing a chromatic glyph in case
    // any one is missing (different platforms cover different sets).
    let mut layout_cx: LayoutContext<[f32; 4]> = LayoutContext::new();
    let text = "\u{1F600}\u{1F389}\u{1F308}"; // 😀 🎉 🌈
    let mut builder = layout_cx.ranged_builder(&mut font_cx, text, 1.0, true);
    builder.push_default(StyleProperty::FontSize(48.0));
    // Brush is irrelevant to COLR rendering — vello uses the layer
    // colors baked into the font — but parley requires one.
    builder.push_default(StyleProperty::Brush([1.0, 1.0, 1.0, 1.0]));
    builder.push_default(StyleProperty::FontFamily(FontFamily::named(&family_name)));

    let mut layout: Layout<[f32; 4]> = builder.build(text);
    layout.break_all_lines(Some(DIM as f32));
    layout.align(Alignment::Start, AlignmentOptions::default());
    let layout_height = layout.height();
    assert!(layout_height > 0.0, "parley laid out zero height for emoji");
    eprintln!("pb3: layout dims = {}×{}", layout.width(), layout_height);

    // Render against an opaque dark background so chromatic pixels
    // stand out against any anti-aliased edges.
    let mut scene = Scene::new(DIM, DIM);
    netrender_text::push_layout(&mut scene, &layout, [16.0, 16.0]);
    let (target, view) = make_target(&handles.device);
    renderer.render_vello(
        &scene,
        &view,
        ColorLoad::Clear(wgpu::Color {
            r: 0.05,
            g: 0.05,
            b: 0.05,
            a: 1.0,
        }),
    );
    let bytes = renderer.wgpu_device.read_rgba8_texture(&target, DIM, DIM);

    // Bucket: count painted pixels (anything substantially above the
    // dark-gray background) vs chromatic pixels (channel-divergent).
    let mut painted = 0usize;
    let mut chromatic = 0usize;
    for chunk in bytes.chunks_exact(4) {
        // Painted: at least one channel substantially above the
        // 0.05 background (~13 in 8-bit).
        let lit = chunk[0] > 32 || chunk[1] > 32 || chunk[2] > 32;
        if lit {
            painted += 1;
            if is_chromatic(chunk, 32) {
                chromatic += 1;
            }
        }
    }

    let chromatic_ratio = if painted == 0 {
        0.0
    } else {
        chromatic as f32 / painted as f32
    };
    eprintln!(
        "pb3: painted={} chromatic={} ratio={:.3}",
        painted, chromatic, chromatic_ratio
    );

    assert!(
        painted > 200,
        "no glyph pixels painted (painted={painted}); something broke before the COLR question — \
         font registration or shaping may have failed"
    );

    // The verdict line. Per roadmap B3 outcomes:
    //   Outcome A — chromatic ratio > 0.05 → COLR layers rendering, file as CLEARED.
    //   Outcome B — chromatic ratio ≈ 0   → silhouettes only; demote to real work item.
    //
    // 5% is generous: even a single colored layer per glyph (e.g.
    // yellow on the grinning-face) covers far more than 5% of the
    // painted pixels for that glyph. If we're below 5%, vello +
    // skrifa is not honoring COLR for this font.
    assert!(
        chromatic_ratio > 0.05,
        "color emoji rendered as silhouettes (chromatic ratio = {chromatic_ratio:.3}, \
         painted = {painted}, chromatic = {chromatic}). \
         B3 outcome B: COLR rendering is missing — file as a real work item against the \
         specific upstream gap (vello glyph path? skrifa? font? parley?)."
    );
}
