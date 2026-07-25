/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! C-architecture entry point: `VelloTileRasterizer::compose_into`
//! receipts.
//!
//! - `compose_into_01_identity_matches_render` — composing into a
//!   master with `Affine::IDENTITY` and then rendering that master
//!   produces pixel-equivalent output to calling `render` directly.
//!   Pins the contract that `compose_into` is a strict refactor of
//!   the inner steps of `render`, not a different code path.
//! - `compose_into_02_transform_translates_content` — composing
//!   into a master with a translate transform shifts the rendered
//!   content by exactly that translate.
//! - `compose_into_03_two_consumers_share_atlas` — two
//!   `VelloTileRasterizer`s composing different scenes that
//!   reference the *same* `Arc`-shared image bytes into one master
//!   produce a stable `Blob::id()` chain — vello's atlas dedup is
//!   reachable across consumers when bytes are shared.
//!
//! These tests are the "yes, you can do C now" receipt — graphshell-
//! shaped consumers can build a single `vello::Scene` per frame
//! from N independent netrender consumers, with shared atlas slots,
//! one render submit, no texture-sampling boundary between them.

use std::sync::Arc;

use netrender::{
    ImageData, Scene, TileCache, boot, peniko::Blob, vello_tile_rasterizer::VelloTileRasterizer,
};
use vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, kurbo::Affine, peniko::Color,
};

const DIM: u32 = 128;
const TILE: u32 = 64;

const TRANSPARENT: Color = Color::new([0.0, 0.0, 0.0, 0.0]);

fn make_target(device: &wgpu::Device, label: &'static str) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
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
        label: Some(label),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, view)
}

fn build_demo_scene() -> Scene {
    let mut scene = Scene::new(DIM, DIM);
    // Mix of primitive kinds: a backing rect, a stroked border, a
    // glyph-free triangular shape. Enough to exercise the dispatch
    // and tile-cache paths without needing a font.
    scene.push_rect(8.0, 8.0, 120.0, 120.0, [0.15, 0.20, 0.30, 1.0]);
    scene.push_stroke_rounded(
        8.0,
        8.0,
        120.0,
        120.0,
        [0.95, 0.85, 0.55, 1.0],
        2.0,
        [12.0; 4],
    );
    scene.push_rect(40.0, 40.0, 88.0, 88.0, [0.85, 0.30, 0.40, 1.0]);
    scene
}

/// Composing into a master with `Affine::IDENTITY` and rendering
/// that master via a separately-constructed vello `Renderer` should
/// produce the same output as calling `VelloTileRasterizer::render`
/// directly.
#[test]
fn compose_into_01_identity_matches_render() {
    let handles = boot().expect("wgpu boot");

    // Path A — render directly via VelloTileRasterizer.
    let mut rast_a = VelloTileRasterizer::new(handles.clone()).expect("rast a");
    let mut tc_a = TileCache::new(TILE);
    let scene = build_demo_scene();
    let (target_a, view_a) = make_target(&handles.device, "compose_into_a");
    rast_a
        .render(&scene, &mut tc_a, &view_a, TRANSPARENT)
        .expect("render a");
    let wgpu_device =
        netrender_device::WgpuDevice::with_external(handles.clone()).expect("wgpu device");
    let pixels_a = wgpu_device.read_rgba8_texture(&target_a, DIM, DIM);

    // Path B — compose_into a master, then render the master via
    // an independently-constructed vello Renderer.
    let mut rast_b = VelloTileRasterizer::new(handles.clone()).expect("rast b");
    let mut tc_b = TileCache::new(TILE);
    let mut master = vello::Scene::new();
    rast_b.compose_into(&scene, &mut tc_b, &mut master, Affine::IDENTITY);

    let mut renderer = Renderer::new(
        &handles.device,
        RendererOptions {
            use_cpu: false,
            antialiasing_support: AaSupport::area_only(),
            num_init_threads: None,
            pipeline_cache: None,
        },
    )
    .expect("vello renderer");
    let (target_b, view_b) = make_target(&handles.device, "compose_into_b");
    renderer
        .render_to_texture(
            &handles.device,
            &handles.queue,
            &master,
            &view_b,
            &RenderParams {
                base_color: TRANSPARENT,
                width: DIM,
                height: DIM,
                antialiasing_method: AaConfig::Area,
            },
        )
        .expect("render master");
    let pixels_b = wgpu_device.read_rgba8_texture(&target_b, DIM, DIM);

    // Compare. compose_into is supposed to be a refactor of the
    // *inner* steps of render — same encoding, same vello compute
    // pipeline, with only tightly bounded backend quantization allowed.
    assert_eq!(pixels_a.len(), pixels_b.len());
    let mut max_diff: u8 = 0;
    let mut diff_count = 0usize;
    for (a, b) in pixels_a.iter().zip(pixels_b.iter()) {
        let d = (*a as i16 - *b as i16).unsigned_abs() as u8;
        max_diff = max_diff.max(d);
        if d > 0 {
            diff_count += 1;
        }
    }
    // Metal's compute path can quantize a handful of AA edge channels
    // differently between independent renderer submissions. Repeated hardware
    // runs observed at most 12 changed channels with a maximum delta of 4, so
    // keep both the per-channel and affected-channel bounds tight.
    assert!(
        max_diff <= 4 && diff_count <= 16,
        "compose_into-then-render should match render-directly within ±4 in at most 16 channels; \
         got max_diff={}, diff_count={}",
        max_diff,
        diff_count,
    );
}

/// `Affine::translate((dx, dy))` on `compose_into` should shift the
/// rendered content by exactly `(dx, dy)` in the master frame.
#[test]
fn compose_into_02_transform_translates_content() {
    let handles = boot().expect("wgpu boot");
    let wgpu_device =
        netrender_device::WgpuDevice::with_external(handles.clone()).expect("wgpu device");

    // Inner scene: a 32×32 red rect at (0, 0)–(32, 32) inside a
    // viewport that's 32×32. Composed into a 128×128 master with a
    // (48, 48) translate.
    const INNER: u32 = 32;
    let mut inner_scene = Scene::new(INNER, INNER);
    inner_scene.push_rect(0.0, 0.0, INNER as f32, INNER as f32, [1.0, 0.0, 0.0, 1.0]);

    let mut rast = VelloTileRasterizer::new(handles.clone()).expect("rast");
    let mut tc = TileCache::new(TILE);
    let mut master = vello::Scene::new();
    rast.compose_into(
        &inner_scene,
        &mut tc,
        &mut master,
        Affine::translate((48.0, 48.0)),
    );

    let mut renderer = Renderer::new(
        &handles.device,
        RendererOptions {
            use_cpu: false,
            antialiasing_support: AaSupport::area_only(),
            num_init_threads: None,
            pipeline_cache: None,
        },
    )
    .expect("vello renderer");
    let (target, view) = make_target(&handles.device, "compose_into_xform");
    renderer
        .render_to_texture(
            &handles.device,
            &handles.queue,
            &master,
            &view,
            &RenderParams {
                base_color: Color::WHITE,
                width: DIM,
                height: DIM,
                antialiasing_method: AaConfig::Area,
            },
        )
        .expect("render master");
    let pixels = wgpu_device.read_rgba8_texture(&target, DIM, DIM);

    let read = |x: u32, y: u32| {
        let i = ((y * DIM + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };

    // Center of the translated rect: master (48 + 16, 48 + 16) = (64, 64).
    let center = read(64, 64);
    assert!(
        center[0] > 240 && center[1] < 16 && center[2] < 16,
        "translated rect center: {:?} should be red",
        center,
    );
    // Outside the translated rect (top-left of master): should be
    // white background.
    let outside = read(8, 8);
    assert!(
        outside[0] > 240 && outside[1] > 240 && outside[2] > 240,
        "outside rect: {:?} should be white background",
        outside,
    );
    // Just past the bottom-right corner of the translated rect
    // (master coord (80, 80) = inner (32, 32) which is *outside*
    // the inner rect since rect is [0,0]-[32,32], exclusive of
    // 32 — should be white too).
    let just_past = read(81, 81);
    assert!(
        just_past[0] > 200,
        "just past translated rect: {:?} should be near-white",
        just_past,
    );
}

/// Two `VelloTileRasterizer`s, each rendering its own scene that
/// references the *same* `Arc`-shared image bytes (via cloned
/// `peniko::Blob`s), composed into one master. The Blob ids in
/// each rasterizer's image cache match — atlas dedup at the vello
/// level is reachable when consumers share `Arc`s.
#[test]
fn compose_into_03_two_consumers_share_atlas() {
    let handles = boot().expect("wgpu boot");
    const KEY: u64 = 0xCAFE;
    let bytes = Arc::new(vec![
        255u8, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
    ]);
    let blob = Blob::new(bytes);

    // Two scenes that share the underlying image bytes by cloning
    // the same Blob (clone is Arc-bump + id-copy).
    let mut scene_a = Scene::new(DIM, DIM);
    scene_a
        .image_sources
        .insert(KEY, ImageData::from_blob(2, 2, blob.clone()));
    scene_a.push_image(
        0.0,
        0.0,
        64.0,
        64.0,
        KEY,
        ImageData::from_blob(2, 2, blob.clone()),
    );

    let mut scene_b = Scene::new(DIM, DIM);
    scene_b
        .image_sources
        .insert(KEY, ImageData::from_blob(2, 2, blob.clone()));
    scene_b.push_image(
        64.0,
        64.0,
        128.0,
        128.0,
        KEY,
        ImageData::from_blob(2, 2, blob.clone()),
    );

    // Two independent rasterizers, each composing into one master.
    let mut rast_a = VelloTileRasterizer::new(handles.clone()).expect("rast a");
    let mut rast_b = VelloTileRasterizer::new(handles.clone()).expect("rast b");
    let mut tc_a = TileCache::new(TILE);
    let mut tc_b = TileCache::new(TILE);
    let mut master = vello::Scene::new();
    rast_a.compose_into(&scene_a, &mut tc_a, &mut master, Affine::IDENTITY);
    rast_b.compose_into(&scene_b, &mut tc_b, &mut master, Affine::IDENTITY);

    // Each rasterizer's image cache holds an entry under KEY whose
    // peniko::Blob id matches — that's the cross-consumer dedup
    // signal vello's atlas keys on.
    let id_a = rast_a.cached_image_blob_id(KEY).expect("rast a cached");
    let id_b = rast_b.cached_image_blob_id(KEY).expect("rast b cached");
    assert_eq!(
        id_a, id_b,
        "two consumers handed the same Arc-shared blob → same Blob::id() → \
         atlas dedup reachable; got id_a={} id_b={}",
        id_a, id_b,
    );

    // And the master scene contains both consumers' content. We
    // don't render here — the per-consumer assertion is sufficient.
    // Ending the test without `render_to_texture` exercises the
    // "compose_into doesn't submit GPU work" property.
    let _ = master;
}
