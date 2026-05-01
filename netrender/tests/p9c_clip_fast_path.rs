/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 9C receipt — `cs_clip_rectangle` fast path.
//!
//! `WgpuDevice::ensure_clip_rectangle(format, has_rounded_corners=false)`
//! returns a pipeline specialized via the `HAS_ROUNDED_CORNERS`
//! override. The fast-path shader skips the SDF and outputs a hard
//! axis-aligned step (anti-aliased over one pixel via the same
//! `clamp(0.5 - d, 0, 1)` smoothing). It's selected at scene-build
//! time when all corner radii are zero — cheaper instructions per
//! pixel and avoids a `length()` for the common rectangular-clip case.
//!
//! Tests:
//!   p9c_01_fast_path_is_axis_aligned_step   — hard corners, no
//!                                              rounded falloff
//!   p9c_02_fast_path_pixel_match_rounded_at_zero_radius
//!     — passing `radius=0` to the rounded variant should produce the
//!       same coverage as the fast path (within blend rounding); the
//!       fast path is just an optimization, not a behavior change.

use std::collections::HashMap;
use std::sync::Arc;

use netrender::{
    ClipRectanglePipeline, EncodeCallback, NetrenderOptions, Renderer, RenderGraph, Task,
    TaskId, boot, create_netrender_instance,
};

const W: u32 = 64;
const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn make_renderer() -> Renderer {
    let handles = boot().expect("wgpu boot");
    create_netrender_instance(handles, NetrenderOptions::default())
        .expect("create_netrender_instance")
}

fn clip_rectangle_callback(
    pipe: ClipRectanglePipeline,
    bounds: [f32; 4],
    radius: f32,
) -> EncodeCallback {
    let mut bytes = [0u8; 32];
    for (i, f) in bounds.iter().enumerate() {
        bytes[i * 4..(i + 1) * 4].copy_from_slice(&f.to_ne_bytes());
    }
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

/// Render one mask via `cs_clip_rectangle` with the given variant.
/// Returns the readback bytes (length = `extent² * 4`).
fn render_mask(
    renderer: &Renderer,
    extent: u32,
    bounds: [f32; 4],
    radius: f32,
    has_rounded_corners: bool,
) -> Vec<u8> {
    let device = renderer.wgpu_device.core.device.clone();
    let queue = renderer.wgpu_device.core.queue.clone();

    let pipe = renderer
        .wgpu_device
        .ensure_clip_rectangle(MASK_FORMAT, has_rounded_corners);

    const MASK: TaskId = 1;
    let mut graph = RenderGraph::new();
    graph.push(Task {
        id: MASK,
        extent: wgpu::Extent3d { width: extent, height: extent, depth_or_array_layers: 1 },
        format: MASK_FORMAT,
        inputs: vec![],
        encode: clip_rectangle_callback(pipe, bounds, radius),
    });

    let mut outputs = graph.execute(&device, &queue, HashMap::new());
    let tex = outputs.remove(&MASK).expect("mask output");
    let bytes = renderer.wgpu_device.read_rgba8_texture(&tex, extent, extent);
    let _ = Arc::new(tex);
    bytes
}

fn pixel(bytes: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// Fast path produces a hard-edged axis-aligned step. Sample inside
/// the rect (full coverage), well outside (zero coverage), and at the
/// hard corner — corner pixels should be either fully in or fully out
/// rather than the rounded falloff the SDF variant would produce.
#[test]
fn p9c_01_fast_path_is_axis_aligned_step() {
    let renderer = make_renderer();
    let bounds = [16.0_f32, 16.0, 48.0, 48.0];
    // radius is irrelevant in fast path; pass an arbitrary value.
    let bytes = render_mask(&renderer, W, bounds, 0.0, false);

    // Inside the rect: full coverage.
    assert_eq!(pixel(&bytes, W, 32, 32)[0], 255, "interior pixel");

    // Outside: zero coverage.
    assert_eq!(pixel(&bytes, W, 0, 0)[0], 0, "far-outside pixel");

    // Pixel just inside the corner (16, 16) — fast path treats it as
    // axis-aligned, so the corner is sharp. Pixel at (16, 16) sits
    // right on the edge of the bounds (the half-open semantics put
    // coverage at >= 16 inside); pixel center (16.5, 16.5) is 0.5 px
    // inside the rect on both axes, so SDF d = max(15.5, 15.5)*sign(neg)
    // — the smoothing gives this pixel partial coverage. But pixel
    // (15, 15) is outside the rect by 0.5 px on both axes → fully 0
    // (with the smoothing, it's actually on the soft-edge, but the
    // fast path's per-axis step still anti-aliases that single pixel).
    let inner_corner = pixel(&bytes, W, 16, 16);
    assert!(
        inner_corner[0] >= 200,
        "inner-corner (16,16) should be near-fully-covered, got {}",
        inner_corner[0]
    );

    // 2 pixels diagonally outside the corner: definitely outside.
    let outer_corner = pixel(&bytes, W, 14, 14);
    assert_eq!(
        outer_corner[0], 0,
        "outer corner (14,14) should be fully outside, got {}",
        outer_corner[0]
    );
}

/// Setting `radius = 0` in the rounded variant should produce a
/// coverage texture matching the fast path (both reduce to an
/// axis-aligned step). Sanity check that the fast path is purely an
/// optimization: same input → same output.
#[test]
fn p9c_02_fast_path_pixel_match_rounded_at_zero_radius() {
    let renderer = make_renderer();
    let bounds = [16.0_f32, 16.0, 48.0, 48.0];

    let fast = render_mask(&renderer, W, bounds, 0.0, false);
    let rounded_zero = render_mask(&renderer, W, bounds, 0.0, true);

    assert_eq!(fast.len(), rounded_zero.len(), "readback length");

    // Per-channel ±2 tolerance — both shaders run the same
    // `clamp(0.5 - d, 0, 1)` smoothing and should produce identical
    // pixels modulo any float-arithmetic reordering between
    // `length(max(q, 0))` (rounded path with r=0) and `max(q.x, q.y)`
    // (fast path).
    let mut max_diff: u8 = 0;
    let mut diff_count = 0usize;
    for (a, b) in fast.iter().zip(rounded_zero.iter()) {
        let d = (*a as i16 - *b as i16).unsigned_abs() as u8;
        if d > 2 {
            diff_count += 1;
        }
        max_diff = max_diff.max(d);
    }
    assert_eq!(
        diff_count, 0,
        "fast-path vs. rounded-with-r=0 mismatch: {} channels diverged by >2 (max {})",
        diff_count, max_diff
    );
}
