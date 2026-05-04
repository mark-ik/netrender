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
    BrushGradientPipeline, BrushImagePipeline, BrushRectSolidPipeline, BrushTextPipeline,
    DrawIntent, GradientKind,
};

use crate::glyph_atlas::GlyphAtlas;
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

/// Optional per-prim filter passed to a batch builder. When `None`,
/// all primitives in the relevant scene Vec are emitted. When
/// `Some(f)`, only indices for which `f(i)` returns `true` are
/// included — used by the per-tile rendering path to skip primitives
/// whose AABB doesn't intersect the tile being rendered.
///
/// Crucially, the global `n_total` (= total primitive count across
/// rects + images + gradients) and the resulting z values are
/// independent of the filter, so the same primitive gets the same z
/// in every tile it appears in.
pub(crate) type PrimFilter<'a> = Option<&'a dyn Fn(usize) -> bool>;

/// Build all [`DrawIntent`]s for solid-color rects in `scene`.
/// Opaques first (front-to-back), then alphas (back-to-front).
pub(crate) fn build_rect_batch(
    scene: &Scene,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    opaque_pipe: &BrushRectSolidPipeline,
    alpha_pipe: &BrushRectSolidPipeline,
    frame_res: &FrameResources,
    filter: PrimFilter<'_>,
) -> Vec<DrawIntent> {
    if scene.rects.is_empty() {
        return Vec::new();
    }

    // Unified depth range shared with image + gradient + text batches.
    let n_total = scene.rects.len()
        + scene.images.len()
        + scene.gradients.len()
        + scene.texts.len();

    let mut opaque_order: Vec<(usize, f32)> = Vec::new();
    let mut alpha_order: Vec<(usize, f32)> = Vec::new();

    for (i, r) in scene.rects.iter().enumerate() {
        if let Some(f) = filter {
            if !f(i) {
                continue;
            }
        }
        let z = (n_total - i) as f32 / (n_total + 1) as f32;
        if r.color[3] >= 1.0 {
            opaque_order.push((i, z));
        } else {
            alpha_order.push((i, z));
        }
    }

    if opaque_order.is_empty() && alpha_order.is_empty() {
        return Vec::new();
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
    filter: PrimFilter<'_>,
) -> Vec<DrawIntent> {
    if scene.images.is_empty() {
        return Vec::new();
    }

    let n_rects = scene.rects.len();
    let n_total = n_rects + scene.images.len() + scene.gradients.len() + scene.texts.len();

    // Classify: (painter_index_j, z, key)
    let mut opaque_items: Vec<(usize, f32, ImageKey)> = Vec::new();
    let mut alpha_items: Vec<(usize, f32, ImageKey)> = Vec::new();

    for (j, img) in scene.images.iter().enumerate() {
        if let Some(f) = filter {
            if !f(j) {
                continue;
            }
        }
        let global_idx = n_rects + j;
        let z = (n_total - global_idx) as f32 / (n_total + 1) as f32;
        if img.color[3] >= 1.0 {
            opaque_items.push((j, z, img.key));
        } else {
            alpha_items.push((j, z, img.key));
        }
    }

    if opaque_items.is_empty() && alpha_items.is_empty() {
        return Vec::new();
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
            write_image_instance(
                &mut bytes,
                [img.x0, img.y0, img.x1, img.y1],
                img.uv,
                img.color,
                img.clip_rect,
                img.transform_id,
                z,
            );
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
    filter: PrimFilter<'_>,
) -> Vec<DrawIntent> {
    if scene.gradients.is_empty() {
        return Vec::new();
    }

    let n_rects = scene.rects.len();
    let n_images = scene.images.len();
    let n_total = n_rects + n_images + scene.gradients.len() + scene.texts.len();

    // Build the per-frame stops storage buffer for the gradients that
    // pass the filter. Stride 32: vec4 color (16) + vec4 offset_pad
    // (16, .x = position). Filtered-out gradients still consume an
    // entry in `stop_ranges` (count = 0) so painter-index lookup by
    // global vec position stays valid.
    let mut stops_bytes: Vec<u8> = Vec::new();
    let mut stop_ranges: Vec<(u32, u32)> = Vec::with_capacity(scene.gradients.len());
    for (i, grad) in scene.gradients.iter().enumerate() {
        if let Some(f) = filter {
            if !f(i) {
                stop_ranges.push((0, 0));
                continue;
            }
        }
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
    // primitives into single draws. Filter-rejected entries don't
    // contribute to any group; they leave a "gap" that breaks
    // batch-coalescing across them, which is fine — correctness wins.
    let mut groups: Vec<(GradientKind, bool, Vec<usize>)> = Vec::new();
    for (i, grad) in scene.gradients.iter().enumerate() {
        if let Some(f) = filter {
            if !f(i) {
                continue;
            }
        }
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

// ── Text batch (Phase 10a.1 grayscale, 10b.3 transform-aware) ─────────

/// Per-run dispatch decision supplied by the renderer to
/// [`build_text_batch`]. The renderer computes one of these per
/// `SceneText`; the batch builder mechanically buckets runs into
/// pipeline + writes their z value into per-glyph instances.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TextRunDispatch {
    /// Phase 10b.3: route this run through the dual-source subpixel
    /// pipeline (when `subpixel_pipe` is available). Falls back to
    /// grayscale otherwise.
    pub use_subpixel: bool,
    /// Per-run depth value. The renderer chooses this differently
    /// for direct vs. tiled paths:
    /// - Direct path: text z fits inside the unified painter-order z
    ///   range (text emits last so it gets the smallest z values,
    ///   front-most).
    /// - Tiled path (B): composite tiles draw at z=0.5; text z must
    ///   be `< 0.5` so text overlays the composited tiles.
    pub z: f32,
}

/// Build all [`DrawIntent`]s for `scene.texts`. Each text run becomes
/// per-quad instances in one of two pipeline buckets (grayscale vs.
/// subpixel-AA dual-source); we emit one `DrawIntent` per non-empty
/// bucket. Glyph instances inside a run are emitted in the order they
/// appear (left-to-right for shaped runs).
///
/// Per-run dispatch comes from the caller via `dispatch_per_run`
/// (length must equal `scene.texts.len()`). All glyphs in one run
/// share the run's `z` (text glyphs don't overlap each other
/// meaningfully within a single run). Per-run `filter` is honored at
/// the run level — filtered-out runs produce no quads. The batch
/// builder skips any run whose glyph keys haven't been uploaded to
/// the atlas yet (tests that forget `set_glyph_raster` will simply
/// render nothing rather than crash).
pub(crate) fn build_text_batch(
    scene: &Scene,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    grayscale_pipe: &BrushTextPipeline,
    subpixel_pipe: Option<&BrushTextPipeline>,
    atlas: &GlyphAtlas,
    sampler: &wgpu::Sampler,
    frame_res: &FrameResources,
    filter: PrimFilter<'_>,
    dispatch_per_run: &[TextRunDispatch],
) -> Vec<DrawIntent> {
    if scene.texts.is_empty() {
        return Vec::new();
    }
    debug_assert_eq!(
        dispatch_per_run.len(),
        scene.texts.len(),
        "dispatch_per_run length must match scene.texts",
    );

    let mut grayscale_bytes: Vec<u8> = Vec::new();
    let mut grayscale_count: u32 = 0;
    let mut subpixel_bytes: Vec<u8> = Vec::new();
    let mut subpixel_count: u32 = 0;

    for (i, run) in scene.texts.iter().enumerate() {
        if let Some(f) = filter {
            if !f(i) {
                continue;
            }
        }
        let dispatch = dispatch_per_run[i];

        // Bucket: subpixel when the renderer opted this run in AND the
        // dual-source pipeline is available. Otherwise grayscale.
        let want_subpixel = dispatch.use_subpixel && subpixel_pipe.is_some();
        let (bytes, count) = if want_subpixel {
            (&mut subpixel_bytes, &mut subpixel_count)
        } else {
            (&mut grayscale_bytes, &mut grayscale_count)
        };

        for g in &run.glyphs {
            let slot = match atlas.get(g.key) {
                Some(s) => s,
                None => continue, // raster not registered — skip silently
            };
            // Pen position + bearings → device-pixel quad.
            let x0 = g.x + slot.bearing_x as f32;
            let y0 = g.y - slot.bearing_y as f32;
            let x1 = x0 + slot.width as f32;
            let y1 = y0 + slot.height as f32;
            write_image_instance(
                bytes,
                [x0, y0, x1, y1],
                slot.uv_rect,
                run.color,
                run.clip_rect,
                run.transform_id,
                dispatch.z,
            );
            *count += 1;
        }
    }
    if grayscale_count == 0 && subpixel_count == 0 {
        return Vec::new();
    }

    let atlas_tex = atlas.texture();
    let atlas_view = atlas_tex.create_view(&wgpu::TextureViewDescriptor::default());

    let mut draws = Vec::with_capacity(2);
    if grayscale_count > 0 {
        draws.push(make_text_draw(
            "ps_text_run grayscale",
            grayscale_pipe,
            &grayscale_bytes,
            grayscale_count,
            &atlas_view,
            sampler,
            frame_res,
            device,
            queue,
        ));
    }
    if subpixel_count > 0 {
        let pipe = subpixel_pipe
            .expect("subpixel_count > 0 implies subpixel_pipe was Some when bucketing");
        draws.push(make_text_draw(
            "ps_text_run subpixel",
            pipe,
            &subpixel_bytes,
            subpixel_count,
            &atlas_view,
            sampler,
            frame_res,
            device,
            queue,
        ));
    }
    draws
}

/// Build one text DrawIntent for a non-empty bucket of glyph instances.
/// Both pipelines (grayscale + dual-source) share the
/// `ps_text_run_layout` bind group layout, so the bind group shape is
/// the same — only the pipeline pointer (and its blend state) differs.
fn make_text_draw(
    label: &'static str,
    pipe: &BrushTextPipeline,
    bytes: &[u8],
    instance_count: u32,
    atlas_view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    frame_res: &FrameResources,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> DrawIntent {
    let instances_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&instances_buf, 0, bytes);

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ps_text_run bind group"),
        layout: &pipe.layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: instances_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: frame_res.transforms.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: frame_res.per_frame.as_entire_binding() },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(atlas_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
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
}

// ── Shared instance writers ───────────────────────────────────────────

/// Append one `ImageInstance` (80-byte stride, std430) to `bytes`.
/// The single struct definition lives here so the layout is shared
/// between `emit_image_draws` (user image batch) and the tile cache's
/// `build_tile_composite_draw` (Phase 7C composite).
///
/// Layout: rect (16) + uv (16) + color (16) + clip (16) + transform_id
/// (4) + z_depth (4) + 8 bytes padding. Matches `brush_image.wgsl`.
pub(crate) fn write_image_instance(
    bytes: &mut Vec<u8>,
    rect: [f32; 4],
    uv: [f32; 4],
    color: [f32; 4],
    clip: [f32; 4],
    transform_id: u32,
    z_depth: f32,
) {
    for f in rect {
        bytes.extend_from_slice(&f.to_ne_bytes());
    }
    for f in uv {
        bytes.extend_from_slice(&f.to_ne_bytes());
    }
    for f in color {
        bytes.extend_from_slice(&f.to_ne_bytes());
    }
    for f in clip {
        bytes.extend_from_slice(&f.to_ne_bytes());
    }
    bytes.extend_from_slice(&transform_id.to_ne_bytes());
    bytes.extend_from_slice(&z_depth.to_ne_bytes());
    bytes.extend_from_slice(&[0u8; 8]);
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
