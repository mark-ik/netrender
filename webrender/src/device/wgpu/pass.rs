/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `GpuPass` impl for `WgpuDevice` (P5 — currently all stubs).
//!
//! Per-pass binding, state, draw commands, blits, readback. wgpu's
//! render-pass model (begin → record → end → submit) doesn't map 1:1
//! to GL's draw-as-you-go pattern; P5 introduces per-frame command
//! encoder lifecycle + bind-group construction at draw time.
//!
//! See FBO design discussion: P5 will likely implement Option II —
//! `WgpuDrawTarget` enum carries the `Arc<wgpu::TextureView>` directly
//! instead of looking up via `WgpuRenderTargetHandle`.

use api::{ImageDescriptor, ImageFormat, MixBlendMode};
use api::units::{DeviceIntRect, DeviceSize, FramebufferIntRect};
use euclid::default::Transform3D;

use crate::internal_types::Swizzle;

use super::super::traits::{BlendMode, GpuPass, GpuResources, GpuShaders};
use super::super::types::{DepthFunction, TextureFilter, TextureSlot};
use super::WgpuDevice;

impl GpuPass for WgpuDevice {
    fn bind_read_target(&mut self, _target: <Self as GpuResources>::ReadTarget) { unimplemented!() }
    fn reset_read_target(&mut self) { unimplemented!() }
    fn bind_draw_target(&mut self, _target: <Self as GpuResources>::DrawTarget) { unimplemented!() }
    fn reset_draw_target(&mut self) { unimplemented!() }
    fn bind_external_draw_target(&mut self, _fbo_id: <Self as GpuResources>::RenderTargetHandle) { unimplemented!() }

    fn bind_program(&mut self, _program: &<Self as GpuShaders>::Program) -> bool { unimplemented!() }
    fn set_uniforms(&self, _program: &<Self as GpuShaders>::Program, _transform: &Transform3D<f32>) { unimplemented!() }
    fn set_shader_texture_size(&self, _program: &<Self as GpuShaders>::Program, _texture_size: DeviceSize) { unimplemented!() }

    fn bind_vao(&mut self, _vao: &<Self as GpuResources>::Vao) { unimplemented!() }
    fn bind_custom_vao(&mut self, _vao: &<Self as GpuResources>::CustomVao) { unimplemented!() }

    fn bind_texture<S>(&mut self, _slot: S, _texture: &<Self as GpuResources>::Texture, _swizzle: Swizzle)
    where
        S: Into<TextureSlot>,
    { unimplemented!() }

    fn bind_external_texture<S>(&mut self, _slot: S, _external_texture: &<Self as GpuResources>::ExternalTexture)
    where
        S: Into<TextureSlot>,
    { unimplemented!() }

    fn clear_target(
        &self,
        _color: Option<[f32; 4]>,
        _depth: Option<f32>,
        _rect: Option<FramebufferIntRect>,
    ) { unimplemented!() }

    fn enable_depth(&self, _depth_func: DepthFunction) { unimplemented!() }
    fn disable_depth(&self) { unimplemented!() }
    fn enable_depth_write(&self) { unimplemented!() }
    fn disable_depth_write(&self) { unimplemented!() }
    fn disable_stencil(&self) { unimplemented!() }

    fn set_scissor_rect(&self, _rect: FramebufferIntRect) { unimplemented!() }
    fn enable_scissor(&self) { unimplemented!() }
    fn disable_scissor(&self) { unimplemented!() }
    fn enable_color_write(&self) { unimplemented!() }
    fn disable_color_write(&self) { unimplemented!() }

    fn set_blend(&mut self, _enable: bool) { unimplemented!() }
    fn set_blend_mode(&mut self, _mode: BlendMode) { unimplemented!() }

    fn draw_triangles_u16(&mut self, _first_vertex: i32, _index_count: i32) { unimplemented!() }
    fn draw_triangles_u32(&mut self, _first_vertex: i32, _index_count: i32) { unimplemented!() }
    fn draw_indexed_triangles(&mut self, _index_count: i32) { unimplemented!() }
    fn draw_indexed_triangles_instanced_u16(&mut self, _index_count: i32, _instance_count: i32) { unimplemented!() }
    fn draw_nonindexed_points(&mut self, _first_vertex: i32, _vertex_count: i32) { unimplemented!() }
    fn draw_nonindexed_lines(&mut self, _first_vertex: i32, _vertex_count: i32) { unimplemented!() }

    fn blit_render_target(
        &mut self,
        _src_target: <Self as GpuResources>::ReadTarget,
        _src_rect: FramebufferIntRect,
        _dest_target: <Self as GpuResources>::DrawTarget,
        _dest_rect: FramebufferIntRect,
        _filter: TextureFilter,
    ) { unimplemented!() }

    fn blit_render_target_invert_y(
        &mut self,
        _src_target: <Self as GpuResources>::ReadTarget,
        _src_rect: FramebufferIntRect,
        _dest_target: <Self as GpuResources>::DrawTarget,
        _dest_rect: FramebufferIntRect,
    ) { unimplemented!() }

    fn read_pixels(&mut self, _img_desc: &ImageDescriptor) -> Vec<u8> { unimplemented!() }
    fn read_pixels_into(
        &mut self,
        _rect: FramebufferIntRect,
        _format: ImageFormat,
        _output: &mut [u8],
    ) { unimplemented!() }
    fn read_pixels_into_pbo(
        &mut self,
        _read_target: <Self as GpuResources>::ReadTarget,
        _rect: DeviceIntRect,
        _format: ImageFormat,
        _pbo: &<Self as GpuResources>::Pbo,
    ) { unimplemented!() }
    fn get_tex_image_into(
        &mut self,
        _texture: &<Self as GpuResources>::Texture,
        _format: ImageFormat,
        _output: &mut [u8],
    ) { unimplemented!() }
}
