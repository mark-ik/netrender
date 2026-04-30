/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! WebRender post-Phase-D wgpu skeleton.
//!
//! This crate is what survived the rip-out: a wgpu device adapter
//! (`device::wgpu`), a minimal `Renderer` that owns it, and the
//! `brush_solid` WGSL pipeline + tests proving the wgpu side renders
//! correctly through `encode_pass`. The frame-builder layer (display-
//! list ingestion → `Frame` → batches → draw calls) is *gone*; it was
//! shaped around GL thread-model assumptions that don't survive
//! contact with wgpu (Send+Sync device, explicit texture handles, no
//! cross-thread queues). Whatever frame-builder lives here next will
//! be authored against this skeleton, not retrofitted.
//!
//! See [wr-wgpu-notes/](../../wr-wgpu-notes/) for the design history
//! and the current direction.

#![allow(
    clippy::unreadable_literal,
    clippy::new_without_default,
    clippy::too_many_arguments,
    unknown_lints,
    mismatched_lifetime_syntaxes
)]

mod device;
mod renderer;

pub use crate::renderer::{Renderer, RendererError};
pub use crate::renderer::init::{WebRenderOptions, create_webrender_instance};
pub use crate::device::wgpu::core::{WgpuHandles, REQUIRED_FEATURES};
pub use crate::device::wgpu::adapter::WgpuDevice;
