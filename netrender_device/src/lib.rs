/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `netrender_device` — wgpu-native device foundation.
//!
//! Phase 0.5 of the netrender design plan
//! ([`netrender-notes/2026-04-30_netrender_design_plan.md`](../../netrender-notes/2026-04-30_netrender_design_plan.md))
//! splits the post-Phase-D wgpu skeleton out of `netrender` into this
//! foundation crate so the renderer-internal types can't leak into
//! consumers that only need the device + WGSL pipeline pattern.
//!
//! The public surface is intentionally narrow (see §3 "Crate-split
//! rationale" of the design plan). Implementation modules
//! (`binding`, `buffer`, `format`, `frame`, `pass` internals,
//! `pipeline` internals, `readback`, `shader`, `texture` internals)
//! are `pub(crate)`; only the items listed below escape the crate.
//!
//! ## Public items
//!
//! - [`WgpuDevice`], [`WgpuHandles`], [`REQUIRED_FEATURES`],
//!   [`BootError`] — adopt or boot the wgpu primitives.
//! - [`DrawIntent`], [`RenderPassTarget`], [`ColorAttachment`],
//!   [`DepthAttachment`] — record draws and target policy for
//!   `WgpuDevice::encode_pass`.
//! - [`BrushSolidPipeline`] + [`build_brush_solid_specialized`] —
//!   the smoke pipeline factory. Its primitive ABI gets re-decided
//!   at Phase 2; today it proves the device path renders correctly.
//! - [`WgpuTexture`], [`TextureDesc`] — texture creation surface.

#![allow(
    clippy::unreadable_literal,
    clippy::new_without_default,
    clippy::too_many_arguments,
    unknown_lints,
    mismatched_lifetime_syntaxes
)]

pub(crate) mod adapter;
pub(crate) mod binding;
pub(crate) mod buffer;
pub(crate) mod core;
pub(crate) mod format;
pub(crate) mod frame;
pub(crate) mod pass;
pub(crate) mod pipeline;
pub(crate) mod readback;
pub(crate) mod shader;
pub(crate) mod texture;

#[cfg(test)]
mod tests;

pub use crate::adapter::WgpuDevice;
pub use crate::core::{BootError, REQUIRED_FEATURES, WgpuHandles, boot};
pub use crate::pass::{ColorAttachment, DepthAttachment, DrawIntent, RenderPassTarget};
pub use crate::pipeline::{
    BrushBlurPipeline, BrushGradientPipeline, BrushImagePipeline, BrushRectSolidPipeline,
    BrushSolidPipeline, BrushTextPipeline, ClipRectanglePipeline, GradientKind,
    build_brush_blur, build_brush_gradient, build_brush_image, build_brush_rect_solid,
    build_brush_solid_specialized, build_brush_text, build_clip_rectangle,
};
pub use crate::texture::{TextureDesc, WgpuTexture};
