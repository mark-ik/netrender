/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use crate::internal_types::RenderTargetInfo;

mod shared;
pub use self::shared::*;

#[cfg(feature = "gl_backend")]
mod gl;
pub mod query_gl;

#[cfg(feature = "gl_backend")]
pub use self::gl::*;
pub use self::query_gl as query;

/// Minimal shared device surface for bootstrap/resource code paths.
pub trait GpuDevice {
    type Texture;

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
}

#[cfg(feature = "wgpu_backend")]
mod wgpu_device;
#[cfg(feature = "wgpu_backend")]
pub use self::wgpu_device::{WgpuDevice, WgpuTexture, WgpuBlendMode, WgpuDepthState, WgpuShaderVariant, TextureBindings};
#[cfg(feature = "wgpu_backend")]
pub(crate) use self::wgpu_device::as_byte_slice;
