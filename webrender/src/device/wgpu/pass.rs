/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Render-pass batching. Ingests `DrawIntent`s; flushes per pass; one
//! `BeginRenderPass` per target switch. See plan §4.8, §6 S1 and the
//! renderer-body adapter plan §A2.X (foundational pass encoding).

use std::ops::Range;

/// Colour attachment policy for one wgpu render pass. This is the
/// wgpu-native replacement for the renderer body's current
/// "bind draw target, then maybe clear" flow: the load operation is
/// declared when the pass begins, not as mutable device state.
pub struct ColorAttachment<'a> {
    pub view: &'a wgpu::TextureView,
    pub load: wgpu::LoadOp<wgpu::Color>,
    pub store: wgpu::StoreOp,
}

impl<'a> ColorAttachment<'a> {
    pub fn clear(view: &'a wgpu::TextureView, color: wgpu::Color) -> Self {
        Self {
            view,
            load: wgpu::LoadOp::Clear(color),
            store: wgpu::StoreOp::Store,
        }
    }

    pub fn load(view: &'a wgpu::TextureView) -> Self {
        Self {
            view,
            load: wgpu::LoadOp::Load,
            store: wgpu::StoreOp::Store,
        }
    }
}

/// Depth attachment policy for one wgpu render pass. This carries the
/// native replacement for `clear_target(..., Some(depth), ...)` and
/// `invalidate_depth_target()`: load and store behavior are declared
/// with the pass, not patched as mutable device state afterward.
pub struct DepthAttachment<'a> {
    pub view: &'a wgpu::TextureView,
    pub load: wgpu::LoadOp<f32>,
    pub store: wgpu::StoreOp,
}

impl<'a> DepthAttachment<'a> {
    pub fn clear(view: &'a wgpu::TextureView, depth: f32) -> Self {
        Self {
            view,
            load: wgpu::LoadOp::Clear(depth),
            store: wgpu::StoreOp::Store,
        }
    }

    pub fn load(view: &'a wgpu::TextureView) -> Self {
        Self {
            view,
            load: wgpu::LoadOp::Load,
            store: wgpu::StoreOp::Store,
        }
    }

    pub fn discard(mut self) -> Self {
        self.store = wgpu::StoreOp::Discard;
        self
    }
}

/// Target description for a single wgpu render pass. A2.X migrates
/// renderer callsites toward constructing this value from render-task
/// targets instead of binding GL FBO state on the device.
pub struct RenderPassTarget<'a> {
    pub label: &'a str,
    pub color: ColorAttachment<'a>,
    pub depth: Option<DepthAttachment<'a>>,
}

/// Recorded but not-yet-executed draw. Display-list traversal records
/// these into per-pass buckets; `flush_pass` flips them into wgpu calls
/// inside a single render-pass scope (per §4.8 — record, never execute
/// inline).
///
/// Carries pipeline + bind-group + buffer references by value: wgpu 29
/// handle types are `Clone` (Arc-wrapped internally), so per-draw
/// cloning is cheap. Multi-pipeline passes work by recording draws
/// with different `pipeline` values; `flush_pass` calls `set_pipeline`
/// per draw and lets wgpu de-dup redundant binds at the encoder level.
#[derive(Clone)]
pub struct DrawIntent {
    pub pipeline: wgpu::RenderPipeline,
    pub bind_group: wgpu::BindGroup,
    /// Vertex buffers bound at slots `0..N` before the draw (full-range).
    /// Empty if the pipeline reads only built-in inputs (e.g. derives
    /// corners from `vertex_index`).
    pub vertex_buffers: Vec<wgpu::Buffer>,
    pub vertex_range: Range<u32>,
    pub instance_range: Range<u32>,
    /// Dynamic offsets into bind-group entries that have
    /// `has_dynamic_offset: true` (per §4.7). The length of this slice
    /// must match the count of dynamic-offset entries in the bind
    /// group's layout. Empty if the bind group has no dynamic offsets.
    pub dynamic_offsets: Vec<u32>,
    /// Push-constant payload (per §4.7); stage VERTEX. Empty if the
    /// pipeline has no push-constant range.
    pub push_constants: Vec<u8>,
}

/// Flush a list of draw intents into a single render pass.
/// One `BeginRenderPass` per call; pipeline switches inside the pass
/// happen per-draw (a draw's `pipeline` field). Colour and depth load
/// / store policy lives on `RenderPassTarget`, matching wgpu's pass
/// model instead of GL's mutable framebuffer state.
pub fn flush_pass(
    encoder: &mut wgpu::CommandEncoder,
    target: RenderPassTarget<'_>,
    draws: &[DrawIntent],
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(target.label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target.color.view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: target.color.load,
                store: target.color.store,
            },
        })],
        depth_stencil_attachment: target.depth.as_ref().map(|depth| {
            wgpu::RenderPassDepthStencilAttachment {
                view: depth.view,
                depth_ops: Some(wgpu::Operations {
                    load: depth.load,
                    store: depth.store,
                }),
                stencil_ops: None,
            }
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    for draw in draws {
        pass.set_pipeline(&draw.pipeline);
        pass.set_bind_group(0, &draw.bind_group, &draw.dynamic_offsets);
        for (slot, vb) in draw.vertex_buffers.iter().enumerate() {
            pass.set_vertex_buffer(slot as u32, vb.slice(..));
        }
        if !draw.push_constants.is_empty() {
            // wgpu 29: `set_immediates(offset, data)` — stage is fixed
            // by the pipeline's `immediate_size` declaration; no stage
            // arg here.
            pass.set_immediates(0, &draw.push_constants);
        }
        pass.draw(draw.vertex_range.clone(), draw.instance_range.clone());
    }
}
