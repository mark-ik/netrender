/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 7' integration receipts — `Renderer::render_vello`.
//!
//! These tests prove the end-to-end wiring: constructing a
//! `Renderer` with `enable_vello: true` and `tile_cache_size:
//! Some(_)`, then calling `render_vello(&scene, &target_view)` to
//! drive the full vello pipeline through the public Renderer API.
//!
//! The receipts re-green the same simple p2 oracles that
//! `p1prime_oracle_regreen.rs` already exercises against the bare
//! `scene_to_vello` translator — but here the path goes through
//! `create_netrender_instance` + `Renderer::render_vello`, the
//! shape an embedder would use.

use std::path::{Path, PathBuf};

use netrender::{NetrenderOptions, Scene, boot, create_netrender_instance};

const DIM: u32 = 256;
const TILE_SIZE: u32 = 64;

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

fn make_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p7' integration target"),
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
        label: Some("p7' integration storage view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, view)
}

fn assert_matches_oracle(name: &str, actual: &[u8]) {
    let oracle_path = oracle_dir().join(format!("{name}.png"));
    let (ow, oh, oracle) = read_png(&oracle_path);
    assert_eq!((ow, oh), (DIM, DIM), "{name}: oracle size mismatch");
    assert_eq!(actual.len(), oracle.len(), "{name}: readback length mismatch");
    let mut diff_count = 0usize;
    let mut max_diff = 0u8;
    for (i, (a, b)) in actual.iter().zip(oracle.iter()).enumerate() {
        let d = (*a as i16 - *b as i16).unsigned_abs() as u8;
        if d > 0 {
            diff_count += 1;
            if diff_count < 4 {
                let pi = i / 4;
                let (x, y) = (pi as u32 % ow, pi as u32 / ow);
                let chan = ['R', 'G', 'B', 'A'][i % 4];
                eprintln!("  {} ({}, {}): {} actual {} oracle {}", name, x, y, chan, *a, *b);
            }
        }
        max_diff = max_diff.max(d);
    }
    assert_eq!(
        diff_count, 0,
        "{name}: {diff_count} channel values differ from oracle (max diff = {max_diff})"
    );
}

fn render_through_renderer(scene: &Scene) -> Vec<u8> {
    let handles = boot().expect("wgpu boot");
    let device = handles.device.clone();
    let renderer = create_netrender_instance(
        handles.clone(),
        NetrenderOptions {
            tile_cache_size: Some(TILE_SIZE),
            enable_vello: true,
        },
    )
    .expect("create_netrender_instance");

    let (target, view) = make_target(&device);
    renderer.render_vello(scene, &view);

    renderer
        .wgpu_device
        .read_rgba8_texture(&target, DIM, DIM)
}

/// Full-canvas opaque red rect, byte-exact match against the
/// existing batched-pipeline oracle.
#[test]
fn p7prime_renderer_p2_01_solid_red() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 0.0, 0.0, 1.0]);
    let actual = render_through_renderer(&scene);
    assert_matches_oracle("p2_01_solid_red", &actual);
}

/// Red full-canvas with a 128×128 blue overlay.
#[test]
fn p7prime_renderer_p2_04_blue_over_red() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 0.0, 0.0, 1.0]);
    scene.push_rect(0.0, 0.0, 128.0, 128.0, [0.0, 0.0, 1.0, 1.0]);
    let actual = render_through_renderer(&scene);
    assert_matches_oracle("p2_04_blue_over_red", &actual);
}

/// Four-quadrants. Tile boundaries fall ON the rect boundaries
/// (256 / 64 = 4 tiles per side; rects are at multiples of 128 = 2
/// tiles). Per-tile clip layers correctly preserve the painter
/// order and rect colors.
#[test]
fn p7prime_renderer_p2_05_four_quadrants() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_rect(0.0, 0.0, 128.0, 128.0, [1.0, 0.0, 0.0, 1.0]);
    scene.push_rect(128.0, 0.0, 256.0, 128.0, [0.0, 1.0, 0.0, 1.0]);
    scene.push_rect(0.0, 128.0, 128.0, 256.0, [0.0, 0.0, 1.0, 1.0]);
    scene.push_rect(128.0, 128.0, 256.0, 256.0, [1.0, 1.0, 0.0, 1.0]);
    let actual = render_through_renderer(&scene);
    assert_matches_oracle("p2_05_four_quadrants", &actual);
}

/// Two-frame test: render, then render again with the same scene.
/// Second frame must match first frame byte-exactly AND report 0
/// dirty tiles (the rasterizer is reused across frames via the
/// shared tile cache).
#[test]
fn p7prime_renderer_two_frames_share_state() {
    let handles = boot().expect("wgpu boot");
    let device = handles.device.clone();
    let renderer = create_netrender_instance(
        handles.clone(),
        NetrenderOptions {
            tile_cache_size: Some(TILE_SIZE),
            enable_vello: true,
        },
    )
    .expect("create_netrender_instance");

    let mut scene = Scene::new(DIM, DIM);
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [0.0, 1.0, 0.0, 1.0]);

    let (tex_a, view_a) = make_target(&device);
    renderer.render_vello(&scene, &view_a);
    let bytes_a = renderer.wgpu_device.read_rgba8_texture(&tex_a, DIM, DIM);

    let (tex_b, view_b) = make_target(&device);
    renderer.render_vello(&scene, &view_b);
    let bytes_b = renderer.wgpu_device.read_rgba8_texture(&tex_b, DIM, DIM);

    assert_eq!(bytes_a, bytes_b, "second-frame output must match first");

    // Confirm the tile cache shared across the two render_vello
    // calls reported zero dirty tiles on the second frame.
    assert_eq!(
        renderer.vello_last_dirty_count(),
        Some(0),
        "second frame: zero dirty tiles"
    );
}
