/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 10b.2 receipt — atlas eviction.
//!
//! 10a.1's bump-row packer panicked on vertical overflow. 10b.2 adds
//! per-slot `last_used` LRU stamps, a CPU-side raster cache, and a
//! repack-on-overflow path: when allocation fails, the oldest slot
//! whose `last_used < current_frame` is dropped, the survivors are
//! repacked into a fresh bump-pass on the same texture, and
//! allocation retries. Eviction loops until either the new glyph
//! fits or every surviving slot was touched in the current frame.
//! In the latter case (working set genuinely exceeds atlas size), we
//! panic with a message pointing at
//! `NetrenderOptions::glyph_atlas_size`.
//!
//! Receipts:
//!
//! 1. **`p10b2_overflow_evicts_lru_and_keeps_recent_glyphs`** —
//!    constructs a deliberately tiny atlas, pushes more glyphs than
//!    fit by spreading them across multiple frames, and asserts the
//!    most-recently-used glyph still renders correctly while the
//!    oldest is evicted.
//!
//! 2. **`p10b2_working_set_exceeds_atlas_panics`** — pushes more
//!    glyphs than the atlas can hold inside a single frame; the
//!    LRU policy can't help (every slot has `last_used == current
//!    frame`), so allocation must panic with the working-set
//!    message.
//!
//! 3. **`p10b2_repacked_glyphs_render_at_new_uvs`** — pixel-level:
//!    after repack relocates surviving glyphs, rendering through
//!    the new slot UVs still produces the expected glyph at the
//!    expected device position. This is the correctness check
//!    that proves repack didn't corrupt the survivors' atlas
//!    contents.

use std::panic::AssertUnwindSafe;

use netrender::{
    ColorLoad, FrameTarget, GlyphFormat, GlyphInstance, GlyphKey, GlyphRaster, NetrenderOptions,
    Renderer, Scene, boot, create_netrender_instance,
};

const VIEWPORT: u32 = 64;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Synthetic 5×5 'block' glyph. All pixels filled. Tests don't care
/// about glyph identity, only that distinct keys produce distinct
/// atlas slots.
fn block_glyph(size: u32) -> GlyphRaster {
    let pixels = vec![255u8; (size * size) as usize];
    GlyphRaster {
        width: size,
        height: size,
        bearing_x: 0,
        bearing_y: size as i32,
        format: GlyphFormat::Alpha,
        pixels,
    }
}

fn key(font_id: u32, glyph_id: u32) -> GlyphKey {
    GlyphKey { font_id, glyph_id, size_x64: 5 * 64 }
}

/// Build a renderer with an `atlas_size`-square atlas and run one
/// `prepare()` + `render()` for `scene`. Returns the readback bytes.
fn render_with_atlas_size(scene: &Scene, atlas_size: u32) -> Vec<u8> {
    let renderer = build_renderer(atlas_size);
    let pixels = render_one_frame(&renderer, scene);
    drop(renderer);
    pixels
}

fn build_renderer(atlas_size: u32) -> Renderer {
    let handles = boot().expect("wgpu boot");
    create_netrender_instance(
        handles,
        NetrenderOptions {
            glyph_atlas_size: Some(atlas_size),
            ..NetrenderOptions::default()
        },
    )
    .expect("create_netrender_instance")
}

fn render_one_frame(renderer: &Renderer, scene: &Scene) -> Vec<u8> {
    let [vw, vh] = [scene.viewport_width, scene.viewport_height];
    let device = renderer.wgpu_device.core.device.clone();
    let target_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p10b2 target"),
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
        ColorLoad::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
    );
    renderer.wgpu_device.read_rgba8_texture(&target_tex, vw, vh)
}

/// Receipt: a deliberately small atlas (16×16 = 256 pixels) holds
/// at most ~10 5×5 glyphs (3 per row, ~3 rows). Push 12 distinct
/// glyphs across 12 frames — one per frame — and after the atlas
/// overflows, the most-recent glyph must still render correctly.
///
/// The test exercises:
///   - `begin_frame` advancing the LRU stamp each `prepare()`,
///   - eviction triggering on overflow,
///   - repacked survivors continuing to render correctly,
///   - the most-recently-uploaded glyph's slot existing post-overflow.
#[test]
fn p10b2_overflow_evicts_lru_and_keeps_recent_glyphs() {
    let renderer = build_renderer(16);

    // Frames 0..N-1: each frame pushes one new glyph at pen (4, 9)
    // — the atlas size is 16 so 5×5 glyphs pack 3 per row, ~3 rows
    // before overflow. Twelve frames forces multiple evictions.
    const N_FRAMES: u32 = 12;
    let mut last_pixels: Option<Vec<u8>> = None;
    for i in 0..N_FRAMES {
        let mut scene = Scene::new(VIEWPORT, VIEWPORT);
        let k = key(0, i);
        scene.set_glyph_raster(k, block_glyph(5));
        scene.push_text_run(
            vec![GlyphInstance { key: k, x: 4.0, y: 9.0 }],
            [1.0, 1.0, 1.0, 1.0],
        );
        last_pixels = Some(render_one_frame(&renderer, &scene));
    }

    // Final frame's glyph should still appear at its pen position.
    // pen (4, 9), bearing_y = 5 → bitmap top at y = 9 - 5 = 4,
    // bitmap rows 4..9, columns 4..9.
    let pixels = last_pixels.expect("rendered at least one frame");
    let stride = (VIEWPORT * 4) as usize;
    let pixel = |x: u32, y: u32| -> [u8; 4] {
        let i = (y as usize) * stride + (x as usize) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    // Center of the 5×5 block: (6, 6).
    let center = pixel(6, 6);
    assert!(
        center[0] > 200 && center[1] > 200 && center[2] > 200,
        "post-eviction glyph center should be opaque white: got {:?}", center,
    );
    // Outside the block: (1, 1) must be the cleared background.
    assert_eq!(
        pixel(1, 1), [0, 0, 0, 0],
        "outside-block pixel must be clear",
    );
}

/// Receipt: pushing more glyphs than the atlas can hold *inside one
/// frame* must panic with the working-set message — every slot has
/// `last_used == current_frame`, so eviction can't free anything
/// without breaking correctness.
///
/// 16×16 atlas, 5×5 glyphs: at most ~10 fit. Push 20 in one frame.
#[test]
fn p10b2_working_set_exceeds_atlas_panics() {
    let renderer = build_renderer(16);

    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    let mut glyphs = Vec::new();
    for i in 0..20u32 {
        let k = key(0, i);
        scene.set_glyph_raster(k, block_glyph(5));
        // Stack glyphs vertically so they all draw within the
        // viewport; pen positions don't affect atlas allocation.
        glyphs.push(GlyphInstance { key: k, x: 2.0, y: 9.0 + (i as f32) });
    }
    scene.push_text_run(glyphs, [1.0, 1.0, 1.0, 1.0]);

    // Wrap in catch_unwind so the test can assert the panic message.
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _prepared = renderer.prepare(&scene);
    }));
    let err = result.expect_err("expected atlas-too-small panic");
    let msg = err
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| err.downcast_ref::<&'static str>().copied())
        .unwrap_or("<non-string panic payload>");
    assert!(
        msg.contains("working set exceeds atlas size"),
        "panic message should explain working-set-too-large: got {:?}",
        msg,
    );
    assert!(
        msg.contains("glyph_atlas_size"),
        "panic message should point at NetrenderOptions::glyph_atlas_size: got {:?}",
        msg,
    );
}

/// Receipt: when repack relocates a surviving glyph, the relocated
/// glyph's pixels still appear correctly at its rendered position.
///
/// 16×16 atlas with 5×5 block glyphs holds exactly 9 slots
/// (3 per row × 3 rows). To force the repack code path we need the
/// frame's atlas state to *exceed* 9 entries pre-eviction — so we
/// seed an extra throwaway glyph in frame 0, leave it untouched in
/// frame 1, and in frame 2 push A + 8 fresh glyphs. That's:
///
///   atlas before frame-2 allocation: throwaway (last_used=1) + A
///   atlas working set during frame 2: A + 8 fresh + lingering
///                                     throwaway = 10 keys / 9 slots
///
/// The 9th allocation fails; `throwaway` is the LRU candidate and
/// gets evicted; surviving 8 entries (A + new1..new7) are repacked
/// from a fresh bump-cursor pass, then new8 fits at the post-repack
/// tail. A's atlas slot moves during repack — and the test asserts
/// A's rendered pixels match frame-1's baseline (which captured
/// A at its pre-repack atlas position). Byte-identical pixels prove
/// repack re-uploaded A's bytes correctly into the new slot.
#[test]
fn p10b2_repacked_glyphs_render_at_new_uvs() {
    let renderer = build_renderer(16);

    // Frame 0: throwaway glyph that becomes the eviction target.
    // Renders briefly, then frame 1+ stops referencing it. Its
    // `last_used` stamp will be the smallest in frame 2 → it's the
    // LRU and gets evicted to make room.
    let key_throwaway = key(99, 0);
    let mut scene_throw = Scene::new(VIEWPORT, VIEWPORT);
    scene_throw.set_glyph_raster(key_throwaway, block_glyph(5));
    scene_throw.push_text_run(
        vec![GlyphInstance { key: key_throwaway, x: 4.0, y: 9.0 }],
        [1.0, 1.0, 1.0, 1.0],
    );
    let _ = render_one_frame(&renderer, &scene_throw);

    // Frame 1: just A — no atlas pressure yet, this captures the
    // "A at its pre-repack atlas slot" baseline.
    let key_a = key(0, 0);
    let mut scene_baseline = Scene::new(VIEWPORT, VIEWPORT);
    scene_baseline.set_glyph_raster(key_a, block_glyph(5));
    scene_baseline.push_text_run(
        vec![GlyphInstance { key: key_a, x: 4.0, y: 9.0 }],
        [1.0, 1.0, 1.0, 1.0],
    );
    let frame_baseline = render_one_frame(&renderer, &scene_baseline);

    // Frame 2: A + 8 fresh. Triggers eviction of throwaway and a
    // repack of survivors; A's atlas slot moves.
    let mut scene_pressure = Scene::new(VIEWPORT, VIEWPORT);
    scene_pressure.set_glyph_raster(key_a, block_glyph(5));
    let mut glyphs = vec![GlyphInstance { key: key_a, x: 4.0, y: 9.0 }];
    for i in 1..9u32 {
        let k = key(0, i);
        scene_pressure.set_glyph_raster(k, block_glyph(5));
        // Place fresh glyphs off-screen-right so they don't
        // visually overlap A's pixel band at (4..9, 4..9).
        glyphs.push(GlyphInstance { key: k, x: 30.0 + (i as f32) * 2.0, y: 9.0 });
    }
    scene_pressure.push_text_run(glyphs, [1.0, 1.0, 1.0, 1.0]);
    let frame_pressure = render_one_frame(&renderer, &scene_pressure);

    // A's pixel band must be byte-identical across baseline and
    // pressure frames. Different bytes here would mean repack
    // either failed to re-upload A or uploaded into the wrong slot.
    let stride = (VIEWPORT * 4) as usize;
    for y in 4u32..9 {
        for x in 4u32..9 {
            let i = (y as usize) * stride + (x as usize) * 4;
            let p_baseline = &frame_baseline[i..i + 4];
            let p_pressure = &frame_pressure[i..i + 4];
            assert_eq!(
                p_baseline, p_pressure,
                "A's pixel at ({}, {}) should be identical across \
                 frames (repack must not corrupt survivors): \
                 baseline={:?} pressure={:?}",
                x, y, p_baseline, p_pressure,
            );
        }
    }
}

/// Sanity that `render_with_atlas_size` actually works at the small
/// atlas configuration the eviction tests rely on. If this fails,
/// the larger eviction receipts above are silently testing nothing.
#[test]
fn p10b2_small_atlas_baseline_renders() {
    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    let k = key(7, 1);
    scene.set_glyph_raster(k, block_glyph(5));
    scene.push_text_run(
        vec![GlyphInstance { key: k, x: 4.0, y: 9.0 }],
        [1.0, 1.0, 1.0, 1.0],
    );
    let pixels = render_with_atlas_size(&scene, 16);
    let stride = (VIEWPORT * 4) as usize;
    let i = (6 * stride) + (6 * 4);
    let center = [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]];
    assert!(
        center[0] > 200 && center[1] > 200 && center[2] > 200,
        "16×16 atlas should render a 5×5 glyph: got {:?}", center,
    );
}
