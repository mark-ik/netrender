/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 8D receipt — N-stop ramp + unified gradient.
//!
//! 8D collapses linear / radial / conic into one `SceneGradient` type
//! with arbitrary-length `stops` and a `GradientKind` discriminator.
//! 2-stop convenience methods (`Scene::push_linear_gradient` etc.)
//! still work by building a 2-stop `SceneGradient` internally — the
//! existing `p8a` / `p8b` / `p8c` receipts pass without change.
//!
//! Tests:
//!   p8d_01_three_stop_linear         — red → green → blue at offsets 0, 0.5, 1
//!   p8d_02_uneven_stop_offsets       — stops at 0, 0.2, 0.8, 1; sub-segment math
//!   p8d_03_painter_order_across_kinds — radial pushed first, linear pushed second;
//!                                       linear overdraws radial (8A-C inverted)
//!   p8d_04_general_push_gradient_api  — N-stop radial via push_gradient(SceneGradient { ... })

use netrender::{
    ColorLoad, FrameTarget, GradientKind, GradientStop, NetrenderOptions, NO_CLIP, Renderer,
    Scene, SceneGradient, boot, create_netrender_instance,
};

const W: u32 = 64;
const H: u32 = 64;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

fn make_renderer() -> Renderer {
    let handles = boot().expect("wgpu boot");
    create_netrender_instance(handles, NetrenderOptions::default())
        .expect("create_netrender_instance")
}

fn render_to_bytes(renderer: &Renderer, scene: &Scene) -> Vec<u8> {
    let device = renderer.wgpu_device.core.device.clone();
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p8d target"),
        size: wgpu::Extent3d {
            width: scene.viewport_width,
            height: scene.viewport_height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let prepared = renderer.prepare(scene);
    renderer.render(
        &prepared,
        FrameTarget {
            view: &view,
            format: TARGET_FORMAT,
            width: scene.viewport_width,
            height: scene.viewport_height,
        },
        ColorLoad::Clear(wgpu::Color::BLACK),
    );
    renderer
        .wgpu_device
        .read_rgba8_texture(&target, scene.viewport_width, scene.viewport_height)
}

fn pixel(bytes: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
}

fn srgb_encode(linear: f32) -> u8 {
    let l = linear.clamp(0.0, 1.0);
    let v = if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

fn channel_diff(a: u8, b: u8) -> u8 {
    (a as i16 - b as i16).unsigned_abs() as u8
}

fn assert_within_tol(actual: [u8; 4], expected: [u8; 4], tol: u8, where_: &str) {
    let diffs = [
        channel_diff(actual[0], expected[0]),
        channel_diff(actual[1], expected[1]),
        channel_diff(actual[2], expected[2]),
        channel_diff(actual[3], expected[3]),
    ];
    let max = *diffs.iter().max().unwrap();
    assert!(
        max <= tol,
        "{}: actual {:?}, expected {:?} (max channel diff = {}, tol = {})",
        where_, actual, expected, max, tol
    );
}

/// Mix between two stops at the given fractional position within the
/// segment. Returns sRGB-encoded RGBA over an opaque-black backdrop
/// (so framebuffer alpha is always 255).
fn segment_mix(c0: [f32; 4], c1: [f32; 4], local_t: f32) -> [u8; 4] {
    let mix = [
        c0[0] * (1.0 - local_t) + c1[0] * local_t,
        c0[1] * (1.0 - local_t) + c1[1] * local_t,
        c0[2] * (1.0 - local_t) + c1[2] * local_t,
    ];
    [
        srgb_encode(mix[0]),
        srgb_encode(mix[1]),
        srgb_encode(mix[2]),
        255,
    ]
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// Three stops: red(0) → green(0.5) → blue(1) along the x-axis. Pixel
/// at x=W/4 sits at t=0.25 — halfway through the first segment, so
/// mix(red, green, 0.5). Pixel at x=3W/4 → mix(green, blue, 0.5).
#[test]
fn p8d_01_three_stop_linear() {
    let renderer = make_renderer();
    let mut scene = Scene::new(W, H);
    let red = [1.0_f32, 0.0, 0.0, 1.0];
    let green = [0.0_f32, 1.0, 0.0, 1.0];
    let blue = [0.0_f32, 0.0, 1.0, 1.0];
    scene.push_gradient(SceneGradient {
        kind: GradientKind::Linear,
        x0: 0.0, y0: 0.0, x1: W as f32, y1: H as f32,
        params: [0.0, 0.0, W as f32, 0.0],
        stops: vec![
            GradientStop { offset: 0.0, color: red },
            GradientStop { offset: 0.5, color: green },
            GradientStop { offset: 1.0, color: blue },
        ],
        transform_id: 0,
        clip_rect: NO_CLIP,
    });

    let bytes = render_to_bytes(&renderer, &scene);

    // x=16 → t = 16.5/64 = 0.2578 → segment [0, 0.5], local_t = 0.5156.
    let t = 16.5 / W as f32;
    let local_t = t / 0.5; // first segment span
    assert_within_tol(
        pixel(&bytes, W, 16, 32),
        segment_mix(red, green, local_t),
        2,
        "x=16 first segment",
    );

    // x=32 → t = 32.5/64 = 0.5078, segment [0.5, 1], local_t = 0.0156.
    let t = 32.5 / W as f32;
    let local_t = (t - 0.5) / 0.5;
    assert_within_tol(
        pixel(&bytes, W, 32, 32),
        segment_mix(green, blue, local_t),
        2,
        "x=32 second segment",
    );

    // x=48 → t = 48.5/64 = 0.7578, segment [0.5, 1], local_t = 0.5156.
    let t = 48.5 / W as f32;
    let local_t = (t - 0.5) / 0.5;
    assert_within_tol(
        pixel(&bytes, W, 48, 32),
        segment_mix(green, blue, local_t),
        2,
        "x=48 second segment",
    );
}

/// Uneven stop offsets at [0.0, 0.2, 0.8, 1.0] — exercises the
/// per-segment span normalization. Sample inside each segment.
#[test]
fn p8d_02_uneven_stop_offsets() {
    let renderer = make_renderer();
    let mut scene = Scene::new(W, H);
    let black = [0.0_f32, 0.0, 0.0, 1.0];
    let red = [1.0_f32, 0.0, 0.0, 1.0];
    let blue = [0.0_f32, 0.0, 1.0, 1.0];
    let white = [1.0_f32, 1.0, 1.0, 1.0];
    let stops = vec![
        GradientStop { offset: 0.0, color: black },
        GradientStop { offset: 0.2, color: red },
        GradientStop { offset: 0.8, color: blue },
        GradientStop { offset: 1.0, color: white },
    ];
    scene.push_gradient(SceneGradient {
        kind: GradientKind::Linear,
        x0: 0.0, y0: 0.0, x1: W as f32, y1: H as f32,
        params: [0.0, 0.0, W as f32, 0.0],
        stops: stops.clone(),
        transform_id: 0,
        clip_rect: NO_CLIP,
    });

    let bytes = render_to_bytes(&renderer, &scene);

    // Helper: pick the right segment for a given t and compute the
    // expected mix at it.
    let expected_at = |x: u32, y: u32| -> [u8; 4] {
        let t = (x as f32 + 0.5) / W as f32;
        if t <= stops[0].offset {
            return [
                srgb_encode(stops[0].color[0]),
                srgb_encode(stops[0].color[1]),
                srgb_encode(stops[0].color[2]),
                255,
            ];
        }
        if t >= stops[stops.len() - 1].offset {
            let c = stops[stops.len() - 1].color;
            return [srgb_encode(c[0]), srgb_encode(c[1]), srgb_encode(c[2]), 255];
        }
        for w in stops.windows(2) {
            let a = w[0];
            let b = w[1];
            if t < b.offset {
                let local = (t - a.offset) / (b.offset - a.offset);
                return segment_mix(a.color, b.color, local);
            }
        }
        let _ = y;
        [0, 0, 0, 255]
    };

    // Sample one column inside each of the three segments.
    for &x in &[6_u32, 32, 56] {
        assert_within_tol(
            pixel(&bytes, W, x, 32),
            expected_at(x, 32),
            2,
            &format!("x={} segment", x),
        );
    }
}

/// Push order: radial first (depth-back), linear second (depth-front).
/// 8A-C bucketed by family (linear < radial), so a radial pushed
/// before a linear would have appeared *in front*. 8D preserves push
/// order: the linear must overdraw the radial at every covered pixel.
#[test]
fn p8d_03_painter_order_across_kinds() {
    let renderer = make_renderer();
    let mut scene = Scene::new(W, H);
    // Radial: red center, blue edge. Pushed FIRST → goes in back.
    scene.push_radial_gradient(
        0.0, 0.0, W as f32, H as f32,
        [W as f32 / 2.0, H as f32 / 2.0],
        [32.0, 32.0],
        [1.0, 0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
    );
    // Linear: solid green. Pushed SECOND → goes in front, overdraws radial.
    scene.push_linear_gradient(
        0.0, 0.0, W as f32, H as f32,
        [0.0, 0.0],
        [W as f32, 0.0],
        [0.0, 1.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 1.0],
    );

    let bytes = render_to_bytes(&renderer, &scene);

    // Every visible pixel must be solid green (linear constant) — the
    // radial behind it is fully occluded.
    for &(x, y) in &[(0_u32, 0_u32), (32, 32), (W - 1, H - 1), (10, 50)] {
        let p = pixel(&bytes, W, x, y);
        assert!(
            p[0] < 5 && p[1] > 240 && p[2] < 5,
            "({}, {}) pixel {:?} should be solid green (linear overrides radial)",
            x, y, p
        );
    }
}

/// Use the general `push_gradient(SceneGradient)` API directly with a
/// 4-stop radial. Verifies the generic surface compiles and produces
/// the same pixel result that an equivalent stop-by-stop construction
/// would predict.
#[test]
fn p8d_04_general_push_gradient_api() {
    let renderer = make_renderer();
    let mut scene = Scene::new(W, H);
    let stops = vec![
        GradientStop { offset: 0.0, color: [1.0, 0.0, 0.0, 1.0] },
        GradientStop { offset: 0.33, color: [1.0, 1.0, 0.0, 1.0] },
        GradientStop { offset: 0.66, color: [0.0, 1.0, 0.0, 1.0] },
        GradientStop { offset: 1.0, color: [0.0, 0.0, 1.0, 1.0] },
    ];
    scene.push_gradient(SceneGradient {
        kind: GradientKind::Radial,
        x0: 0.0, y0: 0.0, x1: W as f32, y1: H as f32,
        params: [W as f32 / 2.0, H as f32 / 2.0, 32.0, 32.0],
        stops: stops.clone(),
        transform_id: 0,
        clip_rect: NO_CLIP,
    });

    let bytes = render_to_bytes(&renderer, &scene);

    // Center of the canvas → t≈0.022 → first segment [0, 0.33],
    // local_t ≈ 0.067 → near red.
    let cx = W as f32 / 2.0;
    let cy = H as f32 / 2.0;
    let dx = (32.5_f32 - cx) / 32.0;
    let dy = (32.5_f32 - cy) / 32.0;
    let t = (dx * dx + dy * dy).sqrt();
    assert!(t < stops[1].offset, "test setup: center should land in first segment");
    let local = (t - stops[0].offset) / (stops[1].offset - stops[0].offset);
    let expected = segment_mix(stops[0].color, stops[1].color, local);
    assert_within_tol(pixel(&bytes, W, 32, 32), expected, 2, "center 4-stop radial");

    // Corners are way outside r=32 (distance ≈ 45) → clamp to last stop = blue.
    for &(x, y) in &[(0_u32, 0_u32), (W - 1, 0), (0, H - 1), (W - 1, H - 1)] {
        assert_within_tol(
            pixel(&bytes, W, x, y),
            [0, 0, 255, 255],
            2,
            &format!("corner ({}, {}) clamps to blue", x, y),
        );
    }
}
