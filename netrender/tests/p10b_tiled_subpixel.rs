/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 10b.6 receipt — option B: text bypasses the tile cache.
//!
//! Before 10b.6, tiled mode rendered text into the tile texture
//! and then sampled it through `brush_image_alpha` for composite.
//! That sampled-intermediate path collapses LCD per-channel
//! coverage at the composite step, so subpixel-AA was unavailable
//! in tiled mode regardless of the consumer's `text_subpixel_aa`
//! setting.
//!
//! Option B (this receipt): `prepare_tiled` now runs a final
//! text-direct sub-pass after composite. Text writes directly into
//! the LCD-aligned sRGB framebuffer, just like the direct path. The
//! per-run transform-aware policy applies; subpixel pixels appear
//! when the run's transform is pure 2D translation.
//!
//! Receipts:
//!
//! 1. **`p10b6_tiled_translated_run_produces_subpixel_pixels`** —
//!    a `Subpixel`-format glyph rendered through tiled mode +
//!    `text_subpixel_aa = true` produces per-channel-different
//!    pixels. This is the headline behavioral change vs. the
//!    pre-10b.6 architecture.
//!
//! 2. **`p10b6_tiled_text_overlay_appears_in_front_of_composite`**
//!    — text rendered via the tile-mode text-direct sub-pass
//!    correctly z-orders in front of composite tiles. Verified by
//!    placing text over an opaque rect; the text pixels must show
//!    the text color, not the rect color underneath.
//!
//! 3. **`p10b6_tiled_text_only_scene_no_dirty_tiles`** — a
//!    text-only scene (no rects/images/gradients) doesn't dirty
//!    tiles because text isn't *in* the tiles anymore. After
//!    `prepare()`, the tile cache reports zero dirty tiles
//!    invalidated even though text was rendered.

use std::sync::Mutex;

use netrender::{
    ColorLoad, FrameTarget, GlyphFormat, GlyphInstance, GlyphKey, GlyphRaster, NetrenderOptions,
    Scene, TileCache, boot, create_netrender_instance,
};

const VIEWPORT: u32 = 64;
const TILE_SIZE: u32 = 32;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

fn dual_source_supported() -> bool {
    let handles = boot().expect("wgpu boot");
    handles.device.features().contains(wgpu::Features::DUAL_SOURCE_BLENDING)
}

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

fn block_glyph_alpha(size: u32) -> GlyphRaster {
    GlyphRaster {
        width: size,
        height: size,
        bearing_x: 0,
        bearing_y: size as i32,
        format: GlyphFormat::Alpha,
        pixels: vec![255u8; (size * size) as usize],
    }
}

const KEY_SUBPIXEL: GlyphKey = GlyphKey { font_id: 600, glyph_id: 0xB6, size_x64: 1 * 64 };
const KEY_BLOCK: GlyphKey = GlyphKey { font_id: 601, glyph_id: 0xB6, size_x64: 5 * 64 };

fn render_tiled_subpixel(scene: &Scene) -> Vec<u8> {
    let [vw, vh] = [scene.viewport_width, scene.viewport_height];
    let handles = boot().expect("wgpu boot");
    let device = handles.device.clone();
    let renderer = create_netrender_instance(
        handles,
        NetrenderOptions {
            text_subpixel_aa: true,
            tile_cache_size: Some(TILE_SIZE),
            ..NetrenderOptions::default()
        },
    )
    .expect("create_netrender_instance");

    let target_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p10b6 target"),
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
        // Opaque background — dual-source subpixel needs an opaque
        // destination at each text pixel (same constraint as direct
        // path).
        ColorLoad::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }),
    );
    renderer.wgpu_device.read_rgba8_texture(&target_tex, vw, vh)
}

/// Headline receipt: in tiled mode, a translated `Subpixel`-format
/// run produces genuine per-channel coverage in the framebuffer.
/// Pre-10b.6 the same scene rendered everything as grayscale because
/// text went through the tile cache.
#[test]
fn p10b6_tiled_translated_run_produces_subpixel_pixels() {
    if !dual_source_supported() {
        println!(
            "  skipping: adapter lacks DUAL_SOURCE_BLENDING — \
             text falls back to grayscale regardless of architecture",
        );
        return;
    }

    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    scene.set_glyph_raster(KEY_SUBPIXEL, synthetic_subpixel_raster_4x1());
    scene.push_text_run(
        vec![GlyphInstance { key: KEY_SUBPIXEL, x: 10.0, y: 20.0 }],
        [1.0, 1.0, 1.0, 1.0],
    );
    let pixels = render_tiled_subpixel(&scene);

    let stride = (VIEWPORT * 4) as usize;
    let pixel = |x: u32, y: u32| -> [u8; 4] {
        let i = (y as usize) * stride + (x as usize) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };

    // 4×1 raster lands at device row 19 (pen_y - bearing_y = 20-1),
    // cols 10..14. Source pixel 0 = pure red coverage.
    let p0 = pixel(10, 19);
    assert!(
        p0[0] > 200 && p0[1] == 0 && p0[2] == 0,
        "tiled mode should produce per-channel coverage on translated runs; \
         expected red-only at (10, 19), got {:?}", p0,
    );

    // At least one of the four rendered pixels has R != G != B.
    let p1 = pixel(11, 19);
    let p2 = pixel(12, 19);
    let p3 = pixel(13, 19);
    let any_per_channel = [p0, p1, p2, p3]
        .iter()
        .any(|p| !(p[0] == p[1] && p[1] == p[2]));
    assert!(
        any_per_channel,
        "tiled mode must produce at least one per-channel-different pixel; \
         all four rendered pixels were grayscale-broadcast — text path is \
         going through the tile cache instead of the direct sub-pass",
    );
}

/// Receipt: text composes correctly on top of an opaque background
/// rect that sits inside the tile cache. The text z-depth must put
/// it in front of the composite (z=0.5); rendering must show text
/// pixels on top of rect pixels at their overlap.
#[test]
fn p10b6_tiled_text_overlay_appears_in_front_of_composite() {
    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    // Opaque red rect filling the viewport. Goes through the tile
    // cache (non-text primitives still do).
    scene.push_rect(
        0.0, 0.0, VIEWPORT as f32, VIEWPORT as f32,
        [1.0, 0.0, 0.0, 1.0], // premultiplied opaque red
    );
    // Block glyph at (20, 25) → 5×5 white block at device (20..25, 20..25).
    scene.set_glyph_raster(KEY_BLOCK, block_glyph_alpha(5));
    scene.push_text_run(
        vec![GlyphInstance { key: KEY_BLOCK, x: 20.0, y: 25.0 }],
        [1.0, 1.0, 1.0, 1.0],
    );

    let handles = boot().expect("wgpu boot");
    let device = handles.device.clone();
    let renderer = create_netrender_instance(
        handles,
        NetrenderOptions {
            tile_cache_size: Some(TILE_SIZE),
            ..NetrenderOptions::default()
        },
    )
    .expect("create_netrender_instance");
    let target_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p10b6 overlay target"),
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
        ColorLoad::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }),
    );
    let pixels = renderer.wgpu_device.read_rgba8_texture(&target_tex, VIEWPORT, VIEWPORT);

    let stride = (VIEWPORT * 4) as usize;
    let pixel = |x: u32, y: u32| -> [u8; 4] {
        let i = (y as usize) * stride + (x as usize) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };

    // Outside the text glyph (e.g., (5, 5)): should be the rect
    // color (red) — composited from the tile cache.
    let bg = pixel(5, 5);
    assert!(
        bg[0] > 200 && bg[1] < 50 && bg[2] < 50,
        "background should be opaque red from the rect: got {:?}", bg,
    );
    // Inside the glyph (center of the 5×5 block: device (22, 22)):
    // text-direct sub-pass overwrites the red rect with white text.
    // White-on-red overlay produces white if the text alpha is 1.
    let center = pixel(22, 22);
    assert!(
        center[0] > 200 && center[1] > 200 && center[2] > 200,
        "text overlay should appear in front of the rect (white text on \
         red bg); got {:?}", center,
    );
}

/// Receipt: a text-only scene (no rects/images/gradients) produces
/// zero dirty tiles. Text isn't in the tile cache anymore, so adding
/// or moving text doesn't trigger tile invalidation. This is the
/// architectural win that motivated option B.
#[test]
fn p10b6_tiled_text_only_scene_no_dirty_tiles() {
    let handles = boot().expect("wgpu boot");
    let renderer = create_netrender_instance(
        handles,
        NetrenderOptions {
            tile_cache_size: Some(TILE_SIZE),
            ..NetrenderOptions::default()
        },
    )
    .expect("create_netrender_instance");

    // Frame 0: empty scene. Stabilises the tile cache.
    let empty = Scene::new(VIEWPORT, VIEWPORT);
    let _ = renderer.prepare(&empty);

    // Frame 1: same dimensions, but with a text run. The text is
    // not a tile-cache primitive, so no tiles should dirty.
    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    scene.set_glyph_raster(KEY_BLOCK, block_glyph_alpha(5));
    scene.push_text_run(
        vec![GlyphInstance { key: KEY_BLOCK, x: 10.0, y: 15.0 }],
        [1.0, 1.0, 1.0, 1.0],
    );
    let _ = renderer.prepare(&scene);

    let tile_cache: &Mutex<TileCache> =
        renderer.tile_cache().expect("tile_cache option set");
    let dirty_count = tile_cache
        .lock()
        .expect("tile_cache lock")
        .dirty_count_last_invalidate();
    assert_eq!(
        dirty_count, 0,
        "text-only frame should produce zero dirty tiles \
         (text bypasses the tile cache); got {} dirty",
        dirty_count,
    );
}
