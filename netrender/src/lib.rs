/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `netrender` — the renderer shell built on top of the
//! [`netrender_device`] foundation.
//!
//! Phase 0.5 of the netrender design plan
//! ([`netrender-notes/2026-04-30_netrender_design_plan.md`](../../netrender-notes/2026-04-30_netrender_design_plan.md))
//! splits the wgpu device foundation into its own crate so the
//! renderer-internal types (`PreparedFrame`, batches, render-task
//! graph, picture cache) can't leak into consumers that only need the
//! device + WGSL pipeline pattern. Today this crate's public surface
//! is small: a [`Renderer`] shell + [`Compositor`] / [`NativeCompositor`]
//! trait shapes (axiom 14 — the seam Phases 5–7 defer to). Display
//! list ingestion lands at Phase 2.

#![allow(
    clippy::unreadable_literal,
    clippy::new_without_default,
    clippy::too_many_arguments,
    unknown_lints,
    mismatched_lifetime_syntaxes
)]

pub(crate) mod batch;
mod compositor;
pub(crate) mod glyph_atlas;
pub(crate) mod image_cache;
pub mod rasterizer;
pub mod render_graph;
mod renderer;
pub mod scene;
pub(crate) mod space;
pub mod tile_cache;

pub use crate::compositor::{Compositor, NativeCompositor};
pub use crate::rasterizer::{BoundRaster, FontHandle, RasterContext};
pub use crate::render_graph::{EncodeCallback, RenderGraph, Task, TaskId};
pub use crate::renderer::init::{NetrenderOptions, create_netrender_instance};
pub use crate::renderer::{
    ColorLoad, FrameTarget, PreparedFrame, Renderer, RendererError, ResourceRefs,
};
pub use crate::scene::{
    GlyphInstance, GlyphKey, GlyphRaster, GradientKind, GradientStop, ImageData, ImageKey,
    NO_CLIP, Scene, SceneGradient, SceneImage, SceneRect, SceneText, Transform,
};
pub use crate::tile_cache::{TileCache, TileCoord};
pub use crate::space::{ROOT_SPATIAL_NODE, SpatialTransform, SpatialTree};

// Re-export the device-foundation surface embedders need to construct
// `WgpuHandles`, pass them through `create_netrender_instance`, and
// build `DrawIntent`s for `PreparedFrame`.
pub use netrender_device::{
    BrushBlurPipeline, BrushGradientPipeline, BrushImagePipeline, BrushRectSolidPipeline,
    BrushSolidPipeline, BrushTextPipeline, ClipRectanglePipeline, ColorAttachment,
    DepthAttachment, DrawIntent, REQUIRED_FEATURES, RenderPassTarget, WgpuDevice, WgpuHandles,
    boot, build_brush_blur, build_brush_gradient, build_brush_image, build_brush_rect_solid,
    build_brush_solid_specialized, build_brush_text, build_clip_rectangle,
};
