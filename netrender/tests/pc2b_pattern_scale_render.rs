/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Pattern scaling render receipt (the CSS `background-size` path).
//!
//! `SceneOp::Pattern.scale` is `[sx, sy]` — the per-axis tile multiplier
//! (a tile spans `image_w * sx` by `image_h * sy`). This GPU-readback test
//! pins the actual rasterized geometry: an 8×32 two-band image scaled
//! `[2, 2]` must tile as 16×64, so a single tile in a 16×64 extent puts the
//! band boundary at y≈32, NOT y≈16. (The pre-fix translator dropped scale and
//! tiled at native size; the bug was invisible to the CPU-only pc2 tests.)

use netrender::scene::{ScenePattern, Transform};
use netrender::{ImageData, Scene, SceneOp, boot, vello_rasterizer::scene_to_vello, NO_CLIP};
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, peniko::Color};

const DIM: u32 = 96;

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
        label: Some("pc2b pattern target"),
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
        label: Some("pc2b storage view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, view)
}

fn render_scene(scene: &Scene) -> Vec<u8> {
    let handles = boot().expect("wgpu boot");
    let device = &handles.device;
    let queue = &handles.queue;
    let mut renderer = make_renderer(device);
    let vscene = scene_to_vello(scene);
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
    wgpu_device.read_rgba8_texture(&target, DIM, DIM)
}

fn px(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * DIM + x) * 4) as usize;
    [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
}

/// 8×32 image: top half (y 0..16) lime, bottom half (y 16..32) aqua.
fn two_band_image() -> ImageData {
    let (w, h) = (8u32, 32u32);
    let mut bytes = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for _x in 0..w {
            let p: [u8; 4] = if y < h / 2 { [0, 255, 0, 255] } else { [0, 255, 255, 255] };
            bytes.extend_from_slice(&p);
        }
    }
    ImageData::from_bytes(w, h, bytes)
}

const KEY: u64 = 0x01;

/// Scale `[2, 2]` over the 8×32 image → a 16×64 tile. One tile fills the
/// 16×64 extent at the origin: lime in y 0..32, aqua in y 32..64. The band
/// boundary at y≈32 (not y≈16) is the proof the per-axis scale is honored.
#[test]
fn pc2b_scaled_pattern_tile_is_image_times_scale() {
    let mut scene = Scene::new(DIM, DIM);
    scene.image_sources.insert(KEY, two_band_image());
    scene.ops.push(SceneOp::Pattern(ScenePattern {
        tile: KEY,
        extent: [0.0, 0.0, 16.0, 64.0],
        scale: [2.0, 2.0],
        transform_id: 0,
        clip_rect: NO_CLIP,
        clip_corner_radii: [0.0; 4],
        nearest: false,
    }));

    let b = render_scene(&scene);

    // Top band (lime) at y=8 and y=24 — both inside the 0..32 lime half.
    for y in [8, 24] {
        let p = px(&b, 8, y);
        assert!(p[0] < 60 && p[1] > 180 && p[2] < 60, "lime at y={y}, got {p:?}");
    }
    // Bottom band (aqua) at y=40 and y=56 — inside the 32..64 aqua half.
    for y in [40, 56] {
        let p = px(&b, 8, y);
        assert!(p[0] < 60 && p[1] > 180 && p[2] > 180, "aqua at y={y}, got {p:?}");
    }
}

/// The same scaled pattern under a non-identity `world` transform (a
/// translate, as genet emits for a positioned box). Reproduces the genet
/// reftest exactly: extent is box-local `[0,0,16,64]`, `world` translates it
/// down by 20. The tile must still be 16×64 (band boundary at world y≈52),
/// proving `world` doesn't corrupt the per-axis brush scale.
#[test]
fn pc2b_scaled_pattern_under_world_translate() {
    let mut scene = Scene::new(DIM, DIM);
    scene.image_sources.insert(KEY, two_band_image());
    let tid = scene.push_transform(Transform::translate_2d(0.0, 20.0));
    scene.ops.push(SceneOp::Pattern(ScenePattern {
        tile: KEY,
        extent: [0.0, 0.0, 16.0, 64.0],
        scale: [2.0, 2.0],
        transform_id: tid,
        clip_rect: NO_CLIP,
        clip_corner_radii: [0.0; 4],
        nearest: false,
    }));

    let b = render_scene(&scene);

    // world y = local y + 20. Lime half local 0..32 → world 20..52; aqua 52..84.
    for y in [28, 44] {
        let p = px(&b, 8, y);
        assert!(p[0] < 60 && p[1] > 180 && p[2] < 60, "lime at world y={y}, got {p:?}");
    }
    for y in [60, 76] {
        let p = px(&b, 8, y);
        assert!(p[0] < 60 && p[1] > 180 && p[2] > 180, "aqua at world y={y}, got {p:?}");
    }
}
