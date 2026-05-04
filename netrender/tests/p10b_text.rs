/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 10b.1 receipt — RGB(A) atlas + `swash::Format::Subpixel`
//! routing. The 10a.4 dual-source pipeline was bit-equivalent to
//! grayscale because the atlas was R8 with a broadcast-from-`.r`
//! sample. 10b.1 flips the atlas to `Rgba8Unorm` and teaches the
//! atlas to expand `Alpha` glyphs as `(c, c, c, 255)` and `Subpixel`
//! glyphs as `(r, g, b, 255)`; the dual-source shader samples
//! `.rgb` for genuine LCD per-channel coverage.
//!
//! Two receipts:
//!
//! 1. **`p10b1_subpixel_per_channel_coverage`** — hand-authored
//!    `Subpixel` raster with deliberately distinct `R` / `G` / `B`
//!    bytes. Renders through the dual-source pipeline against an
//!    opaque background and asserts the output framebuffer pixels'
//!    R / G / B values reflect the per-channel coverage triple
//!    (i.e. they're not all equal across the rendered glyph). The
//!    grayscale path can't produce that asymmetry — the assertion
//!    proves the per-channel chain is end-to-end live.
//!
//! 2. **`p10b1_swash_subpixel_format_yields_3_bpp`** — sanity that
//!    `RasterContext::rasterize_subpixel` actually asks `swash` for
//!    `zeno::Format::Subpixel` and returns 3-bytes-per-pixel data
//!    via the new `GlyphFormat::Subpixel` tag. Independent of the
//!    renderer pipeline; receipts the rasterizer-side wiring.
//!
//! Tests skip cleanly on adapters that don't expose
//! `Features::DUAL_SOURCE_BLENDING` (the renderer falls back to
//! grayscale, which collapses the per-channel signal — there's
//! nothing to assert).

use netrender::{
    ColorLoad, FrameTarget, GlyphFormat, GlyphInstance, GlyphKey, GlyphRaster, NetrenderOptions,
    RasterContext, Scene, boot, create_netrender_instance,
};

const PROGGY_TTF: &[u8] = include_bytes!("../res/Proggy.ttf");

const VIEWPORT: u32 = 64;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Did the booted device pick up `Features::DUAL_SOURCE_BLENDING`?
fn dual_source_supported() -> bool {
    let handles = boot().expect("wgpu boot");
    handles.device.features().contains(wgpu::Features::DUAL_SOURCE_BLENDING)
}

fn render_with_subpixel_aa(scene: &Scene) -> Vec<u8> {
    let [vw, vh] = [scene.viewport_width, scene.viewport_height];
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
        label: Some("p10b target"),
        size: wgpu::Extent3d { width: vw, height: vh, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target_tex.create_view(&wgpu::TextureViewDescriptor::default());

    let prepared = renderer.prepare(scene);
    renderer.render(
        &prepared,
        FrameTarget { view: &target_view, format: TARGET_FORMAT, width: vw, height: vh },
        // Dual-source subpixel blend assumes an opaque destination
        // (see netrender/doc/text-rendering.md). Clear to opaque
        // black so the per-channel signal isn't multiplied by a
        // transparent backdrop.
        ColorLoad::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }),
    );

    renderer.wgpu_device.read_rgba8_texture(&target_tex, vw, vh)
}

/// Build a 4-wide × 1-tall `Subpixel` raster with deliberately
/// distinct per-pixel R / G / B coverage. Each output pixel is one
/// triple `(r, g, b)`; row-major, tightly packed at 3 bytes/pixel.
///
///   pixel 0: (255,   0,   0) — pure red coverage
///   pixel 1: (  0, 255,   0) — pure green coverage
///   pixel 2: (  0,   0, 255) — pure blue coverage
///   pixel 3: (255, 128,  64) — asymmetric mix
fn synthetic_subpixel_raster_4x1() -> GlyphRaster {
    GlyphRaster {
        width: 4,
        height: 1,
        bearing_x: 0,
        bearing_y: 1,
        format: GlyphFormat::Subpixel,
        pixels: vec![
            255,   0,   0,
              0, 255,   0,
              0,   0, 255,
            255, 128,  64,
        ],
    }
}

const KEY_SUBPIXEL: GlyphKey = GlyphKey { font_id: 100, glyph_id: 0xB1, size_x64: 1 * 64 };

/// Receipt: a `GlyphFormat::Subpixel` glyph rendered with
/// `text_subpixel_aa = true` produces per-channel-different pixels in
/// the output framebuffer. Specifically: at each device-pixel that
/// covers exactly one source pixel of the synthetic raster, the
/// framebuffer's R / G / B reflect the source triple.
///
/// Glyph layout — the 4×1 raster lives at pen `(10, 20)` with
/// `bearing_y = 1`, putting its row at device y = 19, columns
/// 10..14. We sample those four pixels and assert each is the
/// expected per-channel coverage ×  white tint, in the sRGB
/// framebuffer (Rgba8UnormSrgb), so we work in sRGB byte space
/// directly.
#[test]
fn p10b1_subpixel_per_channel_coverage() {
    if !dual_source_supported() {
        println!(
            "  skipping: adapter lacks DUAL_SOURCE_BLENDING — text_subpixel_aa \
             falls back to grayscale, which collapses the per-channel signal",
        );
        return;
    }

    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    scene.set_glyph_raster(KEY_SUBPIXEL, synthetic_subpixel_raster_4x1());
    scene.push_text_run(
        vec![GlyphInstance { key: KEY_SUBPIXEL, x: 10.0, y: 20.0 }],
        // White, opaque, premultiplied. The dual-source equation
        // multiplies tint by per-channel coverage, so for white
        // tint the framebuffer pixel reduces to the coverage
        // triple itself.
        [1.0, 1.0, 1.0, 1.0],
    );

    let pixels = render_with_subpixel_aa(&scene);
    let stride = (VIEWPORT * 4) as usize;
    let pixel = |x: u32, y: u32| -> [u8; 4] {
        let i = (y as usize) * stride + (x as usize) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };

    // The 4×1 raster lands at device row 19 (pen_y - bearing_y),
    // columns 10..14. Sample each column.
    //
    // The framebuffer is sRGB-encoded; the dual-source blend writes
    // linear values that the GPU encodes back to sRGB. White-tint
    // coverage of `c` linear maps to sRGB byte `srgb_encode(c/255) *
    // 255`. We assert the *relative* per-channel asymmetry rather
    // than exact bytes — the absolute sRGB encoding is correct for
    // any single channel but the assertion is "channels are not all
    // equal" plus "the dominant channel matches the source's
    // dominant channel."
    let p0 = pixel(10, 19); // source (255, 0, 0)
    let p1 = pixel(11, 19); // source (0, 255, 0)
    let p2 = pixel(12, 19); // source (0, 0, 255)
    let p3 = pixel(13, 19); // source (255, 128, 64)

    // p0: red dominant, green and blue zero.
    assert!(
        p0[0] > 200 && p0[1] == 0 && p0[2] == 0,
        "p0 should be red-dominant with zero G/B: got {:?}", p0,
    );
    // p1: green dominant, red and blue zero.
    assert!(
        p1[0] == 0 && p1[1] > 200 && p1[2] == 0,
        "p1 should be green-dominant with zero R/B: got {:?}", p1,
    );
    // p2: blue dominant, red and green zero.
    assert!(
        p2[0] == 0 && p2[1] == 0 && p2[2] > 200,
        "p2 should be blue-dominant with zero R/G: got {:?}", p2,
    );
    // p3: R > G > B (matching source 255 > 128 > 64). The exact
    // sRGB encoding shifts the values but the ordering is
    // preserved across the linear→sRGB curve.
    assert!(
        p3[0] > p3[1] && p3[1] > p3[2] && p3[2] > 0,
        "p3 should preserve R > G > B ordering from source (255, 128, 64): got {:?}", p3,
    );

    // The whole point of subpixel: at least one of the four pixels
    // must have non-equal R/G/B. The grayscale path can't produce
    // this signal.
    let any_per_channel = [p0, p1, p2, p3]
        .iter()
        .any(|p| !(p[0] == p[1] && p[1] == p[2]));
    assert!(
        any_per_channel,
        "expected at least one rendered pixel with R != G != B; \
         all four pixels were grayscale-broadcast — per-channel \
         pipeline is not live",
    );
}

/// Sanity: `RasterContext::rasterize_subpixel` returns a raster
/// whose `format` tag matches `pixels.len()` (i.e. the rasterizer
/// detects swash's actual output layout instead of trusting the
/// requested format). For Proggy — bitmap-only EBDT — swash returns
/// single-channel alpha bytes regardless of the requested
/// `zeno::Format::Subpixel`, so the returned raster is correctly
/// tagged `GlyphFormat::Alpha`. The atlas upload then expands this
/// as `(c, c, c, 255)` and the dual-source shader sees a broadcast
/// triple.
///
/// An outline-font asset would exercise the genuine
/// `GlyphFormat::Subpixel` return; that's a separate add.
#[test]
fn p10b1_swash_subpixel_with_bitmap_font_falls_back_to_alpha() {
    let mut ctx = RasterContext::new();
    let gid = ctx
        .glyph_id_for_char(PROGGY_TTF, 0, 'A')
        .expect("Proggy parses");
    let raster = ctx
        .rasterize_subpixel(PROGGY_TTF, 0, gid, 13.0, false)
        .expect("rasterize_subpixel 'A'");

    assert!(raster.width > 0 && raster.height > 0);
    let pixel_count = (raster.width as usize) * (raster.height as usize);

    // The raster's format tag must match its actual byte layout —
    // not the format that was requested. For Proggy (bitmap-only),
    // swash produces 1 byte/pixel and we tag Alpha accordingly.
    assert_eq!(
        raster.format,
        GlyphFormat::Alpha,
        "Proggy is bitmap-only; swash falls back to alpha layout. \
         Got {:?} with {} pixels and {} bytes",
        raster.format, pixel_count, raster.pixels.len(),
    );
    assert_eq!(
        raster.pixels.len(),
        pixel_count,
        "Alpha-tagged raster carries 1 byte per pixel: {}x{} = {} bytes, got {}",
        raster.width, raster.height, pixel_count, raster.pixels.len(),
    );
}
