/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 4 / Phase 5 batch builders.
//!
//! **Rects** (Phase 4): classifies, sorts, and uploads solid-color rects.
//! Opaques → front-to-back, depth write ON. Alphas → painter order, blend.
//!
//! **Images** (Phase 5): classifies and uploads textured rects.
//! Same depth / blend policy as rects. Grouped by `ImageKey` so each
//! unique texture gets exactly one `DrawIntent` per pipeline variant.
//!
//! Z assignment — unified across rects and images (images are "on top"):
//!   N_total = N_rects + N_images
//!   Rect at painter index i  → z = (N_total − i)        / (N_total + 1)
//!   Image at painter index j → z = (N_total − N_rects − j) / (N_total + 1)
//! Front rects (high painter index) → small z (near). Back → large z (far).

use std::collections::HashMap;

use netrender_device::{
    BrushGradientPipeline, BrushImagePipeline, BrushRectSolidPipeline, DrawIntent, GradientKind,
};

use crate::image_cache::ImageCache;
use crate::scene::{ImageKey, Scene};

// ── Shared frame resources ────────────────────────────────────────────

/// GPU buffers that are identical for every draw call in a frame:
/// the transform palette and the orthographic per-frame uniform.
/// Built once in `prepare()` and passed by reference to both batch
/// builders so each frame allocates exactly two shared buffers instead
/// of one pair per batch type.
pub(crate) struct FrameResources {
    pub transforms: wgpu::Buffer,
    pub per_frame: wgpu::Buffer,
}

impl FrameResources {
    pub fn new(scene: &Scene, device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self {
            transforms: make_transforms_buf(scene, device, queue),
            per_frame: make_per_frame_buf(scene, device, queue),
        }
    }
}

// ── Rect batch ────────────────────────────────────────────────────────

/// Build all [`DrawIntent`]s for solid-color rects in `scene`.
/// Opaques first (front-to-back), then alphas (back-to-front).
pub(crate) fn build_rect_batch(
    scene: &Scene,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    opaque_pipe: &BrushRectSolidPipeline,
    alpha_pipe: &BrushRectSolidPipeline,
    frame_res: &FrameResources,
) -> Vec<DrawIntent> {
    if scene.rects.is_empty() {
        return Vec::new();
    }

    // Unified depth range shared with image + gradient batches.
    let n_total = scene.rects.len() + scene.images.len() + scene.gradients.len();

    let mut opaque_order: Vec<(usize, f32)> = Vec::new();
    let mut alpha_order: Vec<(usize, f32)> = Vec::new();

    for (i, r) in scene.rects.iter().enumerate() {
        let z = (n_total - i) as f32 / (n_total + 1) as f32;
        if r.color[3] >= 1.0 {
            opaque_order.push((i, z));
        } else {
            alpha_order.push((i, z));
        }
    }

    // Opaques: ascending z = front first → early-Z benefit.
    opaque_order.sort_unstable_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

    let build_batch = |order: &[(usize, f32)], pipe: &BrushRectSolidPipeline| -> DrawIntent {
        let instance_count = order.len() as u32;
        let mut bytes: Vec<u8> = Vec::with_capacity(order.len() * 64);
        for &(idx, z) in order {
            let r = &scene.rects[idx];
            for f in [r.x0, r.y0, r.x1, r.y1] {
                bytes.extend_from_slice(&f.to_ne_bytes());
            }
            for f in r.color {
                bytes.extend_from_slice(&f.to_ne_bytes());
            }
            for f in r.clip_rect {
                bytes.extend_from_slice(&f.to_ne_bytes());
            }
            bytes.extend_from_slice(&r.transform_id.to_ne_bytes());
            bytes.extend_from_slice(&z.to_ne_bytes());
            bytes.extend_from_slice(&[0u8; 8]); // padding → 64 bytes
        }
        let instances_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("brush_rect_solid instances"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&instances_buf, 0, &bytes);

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("brush_rect_solid bind group"),
            layout: &pipe.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: instances_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: frame_res.transforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: frame_res.per_frame.as_entire_binding() },
            ],
        });

        DrawIntent {
            pipeline: pipe.pipeline.clone(),
            bind_group,
            vertex_buffers: vec![],
            vertex_range: 0..4,
            instance_range: 0..instance_count,
            dynamic_offsets: Vec::new(),
            push_constants: Vec::new(),
        }
    };

    let mut draws = Vec::new();
    if !opaque_order.is_empty() {
        draws.push(build_batch(&opaque_order, opaque_pipe));
    }
    if !alpha_order.is_empty() {
        draws.push(build_batch(&alpha_order, alpha_pipe));
    }
    draws
}

// ── Image batch ───────────────────────────────────────────────────────

/// Build all [`DrawIntent`]s for textured-rect (`SceneImage`) entries.
/// Opaques first (front-to-back, grouped by key), then alphas (painter
/// order, grouped by key). Returns empty vec when `scene.images` is empty.
pub(crate) fn build_image_batch(
    scene: &Scene,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    opaque_pipe: &BrushImagePipeline,
    alpha_pipe: &BrushImagePipeline,
    image_cache: &ImageCache,
    sampler: &wgpu::Sampler,
    frame_res: &FrameResources,
) -> Vec<DrawIntent> {
    if scene.images.is_empty() {
        return Vec::new();
    }

    let n_rects = scene.rects.len();
    let n_total = n_rects + scene.images.len() + scene.gradients.len();

    // Classify: (painter_index_j, z, key)
    let mut opaque_items: Vec<(usize, f32, ImageKey)> = Vec::new();
    let mut alpha_items: Vec<(usize, f32, ImageKey)> = Vec::new();

    for (j, img) in scene.images.iter().enumerate() {
        let global_idx = n_rects + j;
        let z = (n_total - global_idx) as f32 / (n_total + 1) as f32;
        if img.color[3] >= 1.0 {
            opaque_items.push((j, z, img.key));
        } else {
            alpha_items.push((j, z, img.key));
        }
    }

    // Opaques: sort front-to-back (ascending z).
    opaque_items.sort_unstable_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    // Alphas: keep original painter order.

    let mut draws = Vec::new();
    emit_image_draws(
        &opaque_items, scene, device, queue, opaque_pipe,
        image_cache, sampler, frame_res, &mut draws,
    );
    emit_image_draws(
        &alpha_items, scene, device, queue, alpha_pipe,
        image_cache, sampler, frame_res, &mut draws,
    );
    draws
}

/// Emit one [`DrawIntent`] per unique `ImageKey` in `items`, maintaining
/// the relative ordering of instances within each key group.
fn emit_image_draws(
    items: &[(usize, f32, ImageKey)],
    scene: &Scene,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipe: &BrushImagePipeline,
    image_cache: &ImageCache,
    sampler: &wgpu::Sampler,
    frame_res: &FrameResources,
    out: &mut Vec<DrawIntent>,
) {
    if items.is_empty() {
        return;
    }

    // Group by key, preserving first-seen order (use Vec as ordered map).
    let mut groups: Vec<(ImageKey, Vec<(usize, f32)>)> = Vec::new();
    let mut key_to_group: HashMap<ImageKey, usize> = HashMap::new();
    for &(j, z, key) in items {
        if let Some(&gi) = key_to_group.get(&key) {
            groups[gi].1.push((j, z));
        } else {
            let gi = groups.len();
            key_to_group.insert(key, gi);
            groups.push((key, vec![(j, z)]));
        }
    }

    for (key, group_items) in &groups {
        let texture = match image_cache.get(*key) {
            Some(t) => t,
            None => continue, // key registered but not yet uploaded; skip
        };
        let tex_view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Build 80-byte-stride instance buffer.
        let instance_count = group_items.len() as u32;
        let mut bytes: Vec<u8> = Vec::with_capacity(group_items.len() * 80);
        for &(j, z) in group_items {
            let img = &scene.images[j];
            // rect (16 bytes)
            for f in [img.x0, img.y0, img.x1, img.y1] {
                bytes.extend_from_slice(&f.to_ne_bytes());
            }
            // uv_rect (16 bytes)
            for f in img.uv {
                bytes.extend_from_slice(&f.to_ne_bytes());
            }
            // color (16 bytes)
            for f in img.color {
                bytes.extend_from_slice(&f.to_ne_bytes());
            }
            // clip (16 bytes)
            for f in img.clip_rect {
                bytes.extend_from_slice(&f.to_ne_bytes());
            }
            // transform_id (4 bytes)
            bytes.extend_from_slice(&img.transform_id.to_ne_bytes());
            // z_depth (4 bytes)
            bytes.extend_from_slice(&z.to_ne_bytes());
            // padding (8 bytes) → stride 80
            bytes.extend_from_slice(&[0u8; 8]);
        }

        let instances_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("brush_image instances"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&instances_buf, 0, &bytes);

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("brush_image bind group"),
            layout: &pipe.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: instances_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: frame_res.transforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: frame_res.per_frame.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&tex_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });

        out.push(DrawIntent {
            pipeline: pipe.pipeline.clone(),
            bind_group,
            vertex_buffers: vec![],
            vertex_range: 0..4,
            instance_range: 0..instance_count,
            dynamic_offsets: Vec::new(),
            push_constants: Vec::new(),
        });
    }
}

// ── Gradient batch (Phase 8D unified linear / radial / conic, N-stop) ─

/// Pipelines for the unified analytic gradient family. The renderer
/// builds one of these per `prepare()` call (or per
/// `render_dirty_tiles` call) and hands it to `build_gradient_batch`.
/// `[GradientKind::Linear as usize]` indexes into each array.
pub(crate) struct GradientPipelines {
    pub opaque: [BrushGradientPipeline; 3],
    pub alpha: [BrushGradientPipeline; 3],
}

/// Build all [`DrawIntent`]s for the analytic gradients in `scene`.
///
/// Phase 8D consolidates the three Phase 8A-C builders into one. The
/// stops storage buffer is built once per call from every gradient's
/// `stops` vec; per-instance `(stops_offset, stops_count)` indexes
/// into it. Within the gradient list the builder walks scene push
/// order, grouping consecutive entries with the same `(kind,
/// alpha_class)` into one `DrawIntent`. A push sequence that
/// interleaves families (linear → radial → linear) emits three draws
/// — painter order is preserved across kinds, fixing the Phase 8A-C
/// family-grouped limitation.
///
/// Z assignment: gradients occupy painter indices `[n_rects +
/// n_images, n_total)`. Front-most primitives (any family) get the
/// smallest z and win the depth test against earlier-drawn batches.
pub(crate) fn build_gradient_batch(
    scene: &Scene,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipes: &GradientPipelines,
    frame_res: &FrameResources,
) -> Vec<DrawIntent> {
    if scene.gradients.is_empty() {
        return Vec::new();
    }

    let n_rects = scene.rects.len();
    let n_images = scene.images.len();
    let n_total = n_rects + n_images + scene.gradients.len();

    // Build the per-frame stops storage buffer. Stride 32: vec4 color
    // (16) + vec4 offset_pad (16, .x = position).
    let mut stops_bytes: Vec<u8> = Vec::new();
    let mut stop_ranges: Vec<(u32, u32)> = Vec::with_capacity(scene.gradients.len());
    for grad in &scene.gradients {
        let offset = (stops_bytes.len() / 32) as u32;
        let count = grad.stops.len() as u32;
        stop_ranges.push((offset, count));
        for stop in &grad.stops {
            for f in stop.color {
                stops_bytes.extend_from_slice(&f.to_ne_bytes());
            }
            stops_bytes.extend_from_slice(&stop.offset.to_ne_bytes());
            stops_bytes.extend_from_slice(&[0u8; 12]); // pad to vec4
        }
    }
    if stops_bytes.is_empty() {
        return Vec::new();
    }
    let stops_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("brush_gradient stops"),
        size: stops_bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&stops_buf, 0, &stops_bytes);

    // Group consecutive same-(kind, alpha_class) entries to preserve
    // painter order across kinds while batching adjacent same-shape
    // primitives into single draws.
    let mut groups: Vec<(GradientKind, bool, Vec<usize>)> = Vec::new();
    for (i, grad) in scene.gradients.iter().enumerate() {
        let is_alpha = grad.stops.iter().any(|s| s.color[3] < 1.0);
        if let Some(last) = groups.last_mut() {
            if last.0 == grad.kind && last.1 == is_alpha {
                last.2.push(i);
                continue;
            }
        }
        groups.push((grad.kind, is_alpha, vec![i]));
    }

    let mut draws = Vec::with_capacity(groups.len());
    for (kind, is_alpha, indices) in &groups {
        let pipe = if *is_alpha {
            &pipes.alpha[kind.as_u32() as usize]
        } else {
            &pipes.opaque[kind.as_u32() as usize]
        };

        // Instance buffer: 64-byte stride.
        let instance_count = indices.len() as u32;
        let mut bytes: Vec<u8> = Vec::with_capacity(indices.len() * 64);
        for &idx in indices {
            let g = &scene.gradients[idx];
            let global_idx = n_rects + n_images + idx;
            let z = (n_total - global_idx) as f32 / (n_total + 1) as f32;
            let (stops_offset, stops_count) = stop_ranges[idx];
            // rect (16)
            for f in [g.x0, g.y0, g.x1, g.y1] {
                bytes.extend_from_slice(&f.to_ne_bytes());
            }
            // params (16)
            for f in g.params {
                bytes.extend_from_slice(&f.to_ne_bytes());
            }
            // clip (16)
            for f in g.clip_rect {
                bytes.extend_from_slice(&f.to_ne_bytes());
            }
            // tail: transform_id (4) + z_depth (4) + stops_offset (4) + stops_count (4)
            bytes.extend_from_slice(&g.transform_id.to_ne_bytes());
            bytes.extend_from_slice(&z.to_ne_bytes());
            bytes.extend_from_slice(&stops_offset.to_ne_bytes());
            bytes.extend_from_slice(&stops_count.to_ne_bytes());
        }
        let instances_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("brush_gradient instances"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&instances_buf, 0, &bytes);

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("brush_gradient bind group"),
            layout: &pipe.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: instances_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: frame_res.transforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: frame_res.per_frame.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: stops_buf.as_entire_binding() },
            ],
        });

        draws.push(DrawIntent {
            pipeline: pipe.pipeline.clone(),
            bind_group,
            vertex_buffers: vec![],
            vertex_range: 0..4,
            instance_range: 0..instance_count,
            dynamic_offsets: Vec::new(),
            push_constants: Vec::new(),
        });
    }

    draws
}

// ── Shared buffer helpers ─────────────────────────────────────────────

pub(crate) fn make_transforms_buf(
    scene: &Scene,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> wgpu::Buffer {
    let mut bytes: Vec<u8> = Vec::with_capacity(scene.transforms.len() * 64);
    for t in &scene.transforms {
        for f in &t.m {
            bytes.extend_from_slice(&f.to_ne_bytes());
        }
    }
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("brush_* transforms"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buf, 0, &bytes);
    buf
}

fn make_per_frame_buf(
    scene: &Scene,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> wgpu::Buffer {
    make_per_frame_buf_for_rect(
        [0.0, 0.0, scene.viewport_width as f32, scene.viewport_height as f32],
        device,
        queue,
    )
}

/// Build a `per_frame` uniform whose orthographic projection maps
/// `world_rect = [x0, y0, x1, y1]` to NDC `(-1, +1)`–`(+1, -1)`.
///
/// For the full viewport this produces the same buffer as `make_per_frame_buf`;
/// Phase 7B uses the per-rect form to render each tile with a tile-local
/// projection so the existing brush pipelines can be reused unchanged.
pub(crate) fn make_per_frame_buf_for_rect(
    world_rect: [f32; 4],
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> wgpu::Buffer {
    let [x0, y0, x1, y1] = world_rect;
    let w = x1 - x0;
    let h = y1 - y0;
    // Column-major: x_ndc = 2*(x-x0)/w - 1, y_ndc = -2*(y-y0)/h + 1
    #[rustfmt::skip]
    let proj: [f32; 16] = [
        2.0 / w,            0.0,             0.0, 0.0,
        0.0,               -2.0 / h,         0.0, 0.0,
        0.0,                0.0,             1.0, 0.0,
       -2.0 * x0 / w - 1.0, 2.0 * y0 / h + 1.0, 0.0, 1.0,
    ];
    let bytes: Vec<u8> = proj.iter().flat_map(|f| f.to_ne_bytes()).collect();
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("brush_* per_frame (tile-local)"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buf, 0, &bytes);
    buf
}
