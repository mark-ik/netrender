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
#[cfg(feature = "gl_backend")]
mod telemetry;

#[cfg(feature = "gl_backend")]
mod batch;
#[cfg(feature = "gl_backend")]
mod border;
#[cfg(feature = "gl_backend")]
mod box_shadow;
#[cfg(any(feature = "capture", feature = "replay"))]
mod capture;
#[cfg(feature = "gl_backend")]
mod clip;
#[cfg(feature = "gl_backend")]
mod space;
#[cfg(feature = "gl_backend")]
mod spatial_tree;
#[cfg(feature = "gl_backend")]
mod command_buffer;
#[cfg(feature = "gl_backend")]
mod composite;
#[cfg(feature = "gl_backend")]
mod compositor;
mod debug_colors;
mod debug_font_data;
mod debug_item;
mod device;
mod ellipse;
#[cfg(feature = "gl_backend")]
mod filterdata;
#[cfg(feature = "gl_backend")]
mod frame_builder;
mod freelist;
#[cfg(feature = "gl_backend")]
mod glyph_cache;
#[cfg(feature = "gl_backend")]
mod gpu_cache;
#[cfg(feature = "gl_backend")]
mod gpu_types;
#[cfg(feature = "gl_backend")]
mod hit_test;
mod internal_types;
mod lru_cache;
#[cfg(feature = "gl_backend")]
mod pattern;
#[cfg(feature = "gl_backend")]
mod picture;
#[cfg(feature = "gl_backend")]
mod picture_graph;
#[cfg(feature = "gl_backend")]
mod prepare;
#[cfg(feature = "gl_backend")]
mod prim_store;
mod print_tree;
#[cfg(feature = "gl_backend")]
mod quad;
#[cfg(feature = "gl_backend")]
mod render_backend;
#[cfg(feature = "gl_backend")]
mod render_target;
#[cfg(feature = "gl_backend")]
mod render_task_graph;
#[cfg(feature = "gl_backend")]
mod render_task_cache;
#[cfg(feature = "gl_backend")]
mod render_task;
#[cfg(feature = "gl_backend")]
mod renderer;
#[cfg(feature = "gl_backend")]
mod resource_cache;
#[cfg(feature = "gl_backend")]
mod scene;
#[cfg(feature = "gl_backend")]
mod scene_builder_thread;
#[cfg(feature = "gl_backend")]
mod scene_building;
#[cfg(feature = "gl_backend")]
mod screen_capture;
mod segment;
#[cfg(feature = "gl_backend")]
mod spatial_node;
#[cfg(feature = "gl_backend")]
mod surface;
#[cfg(feature = "gl_backend")]
mod texture_pack;
#[cfg(feature = "gl_backend")]
mod texture_cache;
#[cfg(feature = "gl_backend")]
mod tile_cache;
mod util;
#[cfg(feature = "gl_backend")]
mod visibility;
#[cfg(feature = "gl_backend")]
mod api_resources;
mod image_tiling;
#[cfg(feature = "gl_backend")]
mod image_source;
mod rectangle_occlusion;
#[cfg(feature = "gl_backend")]
mod picture_textures;
mod frame_allocator;
mod bump_allocator;

///
pub mod intern;
///
#[cfg(feature = "gl_backend")]
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
#[cfg(feature = "gl_backend")]
pub use crate::composite::{CompositorConfig, Compositor, CompositorCapabilities, CompositorSurfaceTransform};
#[cfg(feature = "gl_backend")]
pub use crate::composite::{NativeSurfaceId, NativeTileId, NativeSurfaceInfo, PartialPresentCompositor};
#[cfg(feature = "gl_backend")]
pub use crate::composite::{MappableCompositor, MappedTileInfo, SWGLCompositeSurfaceInfo, WindowVisibility, WindowProperties};
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
    GraphicsApiInfo, PendingShadersToPrecache, PipelineInfo, Renderer, RendererError, RenderResults,
    RendererStats, Shaders, SharedShaders, ShaderPrecacheFlags,
    MAX_VERTEX_TEXTURE_WIDTH,
};
#[cfg(feature = "gl_backend")]
pub use crate::renderer::init::{WebRenderOptions, create_webrender_instance, AsyncPropertySampler, SceneBuilderHooks, RenderBackendHooks, ONE_TIME_USAGE_HINT};
#[cfg(feature = "gl_backend")]
pub use crate::hit_test::SharedHitTester;
pub use crate::internal_types::FastHashMap;
#[cfg(feature = "gl_backend")]
pub use crate::screen_capture::{AsyncScreenshotHandle, RecordedFrameHandle};
#[cfg(feature = "gl_backend")]
pub use crate::texture_cache::TextureCacheConfig;
pub use api as webrender_api;
pub use webrender_build::shader::{ProgramSourceDigest, ShaderKind};
#[cfg(feature = "gl_backend")]
pub use crate::picture::{TileDescriptor, TileId, InvalidationReason};
#[cfg(feature = "gl_backend")]
pub use crate::picture::{PrimitiveCompareResult, CompareHelperResult};
#[cfg(feature = "gl_backend")]
pub use crate::picture::{TileNode, TileNodeKind, TileOffset};
pub use crate::intern::ItemUid;
#[cfg(feature = "gl_backend")]
pub use crate::render_api::*;
#[cfg(feature = "gl_backend")]
pub use crate::tile_cache::{PictureCacheDebugInfo, DirtyTileDebugInfo, TileDebugInfo, SliceDebugInfo};
pub use crate::util::FastTransform;
pub use glyph_rasterizer;
pub use bump_allocator::ChunkPool;

#[cfg(feature = "sw_compositor")]
pub use crate::compositor::sw_compositor;

#[cfg(feature = "debugger")]
mod debugger;
