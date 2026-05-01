/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 9B receipt — box-shadow clip via render-graph chaining.
//!
//! `cs_clip_rectangle` (Phase 9A) generates rounded-rect coverage,
//! then `brush_blur` (Phase 6) runs H + V passes over it, producing a
//! soft-edged drop-shadow mask. The chain is composed entirely
//! through the existing `RenderGraph` — no new shader, just new
//! plumbing.
//!
//! Tests:
//!   p9b_01_blur_softens_mask_edges  — read back the post-blur mask
//!                                      and confirm the edge falloff
//!                                      is wider than the unblurred
//!                                      9A baseline
//!   p9b_02_drop_shadow_composite    — composite blurred shadow under
//!                                      a sharp foreground rect; verify
//!                                      shadow halo is visible outside
//!                                      the foreground

use std::collections::HashMap;
use std::sync::Arc;

use netrender::{
    BrushBlurPipeline, ClipRectanglePipeline, ColorLoad, EncodeCallback, FrameTarget, ImageKey,
    NO_CLIP, NetrenderOptions, Renderer, RenderGraph, Scene, Task, TaskId, boot,
    create_netrender_instance,
};

const W: u32 = 64;
const H: u32 = 64;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn make_renderer() -> Renderer {
    let handles = boot().expect("wgpu boot");
    create_netrender_instance(handles, NetrenderOptions::default())
        .expect("create_netrender_instance")
}

fn make_bilinear_sampler(device: &wgpu::Device) -> Arc<wgpu::Sampler> {
    Arc::new(device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("p9b bilinear clamp"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    }))
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

fn blur_pass_callback(
    pipe: BrushBlurPipeline,
    sampler: Arc<wgpu::Sampler>,
    step_x: f32,
    step_y: f32,
) -> EncodeCallback {
    let mut step_bytes = [0u8; 16];
    step_bytes[0..4].copy_from_slice(&step_x.to_ne_bytes());
    step_bytes[4..8].copy_from_slice(&step_y.to_ne_bytes());

    Box::new(move |device, encoder, inputs, output| {
        assert!(!inputs.is_empty(), "blur task: expected one input view");
        let input_view = &inputs[0];

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blur params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        {
            let mut view = params_buf.slice(..).get_mapped_range_mut();
            view.copy_from_slice(&step_bytes);
        }
        params_buf.unmap();

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blur bind group"),
            layout: &pipe.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_buf.as_entire_binding(),
                },
            ],
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blur pass"),
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

fn pixel(bytes: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
}

/// Build the box-shadow chain: mask → blur_h → blur_v. Returns the
/// final post-blur mask texture and its readback bytes.
fn render_box_shadow_mask(
    renderer: &Renderer,
    extent: u32,
    bounds: [f32; 4],
    radius: f32,
) -> (Arc<wgpu::Texture>, Vec<u8>) {
    let device = renderer.wgpu_device.core.device.clone();
    let queue = renderer.wgpu_device.core.queue.clone();

    let clip_pipe = renderer.wgpu_device.ensure_clip_rectangle(MASK_FORMAT, true);
    let blur_pipe = renderer.wgpu_device.ensure_brush_blur(MASK_FORMAT);
    let sampler = make_bilinear_sampler(&device);
    let step = 1.0 / extent as f32;

    const MASK: TaskId = 1;
    const BLUR_H: TaskId = 2;
    const BLUR_V: TaskId = 3;

    let mut graph = RenderGraph::new();
    graph.push(Task {
        id: MASK,
        extent: wgpu::Extent3d { width: extent, height: extent, depth_or_array_layers: 1 },
        format: MASK_FORMAT,
        inputs: vec![],
        encode: clip_rectangle_callback(clip_pipe, bounds, radius),
    });
    graph.push(Task {
        id: BLUR_H,
        extent: wgpu::Extent3d { width: extent, height: extent, depth_or_array_layers: 1 },
        format: MASK_FORMAT,
        inputs: vec![MASK],
        encode: blur_pass_callback(blur_pipe.clone(), Arc::clone(&sampler), step, 0.0),
    });
    graph.push(Task {
        id: BLUR_V,
        extent: wgpu::Extent3d { width: extent, height: extent, depth_or_array_layers: 1 },
        format: MASK_FORMAT,
        inputs: vec![BLUR_H],
        encode: blur_pass_callback(blur_pipe, Arc::clone(&sampler), 0.0, step),
    });

    let mut outputs = graph.execute(&device, &queue, HashMap::new());
    let final_mask = outputs.remove(&BLUR_V).expect("BLUR_V output");
    let bytes = renderer.wgpu_device.read_rgba8_texture(&final_mask, extent, extent);
    (Arc::new(final_mask), bytes)
}

fn render_to_bytes(renderer: &Renderer, scene: &Scene) -> Vec<u8> {
    let device = renderer.wgpu_device.core.device.clone();
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p9b target"),
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

// ── Tests ──────────────────────────────────────────────────────────────────

/// After H + V blur, the rounded-rect mask's edge is softer than the
/// raw 9A baseline. Specifically, a pixel that was nearly fully
/// covered (deep inside) before blur should now be slightly attenuated
/// near the edge, and a pixel just outside the rect that was zero
/// before blur should now have non-zero spillover.
#[test]
fn p9b_01_blur_softens_mask_edges() {
    let renderer = make_renderer();
    let bounds = [16.0_f32, 16.0, 48.0, 48.0]; // 32×32 inset
    let radius = 8.0_f32;
    let (_tex, bytes) = render_box_shadow_mask(&renderer, W, bounds, radius);

    // Center remains saturated (interior of a 32×32 rect with 8-px
    // corner radius and a 5-tap separable blur — well-inside pixels
    // stay near 1).
    let center = pixel(&bytes, W, 32, 32);
    assert!(
        center[3] >= 240,
        "center should still be near-1 after blur, got {}",
        center[3]
    );

    // A pixel just outside the original mask boundary picks up
    // blur spillover (was ~0 before blur).
    let just_outside = pixel(&bytes, W, 14, 32);
    assert!(
        just_outside[3] > 5 && just_outside[3] < 250,
        "just-outside pixel should be in the soft edge falloff, got {}",
        just_outside[3]
    );

    // Far outside: still zero (the 5-tap blur only spills 2 px each
    // direction per pass, so distance > ~4 px from edge stays 0).
    let far_outside = pixel(&bytes, W, 0, 0);
    assert!(
        far_outside[3] <= 5,
        "far-outside pixel should remain ~0, got {}",
        far_outside[3]
    );
}

/// Composite the blurred shadow as a tinted image over a black
/// backdrop. Verifies the shadow shape: dark inside the (blurred)
/// rounded-rect region, soft falloff in the halo, black far outside.
///
/// (A rect-on-top-of-shadow composite would need image-behind-rect
/// ordering, which Phase 5 doesn't expose — images always paint in
/// front of rects via family ordering. Phase 11 picture grouping is
/// the natural place to lift that.)
#[test]
fn p9b_02_drop_shadow_composite() {
    let renderer = make_renderer();
    let bounds = [16.0_f32, 16.0, 48.0, 48.0];
    let radius = 8.0_f32;
    let (mask_tex, _bytes) = render_box_shadow_mask(&renderer, W, bounds, radius);

    const SHADOW_KEY: ImageKey = 0xCAFE_9B0F;
    renderer.insert_image_gpu(SHADOW_KEY, mask_tex);

    // Tint the blurred mask dark gray, full coverage. With premultiplied
    // src = mask * (0.3, 0.3, 0.3, 0.999) blended over black:
    //   - Where mask is 1 (interior): result ≈ (0.3, 0.3, 0.3, 1).
    //   - Where mask is in the halo: result fades from (0.3, 0.3, 0.3)
    //     toward black.
    //   - Where mask is 0 (far outside): result is black.
    let mut scene = Scene::new(W, H);
    scene.push_image_full(
        0.0, 0.0, W as f32, H as f32,
        [0.0, 0.0, 1.0, 1.0],
        [0.3, 0.3, 0.3, 0.999],
        SHADOW_KEY,
        0,
        NO_CLIP,
    );

    let bytes = render_to_bytes(&renderer, &scene);

    // Interior: dark gray (mask is 1 here, tint of (0.3, 0.3, 0.3)
    // sRGB-encodes to ~149).
    let center = pixel(&bytes, W, 32, 32);
    assert!(
        center[0] > 130 && center[0] < 165,
        "shadow interior should be dark gray (~149), got {:?}",
        center
    );

    // Halo pixel just outside the original 16-px boundary: the blur
    // softens the edge so this picks up partial coverage. The exact
    // value depends on the 5-tap blur; it should be in the falloff
    // (between near-black and the interior gray).
    let halo = pixel(&bytes, W, 14, 32);
    assert!(
        halo[0] > 5 && halo[0] < 150,
        "halo pixel should be a soft falloff value, got {:?}",
        halo
    );

    // Far-outside: black (no shadow reach).
    let far = pixel(&bytes, W, 0, 0);
    assert!(
        far[0] < 5 && far[1] < 5 && far[2] < 5,
        "far-outside should remain black, got {:?}",
        far
    );
}
