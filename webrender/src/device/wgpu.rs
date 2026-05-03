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
use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::os::raw::c_void;
use std::sync::Arc;

use super::traits::{
    BlendMode, GpuFrame, GpuPass, GpuResources, GpuShaders,
};
use super::types::{
    Capabilities, DepthFunction, GpuFrameId, ShaderError, StrideAlignment, Texel,
    TextureFilter, TextureFormatPair, TextureSlot, UploadMethod, VertexAttribute,
    VertexAttributeKind, VertexDescriptor, VertexUsageHint,
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
    /// (currently smoke tests; later the renderer's wgpu path) can build
    /// pipelines, bind groups, and other wgpu-native objects without
    /// going through the `GpuResources`/`GpuPass` traits, which can't
    /// fully express wgpu's pipeline-oriented model.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Direct access to the underlying `wgpu::Queue`.
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
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
    /// Converts a `VertexDescriptor` (WebRender's typed vertex schema, lifted
    /// in P1c to `device::types`) to a pair of `wgpu::VertexBufferLayout`s
    /// suitable for `RenderPipelineDescriptor::vertex.buffers` (plan A3).
    ///
    /// shader_location indices are assigned in declaration order: vertex
    /// attributes get locations `0..N`; instance attributes follow at
    /// `N..N+M`. This matches glslang's `set_auto_map_locations(true)`
    /// behavior in `gen_spirv` (vertex inputs declared in GLSL get
    /// monotonically-increasing locations from 0), so the output is
    /// directly compatible with the SPIR-V committed in
    /// `webrender/res/spirv/`.
    pub fn descriptor_to_wgpu_layouts(desc: &VertexDescriptor) -> WgpuVertexLayouts {
        let mut vertex_attrs = Vec::with_capacity(desc.vertex_attributes.len());
        let mut vertex_offset: u64 = 0;
        for (i, attr) in desc.vertex_attributes.iter().enumerate() {
            vertex_attrs.push(wgpu::VertexAttribute {
                format: attribute_to_wgpu_format(attr),
                offset: vertex_offset,
                shader_location: i as u32,
            });
            vertex_offset += attr.size_in_bytes() as u64;
        }

        let instance_loc_start = desc.vertex_attributes.len() as u32;
        let mut instance_attrs = Vec::with_capacity(desc.instance_attributes.len());
        let mut instance_offset: u64 = 0;
        for (i, attr) in desc.instance_attributes.iter().enumerate() {
            instance_attrs.push(wgpu::VertexAttribute {
                format: attribute_to_wgpu_format(attr),
                offset: instance_offset,
                shader_location: instance_loc_start + i as u32,
            });
            instance_offset += attr.size_in_bytes() as u64;
        }

        WgpuVertexLayouts {
            vertex_attrs,
            instance_attrs,
            vertex_stride: vertex_offset,
            instance_stride: instance_offset,
        }
    }

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
// Vertex schema adapter (P3)
// ============================================================================

/// Owned wgpu vertex buffer layouts derived from a `VertexDescriptor`.
/// Holds the attribute Vecs alive so callers can borrow them as
/// `wgpu::VertexBufferLayout<'_>` slices for pipeline construction.
pub struct WgpuVertexLayouts {
    vertex_attrs: Vec<wgpu::VertexAttribute>,
    instance_attrs: Vec<wgpu::VertexAttribute>,
    vertex_stride: u64,
    instance_stride: u64,
}

impl WgpuVertexLayouts {
    /// Returns `[vertex_buffer, instance_buffer]` borrowing from self,
    /// suitable for `RenderPipelineDescriptor::vertex.buffers`.
    pub fn buffers(&self) -> [wgpu::VertexBufferLayout<'_>; 2] {
        [
            wgpu::VertexBufferLayout {
                array_stride: self.vertex_stride,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &self.vertex_attrs,
            },
            wgpu::VertexBufferLayout {
                array_stride: self.instance_stride,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &self.instance_attrs,
            },
        ]
    }

    pub fn vertex_stride(&self) -> u64 { self.vertex_stride }
    pub fn instance_stride(&self) -> u64 { self.instance_stride }
    pub fn vertex_attrs(&self) -> &[wgpu::VertexAttribute] { &self.vertex_attrs }
    pub fn instance_attrs(&self) -> &[wgpu::VertexAttribute] { &self.instance_attrs }
}

/// Maps a WebRender `VertexAttribute` to its wgpu vertex format.
///
/// The kind+count pair determines the format. Unsupported combinations
/// panic — current shader corpus uses only the supported subset; expand
/// the match when a new combination appears.
fn attribute_to_wgpu_format(attr: &VertexAttribute) -> wgpu::VertexFormat {
    use wgpu::VertexFormat as VF;
    match (attr.kind, attr.count) {
        (VertexAttributeKind::F32, 1) => VF::Float32,
        (VertexAttributeKind::F32, 2) => VF::Float32x2,
        (VertexAttributeKind::F32, 3) => VF::Float32x3,
        (VertexAttributeKind::F32, 4) => VF::Float32x4,
        (VertexAttributeKind::I32, 1) => VF::Sint32,
        (VertexAttributeKind::I32, 2) => VF::Sint32x2,
        (VertexAttributeKind::I32, 4) => VF::Sint32x4,
        (VertexAttributeKind::U8Norm, 2) => VF::Unorm8x2,
        (VertexAttributeKind::U8Norm, 4) => VF::Unorm8x4,
        (VertexAttributeKind::U16Norm, 2) => VF::Unorm16x2,
        (VertexAttributeKind::U16Norm, 4) => VF::Unorm16x4,
        (VertexAttributeKind::U16, 2) => VF::Uint16x2,
        (VertexAttributeKind::U16, 4) => VF::Uint16x4,
        (kind, count) => panic!(
            "no wgpu VertexFormat for VertexAttribute kind={:?} count={}",
            kind, count
        ),
    }
}

#[cfg(test)]
mod vertex_adapter_tests {
    use super::*;

    // VertexDescriptor takes &'static slices, so test schemas live as
    // const tables.

    // ps_clear's vertex schema. Per its GLSL declaration order (matched
    // in bindings.json), per-vertex is aPosition (vec2 from u8norm via
    // auto-convert), per-instance is aRect (vec4 f32) + aColor (vec4 f32).
    const PS_CLEAR_VERT: &[VertexAttribute] = &[VertexAttribute::quad_instance_vertex()];
    const PS_CLEAR_INST: &[VertexAttribute] = &[
        VertexAttribute::f32x4("aRect"),
        VertexAttribute::f32x4("aColor"),
    ];
    const PS_CLEAR: VertexDescriptor = VertexDescriptor {
        vertex_attributes: PS_CLEAR_VERT,
        instance_attributes: PS_CLEAR_INST,
    };

    const EMPTY: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[],
        instance_attributes: &[],
    };

    const MIXED_V: &[VertexAttribute] = &[
        VertexAttribute::f32x2("v0"),
        VertexAttribute::f32x4("v1"),
    ];
    const MIXED_I: &[VertexAttribute] = &[
        VertexAttribute::i32x4("i0"),
        VertexAttribute::f32("i1"),
    ];
    const MIXED: VertexDescriptor = VertexDescriptor {
        vertex_attributes: MIXED_V,
        instance_attributes: MIXED_I,
    };

    const STRIDE_I: &[VertexAttribute] = &[
        VertexAttribute::f32x2("a"),
        VertexAttribute::f32("b"),
        VertexAttribute::i32x4("c"),
    ];
    const STRIDE: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[],
        instance_attributes: STRIDE_I,
    };

    #[test]
    fn ps_clear_layout_matches_oracle() {
        let layouts = WgpuDevice::descriptor_to_wgpu_layouts(&PS_CLEAR);

        // Vertex stride: aPosition (U8Norm count 2) = 2 bytes.
        assert_eq!(layouts.vertex_stride(), 2);
        assert_eq!(layouts.vertex_attrs().len(), 1);
        assert_eq!(layouts.vertex_attrs()[0].format, wgpu::VertexFormat::Unorm8x2);
        assert_eq!(layouts.vertex_attrs()[0].offset, 0);
        assert_eq!(layouts.vertex_attrs()[0].shader_location, 0);

        // Instance stride: aRect (F32 count 4 = 16 bytes) + aColor (16) = 32.
        assert_eq!(layouts.instance_stride(), 32);
        assert_eq!(layouts.instance_attrs().len(), 2);
        assert_eq!(layouts.instance_attrs()[0].format, wgpu::VertexFormat::Float32x4);
        assert_eq!(layouts.instance_attrs()[0].offset, 0);
        assert_eq!(layouts.instance_attrs()[0].shader_location, 1);
        assert_eq!(layouts.instance_attrs()[1].format, wgpu::VertexFormat::Float32x4);
        assert_eq!(layouts.instance_attrs()[1].offset, 16);
        assert_eq!(layouts.instance_attrs()[1].shader_location, 2);

        // shader_location indices [0, 1, 2] match the bindings.json
        // reflection oracle for ps_clear (aPosition=0, aRect=1, aColor=2).
        // The format mismatch (Unorm8x2 vs vec2<f32> reported by naga) is
        // expected — wgpu auto-converts u8norm vertex data to vec2<f32>
        // on shader read, same as GL.
    }

    #[test]
    fn empty_descriptor_yields_zero_strides() {
        let layouts = WgpuDevice::descriptor_to_wgpu_layouts(&EMPTY);
        assert_eq!(layouts.vertex_stride(), 0);
        assert_eq!(layouts.instance_stride(), 0);
        assert!(layouts.vertex_attrs().is_empty());
        assert!(layouts.instance_attrs().is_empty());
    }

    #[test]
    fn locations_continue_across_vertex_to_instance() {
        // Vertex attrs claim 0..N; instance attrs continue at N..N+M
        // (matches glslang's declaration-order auto-mapping in gen_spirv).
        let layouts = WgpuDevice::descriptor_to_wgpu_layouts(&MIXED);
        let v_locs: Vec<u32> = layouts.vertex_attrs().iter().map(|a| a.shader_location).collect();
        let i_locs: Vec<u32> = layouts.instance_attrs().iter().map(|a| a.shader_location).collect();
        assert_eq!(v_locs, vec![0, 1]);
        assert_eq!(i_locs, vec![2, 3]);
    }

    #[test]
    fn stride_accumulates_offsets_correctly() {
        // f32x2 (8) + f32 (4) + i32x4 (16) = 28 bytes total.
        let layouts = WgpuDevice::descriptor_to_wgpu_layouts(&STRIDE);
        assert_eq!(layouts.instance_stride(), 28);
        let offsets: Vec<u64> = layouts.instance_attrs().iter().map(|a| a.offset).collect();
        assert_eq!(offsets, vec![0, 8, 12]);
    }
}

/// Reinterprets a typed slice as bytes for queue.write_buffer/write_texture.
/// Sound under the trait contract that V is plain-old-data shaped.
fn slice_to_bytes<V>(slice: &[V]) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(
            slice.as_ptr() as *const u8,
            slice.len() * std::mem::size_of::<V>(),
        )
    }
}

/// Ensures the `RefCell<Option<wgpu::Buffer>>` slot holds a buffer with
/// at least `bytes_needed` capacity, then writes the given data starting
/// at offset 0. Allocates (or reallocates when growing) as needed; never
/// shrinks. Used by `update_vao_*` and similar.
fn upload_into_vao_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    slot: &RefCell<Option<wgpu::Buffer>>,
    bytes: &[u8],
    usage: wgpu::BufferUsages,
    label: &'static str,
) {
    let needed = bytes.len() as u64;
    if needed == 0 {
        return;
    }
    let mut borrow = slot.borrow_mut();
    let needs_new = match borrow.as_ref() {
        Some(buf) => buf.size() < needed,
        None => true,
    };
    if needs_new {
        *borrow = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: needed,
            usage,
            mapped_at_creation: false,
        }));
    }
    if let Some(buf) = borrow.as_ref() {
        queue.write_buffer(buf, 0, bytes);
    }
}

/// Builds a `Capabilities` describing what this wgpu device supports. Most
/// fields default to `false` because they describe GL extensions with no
/// wgpu equivalent (`KHR_blend_equation_advanced`, `QCOM_tiled_rendering`,
/// `OES_EGL_image_external_essl3`, etc.). The handful of `true` flags
/// reflect things wgpu always supports (texture-to-texture copy, render
/// target invalidation via `LoadOp::Clear`, partial render-target updates).
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

// ============================================================================
// Texture format mapping (P4)
// ============================================================================

/// Maps WebRender's `ImageFormat` to wgpu's `TextureFormat`. Used by
/// `create_texture`. Some WebRender formats have multiple plausible wgpu
/// equivalents (e.g. BGRA8 has both Unorm and UnormSrgb variants); we
/// pick the linear variant by default — sRGB conversions are handled by
/// pipeline state, not by the texture format alone.
pub(crate) fn image_format_to_wgpu(fmt: ImageFormat) -> wgpu::TextureFormat {
    use wgpu::TextureFormat as TF;
    match fmt {
        ImageFormat::R8 => TF::R8Unorm,
        ImageFormat::R16 => TF::R16Unorm,
        ImageFormat::BGRA8 => TF::Bgra8Unorm,
        ImageFormat::RGBAF32 => TF::Rgba32Float,
        ImageFormat::RG8 => TF::Rg8Unorm,
        ImageFormat::RG16 => TF::Rg16Unorm,
        ImageFormat::RGBAI32 => TF::Rgba32Sint,
        ImageFormat::RGBA8 => TF::Rgba8Unorm,
    }
}

// ============================================================================
// Associated-type placeholders + real impls (P4 partial)
// ============================================================================
//
// Marker structs gain real fields as the trait method impls land. Distinct
// types per associated type preserve the type-system contract.

pub struct WgpuProgram;
pub struct WgpuUniformLocation;

/// A wgpu-backed texture. Holds the GPU resource + a default view +
/// metadata mirroring what GL's `Texture` carries. The view is created
/// alongside the texture so `bind_texture` doesn't have to lazily construct
/// one per draw.
pub struct WgpuTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub format: ImageFormat,
    pub size: api::units::DeviceIntSize,
    pub filter: TextureFilter,
    pub target: ImageBufferKind,
    pub is_render_target: bool,
}

/// Vertex array object equivalent. wgpu has no VAO concept — at draw
/// time, the renderer binds vertex/instance/index buffers directly to
/// the RenderPass. WgpuVao bundles them as a unit so the renderer can
/// keep the same logical "VAO = a complete vertex setup" abstraction.
///
/// All three buffers start as `None`; the corresponding `update_vao_*`
/// methods allocate (or reallocate when size grows) on first call. The
/// trait takes `&Self::Vao` (not `&mut`) for those updates — matching
/// the GL device's `&VAO` signature where mutation happens via global
/// GL state — so we use `RefCell`/`Cell` for interior mutability.
pub struct WgpuVao {
    pub vertex_buffer: RefCell<Option<wgpu::Buffer>>,
    pub vertex_count: Cell<usize>,
    pub instance_buffer: RefCell<Option<wgpu::Buffer>>,
    pub instance_count: Cell<usize>,
    pub index_buffer: RefCell<Option<wgpu::Buffer>>,
    pub index_count: Cell<usize>,
    /// The descriptor used to create this VAO; `&'static`-borrowed slices
    /// inside, so owning the struct is cheap.
    pub descriptor: VertexDescriptor,
    pub instance_divisor: u32,
}

/// Custom VAO — multi-stream vertex setup. Stores a `WgpuVbo`-equivalent
/// per stream. wgpu's render-pass bind methods take indexed slot numbers,
/// matching the per-stream model directly.
pub struct WgpuCustomVao {
    /// One buffer per Stream in the original `&[Stream<'_>]`. Streams are
    /// allocated immediately (sized from stream descriptor data).
    pub buffers: Vec<wgpu::Buffer>,
}

/// PBO (Pixel Buffer Object equivalent). In wgpu, this is just a generic
/// `wgpu::Buffer` used for staged uploads and readback. `buffer` is `None`
/// for default-constructed PBOs; `create_pbo_with_size` populates it.
pub struct WgpuPbo {
    pub buffer: Option<wgpu::Buffer>,
    pub size: usize,
}

pub struct WgpuStream<'a>(PhantomData<&'a ()>);

/// Vertex/index buffer (VBO equivalent). Generic over the element type T;
/// the type parameter is enforced at the type-system level only.
/// `buffer` is `None` until `allocate_vbo` runs.
pub struct WgpuVbo<T> {
    pub buffer: Option<wgpu::Buffer>,
    pub count: usize,
    _marker: PhantomData<T>,
}

impl<T> WgpuVbo<T> {
    fn new() -> Self {
        WgpuVbo { buffer: None, count: 0, _marker: PhantomData }
    }
}
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
        // no runtime way to lower it. The renderer's clamp request can be
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
        // The renderer queries by GL extension name; none of those have
        // wgpu equivalents queryable by string. Always false.
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
        target: ImageBufferKind,
        format: ImageFormat,
        width: i32,
        height: i32,
        filter: TextureFilter,
        render_target: Option<crate::internal_types::RenderTargetInfo>,
    ) -> Self::Texture {
        // Clamp to wgpu's max texture dimension (matches GL device behavior
        // which clamps to its own max_texture_size).
        let max_dim = self.device.limits().max_texture_dimension_2d as i32;
        let w = width.min(max_dim).max(1) as u32;
        let h = height.min(max_dim).max(1) as u32;

        let is_render_target = render_target.is_some();
        let mut usage = wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST;
        if is_render_target {
            usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
        }

        let wgpu_format = image_format_to_wgpu(format);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("WgpuDevice::create_texture"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            // ImageBufferKind::TextureExternal/BT709 also resolve to D2 in wgpu;
            // external image interop happens via a separate path (binding a host
            // wgpu::Texture/TextureView), not via create_texture.
            dimension: wgpu::TextureDimension::D2,
            format: wgpu_format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("WgpuDevice::create_texture default view"),
            ..Default::default()
        });

        WgpuTexture {
            texture,
            view,
            format,
            size: api::units::DeviceIntSize::new(w as i32, h as i32),
            filter,
            target,
            is_render_target,
        }
    }

    fn delete_texture(&mut self, _texture: Self::Texture) {
        // wgpu::Texture is Drop-managed; just letting it fall out of scope
        // releases the GPU resource. wgpu defers actual destruction until
        // any in-flight command buffers using it complete.
    }

    fn copy_entire_texture(&mut self, dst: &mut Self::Texture, src: &Self::Texture) {
        // One-shot encoder per call — simple but inefficient. Batching into
        // a per-frame encoder is a P5+ optimization.
        let copy_w = src.size.width.min(dst.size.width).max(0) as u32;
        let copy_h = src.size.height.min(dst.size.height).max(0) as u32;
        if copy_w == 0 || copy_h == 0 {
            return;
        }
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("WgpuDevice::copy_entire_texture"),
        });
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &src.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &dst.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d { width: copy_w, height: copy_h, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
    }

    fn copy_texture_sub_region(
        &mut self,
        src_texture: &Self::Texture,
        src_x: usize,
        src_y: usize,
        dest_texture: &Self::Texture,
        dest_x: usize,
        dest_y: usize,
        width: usize,
        height: usize,
    ) {
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("WgpuDevice::copy_texture_sub_region"),
        });
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &src_texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: src_x as u32, y: src_y as u32, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &dest_texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: dest_x as u32, y: dest_y as u32, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: width as u32,
                height: height as u32,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
    }

    fn invalidate_render_target(&mut self, _texture: &Self::Texture) {
        // GL-only optimization hint (`glInvalidateFramebuffer`). wgpu's
        // render pass `LoadOp::Clear`/`LoadOp::DontCare` semantics achieve
        // the same effect at pass-start time; no per-texture action needed.
    }

    fn invalidate_depth_target(&mut self) {
        // Same as invalidate_render_target — no-op in wgpu's model.
    }

    fn reuse_render_target<T: Texel>(
        &mut self,
        _texture: &mut Self::Texture,
        _rt_info: crate::internal_types::RenderTargetInfo,
    ) {
        // GL: updates last-frame-used cache metadata + lazily attaches a
        // depth render-buffer if `rt_info.has_depth` and texture lacks one.
        // wgpu: textures are immutable in shape post-creation, so
        // depth-attach mid-life would require recreation. For now: no-op;
        // depth-bearing render targets must be created with the right
        // attachment usage from `create_texture` time. Revisit when the
        // renderer's render-target-cache machinery wires through this trait.
    }

    fn create_fbo(&mut self) -> Self::RenderTargetHandle {
        // wgpu has no FBO concept — render passes attach a TextureView
        // directly when started, no separate FBO object. We hand back an
        // opaque marker; the renderer's actual render-pass attachment
        // uses the WgpuTexture's view (P5). The handle is stable but
        // unused at draw time.
        WgpuRenderTargetHandle
    }
    fn create_fbo_for_external_texture(&mut self, _texture_id: u32) -> Self::RenderTargetHandle {
        // Same as create_fbo. The texture_id parameter is a raw GLuint
        // intended to tell the GL device which external texture to
        // attach; for wgpu, external-texture interop happens through a
        // separate path (binding a host wgpu::Texture/TextureView), not
        // via this trait method.
        WgpuRenderTargetHandle
    }
    fn delete_fbo(&mut self, _fbo: Self::RenderTargetHandle) {
        // Marker-only; nothing to release.
    }

    fn create_pbo(&mut self) -> Self::Pbo {
        WgpuPbo { buffer: None, size: 0 }
    }
    fn create_pbo_with_size(&mut self, size: usize) -> Self::Pbo {
        // wgpu enforces "MAP_* combines only with the opposite COPY_*".
        // Sticking to readback orientation (texture/buffer -> CPU) which is
        // what map_pbo_for_readback drives. Upload PBOs in wgpu are
        // typically replaced by queue.write_buffer/write_texture, which
        // don't need a buffer at all.
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("WgpuPbo"),
            size: size as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        WgpuPbo { buffer: Some(buffer), size }
    }
    fn delete_pbo(&mut self, _pbo: Self::Pbo) {
        // wgpu::Buffer is Drop-managed.
    }

    fn create_vao(&mut self, descriptor: &VertexDescriptor, instance_divisor: u32) -> Self::Vao {
        // Buffers are lazy: actual wgpu::Buffer creation happens on first
        // update_vao_* call when we know the data size. This matches GL
        // device's behavior and avoids speculative allocation.
        WgpuVao {
            vertex_buffer: RefCell::new(None),
            vertex_count: Cell::new(0),
            instance_buffer: RefCell::new(None),
            instance_count: Cell::new(0),
            index_buffer: RefCell::new(None),
            index_count: Cell::new(0),
            descriptor: VertexDescriptor {
                vertex_attributes: descriptor.vertex_attributes,
                instance_attributes: descriptor.instance_attributes,
            },
            instance_divisor,
        }
    }

    fn create_vao_with_new_instances(
        &mut self,
        descriptor: &VertexDescriptor,
        base_vao: &Self::Vao,
    ) -> Self::Vao {
        // GL semantics: shares vertex + index buffers with base_vao, gets
        // a fresh instance buffer. wgpu::Buffer isn't Clone, so we can't
        // share by handle directly; renderer must repopulate via
        // update_vao_* if it needs the same data here. (Future opt: wrap
        // the buffer in Arc to enable cheap sharing.)
        WgpuVao {
            vertex_buffer: RefCell::new(None),
            vertex_count: Cell::new(base_vao.vertex_count.get()),
            instance_buffer: RefCell::new(None),
            instance_count: Cell::new(0),
            index_buffer: RefCell::new(None),
            index_count: Cell::new(base_vao.index_count.get()),
            descriptor: VertexDescriptor {
                vertex_attributes: descriptor.vertex_attributes,
                instance_attributes: descriptor.instance_attributes,
            },
            instance_divisor: base_vao.instance_divisor,
        }
    }

    fn delete_vao(&mut self, _vao: Self::Vao) {
        // Drop releases the wgpu::Buffer handles inside.
    }

    fn create_custom_vao<'a>(&mut self, _streams: &[Self::Stream<'a>]) -> Self::CustomVao {
        // wgpu's Self::Stream<'a> is the placeholder WgpuStream<'a> (no
        // constructors). Renderer code that wants a custom multi-stream
        // VAO needs to construct WgpuStream values first, which means
        // this method is effectively unreachable through cross-backend
        // code paths today. Real impl lands when a renderer call site
        // actually wires through.
        unimplemented!("create_custom_vao on wgpu requires a WgpuStream constructor (deferred)")
    }
    fn delete_custom_vao(&mut self, _vao: Self::CustomVao) {
        // Drop releases the buffers.
    }

    fn create_vbo<T>(&mut self) -> Self::Vbo<T> {
        WgpuVbo::new()
    }
    fn delete_vbo<T>(&mut self, _vbo: Self::Vbo<T>) {
        // Drop releases the wgpu::Buffer.
    }
    fn allocate_vbo<V>(
        &mut self,
        vbo: &mut Self::Vbo<V>,
        count: usize,
        _usage_hint: VertexUsageHint,
    ) {
        // wgpu buffers are immutable in size — recreate when size changes.
        // Usage is broad (VERTEX | INDEX | COPY_DST) so the same Vbo can
        // serve as either vertex or index buffer; the renderer picks at
        // draw time. Refining usage from `_usage_hint` is a future
        // optimization (Static vs Dynamic might inform memory hints).
        let size = (count * std::mem::size_of::<V>()) as u64;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("WgpuVbo"),
            size,
            usage: wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::INDEX
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        vbo.buffer = Some(buffer);
        vbo.count = count;
    }
    fn fill_vbo<V>(&mut self, vbo: &Self::Vbo<V>, data: &[V], offset: usize) {
        let buf = vbo
            .buffer
            .as_ref()
            .expect("fill_vbo before allocate_vbo");
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                data.as_ptr() as *const u8,
                data.len() * std::mem::size_of::<V>(),
            )
        };
        let byte_offset = (offset * std::mem::size_of::<V>()) as u64;
        self.queue.write_buffer(buf, byte_offset, bytes);
    }

    fn update_vao_main_vertices<V>(
        &mut self,
        vao: &Self::Vao,
        vertices: &[V],
        _usage_hint: VertexUsageHint,
    ) {
        upload_into_vao_buffer(
            &self.device,
            &self.queue,
            &vao.vertex_buffer,
            slice_to_bytes(vertices),
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            "WgpuVao vertex_buffer",
        );
        vao.vertex_count.set(vertices.len());
    }

    fn update_vao_instances<V: Clone>(
        &mut self,
        vao: &Self::Vao,
        instances: &[V],
        _usage_hint: VertexUsageHint,
        repeat: Option<NonZeroUsize>,
    ) {
        // GL's `repeat` parameter writes the instance data N times for
        // workarounds where instance attribute divisors aren't reliable.
        // Honor by expanding before upload when N > 1.
        let multiplier = repeat.map(|n| n.get()).unwrap_or(1);
        let total_count = instances.len() * multiplier;
        if multiplier == 1 {
            upload_into_vao_buffer(
                &self.device,
                &self.queue,
                &vao.instance_buffer,
                slice_to_bytes(instances),
                wgpu::BufferUsages::VERTEX
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                "WgpuVao instance_buffer",
            );
        } else {
            let mut expanded: Vec<V> = Vec::with_capacity(total_count);
            for v in instances {
                for _ in 0..multiplier {
                    expanded.push(v.clone());
                }
            }
            upload_into_vao_buffer(
                &self.device,
                &self.queue,
                &vao.instance_buffer,
                slice_to_bytes(&expanded),
                wgpu::BufferUsages::VERTEX
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                "WgpuVao instance_buffer",
            );
        }
        vao.instance_count.set(total_count);
    }

    fn update_vao_indices<I>(
        &mut self,
        vao: &Self::Vao,
        indices: &[I],
        _usage_hint: VertexUsageHint,
    ) {
        upload_into_vao_buffer(
            &self.device,
            &self.queue,
            &vao.index_buffer,
            slice_to_bytes(indices),
            wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            "WgpuVao index_buffer",
        );
        vao.index_count.set(indices.len());
    }

    fn upload_texture<'a>(&mut self, _pbo_pool: &'a mut Self::UploadPboPool) -> Self::TextureUploader<'a> { unimplemented!() }

    fn upload_texture_immediate<T: Texel>(&mut self, texture: &Self::Texture, pixels: &[T]) {
        // Convert typed pixel slice to bytes. Texel guarantees Copy + Default;
        // by trait contract, T's size matches texture.format.bytes_per_pixel.
        let pixels_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                pixels.as_ptr() as *const u8,
                pixels.len() * std::mem::size_of::<T>(),
            )
        };

        let width = texture.size.width.max(0) as u32;
        let height = texture.size.height.max(0) as u32;
        let bytes_per_pixel = texture.format.bytes_per_pixel() as u32;
        let bytes_per_row = bytes_per_pixel * width;

        // queue.write_texture handles copy-on-submit (next queue submission
        // will include this upload); no explicit encoder needed.
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels_bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }

    fn map_pbo_for_readback<'a>(&'a mut self, _pbo: &'a Self::Pbo) -> Option<Self::BoundPbo<'a>> { unimplemented!() }

    fn attach_read_texture(&mut self, _texture: &Self::Texture) {
        // GL: binds a texture as the current GL_READ_FRAMEBUFFER attachment
        // for subsequent glReadPixels. wgpu reads happen via
        // copy_texture_to_buffer + buffer mapping (see
        // upload_texture_immediate_writes_pixels test for the pattern); no
        // long-lived "attached read texture" state exists. The renderer's
        // calls to this method are honored implicitly by passing the right
        // texture to its readback paths when they land (P5+).
    }

    fn required_upload_size_and_stride(
        &self,
        size: DeviceIntSize,
        format: ImageFormat,
    ) -> (usize, usize) {
        // Aligned-row pitch for buffer-to-texture copies: 256-byte aligned
        // per wgpu's COPY_BYTES_PER_ROW_ALIGNMENT.
        let bytes_per_pixel = format.bytes_per_pixel() as usize;
        let unaligned = (size.width.max(0) as usize) * bytes_per_pixel;
        let stride = (unaligned + 255) & !255;
        let total = stride * (size.height.max(0) as usize);
        (total, stride)
    }
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
