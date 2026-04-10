/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

/*!
A GPU based renderer for the web.

It serves as an experimental render backend for [Servo](https://servo.org/),
but it can also be used as such in a standalone application.

# External dependencies
WebRender currently depends on [FreeType](https://www.freetype.org/)

# Api Structure
The main entry point to WebRender is the [`crate::Renderer`].

By calling [`Renderer::new(...)`](crate::Renderer::new) you get a [`Renderer`], as well as
a [`RenderApiSender`](api::RenderApiSender). Your [`Renderer`] is responsible to render the
previously processed frames onto the screen.

By calling [`yourRenderApiSender.create_api()`](api::RenderApiSender::create_api), you'll
get a [`RenderApi`](api::RenderApi) instance, which is responsible for managing resources
and documents. A worker thread is used internally to untie the workload from the application
thread and therefore be able to make better use of multicore systems.

## Frame

What is referred to as a `frame`, is the current geometry on the screen.
A new Frame is created by calling [`set_display_list()`](api::Transaction::set_display_list)
on the [`RenderApi`](api::RenderApi). When the geometry is processed, the application will be
informed via a [`RenderNotifier`](api::RenderNotifier), a callback which you pass to
[`Renderer::new`].
More information about [stacking contexts][stacking_contexts].

[`set_display_list()`](api::Transaction::set_display_list) also needs to be supplied with
[`BuiltDisplayList`](api::BuiltDisplayList)s. These are obtained by finalizing a
[`DisplayListBuilder`](api::DisplayListBuilder). These are used to draw your geometry. But it
doesn't only contain trivial geometry, it can also store another
[`StackingContext`](api::StackingContext), as they're nestable.

[stacking_contexts]: https://developer.mozilla.org/en-US/docs/Web/CSS/CSS_Positioning/Understanding_z_index/The_stacking_context
*/

#![allow(
    clippy::unreadable_literal,
    clippy::new_without_default,
    clippy::too_many_arguments,
    unknown_lints,
    mismatched_lifetime_syntaxes
)]


// Cribbed from the |matches| crate, for simplicity.
macro_rules! matches {
    ($expression:expr, $($pattern:tt)+) => {
        match $expression {
            $($pattern)+ => true,
            _ => false
        }
    }
}

#[macro_use]
extern crate bitflags;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
#[macro_use]
extern crate malloc_size_of_derive;
#[cfg(any(feature = "serde"))]
#[macro_use]
extern crate serde;
#[macro_use]
extern crate tracy_rs;
#[macro_use]
extern crate derive_more;
extern crate malloc_size_of;
extern crate svg_fmt;

#[macro_use]
mod profiler;
// ── Scene pipeline modules ────────────────────────────────────────────────────
// These modules implement the WebRender scene processing pipeline (display
// lists → primitives → batches → render tasks).  They are backend-agnostic and
// required by both GL and wgpu render paths.  Gated on "any backend enabled".
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod thread_util;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod telemetry;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod batch;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod border;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod box_shadow;
#[cfg(any(feature = "capture", feature = "replay"))]
mod capture;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod clip;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod space;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod spatial_tree;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod composite;
mod debug_colors;
mod debug_font_data;
mod debug_item;
mod device;
mod ellipse;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod filterdata;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod frame_builder;
mod freelist;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod glyph_cache;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod gpu_cache;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod gpu_types;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod hit_test;
mod internal_types;
mod lru_cache;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod pattern;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod picture;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod picture_graph;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod prepare;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod prim_store;
mod print_tree;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod quad;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod render_backend;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod render_task_graph;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod render_task_cache;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod render_task;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod renderer;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod resource_cache;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod scene;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod scene_builder_thread;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod scene_building;
mod segment;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod spatial_node;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod surface;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod texture_pack;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod texture_cache;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod tile_cache;
mod util;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod visibility;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod api_resources;
mod image_tiling;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod image_source;
mod rectangle_occlusion;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod picture_textures;

// ── Additional scene pipeline modules ──────────────────────────────────────────
// render_target and command_buffer define the abstract render task data
// structures (what to render), used by both GL and wgpu render paths.
// render_api carries the cross-thread message protocol for the render backend.
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod command_buffer;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
mod render_target;

// ── GL-specific modules ────────────────────────────────────────────────────────
// These implement the GL compositor and screen capture.
// Only compiled when the GL backend is enabled.
#[cfg(feature = "gl_backend")]
mod compositor;
#[cfg(feature = "gl_backend")]
mod screen_capture;
mod frame_allocator;
mod bump_allocator;

#[cfg(feature = "wgpu_backend")]
pub use crate::device::{TextureFilter, WgpuDevice, WgpuTexture};
#[cfg(feature = "wgpu_backend")]
pub use crate::internal_types::RenderTargetInfo;
/// Re-export wgpu so downstream consumers can use the same version
/// for creating surfaces to pass to `RendererBackend::Wgpu`.
#[cfg(feature = "wgpu_backend")]
pub use wgpu;

///
pub mod intern;
///
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
pub mod render_api;

pub mod shader_source {
    include!(concat!(env!("OUT_DIR"), "/shaders.rs"));
}

extern crate bincode;
extern crate byteorder;
pub extern crate euclid;
extern crate rustc_hash;
#[cfg(feature = "gl_backend")]
extern crate gleam;
extern crate num_traits;
extern crate plane_split;
extern crate rayon;
#[cfg(feature = "ron")]
extern crate ron;
#[macro_use]
extern crate smallvec;
#[cfg(all(feature = "capture", feature = "png"))]
extern crate png;
#[cfg(test)]
extern crate rand;

pub extern crate api;
extern crate webrender_build;

#[cfg(feature = "gl_backend")]
#[doc(hidden)]
pub use crate::composite::{LayerCompositor, CompositorInputConfig, CompositorSurfaceUsage, ClipRadius};
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
pub use crate::composite::{CompositorConfig, CompositorCapabilities, CompositorSurfaceTransform};
#[cfg(feature = "gl_backend")]
pub use crate::composite::Compositor;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
pub use crate::composite::{NativeSurfaceId, NativeTileId, NativeSurfaceInfo, PartialPresentCompositor};
#[cfg(feature = "gl_backend")]
pub use crate::composite::MappableCompositor;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
pub use crate::composite::{MappedTileInfo, SWGLCompositeSurfaceInfo, WindowVisibility, WindowProperties};
#[cfg(feature = "gl_backend")]
pub use crate::device::{UploadMethod, VertexUsageHint, get_gl_target, get_unoptimized_shader_source};
#[cfg(feature = "gl_backend")]
pub use crate::device::{ProgramBinary, ProgramCache, ProgramCacheObserver, FormatDesc, ShaderError};
#[cfg(feature = "gl_backend")]
pub use crate::device::Device;
pub use crate::profiler::{ProfilerHooks, set_profiler_hooks};
#[cfg(feature = "gl_backend")]
pub use crate::renderer::{
    CpuProfile, DebugFlags, GpuProfile, GraphicsApi,
    GraphicsApiInfo, PipelineInfo, Renderer, RendererError, RenderResults,
    RendererStats, MAX_VERTEX_TEXTURE_WIDTH,
};
// RendererBackend and create_webrender_instance_with_backend cover all
// backends and are always exported (when any backend is enabled).
pub use crate::device::RendererBackend;
#[cfg(feature = "wgpu_backend")]
pub use crate::renderer::{WgpuExternalImageHandler, WgpuExternalImage};
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
pub use crate::renderer::init::{
    WebRenderOptions, create_webrender_instance_with_backend,
    AsyncPropertySampler, SceneBuilderHooks, RenderBackendHooks,
};
#[cfg(feature = "gl_backend")]
pub use crate::renderer::{
    PendingShadersToPrecache, Shaders, SharedShaders, ShaderPrecacheFlags,
};
#[cfg(feature = "gl_backend")]
pub use crate::renderer::init::{create_webrender_instance, ONE_TIME_USAGE_HINT};
#[cfg(feature = "gl_backend")]
pub use crate::hit_test::SharedHitTester;
#[cfg(feature = "wasm")]
pub use crate::thread_util::init_wasm_rayon_pool;
pub use crate::internal_types::FastHashMap;
#[cfg(feature = "gl_backend")]
pub use crate::screen_capture::{AsyncScreenshotHandle, RecordedFrameHandle};
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
pub use crate::texture_cache::TextureCacheConfig;
pub use api as webrender_api;
pub use webrender_build::shader::{ProgramSourceDigest, ShaderKind};
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
pub use crate::picture::{TileDescriptor, TileId, InvalidationReason};
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
pub use crate::picture::{PrimitiveCompareResult, CompareHelperResult};
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
pub use crate::picture::{TileNode, TileNodeKind, TileOffset};
pub use crate::intern::ItemUid;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
pub use crate::render_api::{RenderApiSender, ApiMsg, MemoryReport, DebugCommand, RenderApi};
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
pub use crate::render_api::Transaction;
#[cfg(any(feature = "capture", feature = "replay"))]
pub use crate::render_api::CaptureBits;
#[cfg(any(feature = "gl_backend", feature = "wgpu_backend"))]
pub use crate::tile_cache::{PictureCacheDebugInfo, DirtyTileDebugInfo, TileDebugInfo, SliceDebugInfo};
pub use crate::util::FastTransform;
pub use glyph_rasterizer;
pub use bump_allocator::ChunkPool;

#[cfg(feature = "sw_compositor")]
pub use crate::compositor::sw_compositor;

#[cfg(feature = "debugger")]
mod debugger;
