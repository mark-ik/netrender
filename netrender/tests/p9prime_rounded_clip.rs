/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 9' — rounded-rect clip via vello-native `push_layer` with a
//! `kurbo::RoundedRect` shape.
//!
//! Replaces the indirection currently used by p9a / p9b (where a
//! rounded-rect coverage mask is rendered through the
//! `clip_rectangle` render-graph task and consumed as a tinted image
//! via `insert_image_vello`). The new `clip_corner_radii: [f32; 4]`
//! field on every Scene primitive lets vello apply the rounded clip
//! natively as a `push_layer` shape.
//!
//! Three probes:
//!   p9prime_01_rect_rounded_clip       — solid rect + rounded clip
//!   p9prime_02_image_rounded_clip      — image + rounded clip
//!   p9prime_03_gradient_rounded_clip   — gradient + rounded clip
//!
//! Each probe verifies four sample points:
//!   - center of the clipped region (full coverage; expected color)
//!   - far outside the clip rect (no coverage; transparent)
//!   - just outside the rounded corner arc (no coverage)
//!   - well inside the rect, away from the corner (full coverage)

use netrender::{ImageData, Scene, boot, vello_rasterizer::scene_to_vello};
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, peniko::Color};

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
        label: Some("p9' rounded-clip target"),
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
        label: Some("p9' storage view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, view)
}

fn read_pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * DIM + x) * 4) as usize;
    [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
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

/// Sanity assertions used by all three probes. The clipped region is
/// (16, 16)–(48, 48) with uniform corner radius 8. Sample points:
///
///   inside_center  (32, 32)  →  full coverage; expects `inside_color`
///   outside_far    (4, 4)    →  outside the clip rect entirely
///   inside_edge    (32, 18)  →  just inside the top edge, away from corners
///   outside_corner (17, 17)  →  inside the rect's bbox but outside the
///                               rounded arc (corner radius 8 means the
///                               arc passes through (24, 16) → (16, 24);
///                               (17, 17) is outside)
fn assert_rounded_clip_pattern(bytes: &[u8], inside_color: [u8; 4]) {
    fn within(actual: [u8; 4], expected: [u8; 4], tol: u8) -> bool {
        actual
            .iter()
            .zip(expected.iter())
            .all(|(a, b)| (*a as i16 - *b as i16).unsigned_abs() <= tol as u16)
    }

    let center = read_pixel(bytes, 32, 32);
    assert!(
        within(center, inside_color, 4),
        "center (32,32) inside clip: got {:?}, expected ~{:?}",
        center,
        inside_color
    );
    let far = read_pixel(bytes, 4, 4);
    assert!(
        far[3] < 8,
        "far (4,4) outside clip rect: got {:?}, expected near-transparent",
        far
    );
    let edge = read_pixel(bytes, 32, 18);
    assert!(
        within(edge, inside_color, 8),
        "inside-edge (32,18): got {:?}, expected ~{:?}",
        edge,
        inside_color
    );
    let corner = read_pixel(bytes, 17, 17);
    assert!(
        corner[3] < 50,
        "outside-arc (17,17) of rounded corner: got {:?}, expected near-transparent (alpha < 50)",
        corner
    );
}

#[test]
fn p9prime_01_rect_rounded_clip() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_rect_clipped_rounded(
        0.0,
        0.0,
        DIM as f32,
        DIM as f32,
        [1.0, 0.0, 0.0, 1.0], // opaque red
        0,
        [16.0, 16.0, 48.0, 48.0], // clip rect
        [8.0, 8.0, 8.0, 8.0],     // uniform corner radius 8
    );
    let bytes = render_scene(&scene);
    assert_rounded_clip_pattern(&bytes, [255, 0, 0, 255]);
}

#[test]
fn p9prime_02_image_rounded_clip() {
    const KEY: u64 = 0xC11D;
    // 8×8 solid blue image, stretched to fill the canvas.
    let bytes: Vec<u8> = (0..64).flat_map(|_| [0u8, 0, 255, 255]).collect();
    let img = ImageData::from_bytes(8, 8, bytes);

    let mut scene = Scene::new(DIM, DIM);
    scene.image_sources.insert(KEY, img);
    scene.push_image_full_rounded(
        0.0,
        0.0,
        DIM as f32,
        DIM as f32,
        [0.0, 0.0, 1.0, 1.0],
        [1.0, 1.0, 1.0, 1.0], // no tint
        KEY,
        0,
        [16.0, 16.0, 48.0, 48.0],
        [8.0, 8.0, 8.0, 8.0],
    );
    let actual = render_scene(&scene);
    assert_rounded_clip_pattern(&actual, [0, 0, 255, 255]);
}

#[test]
fn p9prime_03_gradient_rounded_clip() {
    // Solid (single-stop-equivalent) red→red gradient — using a
    // gradient just to exercise the path. Rendering a uniform color
    // simplifies the pixel assertion (no interpolation noise).
    let mut scene = Scene::new(DIM, DIM);
    let g = netrender::SceneGradient {
        x0: 0.0,
        y0: 0.0,
        x1: DIM as f32,
        y1: DIM as f32,
        kind: netrender::GradientKind::Linear,
        repeat: false,
        params: [0.0, 0.0, DIM as f32, 0.0],
        stops: vec![
            netrender::GradientStop {
                offset: 0.0,
                color: [0.0, 1.0, 0.0, 1.0],
            },
            netrender::GradientStop {
                offset: 1.0,
                color: [0.0, 1.0, 0.0, 1.0],
            },
        ],
        transform_id: 0,
        clip_rect: [16.0, 16.0, 48.0, 48.0],
        clip_corner_radii: [8.0, 8.0, 8.0, 8.0],
    };
    scene.push_gradient(g);

    let bytes = render_scene(&scene);
    assert_rounded_clip_pattern(&bytes, [0, 255, 0, 255]);
}
