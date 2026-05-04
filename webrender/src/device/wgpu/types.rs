/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Wrapper / marker structs used as `WgpuDevice`'s associated types.
//!
//! Each type satisfies one of the trait associated types declared in
//! `device::traits::{GpuShaders, GpuResources}`. Distinct types per assoc
//! type preserve the type-system contract; the field shapes match what
//! each method body needs.

use api::{ImageBufferKind, ImageFormat};
use std::cell::{Cell, RefCell};
use std::marker::PhantomData;

use super::super::types::{TextureFilter, VertexDescriptor};

/// A wgpu-backed shader program.
///
/// Bundles vert + frag SPIR-V `ShaderModule`s plus the uniform buffer for
/// the `WrLocals { uTransform }` UBO. The actual `wgpu::RenderPipeline`
/// is built lazily by `link_program` once the `VertexDescriptor` is known
/// (matches GL device's two-stage create + link pattern).
///
/// The pipeline field is wrapped in `RefCell` so trait methods that take
/// `&Program` (the GL contract) can mutate the cached pipeline.
pub struct WgpuProgram {
    pub vert_module: wgpu::ShaderModule,
    pub frag_module: wgpu::ShaderModule,
    /// `None` until link_program builds it.
    pub pipeline: RefCell<Option<wgpu::RenderPipeline>>,
    /// Uniform buffer for WrLocals { mat4 uTransform; }. 64 bytes.
    pub uniform_buffer: wgpu::Buffer,
    /// Stem name (e.g. "ps_clear", "brush_solid_ALPHA_PASS") for diagnostics.
    pub stem: String,
}

/// wgpu doesn't have per-uniform locations — bindings are at the bind-group
/// level, by index not by name. Placeholder that satisfies the trait
/// associated type without carrying meaningful state. Renderer code that
/// writes a uniform value goes through `set_uniforms` (writes to the
/// program's uniform buffer at offset 0), not via this location handle.
pub struct WgpuUniformLocation;

/// A wgpu-backed texture. Holds the GPU resource + a default view +
/// metadata mirroring what GL's `Texture` carries. The view is created
/// alongside the texture so `bind_texture` doesn't have to lazily
/// construct one per draw.
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
/// the RenderPass. `WgpuVao` bundles them so the renderer can keep the
/// "VAO = a complete vertex setup" abstraction.
///
/// All three buffer slots start as `None`; the corresponding `update_vao_*`
/// methods allocate (or reallocate when growing) on first call. The
/// trait takes `&Self::Vao` (not `&mut`) for those updates — matching
/// GL's `&VAO` signature where mutation flowed via global GL state — so
/// we use `RefCell`/`Cell` for interior mutability.
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

/// Custom VAO — multi-stream vertex setup. One buffer per Stream in
/// the original `&[Stream<'_>]`.
pub struct WgpuCustomVao {
    pub buffers: Vec<wgpu::Buffer>,
}

/// PBO equivalent. Generic `wgpu::Buffer` used for staged uploads and
/// readback. `buffer` is `None` for default-constructed PBOs;
/// `create_pbo_with_size` populates it.
pub struct WgpuPbo {
    pub buffer: Option<wgpu::Buffer>,
    pub size: usize,
}

/// Custom-VAO stream descriptor (placeholder — has no constructor).
/// `create_custom_vao` is currently unreachable through cross-backend
/// renderer code paths; real impl lands when a renderer call site wires
/// through.
pub struct WgpuStream<'a>(PhantomData<&'a ()>);

/// Vertex/index buffer (VBO equivalent). Generic over element type T
/// (PhantomData enforcement only). `buffer` is `None` until
/// `allocate_vbo` runs.
pub struct WgpuVbo<T> {
    pub buffer: Option<wgpu::Buffer>,
    pub count: usize,
    _marker: PhantomData<T>,
}

impl<T> WgpuVbo<T> {
    pub(super) fn new() -> Self {
        WgpuVbo { buffer: None, count: 0, _marker: PhantomData }
    }
}

/// Render target identity marker. wgpu has no FBO concept — render passes
/// attach a `TextureView` directly when started. Today this handle is
/// opaque + identity-only; P5+ may evolve it (or, more likely, the
/// `WgpuDrawTarget` enum carries the view directly per Option II of the
/// FBO design discussion).
#[derive(Copy, Clone)]
pub struct WgpuRenderTargetHandle;

pub struct WgpuReadTarget;
pub struct WgpuDrawTarget;
pub struct WgpuExternalTexture;
pub struct WgpuUploadPboPool;

/// Lifetime-bound RAII handle for a CPU-mapped PBO; tied to `&mut self`
/// scope. Real impl lands when `map_pbo_for_readback` is wired up.
pub struct WgpuBoundPbo<'a>(PhantomData<&'a ()>);

/// Per-frame texture-upload session bound to the borrowed PBO pool.
/// Real impl lands when `upload_texture` is wired up.
pub struct WgpuTextureUploader<'a>(PhantomData<&'a ()>);

/// Maps WebRender's `ImageFormat` to wgpu's `TextureFormat`. Used by
/// `create_texture`. BGRA8 picks the linear (`Bgra8Unorm`) variant; sRGB
/// conversions happen at pipeline level, not via the texture format.
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
