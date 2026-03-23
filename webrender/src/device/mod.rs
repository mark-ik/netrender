/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// ── Shared types ─────────────────────────────────────────────────────────────
//
// When `gl_backend` is active, GpuFrameId / TextureFilter / TextureFormatPair
// are already defined inside `device/gl.rs` and re-exported via `pub use
// self::gl::*`.  We only define them here for non-GL backends.

#[cfg(not(feature = "gl_backend"))]
mod shared_types {
    use std::ops::Add;

    /// Sequence number for rendered frames, as tracked by the device layer.
    #[derive(Debug, Copy, Clone, PartialEq, Ord, Eq, PartialOrd)]
    #[cfg_attr(feature = "capture", derive(Serialize))]
    #[cfg_attr(feature = "replay", derive(Deserialize))]
    pub struct GpuFrameId(usize);

    impl GpuFrameId {
        pub fn new(value: usize) -> Self {
            GpuFrameId(value)
        }
    }

    impl Add<usize> for GpuFrameId {
        type Output = GpuFrameId;
        fn add(self, other: usize) -> GpuFrameId {
            GpuFrameId(self.0 + other)
        }
    }

    /// Texture sampling filter.
    #[repr(u32)]
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    #[cfg_attr(feature = "capture", derive(Serialize))]
    #[cfg_attr(feature = "replay", derive(Deserialize))]
    pub enum TextureFilter {
        Nearest,
        Linear,
        Trilinear,
    }

    /// Pair of internal (GPU-native) and external (user-provided) texture formats.
    #[derive(Clone, Debug)]
    #[cfg_attr(feature = "capture", derive(Serialize))]
    #[cfg_attr(feature = "replay", derive(Deserialize))]
    pub struct TextureFormatPair<T> {
        pub internal: T,
        pub external: T,
    }

    impl<T: Copy> From<T> for TextureFormatPair<T> {
        fn from(value: T) -> Self {
            TextureFormatPair {
                internal: value,
                external: value,
            }
        }
    }
}

#[cfg(not(feature = "gl_backend"))]
pub use self::shared_types::*;

/// Metadata used when allocating a texture as a render target.
#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "capture", derive(Serialize))]
#[cfg_attr(feature = "replay", derive(Deserialize))]
pub struct RenderTargetInfo {
    pub has_depth: bool,
}

// ── GpuDevice trait ───────────────────────────────────────────────────────────

#[cfg(not(feature = "gl_backend"))]
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

    fn upload_texture_immediate(&mut self, texture: &Self::Texture, pixels: &[u8]);
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

#[cfg(feature = "gl_backend")]
use std::rc::Rc;

#[cfg(feature = "gl_backend")]
use crate::composite::CompositorConfig;
#[cfg(feature = "gl_backend")]
use crate::renderer::init::WebRenderOptions;
#[cfg(feature = "gl_backend")]
use gleam::gl::Gl as GlApi;

#[cfg(feature = "gl_backend")]
pub enum RendererBackend {
    Gl { gl: Rc<dyn GlApi> },
}

#[cfg(feature = "gl_backend")]
impl RendererBackend {
    fn prepare_options(&self, options: &mut WebRenderOptions) {
        match options.compositor_config {
            CompositorConfig::Draw { .. } | CompositorConfig::Native { .. } => {}
            CompositorConfig::Layer { .. } => {
                options.surface_origin_is_top_left = true;
            }
        }
    }

    fn take_device_config(&self, options: &mut WebRenderOptions) -> DeviceConfig {
        DeviceConfig {
            crash_annotator: options.crash_annotator.take(),
            resource_override_path: options.resource_override_path.clone(),
            use_optimized_shaders: options.use_optimized_shaders,
            upload_method: options.upload_method.clone(),
            batched_upload_threshold: options.batched_upload_threshold,
            cached_programs: options.cached_programs.take(),
            allow_texture_storage_support: options.allow_texture_storage_support,
            allow_texture_swizzling: options.allow_texture_swizzling,
            dump_shader_source: options.dump_shader_source.take(),
            surface_origin_is_top_left: options.surface_origin_is_top_left,
            panic_on_gl_error: options.panic_on_gl_error,
        }
    }

    fn into_device(self, config: DeviceConfig) -> Device {
        match self {
            RendererBackend::Gl { gl } => Device::new(gl, config),
        }
    }

    pub fn create_device(self, options: &mut WebRenderOptions) -> Device {
        self.prepare_options(options);
        let config = self.take_device_config(options);
        self.into_device(config)
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
pub use self::wgpu_device::WgpuDevice;
