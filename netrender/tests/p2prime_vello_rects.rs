/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 2' — netrender Scene → vello::Scene rect ingestion receipt.
//!
//! Smallest possible end-to-end test that proves
//! `vello_rasterizer::scene_to_vello` produces a `vello::Scene` whose
//! `render_to_texture` output matches what we'd expect from the input
//! `netrender::Scene`.
//!
//! Three probes, each covering one Phase 2' axis:
//!
//! - `p2prime_01_two_rects_painter_order` — overlapping rects in
//!   painter order; later rect must paint on top.
//! - `p2prime_02_transformed_rect` — `Transform::translate_2d`
//!   correctly offsets a rect's local-space coords.
//! - `p2prime_03_clipped_rect` — `clip_rect` axis-aligned clip
//!   correctly masks the painted region.

use netrender::{Scene, Transform, boot, vello_rasterizer::scene_to_vello};
use vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions,
    peniko::Color,
};

const DIM: u32 = 64;

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
        label: Some("p2' rect target"),
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
        label: Some("p2' rect storage view"),
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

fn channel_diff(a: u8, b: u8) -> u8 {
    (a as i16 - b as i16).unsigned_abs() as u8
}

#[track_caller]
fn assert_within_tol(actual: [u8; 4], expected: [u8; 4], tol: u8, where_: &str) {
    let max = (0..4)
        .map(|i| channel_diff(actual[i], expected[i]))
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

/// Two opaque rects: red 32×32 at (0, 0)–(32, 32), green 32×32 at
/// (16, 16)–(48, 48). The overlap region must be green (later in
/// painter order = on top).
#[test]
fn p2prime_01_two_rects_painter_order() {
    let mut scene = Scene::new(DIM, DIM);
    // Premultiplied opaque red and green.
    scene.push_rect(0.0, 0.0, 32.0, 32.0, [1.0, 0.0, 0.0, 1.0]);
    scene.push_rect(16.0, 16.0, 48.0, 48.0, [0.0, 1.0, 0.0, 1.0]);

    let bytes = render_scene(&scene);

    // Red-only region (top-left interior of the first rect, outside
    // the green rect's footprint).
    assert_within_tol(
        read_pixel(&bytes, 8, 8),
        [255, 0, 0, 255],
        2,
        "red-only at (8, 8)",
    );
    // Overlap region — green wins (painter order, later = on top).
    assert_within_tol(
        read_pixel(&bytes, 24, 24),
        [0, 255, 0, 255],
        2,
        "overlap at (24, 24) = green",
    );
    // Green-only region (lower-right interior of green rect, outside red).
    assert_within_tol(
        read_pixel(&bytes, 40, 40),
        [0, 255, 0, 255],
        2,
        "green-only at (40, 40)",
    );
    // Outside both rects: clear (transparent).
    assert_within_tol(
        read_pixel(&bytes, 56, 56),
        [0, 0, 0, 0],
        1,
        "clear at (56, 56)",
    );
}

/// Single rect at local-space (0, 0)–(16, 16) with a translate
/// transform of (24, 24). Output must show the rect at device-space
/// (24, 24)–(40, 40).
#[test]
fn p2prime_02_transformed_rect() {
    let mut scene = Scene::new(DIM, DIM);
    let xform = scene.push_transform(Transform::translate_2d(24.0, 24.0));
    scene.push_rect_transformed(0.0, 0.0, 16.0, 16.0, [0.0, 0.0, 1.0, 1.0], xform);

    let bytes = render_scene(&scene);

    // Inside the device-space rect (24, 24)–(40, 40).
    assert_within_tol(
        read_pixel(&bytes, 32, 32),
        [0, 0, 255, 255],
        2,
        "inside translated rect at (32, 32)",
    );
    // Outside (the rect did NOT land at the local-space coords).
    assert_within_tol(
        read_pixel(&bytes, 8, 8),
        [0, 0, 0, 0],
        1,
        "clear at (8, 8) — would be hit if transform were ignored",
    );
    // Just outside the bottom-right corner of the translated rect.
    assert_within_tol(
        read_pixel(&bytes, 48, 48),
        [0, 0, 0, 0],
        1,
        "clear at (48, 48) — outside translated rect",
    );
}

/// Single full-canvas rect with a clip_rect restricting paint to
/// (16, 16)–(48, 48). Paint inside clip = colored; paint outside = clear.
#[test]
fn p2prime_03_clipped_rect() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_rect_clipped(
        0.0,
        0.0,
        DIM as f32,
        DIM as f32,
        [1.0, 1.0, 0.0, 1.0],
        0,
        [16.0, 16.0, 48.0, 48.0],
    );

    let bytes = render_scene(&scene);

    // Inside the clip — yellow.
    assert_within_tol(
        read_pixel(&bytes, 32, 32),
        [255, 255, 0, 255],
        2,
        "inside clip at (32, 32)",
    );
    // Outside the clip — clear (clip masks the paint).
    assert_within_tol(
        read_pixel(&bytes, 8, 8),
        [0, 0, 0, 0],
        1,
        "outside clip at (8, 8)",
    );
    assert_within_tol(
        read_pixel(&bytes, 56, 56),
        [0, 0, 0, 0],
        1,
        "outside clip at (56, 56)",
    );
}
