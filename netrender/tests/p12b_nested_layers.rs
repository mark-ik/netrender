/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 12b' / 9b' — nested-layer (`SceneOp::PushLayer` /
//! `SceneOp::PopLayer`) and arbitrary-path-clip receipts.
//!
//! Receipts:
//!   p12b_01_alpha_layer_fades_inner_content — `PushLayer` with
//!     alpha 0.5 wrapping a red rect over white bg produces a
//!     mid-pink output (the layer alpha modulates inner pixels at
//!     composite time).
//!   p12b_02_rect_clip_layer_culls_outer_pixels — clip-only layer
//!     restricts a full-frame rect's paint to the clip rect.
//!   p12b_03_rounded_clip_layer_clips_corners — rounded-rect
//!     clip on a layer produces visible corner clipping.
//!   p9b_01_path_clip_layer_culls_outside_path — arbitrary
//!     `ScenePath` clip on a layer culls pixels outside the path.
//!   p12b_04_nested_layers_compose — alpha-layer outside, clip-
//!     layer inside; both effects apply.

use netrender::{
    ColorLoad, NetrenderOptions, PathOp, Scene, SceneClip, SceneLayer, ScenePath, boot,
    create_netrender_instance,
};

const DIM: u32 = 64;
const TILE: u32 = 32;

fn make_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p12b target"),
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
        label: Some("p12b view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, view)
}

fn read_pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * DIM + x) * 4) as usize;
    [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
}

fn render_to_bytes(scene: &Scene) -> Vec<u8> {
    let handles = boot().expect("wgpu boot");
    let renderer = create_netrender_instance(
        handles.clone(),
        NetrenderOptions { tile_cache_size: Some(TILE), enable_vello: true, ..Default::default() },
    )
    .expect("create_netrender_instance");
    let (target, view) = make_target(&handles.device);
    renderer.render_vello(scene, &view, ColorLoad::Clear(wgpu::Color::WHITE));
    renderer.wgpu_device.read_rgba8_texture(&target, DIM, DIM)
}

/// Wrap a full-frame red rect in a 50%-alpha layer over a white bg.
/// Composited output should be half-saturation pink: rgb ≈ 255, 127, 127
/// (50% of red over 100% white = (255*0.5 + 255*0.5, 0*0.5 + 255*0.5, 0*0.5 + 255*0.5)).
#[test]
fn p12b_01_alpha_layer_fades_inner_content() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_layer_alpha(0.5);
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 0.0, 0.0, 1.0]);
    scene.pop_layer();

    let bytes = render_to_bytes(&scene);
    let center = read_pixel(&bytes, DIM / 2, DIM / 2);

    // Tolerance of 4 absorbs vello's gamma rounding at the
    // composite boundary.
    let tol: i16 = 4;
    let target: [i16; 3] = [255, 127, 127];
    for (i, ch) in ["R", "G", "B"].iter().enumerate() {
        let diff = (center[i] as i16 - target[i]).abs();
        assert!(
            diff <= tol,
            "alpha-layer {} channel: actual {}, expected ~{}, diff {}",
            ch, center[i], target[i], diff,
        );
    }
}

/// Clip-only layer with a small rect clip wraps a full-frame red
/// rect. Only the clipped region should paint red over the white bg;
/// outside the clip stays white.
#[test]
fn p12b_02_rect_clip_layer_culls_outer_pixels() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_layer_clip(SceneClip::Rect {
        rect: [16.0, 16.0, 48.0, 48.0],
        radii: [0.0; 4],
    });
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 0.0, 0.0, 1.0]);
    scene.pop_layer();

    let bytes = render_to_bytes(&scene);

    // Inside clip: red.
    let inside = read_pixel(&bytes, 32, 32);
    assert!(inside[0] > 240 && inside[1] < 16 && inside[2] < 16,
            "inside clip should be red; got {:?}", inside);

    // Outside clip: white background untouched.
    let outside = read_pixel(&bytes, 4, 4);
    assert!(outside[0] > 240 && outside[1] > 240 && outside[2] > 240,
            "outside clip should be white; got {:?}", outside);
}

/// Rounded clip layer: corner pixels of the clip rect should fall
/// outside the rounded shape and stay white.
#[test]
fn p12b_03_rounded_clip_layer_clips_corners() {
    let mut scene = Scene::new(DIM, DIM);
    scene.push_layer_clip(SceneClip::Rect {
        rect: [8.0, 8.0, 56.0, 56.0],
        radii: [12.0; 4],
    });
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 0.0, 0.0, 1.0]);
    scene.pop_layer();

    let bytes = render_to_bytes(&scene);

    // Center: red (well inside the rounded shape).
    let center = read_pixel(&bytes, 32, 32);
    assert!(center[0] > 240 && center[1] < 16,
            "center should be red; got {:?}", center);

    // Corner of the clip rect (x=9, y=9 — 1 px in from the corner)
    // should be outside the rounded path, still white.
    let corner = read_pixel(&bytes, 9, 9);
    assert!(corner[0] > 240 && corner[1] > 240 && corner[2] > 240,
            "rounded-clip corner should be white; got {:?}", corner);
}

/// Phase 9b' — arbitrary-path clip layer. A triangular clip wraps a
/// full-frame red rect. Pixels inside the triangle paint red;
/// pixels outside stay white.
#[test]
fn p9b_01_path_clip_layer_culls_outside_path() {
    let mut scene = Scene::new(DIM, DIM);

    // Triangle apex at top-center, baseline at the bottom corners.
    let mut path = ScenePath::new();
    path.ops.push(PathOp::MoveTo(32.0, 4.0));
    path.ops.push(PathOp::LineTo(60.0, 60.0));
    path.ops.push(PathOp::LineTo(4.0, 60.0));
    path.ops.push(PathOp::Close);

    scene.push_layer_clip(SceneClip::Path(path));
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 0.0, 0.0, 1.0]);
    scene.pop_layer();

    let bytes = render_to_bytes(&scene);

    // Well inside the triangle.
    let inside = read_pixel(&bytes, 32, 40);
    assert!(inside[0] > 240 && inside[1] < 16,
            "inside triangle should be red; got {:?}", inside);

    // Top-left corner — outside the triangle.
    let outside_tl = read_pixel(&bytes, 4, 4);
    assert!(outside_tl[0] > 240 && outside_tl[1] > 240 && outside_tl[2] > 240,
            "top-left outside triangle should be white; got {:?}", outside_tl);

    // Top-right corner — outside the triangle.
    let outside_tr = read_pixel(&bytes, 60, 4);
    assert!(outside_tr[0] > 240 && outside_tr[1] > 240 && outside_tr[2] > 240,
            "top-right outside triangle should be white; got {:?}", outside_tr);
}

/// Layers nest: outer layer applies alpha 0.5, inner layer clips to
/// a small rect. Pixels inside the clip are red-faded-by-half;
/// pixels outside the clip but inside the alpha layer's viewport
/// stay white-bg (the inner clip means no red there).
#[test]
fn p12b_04_nested_layers_compose() {
    let mut scene = Scene::new(DIM, DIM);
    // Outer: 50% alpha over the whole viewport.
    scene.push_layer(SceneLayer::alpha(0.5));
    // Inner: clip to a 32×32 center rect.
    scene.push_layer_clip(SceneClip::Rect {
        rect: [16.0, 16.0, 48.0, 48.0],
        radii: [0.0; 4],
    });
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 0.0, 0.0, 1.0]);
    scene.pop_layer();
    scene.pop_layer();

    let bytes = render_to_bytes(&scene);

    // Inside both layers: alpha-faded red.
    let inside = read_pixel(&bytes, 32, 32);
    let tol: i16 = 4;
    let target: [i16; 3] = [255, 127, 127];
    for (i, ch) in ["R", "G", "B"].iter().enumerate() {
        let diff = (inside[i] as i16 - target[i]).abs();
        assert!(
            diff <= tol,
            "nested-layer {} channel: actual {}, expected ~{}, diff {}",
            ch, inside[i], target[i], diff,
        );
    }

    // Outside inner clip but still inside outer alpha layer: white
    // (the inner clip prevented the red rect from painting here;
    // the outer layer's alpha just multiplies whatever was drawn).
    let outside = read_pixel(&bytes, 4, 4);
    assert!(outside[0] > 240 && outside[1] > 240 && outside[2] > 240,
            "outside inner clip should stay white; got {:?}", outside);
}
