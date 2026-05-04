/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 10b.7 receipt — outline-font subpixel rasterization.
//!
//! Proggy.ttf (the bundled fixture font) is bitmap-only, so
//! `BoundRaster::rasterize_subpixel` falls back to `GlyphFormat::Alpha`
//! by way of the `detect_format` swash-fallback path. That validates
//! the fallback contract but doesn't exercise swash's actual subpixel
//! rasterization on vector glyphs — the path that produces genuine
//! `(R, G, B)` LCD coverage triples for outline TrueType / OpenType
//! fonts.
//!
//! This receipt fills that gap. It probes a list of likely outline-font
//! locations (an env-var override, then platform-default system fonts),
//! and on the first hit:
//!
//! 1. **`p10b7_outline_font_subpixel_yields_per_channel_data`** —
//!    rasterizes a glyph with `rasterize_subpixel` and asserts the
//!    returned raster carries `GlyphFormat::Subpixel` (3 bytes/pixel)
//!    AND at least one pixel has genuinely different `R`/`G`/`B`
//!    values. Confirms the rasterizer-side LCD path is live.
//!
//! 2. **`p10b7_outline_font_subpixel_renders_with_per_channel_pixels`**
//!    — pushes the same glyph through the netrender pipeline with
//!    `text_subpixel_aa = true`. The framebuffer must show
//!    per-channel-different pixels somewhere within the rendered
//!    glyph's footprint, proving the end-to-end outline-font →
//!    swash → atlas → dual-source shader → framebuffer path.
//!
//! Skips cleanly if no outline font is found. To enable on a CI
//! machine, set `NETRENDER_OUTLINE_FONT=/path/to/font.ttf` or drop
//! a font at one of the platform-default locations.

use std::path::PathBuf;
use std::sync::Arc;

use netrender::{
    ColorLoad, FontHandle, FrameTarget, GlyphFormat, GlyphInstance, NetrenderOptions,
    RasterContext, Scene, boot, create_netrender_instance,
};

const VIEWPORT: u32 = 64;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Locate an outline font for the receipt. Order:
///   1. `NETRENDER_OUTLINE_FONT` env var (most explicit; CI hook
///      for pointing at a specific font without rebuilding).
///   2. Bundled `netrender/res/inconsolata/Inconsolata-Regular.ttf` —
///      OFL-licensed, ~80 KB, hand-tuned at small pixel sizes which
///      is exactly what the subpixel rasterizer should excel at.
///      Present in every clone of this repo; works on CI containers
///      without any system fonts installed.
///   3. Platform-default system fonts (Windows / Linux / macOS) —
///      a fallback for the rare case where the bundled asset is
///      missing.
///
/// Returns `None` only if every candidate is missing, in which case
/// the caller should skip the test with a clear message.
fn find_outline_font() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("NETRENDER_OUTLINE_FONT") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }

    let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("res")
        .join("inconsolata")
        .join("Inconsolata-Regular.ttf");
    if bundled.exists() {
        return Some(bundled);
    }

    // System-font fallbacks for any environment that somehow lacks
    // the bundled file. First match wins.
    let candidates: &[&str] = &[
        // Windows
        r"C:\Windows\Fonts\arial.ttf",
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\calibri.ttf",
        r"C:\Windows\Fonts\consola.ttf",
        // Linux (DejaVu is widely present on desktop installs)
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        // macOS
        "/System/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/Helvetica.ttc",
    ];

    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

fn dual_source_supported() -> bool {
    let handles = boot().expect("wgpu boot");
    handles.device.features().contains(wgpu::Features::DUAL_SOURCE_BLENDING)
}

#[test]
fn p10b7_outline_font_subpixel_yields_per_channel_data() {
    let path = match find_outline_font() {
        Some(p) => p,
        None => {
            println!(
                "  skipping: no outline font found. Set NETRENDER_OUTLINE_FONT \
                 or install a system font at one of the probed paths.",
            );
            return;
        }
    };

    let bytes = std::fs::read(&path).expect("read outline font");
    let handle = FontHandle::new(Arc::from(bytes.as_slice()), 0, 7700);
    let mut ctx = RasterContext::new();
    let mut bound = ctx.bind(&handle, 24.0, true).expect("bind outline font");

    // 'a' has interesting curves at typical body sizes — the bowl
    // is where LCD subpixel divergence shows up most visibly.
    let gid = bound.glyph_id_for_char('a');
    let raster = bound
        .rasterize_subpixel(gid)
        .expect("rasterize_subpixel 'a'");

    assert_eq!(
        raster.format,
        GlyphFormat::Subpixel,
        "outline font + rasterize_subpixel must produce Subpixel-tagged raster; \
         got {:?} (font path {})",
        raster.format,
        path.display(),
    );
    let expected_bytes = 3 * (raster.width as usize) * (raster.height as usize);
    assert_eq!(
        raster.pixels.len(),
        expected_bytes,
        "Subpixel raster must carry 3 bytes per pixel: {}x{} expected {} got {}",
        raster.width, raster.height, expected_bytes, raster.pixels.len(),
    );

    // The whole point: at least one pixel has R != G != B (genuine
    // per-channel LCD coverage). On bitmap-only fonts swash returns
    // R == G == B; on outline fonts the LCD-stripe convolution
    // produces genuinely different per-channel coverage at glyph
    // edges.
    let any_per_channel = raster.pixels.chunks_exact(3).any(|rgb| {
        rgb[0] != rgb[1] || rgb[1] != rgb[2]
    });
    assert!(
        any_per_channel,
        "outline-font subpixel raster should produce per-channel-different \
         pixels somewhere in the glyph; all {} pixels were grayscale-broadcast \
         (font path {}). Either swash's subpixel pipeline isn't engaging on \
         this font, or the chosen glyph + size has no LCD-distinguishable edges.",
        raster.pixels.len() / 3,
        path.display(),
    );
}

#[test]
fn p10b7_outline_font_subpixel_renders_with_per_channel_pixels() {
    if !dual_source_supported() {
        println!(
            "  skipping: adapter lacks DUAL_SOURCE_BLENDING — \
             text falls back to grayscale regardless of rasterizer output",
        );
        return;
    }
    let path = match find_outline_font() {
        Some(p) => p,
        None => {
            println!(
                "  skipping: no outline font found. Set NETRENDER_OUTLINE_FONT \
                 or install a system font at one of the probed paths.",
            );
            return;
        }
    };

    let bytes = std::fs::read(&path).expect("read outline font");
    let handle = FontHandle::new(Arc::from(bytes.as_slice()), 0, 7701);
    let mut ctx = RasterContext::new();
    let mut bound = ctx.bind(&handle, 24.0, true).expect("bind outline font");
    let gid = bound.glyph_id_for_char('a');
    let raster = bound
        .rasterize_subpixel(gid)
        .expect("rasterize_subpixel 'a'");
    let key = bound.key_for_glyph(gid);
    drop(bound);

    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    scene.set_glyph_raster(key, raster);
    scene.push_text_run(
        // Pen at (10, 40) — places the glyph well inside the 64x64
        // viewport for any reasonable advance + bearing.
        vec![GlyphInstance { key, x: 10.0, y: 40.0 }],
        [1.0, 1.0, 1.0, 1.0],
    );

    let handles = boot().expect("wgpu boot");
    let device = handles.device.clone();
    let renderer = create_netrender_instance(
        handles,
        NetrenderOptions {
            text_subpixel_aa: true,
            ..NetrenderOptions::default()
        },
    )
    .expect("create_netrender_instance");
    let target_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p10b7 target"),
        size: wgpu::Extent3d { width: VIEWPORT, height: VIEWPORT, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let prepared = renderer.prepare(&scene);
    renderer.render(
        &prepared,
        FrameTarget { view: &target_view, format: TARGET_FORMAT, width: VIEWPORT, height: VIEWPORT },
        // Opaque background — subpixel needs an opaque destination
        // for clean per-channel output.
        ColorLoad::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }),
    );
    let pixels = renderer.wgpu_device.read_rgba8_texture(&target_tex, VIEWPORT, VIEWPORT);

    // Walk the framebuffer; count pixels with R != G or G != B.
    // The grayscale path can't produce these; option B's
    // direct-to-framebuffer subpixel path can.
    let mut per_channel_pixels = 0usize;
    let mut total_glyph_pixels = 0usize;
    for chunk in pixels.chunks_exact(4) {
        // A glyph pixel is anything with non-zero color (cleared
        // background is opaque-black).
        if chunk[0] != 0 || chunk[1] != 0 || chunk[2] != 0 {
            total_glyph_pixels += 1;
            if chunk[0] != chunk[1] || chunk[1] != chunk[2] {
                per_channel_pixels += 1;
            }
        }
    }

    assert!(
        total_glyph_pixels > 0,
        "outline glyph 'a' should render at least some pixels; \
         got an all-clear framebuffer (font path {})",
        path.display(),
    );
    assert!(
        per_channel_pixels > 0,
        "outline-font subpixel render should produce per-channel-different \
         pixels in the framebuffer; got {} glyph pixels but 0 with R/G/B \
         divergence (font path {})",
        total_glyph_pixels,
        path.display(),
    );
}
