/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! wgpu device backend.
//!
//! P1 stage: trait surface fully implemented (`GpuFrame`, `GpuShaders`,
//! `GpuResources`, `GpuPass`) with `unimplemented!()` stubs. Method bodies
//! land incrementally as the wgpu impl matures (P2+). Associated types are
//! placeholder marker structs (`WgpuTexture`, `WgpuVao`, etc.) — these
//! gain real fields when the relevant methods are filled in.

#![allow(dead_code)] // Skeleton fields wired but not yet read by trait impls.

use api::{ImageBufferKind, ImageDescriptor, ImageFormat, MixBlendMode, Parameter};
use api::units::{DeviceIntRect, DeviceIntSize, DeviceSize, FramebufferIntRect};
use euclid::default::Transform3D;
use crate::internal_types::{Swizzle, SwizzleSettings};
use crate::render_api::MemoryReport;
use malloc_size_of::MallocSizeOfOps;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::os::raw::c_void;
use std::sync::Arc;

use super::traits::{
    BlendMode, GpuFrame, GpuPass, GpuResources, GpuShaders,
};
use super::types::{
    Capabilities, DepthFunction, GpuFrameId, ShaderError, StrideAlignment, Texel,
    TextureFilter, TextureFormatPair, TextureSlot, UploadMethod, VertexDescriptor,
    VertexUsageHint,
};

/// Concrete wgpu-backed device. Sibling to `GlDevice` in `gl.rs`; both
/// implement the four `device::traits` surfaces.
pub struct WgpuDevice {
    instance: Arc<wgpu::Instance>,
    adapter: Arc<wgpu::Adapter>,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    /// Optional surface for windowed targets. Headless renderers (offscreen
    /// reftests, capture replay) construct without one.
    surface: Option<wgpu::Surface<'static>>,
    /// Format chosen at construction time; pipelines that target the surface
    /// are baked against this.
    surface_format: Option<wgpu::TextureFormat>,
}

impl WgpuDevice {
    /// Construct from a pre-existing wgpu instance/adapter/device/queue
    /// (mirrors the parity branch's host-shared-device pattern).
    pub fn from_parts(
        instance: Arc<wgpu::Instance>,
        adapter: Arc<wgpu::Adapter>,
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        surface: Option<wgpu::Surface<'static>>,
        surface_format: Option<wgpu::TextureFormat>,
    ) -> Self {
        WgpuDevice {
            instance,
            adapter,
            device,
            queue,
            surface,
            surface_format,
        }
    }

    /// Loads a SPIR-V blob (committed in `webrender/res/spirv/*.spv`) and
    /// creates a `wgpu::ShaderModule`. wgpu runs naga reflection internally
    /// to validate the module and (later) auto-derive its bind-group layout
    /// when used in a pipeline.
    ///
    /// Errors propagate as `wgpu::Error` via the device's error-scope
    /// machinery; the caller is expected to push/pop a scope around the
    /// call if it wants to capture them, otherwise wgpu surfaces them via
    /// the configured error callback.
    pub fn create_shader_module_from_spv(
        &self,
        label: Option<&str>,
        spirv_bytes: &[u8],
    ) -> wgpu::ShaderModule {
        // SPIR-V is little-endian u32 words.
        assert!(
            spirv_bytes.len() % 4 == 0,
            "spirv length not a multiple of 4: {}",
            spirv_bytes.len()
        );
        let words: Vec<u32> = spirv_bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        self.device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label,
                source: wgpu::ShaderSource::SpirV(std::borrow::Cow::Owned(words)),
            })
    }
}

// ============================================================================
// Associated-type placeholders
// ============================================================================
//
// Marker structs for each trait associated type. Currently empty / phantom
// only — gain real fields (wgpu::Texture, wgpu::Buffer, wgpu::RenderPipeline,
// etc.) as the trait method impls land. Distinct types per associated type
// preserve the type-system contract even with all stubs panicking.

pub struct WgpuProgram;
pub struct WgpuUniformLocation;
pub struct WgpuTexture;
pub struct WgpuVao;
pub struct WgpuCustomVao;
pub struct WgpuPbo;
pub struct WgpuStream<'a>(PhantomData<&'a ()>);
pub struct WgpuVbo<T>(PhantomData<T>);
#[derive(Copy, Clone)]
pub struct WgpuRenderTargetHandle;
pub struct WgpuReadTarget;
pub struct WgpuDrawTarget;
pub struct WgpuExternalTexture;
pub struct WgpuUploadPboPool;
pub struct WgpuBoundPbo<'a>(PhantomData<&'a ()>);
pub struct WgpuTextureUploader<'a>(PhantomData<&'a ()>);

// ============================================================================
// Trait impls (all `unimplemented!()` for now)
// ============================================================================

impl GpuFrame for WgpuDevice {
    fn begin_frame(&mut self) -> GpuFrameId { unimplemented!() }
    fn end_frame(&mut self) { unimplemented!() }
    fn reset_state(&mut self) { unimplemented!() }
    fn set_parameter(&mut self, _param: &Parameter) { unimplemented!() }
    fn get_capabilities(&self) -> &Capabilities { unimplemented!() }
    fn max_texture_size(&self) -> i32 { unimplemented!() }
    fn clamp_max_texture_size(&mut self, _size: i32) { unimplemented!() }
    fn surface_origin_is_top_left(&self) -> bool { unimplemented!() }
    fn preferred_color_formats(&self) -> TextureFormatPair<ImageFormat> { unimplemented!() }
    fn swizzle_settings(&self) -> Option<SwizzleSettings> { unimplemented!() }
    fn supports_extension(&self, _extension: &str) -> bool { unimplemented!() }
    fn depth_bits(&self) -> i32 { unimplemented!() }
    fn max_depth_ids(&self) -> i32 { unimplemented!() }
    fn ortho_near_plane(&self) -> f32 { unimplemented!() }
    fn ortho_far_plane(&self) -> f32 { unimplemented!() }
    fn required_pbo_stride(&self) -> StrideAlignment { unimplemented!() }
    fn upload_method(&self) -> &UploadMethod { unimplemented!() }
    fn use_batched_texture_uploads(&self) -> bool { unimplemented!() }
    fn use_draw_calls_for_texture_copy(&self) -> bool { unimplemented!() }
    fn batched_upload_threshold(&self) -> i32 { unimplemented!() }
    fn echo_driver_messages(&self) { unimplemented!() }
    fn report_memory(&self, _ops: &MallocSizeOfOps, _swgl: *mut c_void) -> MemoryReport { unimplemented!() }
    fn depth_targets_memory(&self) -> usize { unimplemented!() }
}

impl GpuShaders for WgpuDevice {
    type Program = WgpuProgram;
    type UniformLocation = WgpuUniformLocation;

    fn create_program(
        &mut self,
        _base_filename: &'static str,
        _features: &[&'static str],
    ) -> Result<Self::Program, ShaderError> { unimplemented!() }

    fn create_program_linked(
        &mut self,
        _base_filename: &'static str,
        _features: &[&'static str],
        _descriptor: &VertexDescriptor,
    ) -> Result<Self::Program, ShaderError> { unimplemented!() }

    fn link_program(
        &mut self,
        _program: &mut Self::Program,
        _descriptor: &VertexDescriptor,
    ) -> Result<(), ShaderError> { unimplemented!() }

    fn delete_program(&mut self, _program: Self::Program) { unimplemented!() }

    fn get_uniform_location(
        &self,
        _program: &Self::Program,
        _name: &str,
    ) -> Self::UniformLocation { unimplemented!() }

    fn bind_shader_samplers<S>(
        &mut self,
        _program: &Self::Program,
        _bindings: &[(&'static str, S)],
    )
    where
        S: Into<TextureSlot> + Copy,
    { unimplemented!() }
}

impl GpuResources for WgpuDevice {
    type Texture = WgpuTexture;
    type Vao = WgpuVao;
    type CustomVao = WgpuCustomVao;
    type Pbo = WgpuPbo;
    type Stream<'a> = WgpuStream<'a>;
    type Vbo<T> = WgpuVbo<T>;
    type BoundPbo<'a> = WgpuBoundPbo<'a> where Self: 'a;
    type TextureUploader<'a> = WgpuTextureUploader<'a>;
    type RenderTargetHandle = WgpuRenderTargetHandle;
    type ReadTarget = WgpuReadTarget;
    type DrawTarget = WgpuDrawTarget;
    type ExternalTexture = WgpuExternalTexture;
    type UploadPboPool = WgpuUploadPboPool;

    fn create_texture(
        &mut self,
        _target: ImageBufferKind,
        _format: ImageFormat,
        _width: i32,
        _height: i32,
        _filter: TextureFilter,
        _render_target: Option<crate::internal_types::RenderTargetInfo>,
    ) -> Self::Texture { unimplemented!() }

    fn delete_texture(&mut self, _texture: Self::Texture) { unimplemented!() }

    fn copy_entire_texture(&mut self, _dst: &mut Self::Texture, _src: &Self::Texture) { unimplemented!() }

    fn copy_texture_sub_region(
        &mut self,
        _src_texture: &Self::Texture,
        _src_x: usize,
        _src_y: usize,
        _dest_texture: &Self::Texture,
        _dest_x: usize,
        _dest_y: usize,
        _width: usize,
        _height: usize,
    ) { unimplemented!() }

    fn invalidate_render_target(&mut self, _texture: &Self::Texture) { unimplemented!() }
    fn invalidate_depth_target(&mut self) { unimplemented!() }

    fn reuse_render_target<T: Texel>(
        &mut self,
        _texture: &mut Self::Texture,
        _rt_info: crate::internal_types::RenderTargetInfo,
    ) { unimplemented!() }

    fn create_fbo(&mut self) -> Self::RenderTargetHandle { unimplemented!() }
    fn create_fbo_for_external_texture(&mut self, _texture_id: u32) -> Self::RenderTargetHandle { unimplemented!() }
    fn delete_fbo(&mut self, _fbo: Self::RenderTargetHandle) { unimplemented!() }

    fn create_pbo(&mut self) -> Self::Pbo { unimplemented!() }
    fn create_pbo_with_size(&mut self, _size: usize) -> Self::Pbo { unimplemented!() }
    fn delete_pbo(&mut self, _pbo: Self::Pbo) { unimplemented!() }

    fn create_vao(&mut self, _descriptor: &VertexDescriptor, _instance_divisor: u32) -> Self::Vao { unimplemented!() }
    fn create_vao_with_new_instances(
        &mut self,
        _descriptor: &VertexDescriptor,
        _base_vao: &Self::Vao,
    ) -> Self::Vao { unimplemented!() }
    fn delete_vao(&mut self, _vao: Self::Vao) { unimplemented!() }

    fn create_custom_vao<'a>(&mut self, _streams: &[Self::Stream<'a>]) -> Self::CustomVao { unimplemented!() }
    fn delete_custom_vao(&mut self, _vao: Self::CustomVao) { unimplemented!() }

    fn create_vbo<T>(&mut self) -> Self::Vbo<T> { unimplemented!() }
    fn delete_vbo<T>(&mut self, _vbo: Self::Vbo<T>) { unimplemented!() }
    fn allocate_vbo<V>(&mut self, _vbo: &mut Self::Vbo<V>, _count: usize, _usage_hint: VertexUsageHint) { unimplemented!() }
    fn fill_vbo<V>(&mut self, _vbo: &Self::Vbo<V>, _data: &[V], _offset: usize) { unimplemented!() }

    fn update_vao_main_vertices<V>(
        &mut self,
        _vao: &Self::Vao,
        _vertices: &[V],
        _usage_hint: VertexUsageHint,
    ) { unimplemented!() }
    fn update_vao_instances<V: Clone>(
        &mut self,
        _vao: &Self::Vao,
        _instances: &[V],
        _usage_hint: VertexUsageHint,
        _repeat: Option<NonZeroUsize>,
    ) { unimplemented!() }
    fn update_vao_indices<I>(
        &mut self,
        _vao: &Self::Vao,
        _indices: &[I],
        _usage_hint: VertexUsageHint,
    ) { unimplemented!() }

    fn upload_texture<'a>(&mut self, _pbo_pool: &'a mut Self::UploadPboPool) -> Self::TextureUploader<'a> { unimplemented!() }

    fn upload_texture_immediate<T: Texel>(&mut self, _texture: &Self::Texture, _pixels: &[T]) { unimplemented!() }

    fn map_pbo_for_readback<'a>(&'a mut self, _pbo: &'a Self::Pbo) -> Option<Self::BoundPbo<'a>> { unimplemented!() }

    fn attach_read_texture(&mut self, _texture: &Self::Texture) { unimplemented!() }

    fn required_upload_size_and_stride(
        &self,
        _size: DeviceIntSize,
        _format: ImageFormat,
    ) -> (usize, usize) { unimplemented!() }
}

impl GpuPass for WgpuDevice {
    fn bind_read_target(&mut self, _target: Self::ReadTarget) { unimplemented!() }
    fn reset_read_target(&mut self) { unimplemented!() }
    fn bind_draw_target(&mut self, _target: Self::DrawTarget) { unimplemented!() }
    fn reset_draw_target(&mut self) { unimplemented!() }
    fn bind_external_draw_target(&mut self, _fbo_id: Self::RenderTargetHandle) { unimplemented!() }

    fn bind_program(&mut self, _program: &Self::Program) -> bool { unimplemented!() }
    fn set_uniforms(&self, _program: &Self::Program, _transform: &Transform3D<f32>) { unimplemented!() }
    fn set_shader_texture_size(&self, _program: &Self::Program, _texture_size: DeviceSize) { unimplemented!() }

    fn bind_vao(&mut self, _vao: &Self::Vao) { unimplemented!() }
    fn bind_custom_vao(&mut self, _vao: &Self::CustomVao) { unimplemented!() }

    fn bind_texture<S>(&mut self, _slot: S, _texture: &Self::Texture, _swizzle: Swizzle)
    where
        S: Into<TextureSlot>,
    { unimplemented!() }

    fn bind_external_texture<S>(&mut self, _slot: S, _external_texture: &Self::ExternalTexture)
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
        _src_target: Self::ReadTarget,
        _src_rect: FramebufferIntRect,
        _dest_target: Self::DrawTarget,
        _dest_rect: FramebufferIntRect,
        _filter: TextureFilter,
    ) { unimplemented!() }

    fn blit_render_target_invert_y(
        &mut self,
        _src_target: Self::ReadTarget,
        _src_rect: FramebufferIntRect,
        _dest_target: Self::DrawTarget,
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
        _read_target: Self::ReadTarget,
        _rect: DeviceIntRect,
        _format: ImageFormat,
        _pbo: &Self::Pbo,
    ) { unimplemented!() }
    fn get_tex_image_into(
        &mut self,
        _texture: &Self::Texture,
        _format: ImageFormat,
        _output: &mut [u8],
    ) { unimplemented!() }
}
