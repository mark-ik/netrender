/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `GpuPass` impl for `WgpuDevice` (P5).
//!
//! Render-pass / draw machinery. Most state-recording methods (bind_*,
//! enable_*, set_blend_*) update fields on `WgpuDevice`; draw methods
//! consume that state to open a render pass + issue a draw + close it.
//!
//! Current approach is "one render pass per draw" — wasteful but
//! correctness-first. Batching multiple draws into a single pass between
//! `bind_draw_target` boundaries is a future optimization.

use api::{ImageDescriptor, ImageFormat, MixBlendMode};
use api::units::{DeviceIntRect, DeviceSize, FramebufferIntRect};
use euclid::default::Transform3D;

use crate::internal_types::Swizzle;

use super::super::traits::{BlendMode, GpuPass, GpuResources, GpuShaders};
use super::super::types::{DepthFunction, TextureFilter, TextureSlot};
use super::WgpuDevice;

impl GpuPass for WgpuDevice {
    fn bind_read_target(&mut self, _target: <Self as GpuResources>::ReadTarget) {
        // Readback path — separate from the draw path. P5+ wires this.
    }
    fn reset_read_target(&mut self) {
        // No-op for now.
    }

    fn bind_draw_target(&mut self, target: <Self as GpuResources>::DrawTarget) {
        // Records the new target. Render passes are opened lazily at draw
        // time using this target's view. If a previous target's draws are
        // pending in the encoder, they're already finalized (each draw is
        // its own pass in the current minimum-viable approach).
        self.current_target = Some(target);
    }

    fn reset_draw_target(&mut self) {
        self.current_target = None;
    }

    fn bind_external_draw_target(&mut self, _fbo_id: <Self as GpuResources>::RenderTargetHandle) {
        // External draw target binding currently not exercised through
        // wgpu. The renderer's call sites for this are GL-specific.
    }

    fn bind_program(&mut self, program: &<Self as GpuShaders>::Program) -> bool {
        // GL-style returns false if program isn't initialized; for wgpu,
        // a program without a linked pipeline is broken usage.
        let pipeline = program.pipeline.borrow();
        let Some(pipeline) = pipeline.as_ref() else {
            return false;
        };
        // wgpu::RenderPipeline is cheap-clone (internally Arc).
        self.bound_pipeline = Some(pipeline.clone());
        self.bound_uniform_buffer = Some(program.uniform_buffer.clone());
        true
    }

    fn set_uniforms(
        &self,
        program: &<Self as GpuShaders>::Program,
        transform: &Transform3D<f32>,
    ) {
        // WrLocals UBO is `mat4 uTransform;` at offset 0. Write the
        // 64-byte matrix directly via queue.write_buffer (deferred to
        // next submission).
        let bytes = transform.to_array();
        // Transform3D::to_array() is [f32; 16] = 64 bytes.
        let byte_slice: &[u8] = unsafe {
            std::slice::from_raw_parts(
                bytes.as_ptr() as *const u8,
                std::mem::size_of_val(&bytes),
            )
        };
        self.queue.write_buffer(&program.uniform_buffer, 0, byte_slice);
    }

    fn set_shader_texture_size(
        &self,
        _program: &<Self as GpuShaders>::Program,
        _texture_size: DeviceSize,
    ) {
        // GL: writes a uTextureSize uniform (used by some shaders for
        // texelFetch coordinate scaling). For wgpu, this would write to
        // a uniform buffer; current corpus doesn't reflect a uTextureSize
        // location, so this is a no-op until an actual user surfaces.
    }

    fn bind_vao(&mut self, vao: &<Self as GpuResources>::Vao) {
        // wgpu::Buffer is cheap-clone.
        self.bound_vertex_buffer = vao.vertex_buffer.borrow().clone();
        self.bound_instance_buffer = vao.instance_buffer.borrow().clone();
        self.bound_index_buffer = vao.index_buffer.borrow().clone();
    }

    fn bind_custom_vao(&mut self, _vao: &<Self as GpuResources>::CustomVao) {
        // Custom VAOs use multi-stream layout; not yet exercised through
        // wgpu (renderer call sites are GL-specific).
    }

    fn bind_texture<S>(
        &mut self,
        slot: S,
        texture: &<Self as GpuResources>::Texture,
        _swizzle: Swizzle,
    )
    where
        S: Into<TextureSlot>,
    {
        // Records (slot → view) for `issue_draw` to consume when building
        // the frag-stage bind group. wgpu::TextureView is cheap-clone
        // (Arc internally). Swizzle is ignored — wgpu has no per-texture
        // swizzle (matches `swizzle_settings()` returning None).
        let slot_idx = slot.into().0;
        self.bound_textures.insert(slot_idx, texture.view.clone());
    }

    fn bind_external_texture<S>(
        &mut self,
        _slot: S,
        _external_texture: &<Self as GpuResources>::ExternalTexture,
    )
    where
        S: Into<TextureSlot>,
    {
        // Same shape as bind_texture; ExternalTexture isn't yet a real
        // type for wgpu (placeholder), so this stays deferred.
    }

    fn clear_target(
        &self,
        color: Option<[f32; 4]>,
        _depth: Option<f32>,
        _rect: Option<FramebufferIntRect>,
    ) {
        // Records pending clear for the next render pass open. Trait
        // takes &self (matches GL); pending_clear is a Cell for safe
        // interior mutability.
        //
        // Trade-off: this means a renderer that calls clear_target
        // without following draws won't see anything happen. WebRender's
        // pattern is always clear-then-draw, so this is fine. If a
        // standalone clear ever surfaces, we'd open + close a pass
        // immediately (requires RefCell on frame_encoder).
        let Some(rgba) = color else { return };
        self.pending_clear.set(Some(wgpu::Color {
            r: rgba[0] as f64,
            g: rgba[1] as f64,
            b: rgba[2] as f64,
            a: rgba[3] as f64,
        }));
    }

    fn enable_depth(&self, _depth_func: DepthFunction) {}
    fn disable_depth(&self) {}
    fn enable_depth_write(&self) {}
    fn disable_depth_write(&self) {}
    fn disable_stencil(&self) {}
    fn set_scissor_rect(&self, _rect: FramebufferIntRect) {}
    fn enable_scissor(&self) {}
    fn disable_scissor(&self) {}
    fn enable_color_write(&self) {}
    fn disable_color_write(&self) {}
    fn set_blend(&mut self, _enable: bool) {}
    fn set_blend_mode(&mut self, _mode: BlendMode) {}

    fn draw_triangles_u16(&mut self, _first_vertex: i32, index_count: i32) {
        // GL signature passes first_vertex as a byte offset into the
        // index buffer; for u16 indices that's first_vertex/2 in element
        // terms. wgpu's draw_indexed handles this via the index range
        // argument. For typical WebRender draws first_vertex is 0;
        // non-zero offsets are P5+ work.
        self.issue_draw_indexed(index_count as u32, 1, wgpu::IndexFormat::Uint16);
    }

    fn draw_triangles_u32(&mut self, _first_vertex: i32, index_count: i32) {
        self.issue_draw_indexed(index_count as u32, 1, wgpu::IndexFormat::Uint32);
    }

    fn draw_indexed_triangles(&mut self, index_count: i32) {
        self.issue_draw_indexed(index_count as u32, 1, wgpu::IndexFormat::Uint16);
    }

    fn draw_indexed_triangles_instanced_u16(
        &mut self,
        index_count: i32,
        instance_count: i32,
    ) {
        self.issue_draw_indexed(
            index_count as u32,
            instance_count.max(1) as u32,
            wgpu::IndexFormat::Uint16,
        );
    }

    fn draw_nonindexed_points(&mut self, _first_vertex: i32, _vertex_count: i32) {
        // Non-indexed draws need a different code path (no index buffer
        // bind). Used for debug primitives only; defer until those land.
    }
    fn draw_nonindexed_lines(&mut self, _first_vertex: i32, _vertex_count: i32) {
        // Same as draw_nonindexed_points; debug-only path.
    }

    fn blit_render_target(
        &mut self,
        _src_target: <Self as GpuResources>::ReadTarget,
        _src_rect: FramebufferIntRect,
        _dest_target: <Self as GpuResources>::DrawTarget,
        _dest_rect: FramebufferIntRect,
        _filter: TextureFilter,
    ) {
        // Blit via copy_texture_to_texture is the wgpu equivalent;
        // implementation deferred.
    }

    fn blit_render_target_invert_y(
        &mut self,
        _src_target: <Self as GpuResources>::ReadTarget,
        _src_rect: FramebufferIntRect,
        _dest_target: <Self as GpuResources>::DrawTarget,
        _dest_rect: FramebufferIntRect,
    ) {}

    fn read_pixels(&mut self, _img_desc: &ImageDescriptor) -> Vec<u8> {
        Vec::new()
    }
    fn read_pixels_into(
        &mut self,
        _rect: FramebufferIntRect,
        _format: ImageFormat,
        _output: &mut [u8],
    ) {}
    fn read_pixels_into_pbo(
        &mut self,
        _read_target: <Self as GpuResources>::ReadTarget,
        _rect: DeviceIntRect,
        _format: ImageFormat,
        _pbo: &<Self as GpuResources>::Pbo,
    ) {}
    fn get_tex_image_into(
        &mut self,
        _texture: &<Self as GpuResources>::Texture,
        _format: ImageFormat,
        _output: &mut [u8],
    ) {}
}

impl WgpuDevice {
    /// Opens a render pass against `current_target`, replays the bound
    /// pipeline + buffers + bind group(s), issues an instanced indexed
    /// draw, closes the pass. One pass per draw — correctness over
    /// performance for the minimum viable P5.
    fn issue_draw_indexed(
        &mut self,
        index_count: u32,
        instance_count: u32,
        index_format: wgpu::IndexFormat,
    ) {
        let Some(target) = self.current_target.as_ref() else {
            // No target bound — skip draw silently. GL would have
            // undefined behavior here too; the renderer is expected to
            // bind_draw_target before drawing.
            return;
        };
        let Some(pipeline) = self.bound_pipeline.as_ref() else {
            return;
        };
        let Some(vertex_buffer) = self.bound_vertex_buffer.as_ref() else {
            return;
        };
        let Some(instance_buffer) = self.bound_instance_buffer.as_ref() else {
            return;
        };
        let Some(index_buffer) = self.bound_index_buffer.as_ref() else {
            return;
        };
        let Some(uniform_buffer) = self.bound_uniform_buffer.as_ref() else {
            return;
        };
        let Some(encoder) = self.frame_encoder.as_mut() else {
            // begin_frame wasn't called; nothing to record into.
            return;
        };

        // Build bind group for the WrLocals UBO. Auto-derived layout
        // exposes one BindGroupLayout per stage (vert→set 0, frag→set 1
        // per gen_spirv's per-stage descriptor set assignment). For
        // ps_clear, only set 0 (vert) has bindings — the WrLocals UBO
        // at binding 0.
        let bgl0 = pipeline.get_bind_group_layout(0);
        let bind_group_0 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("WgpuDevice issue_draw set 0"),
            layout: &bgl0,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        // Consume any pending clear color (set by clear_target). If the
        // renderer cleared before drawing, this pass starts with a
        // LoadOp::Clear; otherwise it preserves prior content via Load.
        let load_op = match self.pending_clear.take() {
            Some(color) => wgpu::LoadOp::Clear(color),
            None => wgpu::LoadOp::Load,
        };

        // Build set 1 (frag stage textures + samplers) when textures
        // are bound. Empty Vec when none — wgpu accepts no-set-1 only
        // if the pipeline doesn't declare set 1, which matches ps_clear
        // (no textures). Textured shaders must have textures bound.
        let set1_layout;
        let bind_group_1;
        let needs_set1 = !self.bound_textures.is_empty();
        if needs_set1 {
            // Image at binding 2*i, sampler at binding 2*i+1, per
            // gen_spirv's split-then-renumber convention. We bind
            // every (image, sampler) pair — naga's auto-derived layout
            // exposes both.
            let sampler = self.default_sampler.as_ref().expect("default sampler");
            let mut entries: Vec<wgpu::BindGroupEntry<'_>> = Vec::new();
            // Sort slots so the binding order is deterministic.
            let mut slots: Vec<&usize> = self.bound_textures.keys().collect();
            slots.sort();
            for &slot in &slots {
                let view = self.bound_textures.get(slot).unwrap();
                let image_binding = (*slot as u32) * 2;
                let sampler_binding = image_binding + 1;
                entries.push(wgpu::BindGroupEntry {
                    binding: image_binding,
                    resource: wgpu::BindingResource::TextureView(view),
                });
                entries.push(wgpu::BindGroupEntry {
                    binding: sampler_binding,
                    resource: wgpu::BindingResource::Sampler(sampler),
                });
            }
            set1_layout = pipeline.get_bind_group_layout(1);
            bind_group_1 = Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("WgpuDevice issue_draw set 1"),
                layout: &set1_layout,
                entries: &entries,
            }));
        } else {
            bind_group_1 = None;
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("WgpuDevice draw"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target.view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: load_op,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group_0, &[]);
            if let Some(bg1) = bind_group_1.as_ref() {
                pass.set_bind_group(1, bg1, &[]);
            }
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.set_vertex_buffer(1, instance_buffer.slice(..));
            pass.set_index_buffer(index_buffer.slice(..), index_format);
            pass.draw_indexed(0..index_count, 0, 0..instance_count);
        }
    }
}
