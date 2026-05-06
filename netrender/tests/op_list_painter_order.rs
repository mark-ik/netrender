/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Op-list painter-order receipts (post-2026-05-04 Scene refactor).
//!
//! These tests pin the invariant that **consumer push order is
//! painter order**. They construct primitive sequences where the
//! per-type Vec ordering (the pre-refactor design) would produce
//! different output than the op-list ordering.
//!
//! - `op_order_01_rect_after_image_paints_on_top`: a rect pushed
//!   after an image must appear on top. Previously rects always
//!   painted before images regardless of push order.
//! - `op_order_02_image_after_rect_paints_on_top`: the symmetric
//!   case (image after rect). Behaves the same in both designs but
//!   anchors the contract.
//! - `op_order_03_op_list_holds_six_variant_kinds`: structural
//!   check — `Scene::ops` accepts every SceneOp variant from the
//!   public push helpers and reports them in push order.

use netrender::{
    ColorLoad, ImageData, NetrenderOptions, Scene, SceneOp, boot, create_netrender_instance,
};

const DIM: u32 = 64;
const TILE: u32 = 32;

fn make_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("op_list test target"),
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
        label: Some("op_list test view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, view)
}

fn read_pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * DIM + x) * 4) as usize;
    [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
}

/// 8×8 solid-blue image (premultiplied opaque blue).
fn solid_blue_image() -> ImageData {
    const SZ: u32 = 8;
    let mut bytes = Vec::with_capacity((SZ * SZ * 4) as usize);
    for _ in 0..(SZ * SZ) {
        bytes.extend_from_slice(&[0, 0, 255, 255]);
    }
    ImageData::from_bytes(SZ, SZ, bytes)
}

/// Build a scene that fills the whole frame with a blue image, then
/// pushes a fully-opaque red rect over it. Op-list painter order
/// dictates the rect (pushed second) wins → output is red.
///
/// In the pre-refactor type-Vec design, rects painted before images,
/// so the output would have been blue regardless of push order.
#[test]
fn op_order_01_rect_after_image_paints_on_top() {
    let handles = boot().expect("wgpu boot");
    let renderer = create_netrender_instance(
        handles.clone(),
        NetrenderOptions { tile_cache_size: Some(TILE), enable_vello: true, ..Default::default() },
    )
    .expect("create_netrender_instance");

    let mut scene = Scene::new(DIM, DIM);
    const KEY: u64 = 0xB10E_0001;
    scene.set_image_source(KEY, solid_blue_image());

    // 1. Blue image, full frame.
    scene.push_image(0.0, 0.0, DIM as f32, DIM as f32, KEY, solid_blue_image());
    // 2. Opaque red rect, full frame, pushed AFTER → paints on top.
    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 0.0, 0.0, 1.0]);

    let (target, view) = make_target(&handles.device);
    renderer.render_vello(&scene, &view, ColorLoad::Clear(wgpu::Color::BLACK));
    let bytes = renderer.wgpu_device.read_rgba8_texture(&target, DIM, DIM);

    // Center pixel: must be red (top), not blue (under).
    let center = read_pixel(&bytes, DIM / 2, DIM / 2);
    assert_eq!(
        center[0], 255,
        "center pixel R should be 255 (red rect on top); got {:?}",
        center,
    );
    assert!(
        center[2] < 16,
        "center pixel B should be near-zero (blue image obscured); got {:?}",
        center,
    );
}

/// Symmetric case: rect first, then image — image wins. This worked
/// in the pre-refactor design too (images came after rects). We
/// keep it to anchor the contract from the other side.
#[test]
fn op_order_02_image_after_rect_paints_on_top() {
    let handles = boot().expect("wgpu boot");
    let renderer = create_netrender_instance(
        handles.clone(),
        NetrenderOptions { tile_cache_size: Some(TILE), enable_vello: true, ..Default::default() },
    )
    .expect("create_netrender_instance");

    let mut scene = Scene::new(DIM, DIM);
    const KEY: u64 = 0xB10E_0002;
    scene.set_image_source(KEY, solid_blue_image());

    scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 0.0, 0.0, 1.0]);
    scene.push_image(0.0, 0.0, DIM as f32, DIM as f32, KEY, solid_blue_image());

    let (target, view) = make_target(&handles.device);
    renderer.render_vello(&scene, &view, ColorLoad::Clear(wgpu::Color::BLACK));
    let bytes = renderer.wgpu_device.read_rgba8_texture(&target, DIM, DIM);

    let center = read_pixel(&bytes, DIM / 2, DIM / 2);
    assert!(
        center[2] > 240,
        "center pixel B should be near-255 (blue image on top); got {:?}",
        center,
    );
    assert!(
        center[0] < 16,
        "center pixel R should be near-zero (red rect obscured); got {:?}",
        center,
    );
}

/// Structural check: the public Scene push helpers cover all six
/// SceneOp variants and append them in the order called.
#[test]
fn op_order_03_op_list_holds_six_variant_kinds() {
    use netrender::{Glyph, GradientKind, GradientStop, ScenePath, ScenePathStroke, SceneShape};

    let mut scene = Scene::new(DIM, DIM);

    scene.push_rect(0.0, 0.0, 8.0, 8.0, [1.0, 0.0, 0.0, 1.0]);
    scene.push_stroke(0.0, 0.0, 8.0, 8.0, [0.0, 1.0, 0.0, 1.0], 1.0);
    scene.push_gradient(netrender::SceneGradient {
        x0: 0.0, y0: 0.0, x1: 8.0, y1: 8.0,
        kind: GradientKind::Linear,
        params: [0.0, 0.0, 8.0, 0.0],
        stops: vec![
            GradientStop { offset: 0.0, color: [1.0, 0.0, 0.0, 1.0] },
            GradientStop { offset: 1.0, color: [0.0, 0.0, 1.0, 1.0] },
        ],
        transform_id: 0,
        clip_rect: netrender::NO_CLIP,
        clip_corner_radii: netrender::SHARP_CLIP,
    });
    scene.set_image_source(1, ImageData::from_bytes(1, 1, vec![0, 0, 0, 255]));
    scene.push_image(0.0, 0.0, 8.0, 8.0, 1, ImageData::from_bytes(1, 1, vec![0, 0, 0, 255]));
    scene.push_shape(SceneShape {
        path: ScenePath::default(),
        fill_color: Some([0.5, 0.5, 0.5, 1.0]),
        stroke: Some(ScenePathStroke { color: [0.0, 0.0, 0.0, 1.0], width: 1.0 }),
        transform_id: 0,
        clip_rect: netrender::NO_CLIP,
        clip_corner_radii: netrender::SHARP_CLIP,
    });
    scene.push_glyph_run(0, 16.0, vec![Glyph { id: 1, x: 0.0, y: 0.0 }], [1.0; 4]);

    assert_eq!(scene.ops.len(), 6, "expected one op per push call");
    assert!(matches!(scene.ops[0], SceneOp::Rect(_)));
    assert!(matches!(scene.ops[1], SceneOp::Stroke(_)));
    assert!(matches!(scene.ops[2], SceneOp::Gradient(_)));
    assert!(matches!(scene.ops[3], SceneOp::Image(_)));
    assert!(matches!(scene.ops[4], SceneOp::Shape(_)));
    assert!(matches!(scene.ops[5], SceneOp::GlyphRun(_)));
}
