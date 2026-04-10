/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Shared types (GpuFrameId, TextureFilter, TextureFormatPair, Texel, …)
// live in shared.rs and are always re-exported, regardless of which backend
// features are enabled.
mod shared;
pub use self::shared::*;

/// Metadata used when allocating a texture as a render target.
#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "capture", derive(Serialize))]
#[cfg_attr(feature = "replay", derive(Deserialize))]
pub struct RenderTargetInfo {
    pub has_depth: bool,
}

// ── GpuDevice trait ───────────────────────────────────────────────────────────
//
// A backend-agnostic interface over the GPU device.  Both `Device` (GL) and
// `WgpuDevice` implement this trait so that generic initialisation helpers
// (e.g. uploading the dither matrix or debug-font atlas) can be written once
// and used by either backend.
//
// The trait is intentionally narrow: it covers the operations needed during
// Renderer construction and for texture-cache bootstrap.  Higher-level drawing
// is backend-specific and lives in `renderer/mod.rs`.

pub trait GpuDevice {
    type Texture;
    type Program;

    fn begin_frame(&mut self) -> GpuFrameId;
    fn end_frame(&mut self);

    fn create_texture(
        &mut self,
        target: api::ImageBufferKind,
        format: api::ImageFormat,
        width: i32,
        height: i32,
        filter: TextureFilter,
        render_target: Option<RenderTargetInfo>,
    ) -> Self::Texture;

    fn upload_texture_immediate<T: Texel>(&mut self, texture: &Self::Texture, pixels: &[T]);
    fn delete_texture(&mut self, texture: Self::Texture);

    fn draw_triangles_u16(&mut self, first_vertex: i32, index_count: i32);
    fn draw_triangles_u32(&mut self, first_vertex: i32, index_count: i32);

    fn read_pixels_into(
        &mut self,
        rect: api::units::FramebufferIntRect,
        format: api::ImageFormat,
        output: &mut [u8],
    );
}

// ── RendererBackend enum ──────────────────────────────────────────────────────
//
// The top-level discriminant used by `create_webrender_instance_with_backend`.
// All three variants (GL, wgpu with owned device, wgpu with shared/hal device)
// ultimately produce a renderer that satisfies the same contract; they only
// differ in how the GPU context is acquired.

#[cfg(feature = "gl_backend")]
use std::rc::Rc;
#[cfg(feature = "gl_backend")]
use gleam::gl::Gl as GlApi;
#[cfg(feature = "gl_backend")]
use crate::renderer::init::WebRenderOptions;

pub enum RendererBackend {
    /// OpenGL backend.  WebRender owns the GL context via the provided gleam
    /// handle.
    #[cfg(feature = "gl_backend")]
    Gl { gl: Rc<dyn GlApi> },

    /// wgpu backend.  WebRender creates its own adapter + device, optionally
    /// targeting a window surface for presentation.  Pass `None` for both
    /// `instance` and `surface` for headless mode.
    ///
    /// Not available on wasm — use `WgpuShared` with a pre-created device instead.
    #[cfg(feature = "wgpu_native")]
    Wgpu {
        instance: Option<wgpu::Instance>,
        surface: Option<wgpu::Surface<'static>>,
        width: u32,
        height: u32,
    },

    /// Shared-device wgpu backend.  The host application owns the
    /// `wgpu::Device` + `wgpu::Queue` (e.g. created by egui or another
    /// framework).  WebRender renders to offscreen textures and the host
    /// composites using `Renderer::composite_output()`.
    #[cfg(feature = "wgpu_backend")]
    WgpuShared {
        device: wgpu::Device,
        queue: wgpu::Queue,
    },

    /// wgpu-hal backend.  The host provides a factory closure that produces a
    /// `(wgpu::Device, wgpu::Queue)` pair — typically wrapping a raw
    /// Vulkan / DX12 / Metal device via
    /// `wgpu::Adapter::create_device_from_hal()`.  After device creation this
    /// is functionally identical to `WgpuShared`.
    #[cfg(feature = "wgpu_backend")]
    WgpuHal {
        device_factory: Box<dyn FnOnce() -> (wgpu::Device, wgpu::Queue) + Send>,
    },
}

#[cfg(feature = "gl_backend")]
impl RendererBackend {
    /// Consume the backend descriptor and construct a GL `Device`.
    /// Called from `create_webrender_instance_with_backend` for the GL path.
    pub fn create_gl_device(self, options: &mut WebRenderOptions) -> Device {
        options.prepare_for_device_creation();
        match self {
            RendererBackend::Gl { gl } => {
                Device::new(gl, options.take_device_config())
            }
            #[cfg(feature = "wgpu_backend")]
            _ => panic!("create_gl_device called on a non-GL RendererBackend"),
        }
    }
}

// ── Backend modules ───────────────────────────────────────────────────────────

#[cfg(feature = "gl_backend")]
mod gl;
#[cfg(feature = "gl_backend")]
pub mod query_gl;

#[cfg(feature = "gl_backend")]
pub use self::gl::*;
#[cfg(feature = "gl_backend")]
pub use self::query_gl as query;

#[cfg(feature = "wgpu_backend")]
mod wgpu_device;
#[cfg(feature = "wgpu_backend")]
pub use self::wgpu_device::{
    WgpuDevice, WgpuTexture, WgpuBlendMode, WgpuDepthState, WgpuShaderVariant, TextureBindings,
};
#[cfg(feature = "wgpu_backend")]
pub(crate) use self::wgpu_device::as_byte_slice;
