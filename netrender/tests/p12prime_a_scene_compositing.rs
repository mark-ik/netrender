/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 12a' — scene-level alpha + blend-mode compositing.
//!
//! Minimal compositing-correctness slice: the entire scene is
//! composited as a single layer with `root_alpha` and
//! `root_blend_mode` set on `Scene`. Useful for whole-canvas fade
//! transitions and global blend operations.
//!
//! What this slice does **not** do (deferred to a future phase
//! pending Scene API architectural decisions):
//!   - Nested groups (per-element opacity that composites a stack
//!     of overlapping primitives at < 1.0 alpha as a unit, distinct
//!     from the per-primitive alpha that already works).
//!   - Backdrop filters (`backdrop-filter: blur(...)` reading from
//!     pixels under the element).
//!   - Filter chains beyond `Renderer::build_box_shadow_mask`.
//!
//! Receipts:
//!   p12a_01_root_alpha_fade        — root_alpha = 0.5 fades the canvas
//!   p12a_02_root_alpha_one_noop    — default root_alpha = 1.0 doesn't
//!                                    add an outer layer (no-op pass)
//!   p12a_03_root_blend_multiply    — multiply blend over a non-black
//!                                    base color produces darkened result
//!   p12a_04_root_alpha_invalidates_tile_cache
//!                                  — changing root_alpha invalidates
//!                                    cached tile-Scenes

use netrender::{
    ColorLoad, NetrenderOptions, Scene, SceneBlendMode, boot, create_netrender_instance,
};

const DIM: u32 = 64;
const TILE_SIZE: u32 = 32;

fn make_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p12a target"),
        size: wgpu::Extent3d { width: DIM, height: DIM, depth_or_array_layers: 1 },
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
        label: Some("p12a view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, view)
}

fn read_pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * DIM + x) * 4) as usize;
    [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
}

fn render_with_clear(scene: &Scene, clear: wgpu::Color) -> Vec<u8> {
    let handles = boot().expect("wgpu boot");
    let renderer = create_netrender_instance(
        handles.clone(),
        NetrenderOptions { tile_cache_size: Some(TILE_SIZE), enable_vello: true, ..Default::default() },
    )
    .expect("renderer");
    let (target, view) = make_target(&handles.device);
    renderer.render_vello(scene, &view, ColorLoad::Clear(clear));
    renderer.wgpu_device.read_rgba8_texture(&target, DIM, DIM)
}

/// Full-canvas opaque red rect at root_alpha = 0.5. The whole-canvas
/// fade halves the alpha relative to the unfaded reference. Storage
/// is straight-alpha (per p1prime_02), so the painted pixels are
/// (255, 0, 0, 128) instead of (255, 0, 0, 255).
#[test]
fn p12a_01_root_alpha_fade() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 0.0, 0.0, 1.0]);
    scene.root_alpha = 0.5;

    let bytes = render_with_clear(&scene, wgpu::Color::TRANSPARENT);

    // Sample several interior pixels — all should be the half-alpha
    // red. Tolerance ±2 for AA / quantization.
    for &(x, y) in &[(8, 8), (32, 32), (56, 56)] {
        let p = read_pixel(&bytes, x, y);
        let max_diff = (0..4)
            .map(|i| (p[i] as i16 - [255, 0, 0, 128][i] as i16).unsigned_abs())
            .max()
            .unwrap();
        assert!(
            max_diff <= 2,
            "root_alpha=0.5 ({}, {}): {:?} not near (255, 0, 0, 128)",
            x, y, p
        );
    }
}

/// Default root_alpha = 1.0 + Normal blend should be a no-op — output
/// matches the Phase 1' opaque-red oracle byte-exactly.
#[test]
fn p12a_02_root_alpha_one_noop() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 0.0, 0.0, 1.0]);
    // root_alpha left at default 1.0; root_blend_mode at Normal.

    let bytes = render_with_clear(&scene, wgpu::Color::TRANSPARENT);

    // Should be uniformly opaque red.
    for &(x, y) in &[(8, 8), (32, 32), (56, 56)] {
        assert_eq!(
            read_pixel(&bytes, x, y),
            [255, 0, 0, 255],
            "default scene state should produce byte-exact opaque red"
        );
    }
}

/// Multiply blend: a 50%-gray rect over a red canvas should produce
/// dark red where the rect overlaps. Test pattern: clear to red,
/// push a full-canvas (0.5, 0.5, 0.5, 1.0) gray rect with
/// root_blend_mode = Multiply.
///
/// In sRGB-encoded blend space (per §6.3): result =
/// canvas_srgb * src_srgb component-wise. canvas = (1.0, 0, 0)
/// (clear red), src = (0.5, 0.5, 0.5) → result = (0.5, 0, 0) sRGB-
/// encoded → byte 128.
#[test]
fn p12a_03_root_blend_multiply() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [0.5, 0.5, 0.5, 1.0]);
    scene.root_blend_mode = SceneBlendMode::Multiply;

    let bytes = render_with_clear(
        &scene,
        wgpu::Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },  // red base
    );

    // Multiply red × 50% gray. Result: dark red. Storage:
    // (~128, 0, 0, 255). Allow some tolerance for vello's
    // sRGB-encoded interpretation.
    for &(x, y) in &[(8, 8), (32, 32), (56, 56)] {
        let p = read_pixel(&bytes, x, y);
        assert!(
            p[0] >= 100 && p[0] <= 160 && p[1] < 30 && p[2] < 30 && p[3] >= 240,
            "multiply blend ({}, {}): {:?} expected near (128, 0, 0, 255)",
            x, y, p
        );
    }
}

/// Changing root_alpha must invalidate the tile cache, otherwise
/// the next render reuses stale tile-Scenes (which were composed
/// without the new outer layer params).
#[test]
fn p12a_04_root_alpha_invalidates_tile_cache() {
    use netrender::TileCache;

    let mut s_a = Scene::new(DIM, DIM);
    s_a.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 0.0, 0.0, 1.0]);

    let mut tc = TileCache::new(TILE_SIZE);
    let _ = tc.invalidate(&s_a);
    let dirty_unchanged = tc.invalidate(&s_a);
    assert!(
        dirty_unchanged.is_empty(),
        "unchanged scene: zero dirty tiles"
    );

    // Same scene but with root_alpha = 0.5 — should re-dirty.
    let mut s_b = s_a.clone();
    s_b.root_alpha = 0.5;
    let dirty_changed = tc.invalidate(&s_b);
    assert!(
        !dirty_changed.is_empty(),
        "root_alpha change should invalidate tiles"
    );
}
