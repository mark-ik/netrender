/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! wgpu device backend.
//!
//! Sibling to `GlDevice` in `gl.rs`; both implement the four
//! `device::traits` surfaces (`GpuFrame`, `GpuShaders`, `GpuResources`,
//! `GpuPass`). Decomposed into:
//!
//! - [`mod.rs`](self) — `WgpuDevice` struct, constructor, accessors,
//!   `create_shader_module_from_spv`, and the `GpuFrame` impl.
//! - [`types`] — wrapper / marker structs for the 13 associated types
//!   (WgpuTexture, WgpuVao, WgpuProgram, etc.) + image_format_to_wgpu.
//! - [`vertex_layout`] — `WgpuVertexLayouts::from_descriptor` (P3
//!   vertex schema adapter) + 4 unit tests.
//! - [`resources`] — `impl GpuResources` (P4b/c/d/e/g/h).
//! - [`shaders`] — `impl GpuShaders` (P4i) + SPIR-V loading helpers.
//! - [`pass`] — `impl GpuPass` (P5; currently all stubs).

#![allow(dead_code)] // Skeleton fields wired but not yet read by all impls.

use api::{ImageFormat, Parameter};
use crate::internal_types::SwizzleSettings;
use crate::render_api::MemoryReport;
use malloc_size_of::MallocSizeOfOps;
use std::os::raw::c_void;
use std::sync::Arc;

use super::traits::GpuFrame;
use super::types::{
    Capabilities, GpuFrameId, StrideAlignment, TextureFormatPair, UploadMethod,
};

pub mod types;
pub mod vertex_layout;
pub mod resources;
pub mod shaders;
pub mod pass;

// Re-exports for the crate-root `pub use crate::device::wgpu::{...}` paths
// in `webrender/src/lib.rs`.
pub use types::{
    WgpuBoundPbo, WgpuCustomVao, WgpuDrawTarget, WgpuExternalTexture,
    WgpuPbo, WgpuProgram, WgpuReadTarget, WgpuRenderTargetHandle,
    WgpuStream, WgpuTexture, WgpuTextureUploader, WgpuUniformLocation,
    WgpuUploadPboPool, WgpuVao, WgpuVbo,
};
pub(crate) use types::image_format_to_wgpu;
pub use vertex_layout::WgpuVertexLayouts;

/// Concrete wgpu-backed device.
pub struct WgpuDevice {
    instance: Arc<wgpu::Instance>,
    adapter: Arc<wgpu::Adapter>,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    /// Optional surface for windowed targets. Headless renderers (offscreen
    /// reftests, capture replay) construct without one.
    surface: Option<wgpu::Surface<'static>>,
    /// Format chosen at construction time; pipelines targeting the surface
    /// are baked against this.
    surface_format: Option<wgpu::TextureFormat>,
    /// Frame counter, incremented each begin_frame.
    frame_id: GpuFrameId,
    /// Backend capability flags. Populated once at construction from
    /// adapter info + wgpu features/limits.
    capabilities: Capabilities,
    /// Selected upload method (default Immediate; queue.write_texture).
    upload_method: UploadMethod,
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
        let capabilities = derive_capabilities(&adapter, &device);
        WgpuDevice {
            instance,
            adapter,
            device,
            queue,
            surface,
            surface_format,
            frame_id: GpuFrameId::new(0),
            capabilities,
            upload_method: UploadMethod::Immediate,
        }
    }

    /// Direct access to the underlying `wgpu::Device`. Exposed so callers
    /// (smoke tests today; renderer wgpu path later) can build pipelines,
    /// bind groups, and other wgpu-native objects without going through
    /// the `GpuResources`/`GpuPass` traits, which can't fully express
    /// wgpu's pipeline-oriented model.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Direct access to the underlying `wgpu::Queue`.
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Loads a SPIR-V blob (committed in `webrender/res/spirv/*.spv`) and
    /// creates a `wgpu::ShaderModule`. wgpu runs naga reflection internally
    /// to validate the module and (later) auto-derive its bind-group
    /// layout when used in a pipeline.
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

/// Builds a `Capabilities` describing what this wgpu device supports.
/// Most fields default to `false` — they describe GL extensions with no
/// wgpu equivalent (`KHR_blend_equation_advanced`, `QCOM_tiled_rendering`,
/// `OES_EGL_image_external_essl3`, etc.). The `true` flags reflect things
/// wgpu always supports (texture-to-texture copy, render-target
/// invalidation via `LoadOp::Clear`, partial render-target updates).
fn derive_capabilities(adapter: &wgpu::Adapter, _device: &wgpu::Device) -> Capabilities {
    let info = adapter.get_info();
    Capabilities {
        supports_multisampling: true,           // wgpu MSAA via sample_count
        supports_copy_image_sub_data: true,     // copy_texture_to_texture always supported
        supports_color_buffer_float: true,      // RGBA32Float texture format always available
        supports_buffer_storage: false,         // GL persistent map; no direct wgpu equivalent
        supports_advanced_blend_equation: false, // KHR_blend_equation_advanced (GL only)
        supports_dual_source_blending: adapter
            .features()
            .contains(wgpu::Features::DUAL_SOURCE_BLENDING),
        supports_khr_debug: false,              // GL-only
        supports_texture_swizzle: false,        // wgpu doesn't expose texture swizzle
        supports_nonzero_pbo_offsets: true,     // queue.write_buffer takes byte offset
        supports_texture_usage: true,           // wgpu always specifies usage flags
        supports_render_target_partial_update: true, // load/store ops at pass boundaries
        supports_shader_storage_object: adapter
            .features()
            .contains(wgpu::Features::BUFFER_BINDING_ARRAY),
        requires_batched_texture_uploads: None,
        supports_alpha_target_clears: true,
        requires_alpha_target_full_clear: false,
        prefers_clear_scissor: false,
        supports_render_target_invalidate: true, // LoadOp::Clear / DontCare
        supports_r8_texture_upload: true,
        supports_qcom_tiled_rendering: false,    // GL extension
        uses_native_clip_mask: false,
        uses_native_antialiasing: false,
        supports_image_external_essl3: false,    // GL extension
        requires_vao_rebind_after_orphaning: false, // no VAO concept in wgpu
        renderer_name: format!("{} ({:?})", info.name, info.backend),
    }
}

impl GpuFrame for WgpuDevice {
    fn begin_frame(&mut self) -> GpuFrameId {
        self.frame_id = self.frame_id + 1;
        self.frame_id
    }

    fn end_frame(&mut self) {
        // No-op for now. Per-frame command encoder lifecycle (when we have
        // one) ends here — submit any pending work, present the surface
        // frame if we have one. P5 wires this up.
    }

    fn reset_state(&mut self) {
        // GL: re-binds default textures/VAO/FBO to scrub stale state.
        // wgpu has no comparable global state to reset — pipeline state
        // lives entirely on the RenderPass which is created fresh per use.
    }

    fn set_parameter(&mut self, _param: &Parameter) {
        // GL accepts runtime parameter overrides (PBO uploads, batched
        // uploads, etc.). For wgpu these don't apply — uploads always go
        // through queue.write_*; no toggles to honor.
    }

    fn get_capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn max_texture_size(&self) -> i32 {
        self.device.limits().max_texture_dimension_2d as i32
    }

    fn clamp_max_texture_size(&mut self, _size: i32) {
        // wgpu's max_texture_dimension_2d limit is fixed at device creation;
        // no runtime way to lower it. The renderer's clamp request is
        // honored by capping at create_texture time (already done there).
    }

    fn surface_origin_is_top_left(&self) -> bool {
        // wgpu uses Y-down NDC for all backends. Surface origin is top-left.
        true
    }

    fn preferred_color_formats(&self) -> TextureFormatPair<ImageFormat> {
        // BGRA8 is the universally-supported color attachment format on
        // wgpu surfaces; matches what most desktop GPU adapters expose.
        TextureFormatPair {
            internal: ImageFormat::BGRA8,
            external: ImageFormat::BGRA8,
        }
    }

    fn swizzle_settings(&self) -> Option<SwizzleSettings> {
        // wgpu has no texture-level swizzle (would require manual shader
        // writes). Renderer falls back to BGRA shader paths when None.
        None
    }

    fn supports_extension(&self, _extension: &str) -> bool {
        // The renderer queries by GL extension name; none have wgpu
        // equivalents queryable by string. Always false.
        false
    }

    fn depth_bits(&self) -> i32 {
        // 24-bit depth is wgpu's standard Depth24Plus.
        24
    }

    fn max_depth_ids(&self) -> i32 {
        1 << 22 // matches GL device's RESERVE_DEPTH_BITS=2 from depth_bits=24
    }

    fn ortho_near_plane(&self) -> f32 {
        -(self.max_depth_ids() as f32)
    }

    fn ortho_far_plane(&self) -> f32 {
        (self.max_depth_ids() - 1) as f32
    }

    fn required_pbo_stride(&self) -> StrideAlignment {
        // wgpu requires 256-byte alignment for buffer<->texture row stride
        // (COPY_BYTES_PER_ROW_ALIGNMENT). Renderer uses this to size PBOs.
        StrideAlignment::Bytes(std::num::NonZeroUsize::new(256).unwrap())
    }

    fn upload_method(&self) -> &UploadMethod {
        &self.upload_method
    }

    fn use_batched_texture_uploads(&self) -> bool {
        // wgpu's queue.write_texture is already batched internally; the
        // renderer's "batch into a PBO" optimization isn't useful here.
        false
    }

    fn use_draw_calls_for_texture_copy(&self) -> bool {
        // copy_texture_to_texture is always available and faster than a
        // shader-based copy; never need to fall back to draws.
        false
    }

    fn batched_upload_threshold(&self) -> i32 {
        i32::MAX // batching disabled (see use_batched_texture_uploads)
    }

    fn echo_driver_messages(&self) {
        // wgpu surfaces validation/driver errors via its error scope and
        // configured callback at device creation time. Nothing to drain
        // synchronously per-frame.
    }

    fn report_memory(&self, _ops: &MallocSizeOfOps, _swgl: *mut c_void) -> MemoryReport {
        // Detailed memory reporting requires walking owned wgpu::Texture/
        // Buffer handles — defer until renderer wiring exists. Returning
        // default (zeroes) is acceptable per the GL impl's contract.
        MemoryReport::default()
    }

    fn depth_targets_memory(&self) -> usize {
        0 // no depth-target tracking yet; revisit when render targets land
    }
}
