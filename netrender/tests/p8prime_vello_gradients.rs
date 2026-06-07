/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 8' — netrender Scene → vello::Scene gradient ingestion.
//!
//! Three probes covering linear, radial-circular, and conic. The
//! gradients all interpolate in sRGB-encoded space (vello's GPU
//! compute path ignores `interpolation_cs` per p1prime_03), so
//! midpoints land where the existing Phase 8 batched receipts
//! would land them.
//!
//! - `p8prime_01_linear_red_to_blue` — horizontal red→blue gradient,
//!   midpoint expects (128, 0, 128).
//! - `p8prime_02_radial_circular_red_to_transparent` — circular
//!   radial centered at canvas center, red→transparent, sample at
//!   center expects red and far-corner expects transparent.
//! - `p8prime_03_conic_red_to_blue_seam_zero` — conic with seam at
//!   angle 0, red at start, blue at end; sample on the +x axis (just
//!   past seam) is red, on the -x axis (halfway around) is purple
//!   (sRGB-encoded midpoint of red→blue).

use netrender::{Scene, boot, vello_rasterizer::scene_to_vello};
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
        label: Some("p8' gradient target"),
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
        label: Some("p8' gradient storage view"),
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
        where_,
        actual,
        expected,
        max,
        tol
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

/// Horizontal linear red→blue gradient over the full canvas.
/// Midpoint: (128, 0, 128) in sRGB-encoded interp.
#[test]
fn p8prime_01_linear_red_to_blue() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_linear_gradient(
        0.0,
        0.0,
        DIM as f32,
        DIM as f32,
        [0.0, (DIM as f32) / 2.0],
        [DIM as f32, (DIM as f32) / 2.0],
        [1.0, 0.0, 0.0, 1.0], // premultiplied red
        [0.0, 0.0, 1.0, 1.0], // premultiplied blue
    );

    let bytes = render_scene(&scene);

    // Pixel center for column x sits at x + 0.5; t = (x + 0.5) / DIM.
    // sRGB-encoded interp gives R = 255*(1-t), B = 255*t.
    let expected_at = |x: u32| -> [u8; 4] {
        let t = (x as f32 + 0.5) / DIM as f32;
        let r = ((1.0 - t) * 255.0).round() as u8;
        let b = (t * 255.0).round() as u8;
        [r, 0, b, 255]
    };

    for &x in &[1u32, 16, 32, 48, 62] {
        assert_within_tol(
            read_pixel(&bytes, x, 32),
            expected_at(x),
            4,
            &format!("linear lerp at x = {}", x),
        );
    }
}

/// Circular radial gradient at canvas center, red at center,
/// transparent at radius DIM/2. Center pixel is red; corner pixel
/// (≥ radius) is fully transparent.
#[test]
fn p8prime_02_radial_circular_red_to_transparent() {
    let mut scene = Scene::new(DIM, DIM);
    let r = (DIM as f32) / 2.0;
    scene.push_radial_gradient(
        0.0,
        0.0,
        DIM as f32,
        DIM as f32,
        [r, r],
        [r, r],
        [1.0, 0.0, 0.0, 1.0], // premultiplied red, opaque, at center
        [0.0, 0.0, 0.0, 0.0], // transparent at boundary
    );

    let bytes = render_scene(&scene);

    // Sample at the geometric center — should be near-fully-red.
    // Bilinear/aa might shave 1-2/255; allow a wider tol because
    // gradient quantization plus center-sample roundoff stack.
    let center = read_pixel(&bytes, 32, 32);
    assert!(
        center[0] >= 240 && center[3] >= 240 && center[1] < 16 && center[2] < 16,
        "radial center at (32, 32): {:?} not near (255, 0, 0, 255)",
        center
    );

    // Corner — outside the gradient's radius (length from center is
    // r * sqrt(2) > r). Stops clamp at the boundary value, so this
    // should be fully transparent.
    let corner = read_pixel(&bytes, 1, 1);
    assert!(
        corner[3] < 16,
        "radial corner at (1, 1): {:?} not near transparent",
        corner
    );
}

/// Elliptical radial gradient: rx = DIM/2, ry = DIM/4. Exercises the
/// brush_xform path (peniko::Gradient::new_radial only supports
/// circular, so the translator builds a unit-circle radial and warps
/// shape→brush coordinates).
///
/// At center: red. At the horizontal edge (DIM, DIM/2) — distance
/// rx = DIM/2 — boundary, so transparent. Same vertically at
/// (DIM/2, DIM/2 - ry).
#[test]
fn p8prime_04_radial_elliptical() {
    let mut scene = Scene::new(DIM, DIM);
    let cx = (DIM as f32) / 2.0;
    let cy = (DIM as f32) / 2.0;
    let rx = (DIM as f32) / 2.0;
    let ry = (DIM as f32) / 4.0;
    scene.push_radial_gradient(
        0.0,
        0.0,
        DIM as f32,
        DIM as f32,
        [cx, cy],
        [rx, ry],
        [1.0, 0.0, 0.0, 1.0], // red opaque at center
        [0.0, 0.0, 0.0, 0.0], // transparent at boundary
    );

    let bytes = render_scene(&scene);

    // Center: near-fully red.
    let center = read_pixel(&bytes, 32, 32);
    assert!(
        center[0] >= 240 && center[3] >= 240,
        "elliptical center at (32, 32): {:?} not near (255, 0, 0, 255)",
        center
    );
    // Storage convention (p1prime_02): straight-alpha. With both
    // stops being red-of-some-alpha [(1,0,0,1) and (0,0,0,0)],
    // unpremultiplied ramp samples are (1, 0, 0, 1-t) — RGB stays
    // at (255, 0, 0); only alpha varies with the parameter t.

    // 4 pixels above center: t = 4/ry = 0.25, alpha ≈ 255*(1-0.25) = 191.
    let near_y = read_pixel(&bytes, 32, 28);
    assert!(
        near_y[0] >= 240 && near_y[3] >= 170 && near_y[3] <= 210,
        "elliptical near-center along y at (32, 28): {:?} expected red ~191α",
        near_y
    );
    // 12 pixels above center: t = 12/ry = 0.75, alpha ≈ 64.
    let mid_y = read_pixel(&bytes, 32, 20);
    assert!(
        mid_y[3] > 30 && mid_y[3] < 100,
        "elliptical at (32, 20): {:?} not partial alpha",
        mid_y
    );
    // 16 pixels above center: at the y-radius edge → transparent.
    let edge_y = read_pixel(&bytes, 32, 16);
    assert!(
        edge_y[3] < 16,
        "elliptical y-edge at (32, 16): {:?} not transparent",
        edge_y
    );
    // Way outside the ellipse: (DIM-1, 16) is 31 right + 16 above
    // center, normalized distance ≈ sqrt((31/32)² + (16/16)²) ≈ 1.39
    // → fully transparent. Confirms the ellipse axes are oriented
    // correctly (rx along x, ry along y) and not swapped.
    let outside = read_pixel(&bytes, DIM - 1, 16);
    assert!(
        outside[3] < 8,
        "outside ellipse at ({}, 16): {:?} not transparent",
        DIM - 1,
        outside
    );
}

/// Conic gradient centered at canvas center, seam at angle 0
/// (positive x axis). Red at start (t=0), blue at end (t=1, also at
/// the seam approached from the other side). Sample due-east of
/// center is just past the seam → red. Sample due-west is at
/// t=0.5 → midpoint (128, 0, 128) sRGB-encoded.
#[test]
fn p8prime_03_conic_red_to_blue_seam_zero() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_conic_gradient(
        0.0,
        0.0,
        DIM as f32,
        DIM as f32,
        [(DIM as f32) / 2.0, (DIM as f32) / 2.0],
        0.0,
        [1.0, 0.0, 0.0, 1.0], // red at t=0
        [0.0, 0.0, 1.0, 1.0], // blue at t=1
    );

    let bytes = render_scene(&scene);

    // Just past the seam, due east of center: red.
    let east = read_pixel(&bytes, 60, 32);
    assert!(
        east[0] >= 200 && east[2] < 50,
        "conic just-past-seam (east) at (60, 32): {:?} not red-dominant",
        east
    );

    // Halfway around (due west of center, t=0.5): midpoint.
    // sRGB-encoded interp puts midpoint at ~(128, 0, 128).
    let west = read_pixel(&bytes, 4, 32);
    assert_within_tol(west, [128, 0, 128, 255], 8, "conic midpoint (west)");
}

/// Repeating linear gradient: one period (red→blue) spans the left half, then
/// tiles. At x = 0.75·DIM (1.5 periods along the line) the pattern is back at
/// its midpoint (purple), where a non-repeating (Pad) gradient would have
/// clamped to blue past its last stop.
#[test]
fn p8prime_05_repeating_linear_tiles() {
    let grad = |repeat: bool| netrender::SceneGradient {
        x0: 0.0,
        y0: 0.0,
        x1: DIM as f32,
        y1: DIM as f32,
        kind: netrender::GradientKind::Linear,
        repeat,
        // Gradient line spans the left half — one repeat period.
        params: [0.0, (DIM as f32) / 2.0, (DIM as f32) / 2.0, (DIM as f32) / 2.0],
        stops: vec![
            netrender::GradientStop { offset: 0.0, color: [1.0, 0.0, 0.0, 1.0] },
            netrender::GradientStop { offset: 1.0, color: [0.0, 0.0, 1.0, 1.0] },
        ],
        transform_id: 0,
        clip_rect: netrender::NO_CLIP,
        clip_corner_radii: [0.0; 4],
    };
    let sx = (DIM as f32 * 0.75) as u32; // 1.5 periods along the line

    let mut repeating = Scene::new(DIM, DIM);
    repeating.push_gradient(grad(true));
    let rep = read_pixel(&render_scene(&repeating), sx, DIM / 2);

    let mut clamped = Scene::new(DIM, DIM);
    clamped.push_gradient(grad(false));
    let pad = read_pixel(&render_scene(&clamped), sx, DIM / 2);

    assert!(
        rep[0] > 80 && rep[2] > 80,
        "repeating gradient shows the mid-pattern (purple) at 1.5 periods: {rep:?}"
    );
    assert!(
        pad[0] < 40 && pad[2] > 200,
        "non-repeating gradient clamps to blue past the last stop: {pad:?}"
    );
}
