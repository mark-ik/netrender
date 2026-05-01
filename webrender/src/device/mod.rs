/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

#[cfg(feature = "gl_backend")]
mod gl;
#[cfg(feature = "gl_backend")]
pub mod query_gl;
#[cfg(feature = "wgpu_backend")]
pub mod wgpu;
pub mod traits;

#[cfg(feature = "gl_backend")]
pub use self::gl::*;
#[cfg(feature = "gl_backend")]
pub use self::query_gl as query;
#[cfg(feature = "wgpu_backend")]
pub use self::wgpu::WgpuDevice;
pub use self::traits::{BlendMode, GpuFrame, GpuPass, GpuResources, GpuShaders};

/// Alias retained so renderer code that still names `Device` resolves to
/// the (renamed) `GlDevice`. P0c rename: the GL backend's concrete type is
/// `GlDevice`; the alias preserves source compatibility for the ~116
/// external call sites that name `Device` in field types and function
/// signatures. Migrating those sites to `GlDevice` directly is a future
/// cosmetic cleanup; the alias is permanent for now.
#[cfg(feature = "gl_backend")]
pub type Device = self::gl::GlDevice;
