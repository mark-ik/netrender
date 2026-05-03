/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `netrender_device` — wgpu-native device foundation.
//!
//! Phase 0.5 of the netrender design plan
//! ([`netrender-notes/2026-04-30_netrender_design_plan.md`](../../netrender-notes/2026-04-30_netrender_design_plan.md))
//! splits the wgpu skeleton out of `netrender` into this foundation
//! crate so the renderer-internal types can't leak into consumers
//! that only need the device + render-graph pipeline factories.
//!
//! The crate's primary purpose post-batched-rasterizer-cleanup:
//! provide the [`BrushBlurPipeline`] and [`ClipRectanglePipeline`]
//! factories used by render-graph tasks (separable Gaussian blur and
//! rounded-rect clip mask), plus the GPU readback helper used by
//! tests. The brush_solid / brush_rect_solid / brush_image /
//! brush_gradient pipeline factories were retired alongside
//! netrender's batched WGSL rasterizer; vello is the sole rasterizer
//! on main now.
//!
//! ## Public items
//!
//! - [`WgpuDevice`], [`WgpuHandles`], [`REQUIRED_FEATURES`],
//!   [`BootError`], [`boot`] — adopt or boot the wgpu primitives.
//! - [`BrushBlurPipeline`] / [`build_brush_blur`] — separable
//!   Gaussian blur pipeline for render-graph tasks.
//! - [`ClipRectanglePipeline`] / [`build_clip_rectangle`] — rounded-
//!   rect clip-mask coverage pipeline for render-graph tasks.
//! - [`GradientKind`] — kind tag used by `netrender::SceneGradient`
//!   and the vello rasterizer; lives here so `netrender_device` can
//!   stay independent of `netrender`'s scene types.

#![allow(
    clippy::unreadable_literal,
    clippy::new_without_default,
    clippy::too_many_arguments,
    unknown_lints,
    mismatched_lifetime_syntaxes
)]

pub(crate) mod adapter;
pub(crate) mod binding;
pub(crate) mod core;
pub(crate) mod frame;
pub(crate) mod pipeline;
pub(crate) mod readback;
pub(crate) mod shader;

pub use crate::adapter::WgpuDevice;
pub use crate::core::{BootError, REQUIRED_FEATURES, WgpuHandles, boot};
pub use crate::pipeline::{
    BrushBlurPipeline, ClipRectanglePipeline, GradientKind, build_brush_blur,
    build_clip_rectangle,
};
