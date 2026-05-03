/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 5b' — chromatic image tints via Mix::Multiply layer.
//!
//! Three probes:
//!
//! - `p5b_01_chromatic_red_tint_on_white` — white image with full
//!   red tint `[1.0, 0.0, 0.0, 1.0]`; output should be pure red.
//! - `p5b_02_chromatic_gray_with_alpha` — white image with the
//!   shadow-style tint `[0.1, 0.1, 0.1, 0.5]` (premultiplied
//!   straight-alpha gray, 50% opacity, 20% straight value); output
//!   matches the expected (51, 51, 51, 128) within tolerance.
//! - `p5b_03_chromatic_preserves_image_alpha` — image with a
//!   transparent border tinted with a chromatic color: tint must
//!   only affect non-transparent pixels (SrcAtop), transparent
//!   border stays transparent.

use netrender::{ImageData, Scene, boot, vello_rasterizer::scene_to_vello};
use vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions,
    peniko::Color,
};

const DIM: u32 = 64;
const IMG_KEY: u64 = 0xCC;

fn make_renderer(device: &wgpu::Device) -> Renderer {
    Renderer::new(
        device,
        RendererOptions {
            use_cpu: false,
            antialiasing_support: AaSupport::area_only(),
            num_init_threads: None,
            pipeline_cache: None,
        },
    )
    .expect("vello::Renderer::new")
}

fn make_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p5b' tint target"),
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
        label: Some("p5b' tint storage view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, view)
}

fn render_params() -> RenderParams {
    RenderParams {
        base_color: Color::from_rgba8(0, 0, 0, 0),
        width: DIM,
        height: DIM,
        antialiasing_method: AaConfig::Area,
    }
}

fn read_pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * DIM + x) * 4) as usize;
    [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
}

#[track_caller]
fn assert_within_tol(actual: [u8; 4], expected: [u8; 4], tol: u8, where_: &str) {
    let max = (0..4)
        .map(|i| (actual[i] as i16 - expected[i] as i16).unsigned_abs() as u8)
        .max()
        .unwrap();
    assert!(
        max <= tol,
        "{}: actual {:?}, expected {:?} (max channel diff = {}, tol = {})",
        where_, actual, expected, max, tol
    );
}

fn render_scene(scene: &Scene) -> Vec<u8> {
    let handles = boot().expect("wgpu boot");
    let device = &handles.device;
    let queue = &handles.queue;
    let mut renderer = make_renderer(device);

    let vscene = scene_to_vello(scene);
    let (target, view) = make_target(device);
    renderer
        .render_to_texture(device, queue, &vscene, &view, &render_params())
        .expect("vello render_to_texture");

    let wgpu_device = netrender_device::WgpuDevice::with_external(handles.clone())
        .expect("WgpuDevice::with_external");
    wgpu_device.read_rgba8_texture(&target, DIM, DIM)
}

fn solid_white_image(size: u32) -> ImageData {
    let bytes: Vec<u8> = (0..size * size).flat_map(|_| [255u8, 255, 255, 255]).collect();
    ImageData { width: size, height: size, bytes }
}

/// White image with full red tint `[1.0, 0.0, 0.0, 1.0]` — output
/// should be pure red where the image renders.
#[test]
fn p5b_01_chromatic_red_tint_on_white() {
    let mut scene = Scene::new(DIM, DIM);
    scene.image_sources.insert(IMG_KEY, solid_white_image(16));
    scene.push_image_full(
        16.0, 16.0, 48.0, 48.0,
        [0.0, 0.0, 1.0, 1.0],
        [1.0, 0.0, 0.0, 1.0],
        IMG_KEY,
        0,
        netrender::NO_CLIP,
    );

    let bytes = render_scene(&scene);

    for &(x, y) in &[(20, 20), (32, 32), (44, 44)] {
        assert_within_tol(
            read_pixel(&bytes, x, y),
            [255, 0, 0, 255],
            3,
            &format!("red-tinted at ({}, {})", x, y),
        );
    }
    // Outside image rect: clear.
    assert_within_tol(read_pixel(&bytes, 4, 4), [0, 0, 0, 0], 1, "outside");
}

/// White image with shadow tint `[0.1, 0.1, 0.1, 0.5]` (premultiplied).
/// Decomposes into alpha factor 0.5 and chromatic factor (0.2, 0.2,
/// 0.2). Output: 50%-alpha gray. Storage convention is straight-alpha,
/// so storage value = (0.2*255, 0.2*255, 0.2*255, 0.5*255) = (51, 51,
/// 51, 128).
#[test]
fn p5b_02_chromatic_gray_with_alpha() {
    let mut scene = Scene::new(DIM, DIM);
    scene.image_sources.insert(IMG_KEY, solid_white_image(16));
    scene.push_image_full(
        16.0, 16.0, 48.0, 48.0,
        [0.0, 0.0, 1.0, 1.0],
        [0.1, 0.1, 0.1, 0.5],
        IMG_KEY,
        0,
        netrender::NO_CLIP,
    );

    let bytes = render_scene(&scene);

    for &(x, y) in &[(24, 24), (32, 32), (40, 40)] {
        assert_within_tol(
            read_pixel(&bytes, x, y),
            [51, 51, 51, 128],
            3,
            &format!("gray-shadow tint at ({}, {})", x, y),
        );
    }
}

/// 16×16 image with a 4-pixel transparent border around an 8×8
/// opaque white center. Apply red tint. SrcAtop should keep the
/// border transparent and tint only the center.
#[test]
fn p5b_03_chromatic_preserves_image_alpha() {
    const SZ: u32 = 16;
    let mut bytes = Vec::with_capacity((SZ * SZ * 4) as usize);
    for y in 0..SZ {
        for x in 0..SZ {
            let inside = x >= 4 && x < 12 && y >= 4 && y < 12;
            let pixel = if inside {
                [255u8, 255, 255, 255]
            } else {
                [0u8, 0, 0, 0]
            };
            bytes.extend_from_slice(&pixel);
        }
    }
    let img = ImageData { width: SZ, height: SZ, bytes };

    let mut scene = Scene::new(DIM, DIM);
    scene.image_sources.insert(IMG_KEY, img);
    // Stretch 16×16 → 32×32 at (16,16)–(48,48). The image's
    // 8×8-opaque center maps to 16×16 at (24,24)–(40,40).
    scene.push_image_full(
        16.0, 16.0, 48.0, 48.0,
        [0.0, 0.0, 1.0, 1.0],
        [1.0, 0.0, 0.0, 1.0],
        IMG_KEY,
        0,
        netrender::NO_CLIP,
    );

    let bytes_out = render_scene(&scene);

    // Center of opaque region — fully red.
    assert_within_tol(
        read_pixel(&bytes_out, 32, 32),
        [255, 0, 0, 255],
        4,
        "tinted center",
    );
    // Far inside what would be the image's transparent border
    // (just inside the 32×32 target rect, but in the image's
    // transparent region). Bilinear sampling at (18, 18) in target
    // space → (1, 1) in image space, which is in the transparent
    // border → output should be transparent.
    let border = read_pixel(&bytes_out, 18, 18);
    assert!(
        border[3] < 32,
        "image transparent border at (18, 18) should not be tinted: {:?}",
        border
    );
}
