/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 5c' — `register_texture` (Path B) image source.
//!
//! Receipt that GPU-resident textures (the typical render-graph
//! output shape: blur result, mask texture, etc.) can be used as
//! image sources for a vello scene without going through CPU bytes.
//!
//! - `p5c_01_register_texture_round_trip` — upload a known pattern
//!   to a wgpu::Texture, register it with vello, draw it through a
//!   netrender Scene, verify the pattern survives.

use std::collections::HashMap;

use netrender::{
    ImageKey, Scene, boot, vello_rasterizer::scene_to_vello_with_overrides,
};
use vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions,
    peniko::Color,
};

const DIM: u32 = 64;
const SRC_DIM: u32 = 16;
const KEY: ImageKey = 0xFEED;

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
        label: Some("p5c' target"),
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
        label: Some("p5c' storage view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, view)
}

fn upload_source_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bytes: &[u8],
) -> wgpu::Texture {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p5c' source"),
        size: wgpu::Extent3d {
            width: SRC_DIM,
            height: SRC_DIM,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SRC_DIM * 4),
            rows_per_image: Some(SRC_DIM),
        },
        wgpu::Extent3d {
            width: SRC_DIM,
            height: SRC_DIM,
            depth_or_array_layers: 1,
        },
    );
    tex
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

/// Upload a 16×16 four-quadrant texture (red TL, green TR, blue BL,
/// yellow BR), register it with vello, draw it through scene_to_vello
/// stretched to a 32×32 target, verify each quadrant lands in the
/// expected output region.
#[test]
fn p5c_01_register_texture_round_trip() {
    let handles = boot().expect("wgpu boot");
    let device = &handles.device;
    let queue = &handles.queue;

    // Build the source pattern.
    let half = SRC_DIM / 2;
    let mut bytes = Vec::with_capacity((SRC_DIM * SRC_DIM * 4) as usize);
    for y in 0..SRC_DIM {
        for x in 0..SRC_DIM {
            let pixel: [u8; 4] = match (x < half, y < half) {
                (true, true) => [255, 0, 0, 255],
                (false, true) => [0, 255, 0, 255],
                (true, false) => [0, 0, 255, 255],
                (false, false) => [255, 255, 0, 255],
            };
            bytes.extend_from_slice(&pixel);
        }
    }
    let source_tex = upload_source_texture(device, queue, &bytes);

    let mut renderer = make_renderer(device);
    // Register the GPU texture with vello — Path B per §3.5.
    let registered = renderer.register_texture(source_tex);

    // Build a netrender Scene that references the same key.
    // image_sources is INTENTIONALLY empty: no CPU bytes for this
    // image. The override map below resolves the key to the
    // registered ImageData.
    let mut scene = Scene::new(DIM, DIM);
    scene.push_image_full(
        16.0, 16.0, 48.0, 48.0,
        [0.0, 0.0, 1.0, 1.0],
        [1.0, 1.0, 1.0, 1.0],
        KEY,
        0,
        netrender::NO_CLIP,
    );

    let mut overrides = HashMap::new();
    overrides.insert(KEY, registered);
    let vscene = scene_to_vello_with_overrides(&scene, &overrides);

    let (target, view) = make_target(device);
    renderer
        .render_to_texture(
            device,
            queue,
            &vscene,
            &view,
            &RenderParams {
                base_color: Color::from_rgba8(0, 0, 0, 0),
                width: DIM,
                height: DIM,
                antialiasing_method: AaConfig::Area,
            },
        )
        .expect("vello render_to_texture");

    let wgpu_device = netrender_device::WgpuDevice::with_external(handles.clone())
        .expect("WgpuDevice::with_external");
    let bytes = wgpu_device.read_rgba8_texture(&target, DIM, DIM);

    // Each output quadrant should hold its source color (matching
    // p5prime_01's UV-stretch pattern).
    assert_within_tol(read_pixel(&bytes, 20, 20), [255, 0, 0, 255], 4, "TL red");
    assert_within_tol(read_pixel(&bytes, 44, 20), [0, 255, 0, 255], 4, "TR green");
    assert_within_tol(read_pixel(&bytes, 20, 44), [0, 0, 255, 255], 4, "BL blue");
    assert_within_tol(read_pixel(&bytes, 44, 44), [255, 255, 0, 255], 4, "BR yellow");
    // Outside target: clear.
    assert_within_tol(read_pixel(&bytes, 4, 4), [0, 0, 0, 0], 1, "outside");
}
