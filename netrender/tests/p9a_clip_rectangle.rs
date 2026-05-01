/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 9A receipt — `cs_clip_rectangle` rounded-rect clip mask.
//!
//! Generates an `Rgba8Unorm` coverage texture via the render graph,
//! verifies the SDF math at sample points, then composites the mask
//! into a scene as a tinted image (`brush_image` with a red tint over
//! a black backdrop) and verifies the rounded-rect-of-red rendering.
//!
//! Tests:
//!   p9a_01_mask_pixels_match_sdf      — read back mask texture, verify
//!                                        coverage at center / corner /
//!                                        boundary
//!   p9a_02_mask_composes_as_red_rect  — insert mask into image cache,
//!                                        render as tinted image, verify
//!                                        rounded red rect over black

use std::collections::HashMap;
use std::sync::Arc;

use netrender::{
    ClipRectanglePipeline, ColorLoad, EncodeCallback, FrameTarget, ImageKey, NO_CLIP,
    NetrenderOptions, Renderer, RenderGraph, Scene, Task, TaskId, boot,
    create_netrender_instance,
};

const W: u32 = 64;
const H: u32 = 64;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

// ── Helpers ────────────────────────────────────────────────────────────────

fn make_renderer() -> Renderer {
    let handles = boot().expect("wgpu boot");
    create_netrender_instance(handles, NetrenderOptions::default())
        .expect("create_netrender_instance")
}

/// Build an encode callback that runs `cs_clip_rectangle` with the
/// given bounds + corner radius. Pre-builds the uniform buffer and
/// captures it; the bind group is created at encode time.
fn clip_rectangle_callback(
    pipe: ClipRectanglePipeline,
    bounds: [f32; 4],
    radius: f32,
) -> EncodeCallback {
    // ClipParams: bounds (vec4) + radii (vec4) = 32 bytes.
    let mut bytes = [0u8; 32];
    for (i, f) in bounds.iter().enumerate() {
        bytes[i * 4..(i + 1) * 4].copy_from_slice(&f.to_ne_bytes());
    }
    // radii: replicate `radius` to all 4 channels (uniform corners).
    for i in 4..8 {
        bytes[i * 4..(i + 1) * 4].copy_from_slice(&radius.to_ne_bytes());
    }

    Box::new(move |device, encoder, _inputs, output| {
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("clip_rectangle params"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        {
            let mut view = params_buf.slice(..).get_mapped_range_mut();
            view.copy_from_slice(&bytes);
        }
        params_buf.unmap();

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("clip_rectangle bind group"),
            layout: &pipe.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buf.as_entire_binding(),
            }],
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clip_rectangle pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: output,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipe.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..4, 0..1);
    })
}

fn render_to_bytes(renderer: &Renderer, scene: &Scene) -> Vec<u8> {
    let device = renderer.wgpu_device.core.device.clone();
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p9a target"),
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

/// Render a 64×64 rounded-rect clip mask via the render graph and
/// return its `Arc<wgpu::Texture>` plus the readback bytes.
fn render_clip_mask(
    renderer: &Renderer,
    extent: u32,
    bounds: [f32; 4],
    radius: f32,
) -> (Arc<wgpu::Texture>, Vec<u8>) {
    let device = renderer.wgpu_device.core.device.clone();
    let queue = renderer.wgpu_device.core.queue.clone();

    let pipe = renderer
        .wgpu_device
        .ensure_clip_rectangle(MASK_FORMAT, true);

    const MASK_TASK: TaskId = 1;
    let mut graph = RenderGraph::new();
    graph.push(Task {
        id: MASK_TASK,
        extent: wgpu::Extent3d { width: extent, height: extent, depth_or_array_layers: 1 },
        format: MASK_FORMAT,
        inputs: vec![],
        encode: clip_rectangle_callback(pipe, bounds, radius),
    });

    let mut outputs = graph.execute(&device, &queue, HashMap::new());
    let mask_tex = outputs.remove(&MASK_TASK).expect("mask output");
    let bytes = renderer
        .wgpu_device
        .read_rgba8_texture(&mask_tex, extent, extent);
    (Arc::new(mask_tex), bytes)
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// Render the mask in isolation and verify SDF behavior at three
/// representative points: center (full coverage), far corner (zero
/// coverage outside the rounded corner), and approximately on the
/// rounded-corner arc.
#[test]
fn p9a_01_mask_pixels_match_sdf() {
    let renderer = make_renderer();

    // Rounded rect spanning the full 64×64 target with a 16-pixel
    // corner radius. Inset slightly from the very corners so the
    // coverage values are non-trivial but still pixel-exact.
    let bounds = [4.0_f32, 4.0, 60.0, 60.0];
    let radius = 16.0_f32;
    let (_tex, bytes) = render_clip_mask(&renderer, W, bounds, radius);

    // Center of the inset rect is deep inside → coverage ≈ 1.
    let center = pixel(&bytes, W, 32, 32);
    assert_eq!(center[3], 255, "center should have full alpha coverage");
    // All four channels should match (we wrote vec4(coverage)).
    for &c in &center[..4] {
        assert!(c >= 250, "center channel {} should be near-1.0, got {}", c, c);
    }

    // Far top-left pixel (1, 1) — outside the rounded corner at (4,4)
    // by SDF distance ~3 px. Coverage should be ~0.
    let outside = pixel(&bytes, W, 1, 1);
    for &c in &outside[..4] {
        assert!(c <= 5, "far-corner channel {} should be ~0, got {}", c, c);
    }

    // A pixel near the inset corner — between (4, 4) and the arc.
    // Inside the rect's rectangular hull but possibly outside the arc.
    // Pixel (5, 5): inside the inset rect (>4,>4); SDF = sdRoundedRect
    //   from rect-center (32, 32) with half-size (28, 28), radius 16.
    //   q = abs((5,5)-(32,32)) - (28,28) + (16,16) = (-1+16, -1+16) = (15, 15)... wait
    //   abs(p) here means abs from center, so abs(5-32, 5-32) = (27, 27)
    //   q = (27, 27) - (28, 28) + (16, 16) = (15, 15)
    //   max(q, 0) = (15, 15) → length = 21.21 → 21.21 - 16 = 5.21 (positive, outside arc)
    //   coverage = clamp(0.5 - 5.21, 0, 1) = 0
    let arc_outside = pixel(&bytes, W, 5, 5);
    for &c in &arc_outside[..4] {
        assert!(
            c <= 5,
            "(5,5) outside the rounded corner arc: channel {} should be ~0",
            c
        );
    }

    // A pixel well inside the corner radius — e.g. (32, 5) along the
    // top edge, which has SDF distance ≈ -23 (deep inside vertically,
    // 5 px from the top edge): should be saturated.
    let edge_inside = pixel(&bytes, W, 32, 5);
    for &c in &edge_inside[..4] {
        assert!(
            c >= 250,
            "(32,5) on the top edge (deep inside the rounded rect): channel {} should be ~1",
            c
        );
    }
}

/// End-to-end: render the mask, insert it into the image cache, render
/// a scene that draws the mask as a red-tinted image. Verify that the
/// resulting framebuffer shows a rounded-corner red rect over black.
#[test]
fn p9a_02_mask_composes_as_red_rect() {
    let renderer = make_renderer();

    let bounds = [4.0_f32, 4.0, 60.0, 60.0];
    let radius = 16.0_f32;
    let (mask_tex, _bytes) = render_clip_mask(&renderer, W, bounds, radius);

    const MASK_KEY: ImageKey = 0xCAFE_9A1F;
    renderer.insert_image_gpu(MASK_KEY, mask_tex);

    // Draw the mask as a red-tinted image. The tint alpha is set to
    // 0.999 (rather than 1.0) to route through the alpha-blend
    // pipeline — Phase 5's image routing classifies by *tint* alpha,
    // not texture alpha, so a fully-opaque tint takes the no-blend
    // opaque path which would overwrite the framebuffer with
    // (0,0,0,0) at zero-coverage pixels. With premultiplied alpha
    // blend on, src = mask*tint = (cov, 0, 0, cov*0.999); over a
    // BLACK clear (0,0,0,1), the result is approximately (cov, 0, 0,
    // 1) — visually indistinguishable from a 1.0 tint. The "image
    // with variable mask alpha but opaque tint" routing is documented
    // as a Phase 5 limitation; a "force-alpha" hint or texture-alpha
    // peek can land later.
    let mut scene = Scene::new(W, H);
    scene.push_image_full(
        0.0, 0.0, W as f32, H as f32,
        [0.0, 0.0, 1.0, 1.0],
        [1.0, 0.0, 0.0, 0.999],
        MASK_KEY,
        0,
        NO_CLIP,
    );

    let bytes = render_to_bytes(&renderer, &scene);

    // Center: fully covered → opaque red.
    assert_within_tol(pixel(&bytes, W, 32, 32), [255, 0, 0, 255], 2, "center red");

    // Far corner: outside rounded rect → black backdrop unchanged.
    assert_within_tol(pixel(&bytes, W, 0, 0), [0, 0, 0, 255], 2, "(0,0) black");
    assert_within_tol(pixel(&bytes, W, 63, 0), [0, 0, 0, 255], 2, "(63,0) black");
    assert_within_tol(pixel(&bytes, W, 0, 63), [0, 0, 0, 255], 2, "(0,63) black");
    assert_within_tol(pixel(&bytes, W, 63, 63), [0, 0, 0, 255], 2, "(63,63) black");

    // Top edge (deep inside vertically, 5 px from top): full red.
    assert_within_tol(pixel(&bytes, W, 32, 5), [255, 0, 0, 255], 2, "(32,5) edge red");
}
