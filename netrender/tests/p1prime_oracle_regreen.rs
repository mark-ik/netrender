/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 1' completion — re-green selected existing oracles through
//! the vello path.
//!
//! The Phase 1' goal in the rasterizer plan §12 says: "Receipt:
//! oracle smoke green through `VelloRasterizer`." This file delivers
//! that receipt against the simplest existing oracles: full-canvas
//! opaque rects from the Phase 2 set, where the existing pipeline
//! and vello converge byte-exactly (no alpha blend, no AA on
//! axis-aligned rects).
//!
//! The PNGs in `tests/oracle/p2/` were captured from the existing
//! batched WGSL pipeline, which renders to `Rgba8UnormSrgb` and
//! sRGB-encodes-on-store at the hardware level. The vello path
//! renders to `Rgba8Unorm` storage with sRGB-encoded bytes inside
//! (per p1prime_01-03 receipts). For **opaque primary-color rects**
//! the output bytes coincide: sRGB(1.0) at endpoints is identity,
//! and no alpha blending takes place. Mid-tone alpha cases would
//! diverge (linear-light blend vs sRGB-encoded blend) and are not
//! covered here — those are Phase 7' / 9' work.

use std::path::{Path, PathBuf};

use netrender::{Scene, boot, vello_rasterizer::scene_to_vello};
use vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions,
    peniko::Color,
};

const DIM: u32 = 256;

fn oracle_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("oracle")
        .join("p2")
}

fn read_png(path: &Path) -> (u32, u32, Vec<u8>) {
    let file = std::fs::File::open(path)
        .unwrap_or_else(|e| panic!("opening {}: {}", path.display(), e));
    let dec = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = dec.read_info().expect("png read_info");
    let info = reader.info();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    let (w, h) = (info.width, info.height);
    let mut buf = vec![0u8; reader.output_buffer_size()];
    reader.next_frame(&mut buf).expect("png decode");
    (w, h, buf)
}

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

fn make_target(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p1' regreen target"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
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
        label: Some("p1' regreen storage view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, view)
}

fn render_scene_through_vello(scene: &Scene) -> Vec<u8> {
    let handles = boot().expect("wgpu boot");
    let device = &handles.device;
    let queue = &handles.queue;
    let mut renderer = make_renderer(device);

    let vscene = scene_to_vello(scene);
    let (target, view) = make_target(device, scene.viewport_width, scene.viewport_height);
    renderer
        .render_to_texture(
            device,
            queue,
            &vscene,
            &view,
            &RenderParams {
                base_color: Color::from_rgba8(0, 0, 0, 0),
                width: scene.viewport_width,
                height: scene.viewport_height,
                antialiasing_method: AaConfig::Area,
            },
        )
        .expect("vello render_to_texture");

    let wgpu_device = netrender_device::WgpuDevice::with_external(handles.clone())
        .expect("WgpuDevice::with_external");
    wgpu_device.read_rgba8_texture(&target, scene.viewport_width, scene.viewport_height)
}

/// Diff against an existing oracle PNG and count pixels that differ
/// by more than `tol` on any channel. Within-tol mismatches are
/// counted but not failures — the existing oracles were captured
/// from the batched WGSL path, which can differ from vello in the
/// last bit on edge pixels even for nominally identical scenes.
fn assert_matches_oracle(name: &str, actual: &[u8], tol: u8) {
    let oracle_path = oracle_dir().join(format!("{name}.png"));
    let (ow, oh, oracle) = read_png(&oracle_path);
    assert_eq!((ow as usize) * (oh as usize) * 4, actual.len(), "{name}: size mismatch");
    let mut max_diff: u8 = 0;
    let mut over_tol = 0usize;
    for (i, (a, b)) in actual.iter().zip(oracle.iter()).enumerate() {
        let d = (*a as i16 - *b as i16).unsigned_abs() as u8;
        if d > tol {
            over_tol += 1;
            if over_tol < 4 {
                let pi = i / 4;
                let (x, y) = (pi as u32 % ow, pi as u32 / ow);
                let chan = ['R', 'G', 'B', 'A'][i % 4];
                eprintln!(
                    "  {} ({}, {}): {chan} actual {} oracle {} (diff {})",
                    name, x, y, *a, *b, d
                );
            }
        }
        max_diff = max_diff.max(d);
    }
    assert_eq!(
        over_tol, 0,
        "{name}: {over_tol} channel values differ from oracle by >{tol} (max diff = {max_diff})"
    );
}

/// Single full-canvas opaque red rect. Both pipelines produce
/// (255, 0, 0, 255) in every pixel.
#[test]
fn p1prime_oracle_p2_01_solid_red() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 0.0, 0.0, 1.0]);
    let actual = render_scene_through_vello(&scene);
    assert_matches_oracle("p2_01_solid_red", &actual, 0);
}

/// Single full-canvas opaque blue rect.
#[test]
fn p1prime_oracle_p2_02_solid_blue() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [0.0, 0.0, 1.0, 1.0]);
    let actual = render_scene_through_vello(&scene);
    assert_matches_oracle("p2_02_solid_blue", &actual, 0);
}

/// Single full-canvas opaque white rect.
#[test]
fn p1prime_oracle_p2_03_solid_white() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 1.0, 1.0, 1.0]);
    let actual = render_scene_through_vello(&scene);
    assert_matches_oracle("p2_03_solid_white", &actual, 0);
}

/// Red full-canvas with a 128×128 blue overlay in the top-left.
/// Both rects opaque; blue simply overwrites red where they overlap.
/// No alpha blend happens so byte-exact match is expected.
#[test]
fn p1prime_oracle_p2_04_blue_over_red() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 0.0, 0.0, 1.0]);
    scene.push_rect(0.0, 0.0, 128.0, 128.0, [0.0, 0.0, 1.0, 1.0]);
    let actual = render_scene_through_vello(&scene);
    assert_matches_oracle("p2_04_blue_over_red", &actual, 0);
}

/// Four 128×128 quadrants (red, green, blue, white).
#[test]
fn p1prime_oracle_p2_05_four_quadrants() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_rect(0.0, 0.0, 128.0, 128.0, [1.0, 0.0, 0.0, 1.0]);
    scene.push_rect(128.0, 0.0, 256.0, 128.0, [0.0, 1.0, 0.0, 1.0]);
    scene.push_rect(0.0, 128.0, 128.0, 256.0, [0.0, 0.0, 1.0, 1.0]);
    scene.push_rect(128.0, 128.0, 256.0, 256.0, [1.0, 1.0, 0.0, 1.0]);
    let actual = render_scene_through_vello(&scene);
    assert_matches_oracle("p2_05_four_quadrants", &actual, 0);
}
