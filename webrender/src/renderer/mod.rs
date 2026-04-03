/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The high-level module responsible for interfacing with the GPU.
//!
//! Much of WebRender's design is driven by separating work into different
//! threads. To avoid the complexities of multi-threaded GPU access, we restrict
//! all communication with the GPU to one thread, the render thread. But since
//! issuing GPU commands is often a bottleneck, we move everything else (i.e.
//! the computation of what commands to issue) to another thread, the
//! RenderBackend thread. The RenderBackend, in turn, may delegate work to other
//! thread (like the SceneBuilder threads or Rayon workers), but the
//! Render-vs-RenderBackend distinction is the most important.
//!
//! The consumer is responsible for initializing the render thread before
//! calling into WebRender, which means that this module also serves as the
//! initial entry point into WebRender, and is responsible for spawning the
//! various other threads discussed above. That said, WebRender initialization
//! returns both the `Renderer` instance as well as a channel for communicating
//! directly with the `RenderBackend`. Aside from a few high-level operations
//! like 'render now', most of interesting commands from the consumer go over
//! that channel and operate on the `RenderBackend`.
//!
//! ## Space conversion guidelines
//! At this stage, we shuld be operating with `DevicePixel` and `FramebufferPixel` only.
//! "Framebuffer" space represents the final destination of our rendeing,
//! and it happens to be Y-flipped on OpenGL. The conversion is done as follows:
//!   - for rasterized primitives, the orthographics projection transforms
//! the content rectangle to -1 to 1
//!   - the viewport transformation is setup to map the whole range to
//! the framebuffer rectangle provided by the document view, stored in `DrawTarget`
//!   - all the direct framebuffer operations, like blitting, reading pixels, and setting
//! up the scissor, are accepting already transformed coordinates, which we can get by
//! calling `DrawTarget::to_framebuffer_rect`

use api::{ClipMode, ColorF, ColorU, MixBlendMode};
use api::{DocumentId, Epoch, ExternalImageHandler, RenderReasons};
#[cfg(feature = "replay")]
use api::ExternalImageId;
use api::{ExternalImageSource, ExternalImageType, ImageFormat, PremultipliedColorF};
use api::{PipelineId, ImageRendering, Checkpoint, NotificationRequest, ImageBufferKind};
#[cfg(feature = "replay")]
use api::ExternalImage;
use api::FramePublishId;
use api::units::*;
use api::channel::{Sender, Receiver};
pub use api::DebugFlags;
use core::time::Duration;

use crate::pattern::PatternKind;
use crate::render_api::{DebugCommand, ApiMsg, MemoryReport};
use crate::batch::{AlphaBatchContainer, BatchKind, BatchFeatures, BatchTextures, BrushBatchKind, ClipBatchList, PrimitiveBatch};
use crate::batch::ClipMaskInstanceList;
#[cfg(any(feature = "capture", feature = "replay"))]
use crate::capture::{CaptureConfig, ExternalCaptureImage, PlainExternalImage};
use crate::composite::{CompositeState, CompositeTile, CompositeTileSurface, CompositorInputLayer, CompositorSurfaceTransform, ResolvedExternalSurface};
use crate::composite::{CompositorKind, NativeTileId, CompositeFeatures, CompositeSurfaceFormat, ResolvedExternalSurfaceColorData};
#[cfg(feature = "gl_backend")]
use crate::composite::Compositor;
use crate::composite::{CompositorConfig, NativeSurfaceOperationDetails, NativeSurfaceId, NativeSurfaceOperation, ClipRadius};
use crate::composite::TileKind;
#[cfg(feature = "debugger")]
use api::debugger::CompositorDebugInfo;
use crate::segment::SegmentBuilder;
use crate::{debug_colors, CompositorInputConfig, CompositorSurfaceUsage};
use crate::device::{GpuDevice, GpuFrameId, TextureFilter, TextureFlags, TextureSlot, Texel};
#[cfg(feature = "gl_backend")]
use crate::device::{DepthFunction, Device, DrawTarget, ExternalTexture};
#[cfg(feature = "gl_backend")]
use crate::device::{ReadTarget, ShaderError, Texture};
#[cfg(feature = "wgpu_backend")]
use crate::device::WgpuDevice;
use crate::device::query::{GpuSampler, GpuTimer};
#[cfg(all(feature = "capture", feature = "gl_backend"))]
use crate::device::FBOId;
use crate::debug_item::DebugItem;
use crate::frame_builder::Frame;
use glyph_rasterizer::GlyphFormat;
use crate::gpu_cache::{GpuCacheUpdate, GpuCacheUpdateList};
use crate::gpu_cache::{GpuCacheDebugChunk, GpuCacheDebugCmd};
use crate::gpu_types::{ScalingInstance, SvgFilterInstance, SVGFEFilterInstance, CopyInstance, PrimitiveInstanceData};
use crate::gpu_types::{BlurInstance, ClearInstance, CompositeInstance, ZBufferId};
use crate::internal_types::{TextureSource, TextureSourceExternal, TextureCacheCategory, FrameId, FrameVec};
#[cfg(any(feature = "capture", feature = "replay"))]
use crate::internal_types::DebugOutput;
use crate::internal_types::{CacheTextureId, FastHashMap, FastHashSet, RenderedDocument, ResultMsg};
use crate::internal_types::{TextureCacheAllocInfo, TextureCacheAllocationKind, TextureUpdateList};
use crate::internal_types::{RenderTargetInfo, Swizzle, DeferredResolveIndex};
use crate::picture::{ResolvedSurfaceTexture, TileId};
use crate::prim_store::DeferredResolve;
use crate::profiler::{self, GpuProfileTag, TransactionProfile};
use crate::profiler::{Profiler, add_event_marker, add_text_marker, thread_is_being_profiled};
use crate::device::query::GpuProfiler;
use crate::render_target::ResolveOp;
use crate::render_task_graph::RenderTaskGraph;
use crate::render_task::{RenderTask, RenderTaskKind, ReadbackTask};
#[cfg(feature = "gl_backend")]
use crate::screen_capture::AsyncScreenshotGrabber;
use crate::render_target::{RenderTarget, PictureCacheTarget, PictureCacheTargetKind};
use crate::render_target::{RenderTargetKind, BlitJob};
use crate::telemetry::Telemetry;
use crate::tile_cache::PictureCacheDebugInfo;
use crate::util::drain_filter;
use crate::rectangle_occlusion as occlusion;
#[cfg(feature = "debugger")]
use crate::debugger::{Debugger, DebugQueryKind};
#[cfg(feature = "gl_backend")]
use upload::{upload_to_texture_cache, RendererUploadState};
use init::*;

use euclid::{rect, Transform3D, Scale, default};
#[cfg(feature = "gl_backend")]
use gleam::gl;
use malloc_size_of::MallocSizeOfOps;

#[cfg(feature = "replay")]
use std::sync::Arc;

use std::{
    cell::RefCell,
    collections::VecDeque,
    f32,
    ffi::c_void,
    mem,
    path::PathBuf,
    rc::Rc,
};
#[cfg(any(feature = "capture", feature = "replay"))]
use std::collections::hash_map::Entry;

#[cfg(feature = "gl_backend")]
mod debug;
mod gpu_buffer;
#[cfg(feature = "gl_backend")]
mod gpu_cache;
#[cfg(feature = "gl_backend")]
mod shade;
#[cfg(feature = "gl_backend")]
mod vertex;
#[cfg(feature = "gl_backend")]
mod upload;
pub(crate) mod init;

#[cfg(feature = "gl_backend")]
pub use debug::DebugRenderer;
#[cfg(feature = "gl_backend")]
pub use shade::{PendingShadersToPrecache, Shaders, SharedShaders};
#[cfg(not(feature = "gl_backend"))]
pub type SharedShaders = ();
#[cfg(feature = "gl_backend")]
use shade::LazilyCompiledShader;
#[cfg(feature = "gl_backend")]
pub use vertex::{desc, VertexArrayKind, MAX_VERTEX_TEXTURE_WIDTH};
#[cfg(not(feature = "gl_backend"))]
pub const MAX_VERTEX_TEXTURE_WIDTH: usize = webrender_build::MAX_VERTEX_TEXTURE_WIDTH;
pub use gpu_buffer::{GpuBuffer, GpuBufferF, GpuBufferBuilderF, GpuBufferI, GpuBufferBuilderI};
pub use gpu_buffer::{GpuBufferAddress, GpuBufferBuilder, GpuBufferWriterF};

/// The size of the array of each type of vertex data texture that
/// is round-robin-ed each frame during bind_frame_data. Doing this
/// helps avoid driver stalls while updating the texture in some
/// drivers. The size of these textures are typically very small
/// (e.g. < 16 kB) so it's not a huge waste of memory. Despite that,
/// this is a short-term solution - we want to find a better way
/// to provide this frame data, which will likely involve some
/// combination of UBO/SSBO usage. Although this only affects some
/// platforms, it's enabled on all platforms to reduce testing
/// differences between platforms.
pub const VERTEX_DATA_TEXTURE_COUNT: usize = 3;

/// Number of GPU blocks per UV rectangle provided for an image.
pub const BLOCKS_PER_UV_RECT: usize = 2;

/// Shared helpers for computing the row-major texture layout used to
/// upload structured data (vertex headers, transforms, render tasks)
/// into data textures readable by the shader as `texelFetch`.
///
/// Both the GL path (`VertexDataTexture::update`) and the wgpu path
/// (`upload_frame_data_textures`) use the same layout: items are packed
/// row-by-row into a texture of width `MAX_VERTEX_TEXTURE_WIDTH` texels,
/// where each item occupies `texels_per_item` consecutive texels (each
/// texel = 16 bytes = one `vec4`).
pub(super) mod data_texture_layout {
    use super::MAX_VERTEX_TEXTURE_WIDTH;

    /// How many texels (vec4s) one item of type T occupies.
    /// T must be a multiple of 16 bytes (one RGBA32F texel).
    pub fn texels_per_item<T>() -> usize {
        let t = std::mem::size_of::<T>() / 16;
        debug_assert!(std::mem::size_of::<T>() % 16 == 0);
        debug_assert!(t > 0);
        t
    }

    /// How many items fit in one texture row.
    pub fn items_per_row(texels_per_item: usize) -> usize {
        debug_assert_ne!(texels_per_item, 0);
        MAX_VERTEX_TEXTURE_WIDTH / texels_per_item
    }

    /// Compute the texture height needed for `item_count` items.
    /// Returns at least 1 so the texture is always valid.
    pub fn required_height(item_count: usize, items_per_row: usize) -> usize {
        if item_count == 0 {
            1
        } else {
            (item_count + items_per_row - 1) / items_per_row
        }
    }

    /// The logical width in texels for uploading data.
    /// For a single row, this is just `item_count * texels_per_item`.
    /// For multiple rows, it is the largest aligned multiple of
    /// `texels_per_item` that fits in `MAX_VERTEX_TEXTURE_WIDTH`.
    pub fn logical_width(item_count: usize, texels_per_item: usize, height: usize) -> usize {
        if height == 1 {
            item_count * texels_per_item
        } else {
            MAX_VERTEX_TEXTURE_WIDTH - (MAX_VERTEX_TEXTURE_WIDTH % texels_per_item)
        }
    }
}

/// Shared GPU cache utilities used by both GL and wgpu backends.
///
/// `CacheRow` mirrors one row of the GPU cache texture on the CPU, with
/// dirty-range tracking for efficient partial uploads.
///
/// `for_each_gpu_cache_copy` centralises the `GpuCacheUpdate::Copy`
/// dispatch so both backends iterate updates through one code path.
pub(super) mod gpu_cache_utils {
    use crate::gpu_cache::{GpuBlockData, GpuCacheUpdate, GpuCacheUpdateList};
    use super::MAX_VERTEX_TEXTURE_WIDTH;

    /// CPU-side mirror of one row of the GPU cache texture,
    /// with dirty-range tracking for batched uploads.
    pub struct CacheRow {
        /// Mirrored block data on CPU for this row.
        pub cpu_blocks: Box<[GpuBlockData; MAX_VERTEX_TEXTURE_WIDTH]>,
        /// The first offset in this row that is dirty.
        min_dirty: u16,
        /// The last offset in this row that is dirty.
        max_dirty: u16,
    }

    impl CacheRow {
        pub fn new() -> Self {
            CacheRow {
                cpu_blocks: Box::new([GpuBlockData::EMPTY; MAX_VERTEX_TEXTURE_WIDTH]),
                min_dirty: MAX_VERTEX_TEXTURE_WIDTH as _,
                max_dirty: 0,
            }
        }

        pub fn is_dirty(&self) -> bool {
            self.min_dirty < self.max_dirty
        }

        pub fn clear_dirty(&mut self) {
            self.min_dirty = MAX_VERTEX_TEXTURE_WIDTH as _;
            self.max_dirty = 0;
        }

        pub fn add_dirty(&mut self, block_offset: usize, block_count: usize) {
            self.min_dirty = self.min_dirty.min(block_offset as _);
            self.max_dirty = self.max_dirty.max((block_offset + block_count) as _);
        }

        pub fn dirty_blocks(&self) -> &[GpuBlockData] {
            &self.cpu_blocks[self.min_dirty as usize .. self.max_dirty as usize]
        }

        pub fn min_dirty(&self) -> u16 {
            self.min_dirty
        }
    }

    /// Iterate over `GpuCacheUpdate::Copy` entries in an update list,
    /// calling `f(row, col_offset, source_blocks)` for each one.
    ///
    /// Both the GL (PixelBuffer) and wgpu backends share this dispatch
    /// logic — only the write-to-storage step differs.
    pub fn for_each_gpu_cache_copy(
        list: &GpuCacheUpdateList,
        mut f: impl FnMut(usize, usize, &[GpuBlockData]),
    ) {
        for update in &list.updates {
            match *update {
                GpuCacheUpdate::Copy {
                    block_index,
                    block_count,
                    address,
                } => {
                    let blocks = &list.blocks[block_index .. block_index + block_count];
                    f(address.v as usize, address.u as usize, blocks);
                }
            }
        }
    }
}

const GPU_TAG_BRUSH_OPACITY: GpuProfileTag = GpuProfileTag {
    label: "B_Opacity",
    color: debug_colors::DARKMAGENTA,
};
const GPU_TAG_BRUSH_LINEAR_GRADIENT: GpuProfileTag = GpuProfileTag {
    label: "B_LinearGradient",
    color: debug_colors::POWDERBLUE,
};
const GPU_TAG_BRUSH_YUV_IMAGE: GpuProfileTag = GpuProfileTag {
    label: "B_YuvImage",
    color: debug_colors::DARKGREEN,
};
const GPU_TAG_BRUSH_MIXBLEND: GpuProfileTag = GpuProfileTag {
    label: "B_MixBlend",
    color: debug_colors::MAGENTA,
};
const GPU_TAG_BRUSH_BLEND: GpuProfileTag = GpuProfileTag {
    label: "B_Blend",
    color: debug_colors::ORANGE,
};
const GPU_TAG_BRUSH_IMAGE: GpuProfileTag = GpuProfileTag {
    label: "B_Image",
    color: debug_colors::SPRINGGREEN,
};
const GPU_TAG_BRUSH_SOLID: GpuProfileTag = GpuProfileTag {
    label: "B_Solid",
    color: debug_colors::RED,
};
const GPU_TAG_CACHE_CLIP: GpuProfileTag = GpuProfileTag {
    label: "C_Clip",
    color: debug_colors::PURPLE,
};
const GPU_TAG_CACHE_BORDER: GpuProfileTag = GpuProfileTag {
    label: "C_Border",
    color: debug_colors::CORNSILK,
};
const GPU_TAG_CACHE_LINE_DECORATION: GpuProfileTag = GpuProfileTag {
    label: "C_LineDecoration",
    color: debug_colors::YELLOWGREEN,
};
const GPU_TAG_CACHE_FAST_LINEAR_GRADIENT: GpuProfileTag = GpuProfileTag {
    label: "C_FastLinearGradient",
    color: debug_colors::BROWN,
};
const GPU_TAG_CACHE_LINEAR_GRADIENT: GpuProfileTag = GpuProfileTag {
    label: "C_LinearGradient",
    color: debug_colors::BROWN,
};
const GPU_TAG_GRADIENT: GpuProfileTag = GpuProfileTag {
    label: "C_Gradient",
    color: debug_colors::BROWN,
};
const GPU_TAG_RADIAL_GRADIENT: GpuProfileTag = GpuProfileTag {
    label: "C_RadialGradient",
    color: debug_colors::BROWN,
};
const GPU_TAG_CONIC_GRADIENT: GpuProfileTag = GpuProfileTag {
    label: "C_ConicGradient",
    color: debug_colors::BROWN,
};
const GPU_TAG_SETUP_TARGET: GpuProfileTag = GpuProfileTag {
    label: "target init",
    color: debug_colors::SLATEGREY,
};
const GPU_TAG_SETUP_DATA: GpuProfileTag = GpuProfileTag {
    label: "data init",
    color: debug_colors::LIGHTGREY,
};
const GPU_TAG_PRIM_SPLIT_COMPOSITE: GpuProfileTag = GpuProfileTag {
    label: "SplitComposite",
    color: debug_colors::DARKBLUE,
};
const GPU_TAG_PRIM_TEXT_RUN: GpuProfileTag = GpuProfileTag {
    label: "TextRun",
    color: debug_colors::BLUE,
};
const GPU_TAG_PRIMITIVE: GpuProfileTag = GpuProfileTag {
    label: "Primitive",
    color: debug_colors::RED,
};
const GPU_TAG_INDIRECT_PRIM: GpuProfileTag = GpuProfileTag {
    label: "Primitive (indirect)",
    color: debug_colors::YELLOWGREEN,
};
const GPU_TAG_INDIRECT_MASK: GpuProfileTag = GpuProfileTag {
    label: "Mask (indirect)",
    color: debug_colors::IVORY,
};
const GPU_TAG_BLUR: GpuProfileTag = GpuProfileTag {
    label: "Blur",
    color: debug_colors::VIOLET,
};
const GPU_TAG_BLIT: GpuProfileTag = GpuProfileTag {
    label: "Blit",
    color: debug_colors::LIME,
};
const GPU_TAG_SCALE: GpuProfileTag = GpuProfileTag {
    label: "Scale",
    color: debug_colors::GHOSTWHITE,
};
const GPU_SAMPLER_TAG_ALPHA: GpuProfileTag = GpuProfileTag {
    label: "Alpha targets",
    color: debug_colors::BLACK,
};
const GPU_SAMPLER_TAG_OPAQUE: GpuProfileTag = GpuProfileTag {
    label: "Opaque pass",
    color: debug_colors::BLACK,
};
const GPU_SAMPLER_TAG_TRANSPARENT: GpuProfileTag = GpuProfileTag {
    label: "Transparent pass",
    color: debug_colors::BLACK,
};
const GPU_TAG_SVG_FILTER: GpuProfileTag = GpuProfileTag {
    label: "SvgFilter",
    color: debug_colors::LEMONCHIFFON,
};
const GPU_TAG_SVG_FILTER_NODES: GpuProfileTag = GpuProfileTag {
    label: "SvgFilterNodes",
    color: debug_colors::LEMONCHIFFON,
};
const GPU_TAG_COMPOSITE: GpuProfileTag = GpuProfileTag {
    label: "Composite",
    color: debug_colors::TOMATO,
};

type CompositeShaderParams = (
    CompositeSurfaceFormat,
    ImageBufferKind,
    CompositeFeatures,
    Option<DeviceSize>,
);

struct CompositeBatchState {
    shader_params: CompositeShaderParams,
    textures: BatchTextures,
    instances: Vec<CompositeInstance>,
}

impl CompositeBatchState {
    fn new() -> Self {
        Self {
            shader_params: (
                CompositeSurfaceFormat::Rgba,
                ImageBufferKind::Texture2D,
                CompositeFeatures::empty(),
                None,
            ),
            textures: BatchTextures::empty(),
            instances: Vec::new(),
        }
    }
}

/// Per-container pass state for alpha batch drawing.
///
/// Tracks blend-mode transitions across batches within a single
/// `draw_alpha_batch_container` call. This is the first subsystem
/// boundary for the alpha-batch draw path — it owns policy state
/// while leaving GL execution to `Renderer` / `Device`.
struct AlphaBatchPassState {
    prev_blend_mode: BlendMode,
}

impl AlphaBatchPassState {
    fn new() -> Self {
        Self {
            prev_blend_mode: BlendMode::None,
        }
    }

    /// Returns true if the blend mode changed and the caller needs
    /// to apply the new mode on the device.
    fn transition_blend_mode(&mut self, blend_mode: BlendMode) -> bool {
        if blend_mode != self.prev_blend_mode {
            self.prev_blend_mode = blend_mode;
            true
        } else {
            false
        }
    }
}

// Key used when adding compositing tiles to the occlusion tracker.
// Since an entire tile may have a mask, but we may segment that in
// to masked and non-masked regions, we need to track which of the
// occlusion tracker outputs need a mask
#[derive(Debug, Copy, Clone)]
struct OcclusionItemKey {
    tile_index: usize,
    needs_mask: bool,
}

// Defines the content that we will draw to a given swapchain / layer, calculated
// after occlusion culling.
struct SwapChainLayer {
    occlusion: occlusion::FrontToBackBuilder<OcclusionItemKey>,
    clear_tiles: Vec<occlusion::Item<OcclusionItemKey>>,
}

// Store rects state of tile used for compositing with layer compositor
struct CompositeTileState {
    pub local_rect: PictureRect,
    pub local_valid_rect: PictureRect,
    pub device_clip_rect: DeviceRect,
    pub z_id: ZBufferId,
    pub device_tile_box: DeviceRect,
    pub visible_rects: Vec<DeviceRect>,
}

impl CompositeTileState {
    pub fn same_state(&self, other: &CompositeTileState) -> bool {
        self.local_rect == other.local_rect &&
        self.local_valid_rect == other.local_valid_rect &&
        self.device_clip_rect == other.device_clip_rect &&
        self.z_id == other.z_id &&
        self.device_tile_box == other.device_tile_box
    }
}

/// The list of tiles and rects used for compositing to a frame with layer compositor
struct LayerCompositorFrameState {
    tile_states: FastHashMap<TileId, CompositeTileState>,
    pub rects_without_id: Vec<DeviceRect>,
}

/// The clear color used for the texture cache when the debug display is enabled.
/// We use a shade of blue so that we can still identify completely blue items in
/// the texture cache.
pub const TEXTURE_CACHE_DBG_CLEAR_COLOR: [f32; 4] = [0.0, 0.0, 0.8, 1.0];

impl BatchKind {
    fn sampler_tag(&self) -> GpuProfileTag {
        match *self {
            BatchKind::SplitComposite => GPU_TAG_PRIM_SPLIT_COMPOSITE,
            BatchKind::Brush(kind) => {
                match kind {
                    BrushBatchKind::Solid => GPU_TAG_BRUSH_SOLID,
                    BrushBatchKind::Image(..) => GPU_TAG_BRUSH_IMAGE,
                    BrushBatchKind::Blend => GPU_TAG_BRUSH_BLEND,
                    BrushBatchKind::MixBlend { .. } => GPU_TAG_BRUSH_MIXBLEND,
                    BrushBatchKind::YuvImage(..) => GPU_TAG_BRUSH_YUV_IMAGE,
                    BrushBatchKind::LinearGradient => GPU_TAG_BRUSH_LINEAR_GRADIENT,
                    BrushBatchKind::Opacity => GPU_TAG_BRUSH_OPACITY,
                }
            }
            BatchKind::TextRun(_) => GPU_TAG_PRIM_TEXT_RUN,
            BatchKind::Quad(PatternKind::ColorOrTexture) => GPU_TAG_PRIMITIVE,
            BatchKind::Quad(PatternKind::Gradient) => GPU_TAG_GRADIENT,
            BatchKind::Quad(PatternKind::RadialGradient) => GPU_TAG_RADIAL_GRADIENT,
            BatchKind::Quad(PatternKind::ConicGradient) => GPU_TAG_CONIC_GRADIENT,
            BatchKind::Quad(PatternKind::Mask) => GPU_TAG_INDIRECT_MASK,
        }
    }
}

fn flag_changed(before: DebugFlags, after: DebugFlags, select: DebugFlags) -> Option<bool> {
    if before & select != after & select {
        Some(after.contains(select))
    } else {
        None
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub enum ShaderColorMode {
    Alpha = 0,
    SubpixelDualSource = 1,
    BitmapShadow = 2,
    ColorBitmap = 3,
    Image = 4,
    MultiplyDualSource = 5,
}

impl From<GlyphFormat> for ShaderColorMode {
    fn from(format: GlyphFormat) -> ShaderColorMode {
        match format {
            GlyphFormat::Alpha |
            GlyphFormat::TransformedAlpha |
            GlyphFormat::Bitmap => ShaderColorMode::Alpha,
            GlyphFormat::Subpixel | GlyphFormat::TransformedSubpixel => {
                panic!("Subpixel glyph formats must be handled separately.");
            }
            GlyphFormat::ColorBitmap => ShaderColorMode::ColorBitmap,
        }
    }
}

/// Enumeration of the texture samplers used across the various WebRender shaders.
///
/// Each variant corresponds to a uniform declared in shader source. We only bind
/// the variants we need for a given shader, so not every variant is bound for every
/// batch.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum TextureSampler {
    Color0,
    Color1,
    Color2,
    GpuCache,
    TransformPalette,
    RenderTasks,
    Dither,
    PrimitiveHeadersF,
    PrimitiveHeadersI,
    ClipMask,
    GpuBufferF,
    GpuBufferI,
}

impl TextureSampler {
    pub(crate) fn color(n: usize) -> TextureSampler {
        match n {
            0 => TextureSampler::Color0,
            1 => TextureSampler::Color1,
            2 => TextureSampler::Color2,
            _ => {
                panic!("There are only 3 color samplers.");
            }
        }
    }
}

impl Into<TextureSlot> for TextureSampler {
    fn into(self) -> TextureSlot {
        match self {
            TextureSampler::Color0 => TextureSlot(0),
            TextureSampler::Color1 => TextureSlot(1),
            TextureSampler::Color2 => TextureSlot(2),
            TextureSampler::GpuCache => TextureSlot(3),
            TextureSampler::TransformPalette => TextureSlot(4),
            TextureSampler::RenderTasks => TextureSlot(5),
            TextureSampler::Dither => TextureSlot(6),
            TextureSampler::PrimitiveHeadersF => TextureSlot(7),
            TextureSampler::PrimitiveHeadersI => TextureSlot(8),
            TextureSampler::ClipMask => TextureSlot(9),
            TextureSampler::GpuBufferF => TextureSlot(10),
            TextureSampler::GpuBufferI => TextureSlot(11),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum GraphicsApi {
    OpenGL,
}

#[derive(Clone, Debug)]
pub struct GraphicsApiInfo {
    pub kind: GraphicsApi,
    pub renderer: String,
    pub version: String,
}

#[derive(Debug)]
pub struct GpuProfile {
    pub frame_id: GpuFrameId,
    pub paint_time_ns: u64,
}

impl GpuProfile {
    fn new(frame_id: GpuFrameId, timers: &[GpuTimer]) -> GpuProfile {
        let mut paint_time_ns = 0;
        for timer in timers {
            paint_time_ns += timer.time_ns;
        }
        GpuProfile {
            frame_id,
            paint_time_ns,
        }
    }
}

#[derive(Debug)]
pub struct CpuProfile {
    pub frame_id: GpuFrameId,
    pub backend_time_ns: u64,
    pub composite_time_ns: u64,
    pub draw_calls: usize,
}

impl CpuProfile {
    fn new(
        frame_id: GpuFrameId,
        backend_time_ns: u64,
        composite_time_ns: u64,
        draw_calls: usize,
    ) -> CpuProfile {
        CpuProfile {
            frame_id,
            backend_time_ns,
            composite_time_ns,
            draw_calls,
        }
    }
}

/// The selected partial present mode for a given frame.
#[derive(Debug, Copy, Clone)]
enum PartialPresentMode {
    /// The device supports fewer dirty rects than the number of dirty rects
    /// that WR produced. In this case, the WR dirty rects are union'ed into
    /// a single dirty rect, that is provided to the caller.
    Single {
        dirty_rect: DeviceRect,
    },
}

#[cfg(feature = "gl_backend")]
struct CacheTexture {
    texture: Texture,
    category: TextureCacheCategory,
}

/// Helper struct for resolving device Textures for use during rendering passes.
///
/// Manages the mapping between the at-a-distance texture handles used by the
/// `RenderBackend` (which does not directly interface with the GPU) and actual
/// device texture handles.
#[cfg(feature = "gl_backend")]
struct TextureResolver {
    /// A map to resolve texture cache IDs to native textures.
    texture_cache_map: FastHashMap<CacheTextureId, CacheTexture>,

    /// Map of external image IDs to native textures.
    external_images: FastHashMap<DeferredResolveIndex, ExternalTexture>,

    /// A special 1x1 dummy texture used for shaders that expect to work with
    /// the output of the previous pass but are actually running in the first
    /// pass. None in wgpu-only mode (GL draw paths are not used).
    dummy_cache_texture: Option<Texture>,
}

#[cfg(feature = "gl_backend")]
fn create_dummy_cache_texture<D: GpuDevice<Texture = Texture>>(device: &mut D) -> Texture {
    let dummy_cache_texture = device.create_texture(
        ImageBufferKind::Texture2D,
        ImageFormat::RGBA8,
        1,
        1,
        TextureFilter::Linear,
        None,
    );
    device.upload_texture_immediate(&dummy_cache_texture, &[0xff, 0xff, 0xff, 0xff]);
    dummy_cache_texture
}

#[cfg(feature = "gl_backend")]
fn create_gpu_buffer_texture<D: GpuDevice<Texture = Texture>, T: Texel>(
    device: &mut D,
    buffer: &GpuBuffer<T>,
) -> Option<Texture> {
    if buffer.is_empty() {
        None
    } else {
        let gpu_buffer_texture = device.create_texture(
            ImageBufferKind::Texture2D,
            buffer.format,
            buffer.size.width,
            buffer.size.height,
            TextureFilter::Nearest,
            None,
        );

        device.upload_texture_immediate(&gpu_buffer_texture, &buffer.data);
        Some(gpu_buffer_texture)
    }
}

#[cfg(feature = "gl_backend")]
fn create_cache_texture<D: GpuDevice<Texture = Texture>>(
    device: &mut D,
    info: &TextureCacheAllocInfo,
) -> Texture {
    device.create_texture(
        info.target,
        info.format,
        info.width,
        info.height,
        info.filter,
        Some(RenderTargetInfo { has_depth: info.has_depth }),
    )
}

#[cfg(feature = "gl_backend")]
impl TextureResolver {
    fn new(device: &mut Device) -> TextureResolver {
        let dummy_cache_texture = create_dummy_cache_texture(device);

        TextureResolver {
            texture_cache_map: FastHashMap::default(),
            external_images: FastHashMap::default(),
            dummy_cache_texture: Some(dummy_cache_texture),
        }
    }

    fn new_without_gl() -> TextureResolver {
        TextureResolver {
            texture_cache_map: FastHashMap::default(),
            external_images: FastHashMap::default(),
            dummy_cache_texture: None,
        }
    }

    fn deinit(self, device: &mut Device) {
        if let Some(tex) = self.dummy_cache_texture {
            device.delete_texture(tex);
        }

        for (_id, item) in self.texture_cache_map {
            device.delete_texture(item.texture);
        }
    }

    fn begin_frame(&mut self) {
    }

    fn end_pass(
        &mut self,
        device: &mut Device,
        textures_to_invalidate: &[CacheTextureId],
    ) {
        // For any texture that is no longer needed, immediately
        // invalidate it so that tiled GPUs don't need to resolve it
        // back to memory.
        for texture_id in textures_to_invalidate {
            let render_target = &self.texture_cache_map[texture_id].texture;
            device.invalidate_render_target(render_target);
        }
    }

    // Bind a source texture to the device.
    fn bind(&self, texture_id: &TextureSource, sampler: TextureSampler, device: &mut Device) -> Swizzle {
        match *texture_id {
            TextureSource::Invalid => {
                Swizzle::default()
            }
            TextureSource::Dummy => {
                let swizzle = Swizzle::default();
                device.bind_texture(sampler, self.dummy_cache_texture.as_ref().unwrap(), swizzle);
                swizzle
            }
            TextureSource::External(TextureSourceExternal { ref index, .. }) => {
                let texture = self.external_images
                    .get(index)
                    .expect("BUG: External image should be resolved by now");
                device.bind_external_texture(sampler, texture);
                Swizzle::default()
            }
            TextureSource::TextureCache(index, swizzle) => {
                let texture = &self.texture_cache_map[&index].texture;
                device.bind_texture(sampler, texture, swizzle);
                swizzle
            }
        }
    }

    fn bind_batch_textures(
        &self,
        textures: &BatchTextures,
        device: &mut Device,
        aux_textures: &RendererAuxTextures,
    ) {
        for i in 0 .. 3 {
            self.bind(
                &textures.input.colors[i],
                TextureSampler::color(i),
                device,
            );
        }

        self.bind(
            &textures.clip_mask,
            TextureSampler::ClipMask,
            device,
        );

        if let Some(texture) = aux_textures.dither_texture() {
            device.bind_texture(TextureSampler::Dither, texture, Swizzle::default());
        }
    }

    // Get the real (OpenGL) texture ID for a given source texture.
    // For a texture cache texture, the IDs are stored in a vector
    // map for fast access.
    fn resolve(&self, texture_id: &TextureSource) -> Option<(&Texture, Swizzle)> {
        match *texture_id {
            TextureSource::Invalid => None,
            TextureSource::Dummy => {
                self.dummy_cache_texture.as_ref().map(|t| (t, Swizzle::default()))
            }
            TextureSource::External(..) => {
                panic!("BUG: External textures cannot be resolved, they can only be bound.");
            }
            TextureSource::TextureCache(index, swizzle) => {
                Some((&self.texture_cache_map[&index].texture, swizzle))
            }
        }
    }

    // Retrieve the deferred / resolved UV rect if an external texture, otherwise
    // return the default supplied UV rect.
    fn get_uv_rect(
        &self,
        source: &TextureSource,
        default_value: TexelRect,
    ) -> TexelRect {
        match source {
            TextureSource::External(TextureSourceExternal { ref index, .. }) => {
                let texture = self.external_images
                    .get(index)
                    .expect("BUG: External image should be resolved by now");
                texture.get_uv_rect()
            }
            _ => {
                default_value
            }
        }
    }

    /// Returns the size of the texture in pixels
    fn get_texture_size(&self, texture: &TextureSource) -> DeviceIntSize {
        match *texture {
            TextureSource::Invalid => DeviceIntSize::zero(),
            TextureSource::TextureCache(id, _) => {
                self.texture_cache_map[&id].texture.get_dimensions()
            },
            TextureSource::External(TextureSourceExternal { index, .. }) => {
                // If UV coords are normalized then this value will be incorrect. However, the
                // texture size is currently only used to set the uTextureSize uniform, so that
                // shaders without access to textureSize() can normalize unnormalized UVs. Which
                // means this is not a problem.
                let uv_rect = self.external_images[&index].get_uv_rect();
                (uv_rect.uv1 - uv_rect.uv0).abs().to_size().to_i32()
            },
            TextureSource::Dummy => DeviceIntSize::new(1, 1),
        }
    }

    fn report_memory(&self) -> MemoryReport {
        let mut report = MemoryReport::default();

        // We're reporting GPU memory rather than heap-allocations, so we don't
        // use size_of_op.
        for item in self.texture_cache_map.values() {
            let counter = match item.category {
                TextureCacheCategory::Atlas => &mut report.atlas_textures,
                TextureCacheCategory::Standalone => &mut report.standalone_textures,
                TextureCacheCategory::PictureTile => &mut report.picture_tile_textures,
                TextureCacheCategory::RenderTarget => &mut report.render_target_textures,
            };
            *counter += item.texture.size_in_bytes();
        }

        report
    }

    fn update_profile(&self, profile: &mut TransactionProfile) {
        let mut external_image_bytes = 0;
        for img in self.external_images.values() {
            let uv_rect = img.get_uv_rect();
            // If UV coords are normalized then this value will be incorrect. This is unfortunate
            // but doesn't impact end users at all.
            let size = (uv_rect.uv1 - uv_rect.uv0).abs().to_size().to_i32();

            // Assume 4 bytes per pixels which is true most of the time but
            // not always.
            let bpp = 4;
            external_image_bytes += size.area() as usize * bpp;
        }

        profile.set(profiler::EXTERNAL_IMAGE_BYTES, profiler::bytes_to_mb(external_image_bytes));
    }

    fn get_cache_texture_mut(&mut self, id: &CacheTextureId) -> &mut Texture {
        &mut self.texture_cache_map
            .get_mut(id)
            .expect("bug: texture not allocated")
            .texture
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
#[cfg_attr(feature = "capture", derive(Serialize))]
#[cfg_attr(feature = "replay", derive(Deserialize))]
pub enum BlendMode {
    None,
    Alpha,
    PremultipliedAlpha,
    PremultipliedDestOut,
    SubpixelDualSource,
    Advanced(MixBlendMode),
    MultiplyDualSource,
    Screen,
    Exclusion,
    PlusLighter,
}

impl BlendMode {
    /// Decides when a given mix-blend-mode can be implemented in terms of
    /// simple blending, dual-source blending, advanced blending, or not at
    /// all based on available capabilities.
    pub fn from_mix_blend_mode(
        mode: MixBlendMode,
        advanced_blend: bool,
        coherent: bool,
        dual_source: bool,
    ) -> Option<BlendMode> {
        // If we emulate a mix-blend-mode via simple or dual-source blending,
        // care must be taken to output alpha As + Ad*(1-As) regardless of what
        // the RGB output is to comply with the mix-blend-mode spec.
        Some(match mode {
            // If we have coherent advanced blend, just use that.
            _ if advanced_blend && coherent => BlendMode::Advanced(mode),
            // Screen can be implemented as Cs + Cd - Cs*Cd => Cs + Cd*(1-Cs)
            MixBlendMode::Screen => BlendMode::Screen,
            // Exclusion can be implemented as Cs + Cd - 2*Cs*Cd => Cs*(1-Cd) + Cd*(1-Cs)
            MixBlendMode::Exclusion => BlendMode::Exclusion,
            // PlusLighter is basically a clamped add.
            MixBlendMode::PlusLighter => BlendMode::PlusLighter,
            // Multiply can be implemented as Cs*Cd + Cs*(1-Ad) + Cd*(1-As) => Cs*(1-Ad) + Cd*(1 - SRC1=(As-Cs))
            MixBlendMode::Multiply if dual_source => BlendMode::MultiplyDualSource,
            // Otherwise, use advanced blend without coherency if available.
            _ if advanced_blend => BlendMode::Advanced(mode),
            // If advanced blend is not available, then we have to use brush_mix_blend.
            _ => return None,
        })
    }
}

/// Information about the state of the debugging / profiler overlay in native compositing mode.
struct DebugOverlayState {
    /// True if any of the current debug flags will result in drawing a debug overlay.
    is_enabled: bool,

    /// The current size of the debug overlay surface. None implies that the
    /// debug surface isn't currently allocated.
    current_size: Option<DeviceIntSize>,

    layer_index: usize,
}

impl DebugOverlayState {
    fn new() -> Self {
        DebugOverlayState {
            is_enabled: false,
            current_size: None,
            layer_index: 0,
        }
    }
}

#[cfg(feature = "gl_backend")]
struct GlRendererAuxTextures {
    dither_matrix_texture: Option<Texture>,
    zoom_debug_texture: Option<Texture>,
}

#[cfg(feature = "wgpu_backend")]
#[allow(dead_code)]
struct WgpuRendererAuxTextures;

/// Persistent GPU cache state for the wgpu render path.
///
/// In the GL path, the GPU cache is a persistent GL texture updated
/// incrementally each frame.  For the wgpu path, we maintain a CPU-side
/// mirror of the entire cache and upload it as a wgpu texture.
#[cfg(feature = "wgpu_backend")]
use crate::device::WgpuTexture;

#[cfg(feature = "wgpu_backend")]
pub(super) struct WgpuGpuCacheState {
    /// CPU mirror of GPU cache contents.  Row-major, `width` texels per row.
    /// Each texel is `[f32; 4]` (RGBA32F).
    data: Vec<[f32; 4]>,
    /// Width of the cache in texels (= MAX_VERTEX_TEXTURE_WIDTH = 1024).
    width: u32,
    /// Current height of the cache in rows.
    height: u32,
    /// The uploaded wgpu texture, if any.
    texture: Option<WgpuTexture>,
}

#[cfg(feature = "wgpu_backend")]
impl WgpuGpuCacheState {
    fn new() -> Self {
        let width = MAX_VERTEX_TEXTURE_WIDTH as u32;
        let initial_height = crate::gpu_cache::GPU_CACHE_INITIAL_HEIGHT as u32;
        WgpuGpuCacheState {
            data: vec![[0.0; 4]; (width * initial_height) as usize],
            width,
            height: initial_height,
            texture: None,
        }
    }

    /// Apply pending GPU cache update lists (sparse writes into the mirror).
    fn apply_updates(&mut self, updates: &[crate::gpu_cache::GpuCacheUpdateList]) {
        for list in updates {
            // Resize if needed.
            let new_h = list.height as u32;
            if new_h > self.height {
                self.data.resize((self.width * new_h) as usize, [0.0; 4]);
                self.height = new_h;
            }
            if list.clear {
                for v in self.data.iter_mut() {
                    *v = [0.0; 4];
                }
            }
            gpu_cache_utils::for_each_gpu_cache_copy(list, |row, col, blocks| {
                let dst_start = row * self.width as usize + col;
                // GpuBlockData wraps [f32; 4] — safe to reinterpret.
                let block_data: &[[f32; 4]] = unsafe {
                    std::slice::from_raw_parts(
                        blocks.as_ptr() as *const [f32; 4],
                        blocks.len(),
                    )
                };
                for (i, &texel) in block_data.iter().enumerate() {
                    let dst = dst_start + i;
                    if dst < self.data.len() {
                        self.data[dst] = texel;
                    }
                }
            });
        }
    }

    /// Upload the CPU mirror to a wgpu texture, creating or resizing as needed.
    fn upload(&mut self, device: &WgpuDevice) {
        let bytes = crate::device::as_byte_slice(&self.data);
        match self.texture {
            Some(ref mut tex) => {
                device.update_data_texture(tex, self.width, self.height, bytes);
            }
            None => {
                self.texture = Some(device.create_data_texture(
                    "wgpu GPU cache",
                    self.width,
                    self.height,
                    wgpu::TextureFormat::Rgba32Float,
                    bytes,
                ));
            }
        }
    }

    fn texture_view(&self) -> Option<wgpu::TextureView> {
        self.texture.as_ref().map(|t| t.create_view())
    }
}

/// Per-frame data textures uploaded to the wgpu device for alpha batch rendering.
#[cfg(feature = "wgpu_backend")]
struct WgpuFrameDataTextures {
    prim_headers_f: WgpuTexture,
    prim_headers_i: WgpuTexture,
    transform_palette: WgpuTexture,
    render_tasks: WgpuTexture,
    gpu_buffer_f: Option<WgpuTexture>,
    gpu_buffer_i: Option<WgpuTexture>,
}

/// Frame-level state shared by all draw calls in a render frame.
///
/// Bundles the texture cache reference and all per-frame texture views that
/// previously threaded through every draw function as individual parameters.
/// The wgpu device is passed separately to avoid borrow conflicts with
/// `&mut self` on the renderer.
#[cfg(feature = "wgpu_backend")]
pub(crate) struct WgpuDrawContext<'a> {
    pub texture_cache: &'a FastHashMap<CacheTextureId, WgpuTexture>,

    // Frame data texture views (from persistent WgpuFrameDataTextures)
    pub transform_palette: &'a wgpu::TextureView,
    pub render_tasks: &'a wgpu::TextureView,
    pub prim_headers_f: &'a wgpu::TextureView,
    pub prim_headers_i: &'a wgpu::TextureView,
    pub gpu_cache: Option<&'a wgpu::TextureView>,
    pub gpu_buffer_f: Option<&'a wgpu::TextureView>,
    pub gpu_buffer_i: Option<&'a wgpu::TextureView>,
    pub dither: Option<&'a wgpu::TextureView>,
}

#[cfg_attr(feature = "wgpu_backend", allow(dead_code))]
enum RendererAuxTextures {
    #[cfg(feature = "gl_backend")]
    Gl(GlRendererAuxTextures),
    #[cfg(feature = "wgpu_backend")]
    Wgpu(WgpuRendererAuxTextures),
}

#[cfg(feature = "gl_backend")]
impl RendererAuxTextures {
    fn new_gl(dither_matrix_texture: Option<Texture>) -> Self {
        Self::Gl(GlRendererAuxTextures {
            dither_matrix_texture,
            zoom_debug_texture: None,
        })
    }

    fn gl(&self) -> &GlRendererAuxTextures {
        match self {
            Self::Gl(state) => state,
            #[cfg(feature = "wgpu_backend")]
            Self::Wgpu(..) => unreachable!("wgpu aux textures are not wired yet"),
        }
    }

    fn gl_mut(&mut self) -> &mut GlRendererAuxTextures {
        match self {
            Self::Gl(state) => state,
            #[cfg(feature = "wgpu_backend")]
            Self::Wgpu(..) => unreachable!("wgpu aux textures are not wired yet"),
        }
    }

    fn dither_texture(&self) -> Option<&Texture> {
        self.gl().dither_matrix_texture.as_ref()
    }

    fn ensure_zoom_texture(
        &mut self,
        device: &mut Device,
        source_rect: DeviceIntRect,
    ) -> &Texture {
        let state = self.gl_mut();
        if state.zoom_debug_texture.is_none() {
            let texture = device.create_texture(
                ImageBufferKind::Texture2D,
                ImageFormat::BGRA8,
                source_rect.width(),
                source_rect.height(),
                TextureFilter::Nearest,
                Some(RenderTargetInfo { has_depth: false }),
            );
            state.zoom_debug_texture = Some(texture);
        }
        state.zoom_debug_texture.as_ref().unwrap()
    }

    fn zoom_texture(&self) -> &Texture {
        self.gl().zoom_debug_texture.as_ref().unwrap()
    }

    fn deinit(self, device: &mut Device) {
        match self {
            Self::Gl(state) => {
                if let Some(texture) = state.dither_matrix_texture {
                    device.delete_texture(texture);
                }
                if let Some(texture) = state.zoom_debug_texture {
                    device.delete_texture(texture);
                }
            }
            #[cfg(feature = "wgpu_backend")]
            Self::Wgpu(..) => {}
        }
    }
}

/// Tracks buffer damage rects over a series of frames.
#[derive(Debug, Default)]
pub(crate) struct BufferDamageTracker {
    damage_rects: [DeviceRect; 4],
    current_offset: usize,
}

impl BufferDamageTracker {
    /// Sets the damage rect for the current frame. Should only be called *after*
    /// get_damage_rect() has been called to get the current backbuffer's damage rect.
    fn push_dirty_rect(&mut self, rect: &DeviceRect) {
        self.damage_rects[self.current_offset] = rect.clone();
        self.current_offset = match self.current_offset {
            0 => self.damage_rects.len() - 1,
            n => n - 1,
        }
    }

    /// Gets the damage rect for the current backbuffer, given the backbuffer's age.
    /// (The number of frames since it was previously the backbuffer.)
    /// Returns an empty rect if the buffer is valid, and None if the entire buffer is invalid.
    fn get_damage_rect(&self, buffer_age: usize) -> Option<DeviceRect> {
        match buffer_age {
            // 0 means this is a new buffer, so is completely invalid.
            0 => None,
            // 1 means this backbuffer was also the previous frame's backbuffer
            // (so must have been copied to the frontbuffer). It is therefore entirely valid.
            1 => Some(DeviceRect::zero()),
            // We must calculate the union of the damage rects since this buffer was previously
            // the backbuffer.
            n if n <= self.damage_rects.len() + 1 => {
                Some(
                    self.damage_rects.iter()
                        .cycle()
                        .skip(self.current_offset + 1)
                        .take(n - 1)
                        .fold(DeviceRect::zero(), |acc, r| acc.union(r))
                )
            }
            // The backbuffer is older than the number of frames for which we track,
            // so we treat it as entirely invalid.
            _ => None,
        }
    }
}

/// The renderer is responsible for submitting to the GPU the work prepared by the
/// RenderBackend.
///
/// We have a separate `Renderer` instance for each instance of WebRender (generally
/// one per OS window), and all instances share the same thread.
pub struct Renderer {
    result_rx: Receiver<ResultMsg>,
    api_tx: Sender<ApiMsg>,
    #[cfg(feature = "gl_backend")]
    pub device: Option<Device>,
    pending_texture_updates: Vec<TextureUpdateList>,
    /// True if there are any TextureCacheUpdate pending.
    pending_texture_cache_updates: bool,
    pending_native_surface_updates: Vec<NativeSurfaceOperation>,
    pending_gpu_cache_updates: Vec<GpuCacheUpdateList>,
    pending_gpu_cache_clear: bool,
    pending_shader_updates: Vec<PathBuf>,
    active_documents: FastHashMap<DocumentId, RenderedDocument>,

    #[cfg(feature = "gl_backend")]
    shaders: Option<Rc<RefCell<Shaders>>>,

    max_recorded_profiles: usize,

    clear_color: ColorF,
    enable_clear_scissor: bool,
    enable_advanced_blend_barriers: bool,
    clear_caches_with_quads: bool,
    clear_alpha_targets_with_quads: bool,

    #[cfg(feature = "gl_backend")]
    debug: debug::LazyInitializedDebugRenderer,
    debug_flags: DebugFlags,
    profile: TransactionProfile,
    frame_counter: u64,
    resource_upload_time: f64,
    gpu_cache_upload_time: f64,
    profiler: Profiler,
    #[cfg(feature = "debugger")]
    debugger: Debugger,

    last_time: u64,

    pub gpu_profiler: GpuProfiler,
    #[cfg(feature = "gl_backend")]
    vaos: vertex::RendererVaoState,

    #[cfg(feature = "gl_backend")]
    gpu_cache_texture: gpu_cache::RendererGpuCache,
    #[cfg(feature = "gl_backend")]
    vertex_data_textures: vertex::RendererVertexData,

    /// When the GPU cache debugger is enabled, we keep track of the live blocks
    /// in the GPU cache so that we can use them for the debug display. This
    /// member stores those live blocks, indexed by row.
    gpu_cache_debug_chunks: Vec<Vec<GpuCacheDebugChunk>>,

    gpu_cache_frame_id: FrameId,
    gpu_cache_overflow: bool,

    pipeline_info: PipelineInfo,

    // Manages and resolves source textures IDs to real texture IDs.
    #[cfg(feature = "gl_backend")]
    texture_resolver: TextureResolver,

    #[cfg(feature = "gl_backend")]
    upload_state: RendererUploadState,
    #[cfg(feature = "gl_backend")]
    aux_textures: RendererAuxTextures,

    /// Optional trait object that allows the client
    /// application to provide external buffers for image data.
    external_image_handler: Option<Box<dyn ExternalImageHandler>>,

    /// Optional function pointers for measuring memory used by a given
    /// heap-allocated pointer.
    size_of_ops: Option<MallocSizeOfOps>,

    pub renderer_errors: Vec<RendererError>,

    #[cfg(feature = "gl_backend")]
    pub(in crate) async_frame_recorder: Option<AsyncScreenshotGrabber>,
    #[cfg(feature = "gl_backend")]
    pub(in crate) async_screenshots: Option<AsyncScreenshotGrabber>,

    /// List of profile results from previous frames. Can be retrieved
    /// via get_frame_profiles().
    cpu_profiles: VecDeque<CpuProfile>,
    gpu_profiles: VecDeque<GpuProfile>,

    /// Notification requests to be fulfilled after rendering.
    notifications: Vec<NotificationRequest>,

    device_size: Option<DeviceIntSize>,

    /// The current mouse position. This is used for debugging
    /// functionality only, such as the debug zoom widget.
    cursor_position: DeviceIntPoint,

    /// Guards to check if we might be rendering a frame with expired texture
    /// cache entries.
    shared_texture_cache_cleared: bool,

    /// The set of documents which we've seen a publish for since last render.
    documents_seen: FastHashSet<DocumentId>,

    #[cfg(all(feature = "capture", feature = "gl_backend"))]
    read_fbo: FBOId,
    #[cfg(all(feature = "replay", feature = "gl_backend"))]
    owned_external_images: FastHashMap<(ExternalImageId, u8), ExternalTexture>,

    /// The compositing config, affecting how WR composites into the final scene.
    compositor_config: CompositorConfig,
    current_compositor_kind: CompositorKind,

    /// Maintains a set of allocated native composite surfaces. This allows any
    /// currently allocated surfaces to be cleaned up as soon as deinit() is
    /// called (the normal bookkeeping for native surfaces exists in the
    /// render backend thread).
    allocated_native_surfaces: FastHashSet<NativeSurfaceId>,

    /// If true, partial present state has been reset and everything needs to
    /// be drawn on the next render.
    force_redraw: bool,

    /// State related to the debug / profiling overlays
    debug_overlay_state: DebugOverlayState,

    /// Tracks the dirty rectangles from previous frames. Used on platforms
    /// that require keeping the front buffer fully correct when doing
    /// partial present (e.g. unix desktop with EGL_EXT_buffer_age).
    buffer_damage_tracker: BufferDamageTracker,

    max_primitive_instance_count: usize,
    enable_instancing: bool,

    /// Count consecutive oom frames to detectif we are stuck unable to render
    /// in a loop.
    consecutive_oom_frames: u32,

    /// update() defers processing of ResultMsg, if frame_publish_id of
    /// ResultMsg::PublishDocument exceeds target_frame_publish_id.
    target_frame_publish_id: Option<FramePublishId>,

    /// Hold a next ResultMsg that will be handled by update().
    pending_result_msg: Option<ResultMsg>,

    /// Hold previous frame compositing state with layer compositor.
    layer_compositor_frame_state_in_prev_frame: Option<LayerCompositorFrameState>,

    /// Optional wgpu backend device. When present, eligible draw paths
    /// (currently composite tiles) can be routed through wgpu instead of GL.
    /// Created alongside the GL device when the `wgpu_backend` feature is
    /// enabled; will eventually be the sole device for `RendererBackend::Wgpu`.
    #[cfg(feature = "wgpu_backend")]
    pub wgpu_device: Option<WgpuDevice>,

    /// Texture cache map for wgpu-only mode. Maps CacheTextureId to wgpu textures.
    /// Only populated when device is None (wgpu-only Renderer).
    #[cfg(feature = "wgpu_backend")]
    wgpu_texture_cache: FastHashMap<CacheTextureId, crate::device::WgpuTexture>,

    /// Persistent GPU cache state for the wgpu render path.
    #[cfg(feature = "wgpu_backend")]
    wgpu_gpu_cache: WgpuGpuCacheState,

    /// Pre-created dither texture for wgpu gradient rendering.
    #[cfg(feature = "wgpu_backend")]
    wgpu_dither_texture: Option<crate::device::WgpuTexture>,

    /// Persistent per-frame data textures reused across frames.
    #[cfg(feature = "wgpu_backend")]
    wgpu_frame_data: Option<WgpuFrameDataTextures>,

    /// Offscreen render target holding the last composited frame.
    /// Used by `read_pixels_rgba8()` for wgpu readback (e.g. wrench reftests).
    #[cfg(feature = "wgpu_backend")]
    wgpu_readback_texture: Option<crate::device::WgpuTexture>,
}

#[derive(Debug)]
pub enum RendererError {
    #[cfg(feature = "gl_backend")]
    Shader(ShaderError),
    Thread(std::io::Error),
    MaxTextureSize,
    SoftwareRasterizer,
    OutOfMemory,
    UnsupportedBackend(&'static str),
}

#[cfg(feature = "gl_backend")]
impl From<ShaderError> for RendererError {
    fn from(err: ShaderError) -> Self {
        RendererError::Shader(err)
    }
}

impl From<std::io::Error> for RendererError {
    fn from(err: std::io::Error) -> Self {
        RendererError::Thread(err)
    }
}

impl Renderer {
    #[cfg(feature = "gl_backend")]
    /// Returns a reference to the GL device. Panics in wgpu-only mode.
    pub fn gl_device(&self) -> &Device {
        self.device.as_ref().expect("GL device not available in wgpu mode")
    }

    #[cfg(feature = "gl_backend")]
    /// Returns a mutable reference to the GL device. Panics in wgpu-only mode.
    pub fn gl_device_mut(&mut self) -> &mut Device {
        self.device.as_mut().expect("GL device not available in wgpu mode")
    }

    pub fn device_size(&self) -> Option<DeviceIntSize> {
        self.device_size
    }

    /// Returns true if this Renderer is in wgpu-only mode (no GL device).
    pub fn is_wgpu_only(&self) -> bool {
        #[cfg(feature = "gl_backend")]
        { self.device.is_none() }
        #[cfg(not(feature = "gl_backend"))]
        { true }
    }

    /// wgpu render path.  Processes pending texture cache and GPU cache
    /// updates, uploads per-frame data textures, processes render passes
    /// (drawing alpha batches into picture cache tiles), then composites
    /// the final frame to the surface.
    #[cfg(feature = "wgpu_backend")]
    fn render_wgpu(
        &mut self,
        device_size: DeviceIntSize,
    ) -> Result<RenderResults, Vec<RendererError>> {
        use crate::composite::CompositeTileSurface;

        let results = RenderResults::default();

        // Process any pending texture cache updates before rendering.
        self.update_texture_cache_wgpu();

        // ── GPU cache: apply pending updates and upload ─────────────
        {
            let pending = std::mem::take(&mut self.pending_gpu_cache_updates);
            if !pending.is_empty() {
                self.wgpu_gpu_cache.apply_updates(&pending);
                if let Some(ref dev) = self.wgpu_device {
                    self.wgpu_gpu_cache.upload(dev);
                }
            }
        }

        // ── Frame data textures ─────────────────────────────────────
        // Upload the per-frame data arrays as wgpu textures so they can
        // be bound when drawing alpha batches.  Textures are reused across
        // frames; only the data is re-uploaded (or reallocated if size changed).
        let gpu_cache_view = self.wgpu_gpu_cache.texture_view();

        // Upload per-frame data textures, then draw passes.
        // Scoped carefully to avoid holding borrows across mutable access.
        let doc_id = self.active_documents.keys().last().cloned();
        if let Some(ref doc_id) = doc_id {
            if let Some(dev) = self.wgpu_device.as_ref() {
                if let Some(doc) = self.active_documents.get(doc_id) {
                    Self::upload_frame_data_textures(dev, &mut self.wgpu_frame_data, &doc.frame);
                }
            }
        }

        // ── Process render passes ───────────────────────────────────
        // Each pass renders into picture cache tiles / texture cache
        // targets.  We iterate the alpha batch containers and draw them
        // through wgpu pipelines.
        if let Some(ref doc_id) = doc_id {
            let frame_rendered = self.active_documents.get(doc_id)
                .map(|d| d.frame.has_been_rendered)
                .unwrap_or(true);

            if !frame_rendered {
                // Take frame data out to avoid borrow conflict with &mut self.
                if let Some(ft) = self.wgpu_frame_data.take() {
                    self.draw_passes_wgpu(
                        doc_id,
                        &ft,
                        gpu_cache_view.as_ref(),
                    );
                    self.wgpu_frame_data = Some(ft);
                }

            }
        }

        let doc_id = self.active_documents.keys().last().cloned();
        let Some(doc_id) = doc_id else {
            return Ok(results);
        };

        let doc = match self.active_documents.get(&doc_id) {
            Some(doc) => doc,
            None => return Ok(results),
        };

        let wgpu_dev = match self.wgpu_device {
            Some(ref mut dev) => dev,
            None => return Ok(results),
        };

        // Always render to an offscreen RT so pixels survive present() for readback.
        let w = device_size.width as u32;
        let h = device_size.height as u32;
        let surface_texture = wgpu_dev.acquire_surface_texture();

        // Reuse the readback texture if dimensions match, else (re)create.
        let reuse = self.wgpu_readback_texture.as_ref()
            .map(|t| t.width == w && t.height == h)
            .unwrap_or(false);
        if !reuse {
            self.wgpu_readback_texture = Some(wgpu_dev.create_render_target(w, h));
        }
        let target_view = self.wgpu_readback_texture.as_ref().unwrap().create_view();

        // Collect composite tiles into batches by type.
        let composite_state = &doc.frame.composite_state;
        let mut color_instances: Vec<CompositeInstance> = Vec::new();
        // Group texture-backed tiles by their CacheTextureId.
        let mut textured_batches: FastHashMap<CacheTextureId, Vec<CompositeInstance>> =
            FastHashMap::default();
        let mut skipped_tiles = 0u32;

        for (tile_idx, tile) in composite_state.tiles.iter().enumerate() {
            let tile_rect = composite_state.get_device_rect(
                &tile.local_rect,
                tile.transform_index,
            );
            let transform = composite_state.get_device_transform(tile.transform_index);
            let flip = (transform.scale.x < 0.0, transform.scale.y < 0.0);
            let clip_rect = tile.device_clip_rect;

            match tile.surface {
                CompositeTileSurface::Color { color } => {
                    let instance = CompositeInstance::new(
                        tile_rect,
                        clip_rect,
                        color.premultiplied(),
                        flip,
                        None,
                    );
                    color_instances.push(instance);
                }
                CompositeTileSurface::Texture {
                    surface: ResolvedSurfaceTexture::TextureCache { texture }
                } => {
                    if let TextureSource::TextureCache(cache_id, _swizzle) = texture {
                        if self.wgpu_texture_cache.contains_key(&cache_id) {
                            let instance = CompositeInstance::new(
                                tile_rect,
                                clip_rect,
                                PremultipliedColorF::WHITE,
                                flip,
                                None,
                            );
                            textured_batches
                                .entry(cache_id)
                                .or_insert_with(Vec::new)
                                .push(instance);
                        } else {
                            skipped_tiles += 1;
                        }
                    } else {
                        skipped_tiles += 1;
                    }
                }
                _ => {
                    // Clear, ExternalSurface, Native — skip for now.
                    skipped_tiles += 1;
                }
            }
        }

        // Use the renderer's configured clear color for the composite surface.
        let surface_clear = wgpu::Color {
            r: self.clear_color.r as f64,
            g: self.clear_color.g as f64,
            b: self.clear_color.b as f64,
            a: self.clear_color.a as f64,
        };
        // Diagnostic: log tile breakdown on first few frames.
        if !composite_state.tiles.is_empty() || skipped_tiles > 0 {
            info!(
                "wgpu composite: {} total tiles, {} color, {} textured batches, {} skipped, surface={}x{}",
                composite_state.tiles.len(),
                color_instances.len(),
                textured_batches.len(),
                skipped_tiles,
                w, h,
            );
            // Log the surface types of skipped tiles (first frame only via has_been_rendered).
            if !doc.frame.has_been_rendered {
                for tile in &composite_state.tiles {
                    match tile.surface {
                        CompositeTileSurface::Color { .. } => {},
                        CompositeTileSurface::Texture {
                            surface: ResolvedSurfaceTexture::TextureCache { ref texture }
                        } => {
                            if let TextureSource::TextureCache(id, _) = texture {
                                if !self.wgpu_texture_cache.contains_key(&id) {
                                    info!("  missing texture {:?} in wgpu cache", id);
                                }
                            }
                        }
                        ref other => {
                            info!("  skipped tile surface: {:?}", std::mem::discriminant(other));
                        }
                    }
                }
            }
        }

        // All composite draws to the surface share a single render pass.
        // Always clear the render target to the configured clear color,
        // even when there are no tiles to draw (e.g. blank scenes).
        let has_composite_work = !color_instances.is_empty() || !textured_batches.is_empty();
        {
            // Use the readback texture format, not the surface format,
            // since the composite pass renders into the offscreen RT.
            let surface_fmt = self.wgpu_readback_texture.as_ref()
                .map(|t| t.format())
                .unwrap_or(wgpu::TextureFormat::Bgra8Unorm);
            let (transform_buf, tex_size_buf) = wgpu_dev.create_target_uniforms(w, h);
            let mut encoder = wgpu_dev.take_encoder();
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("composite pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(surface_clear),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });

                // Color tiles: use the Composite shader with no source texture.
                if has_composite_work && !color_instances.is_empty() {
                    let instance_bytes = crate::device::as_byte_slice(&color_instances);
                    let textures = crate::device::TextureBindings::default();
                    wgpu_dev.record_draw(
                        &mut pass,
                        crate::device::WgpuShaderVariant::Composite,
                        crate::device::WgpuBlendMode::PremultipliedAlpha,
                        crate::device::WgpuDepthState::None,
                        surface_fmt,
                        w,
                        h,
                        &textures,
                        &transform_buf,
                        &tex_size_buf,
                        instance_bytes,
                        color_instances.len() as u32,
                        None,
                    );
                }

                // Texture-backed tile batches: use CompositeFastPath with source texture.
                for (cache_id, instances) in &textured_batches {
                    let source_view = match self.wgpu_texture_cache.get(cache_id) {
                        Some(t) => t.create_view(),
                        None => continue,
                    };
                    let instance_bytes = crate::device::as_byte_slice(instances.as_slice());
                    let textures = crate::device::TextureBindings {
                        color0: Some(&source_view),
                        ..Default::default()
                    };
                    wgpu_dev.record_draw(
                        &mut pass,
                        crate::device::WgpuShaderVariant::CompositeFastPath,
                        crate::device::WgpuBlendMode::PremultipliedAlpha,
                        crate::device::WgpuDepthState::None,
                        surface_fmt,
                        w,
                        h,
                        &textures,
                        &transform_buf,
                        &tex_size_buf,
                        instance_bytes,
                        instances.len() as u32,
                        None,
                    );
                }
            } // render pass dropped
            wgpu_dev.return_encoder(encoder);
        }

        let total_textured: usize = textured_batches.values().map(|v| v.len()).sum();
        if !color_instances.is_empty() || total_textured > 0 {
            info!(
                "wgpu: composited {} color + {} textured tiles ({} skipped, {}x{})",
                color_instances.len(),
                total_textured,
                skipped_tiles,
                device_size.width,
                device_size.height,
            );
        }

        // Flush any pending GPU commands before copying/presenting.
        wgpu_dev.flush_encoder();

        // Copy offscreen RT to surface texture for display, then present.
        if let Some(st) = surface_texture {
            if let Some(ref rt) = self.wgpu_readback_texture {
                let mut encoder = wgpu_dev.take_encoder();
                encoder.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &rt.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &st.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                );
                wgpu_dev.return_encoder(encoder);
                wgpu_dev.flush_encoder();
            }
            st.present();
        }

        // Drain notifications that expect FrameRendered
        drain_filter(
            &mut self.notifications,
            |n| n.when() == Checkpoint::FrameRendered,
            |n| n.notify(),
        );
        self.notifications.clear();

        Ok(results)
    }

    /// Upload per-frame data textures to the wgpu device, reusing existing
    /// allocations when dimensions haven't changed.
    ///
    /// These correspond to the `sTransformPalette`, `sRenderTasks`,
    /// `sPrimitiveHeadersF`, `sPrimitiveHeadersI`, `sGpuBufferF`, and
    /// `sGpuBufferI` shader bindings.
    #[cfg(feature = "wgpu_backend")]
    fn upload_frame_data_textures(
        dev: &WgpuDevice,
        existing: &mut Option<WgpuFrameDataTextures>,
        frame: &Frame,
    ) {
        use data_texture_layout as layout;
        use crate::device::as_byte_slice;

        let w = MAX_VERTEX_TEXTURE_WIDTH as u32;

        // Helper: compute required texture height from data and layout.
        let tex_height = |data_bytes: usize, texels_per_item: usize| -> u32 {
            let item_count = data_bytes / (texels_per_item * 16);
            let ipr = layout::items_per_row(texels_per_item);
            layout::required_height(item_count, ipr) as u32
        };

        let prim_f_bytes = as_byte_slice(&frame.prim_headers.headers_float);
        let prim_i_bytes = as_byte_slice(&frame.prim_headers.headers_int);
        let transforms_bytes = as_byte_slice(&frame.transform_palette);
        let tasks_bytes = as_byte_slice(&frame.render_tasks.task_data);

        match existing {
            Some(ref mut ft) => {
                // Reuse existing textures — update_data_texture handles
                // both same-size (write) and different-size (recreate).
                let h = tex_height(prim_f_bytes.len(), 2);
                dev.update_data_texture(&mut ft.prim_headers_f, w, h, prim_f_bytes);

                let h = tex_height(prim_i_bytes.len(), 2);
                dev.update_data_texture(&mut ft.prim_headers_i, w, h, prim_i_bytes);

                let h = tex_height(transforms_bytes.len(), 8);
                dev.update_data_texture(&mut ft.transform_palette, w, h, transforms_bytes);

                let h = tex_height(tasks_bytes.len(), 2);
                dev.update_data_texture(&mut ft.render_tasks, w, h, tasks_bytes);

                // GPU buffers: update if present, drop if frame has none.
                if !frame.gpu_buffer_f.is_empty() {
                    let bytes = as_byte_slice(&frame.gpu_buffer_f.data);
                    let sz = frame.gpu_buffer_f.size;
                    let bw = sz.width as u32;
                    let bh = (sz.height as u32).max(1);
                    match ft.gpu_buffer_f {
                        Some(ref mut tex) => dev.update_data_texture(tex, bw, bh, bytes),
                        None => {
                            ft.gpu_buffer_f = Some(dev.create_data_texture(
                                "gpu_buffer_f", bw, bh,
                                wgpu::TextureFormat::Rgba32Float, bytes,
                            ));
                        }
                    }
                } else {
                    ft.gpu_buffer_f = None;
                }

                if !frame.gpu_buffer_i.is_empty() {
                    let bytes = as_byte_slice(&frame.gpu_buffer_i.data);
                    let sz = frame.gpu_buffer_i.size;
                    let bw = sz.width as u32;
                    let bh = (sz.height as u32).max(1);
                    match ft.gpu_buffer_i {
                        Some(ref mut tex) => dev.update_data_texture(tex, bw, bh, bytes),
                        None => {
                            ft.gpu_buffer_i = Some(dev.create_data_texture(
                                "gpu_buffer_i", bw, bh,
                                wgpu::TextureFormat::Rgba32Sint, bytes,
                            ));
                        }
                    }
                } else {
                    ft.gpu_buffer_i = None;
                }
            }
            None => {
                // First frame: allocate all textures.
                let make_tex = |label: &str, data: &[u8], texels_per_item: usize, format: wgpu::TextureFormat| -> WgpuTexture {
                    let h = tex_height(data.len(), texels_per_item);
                    dev.create_data_texture(label, w, h, format, data)
                };

                let prim_headers_f = make_tex("prim_headers_f", prim_f_bytes, 2, wgpu::TextureFormat::Rgba32Float);
                let prim_headers_i = make_tex("prim_headers_i", prim_i_bytes, 2, wgpu::TextureFormat::Rgba32Sint);
                let transform_palette = make_tex("transform_palette", transforms_bytes, 8, wgpu::TextureFormat::Rgba32Float);
                let render_tasks = make_tex("render_tasks", tasks_bytes, 2, wgpu::TextureFormat::Rgba32Float);

                let gpu_buf_f = if !frame.gpu_buffer_f.is_empty() {
                    let bytes = as_byte_slice(&frame.gpu_buffer_f.data);
                    let sz = frame.gpu_buffer_f.size;
                    Some(dev.create_data_texture(
                        "gpu_buffer_f", sz.width as u32, (sz.height as u32).max(1),
                        wgpu::TextureFormat::Rgba32Float, bytes,
                    ))
                } else { None };

                let gpu_buf_i = if !frame.gpu_buffer_i.is_empty() {
                    let bytes = as_byte_slice(&frame.gpu_buffer_i.data);
                    let sz = frame.gpu_buffer_i.size;
                    Some(dev.create_data_texture(
                        "gpu_buffer_i", sz.width as u32, (sz.height as u32).max(1),
                        wgpu::TextureFormat::Rgba32Sint, bytes,
                    ))
                } else { None };

                *existing = Some(WgpuFrameDataTextures {
                    prim_headers_f,
                    prim_headers_i,
                    transform_palette,
                    render_tasks,
                    gpu_buffer_f: gpu_buf_f,
                    gpu_buffer_i: gpu_buf_i,
                });
            }
        }
    }

    /// Process render passes through wgpu, drawing alpha batch containers
    /// into picture cache / texture cache render targets.
    #[cfg(feature = "wgpu_backend")]
    fn draw_passes_wgpu(
        &mut self,
        doc_id: &DocumentId,
        frame_textures: &WgpuFrameDataTextures,
        gpu_cache_view: Option<&wgpu::TextureView>,
    ) {
        use crate::device::{TextureBindings, WgpuBlendMode, WgpuDepthState, WgpuShaderVariant};

        if self.wgpu_device.is_none() {
            return;
        }

        // Use an immutable borrow for the render loop so the closure can also
        // access self.wgpu_texture_cache.  We set has_been_rendered after.
        let frame = match self.active_documents.get(doc_id) {
            Some(doc) => &doc.frame,
            None => return,
        };
        if frame.has_been_rendered || frame.passes.is_empty() {
            return;
        }

        // Build texture views for the frame data textures.
        let ft_prim_f_view = frame_textures.prim_headers_f.create_view();
        let ft_prim_i_view = frame_textures.prim_headers_i.create_view();
        let ft_transforms_view = frame_textures.transform_palette.create_view();
        let ft_tasks_view = frame_textures.render_tasks.create_view();
        let ft_gpu_buf_f_view = frame_textures.gpu_buffer_f.as_ref().map(|t| t.create_view());
        let ft_gpu_buf_i_view = frame_textures.gpu_buffer_i.as_ref().map(|t| t.create_view());
        let dither_view = self.wgpu_dither_texture.as_ref().map(|t| t.create_view());

        let draw_ctx = WgpuDrawContext {
            texture_cache: &self.wgpu_texture_cache,
            transform_palette: &ft_transforms_view,
            render_tasks: &ft_tasks_view,
            prim_headers_f: &ft_prim_f_view,
            prim_headers_i: &ft_prim_i_view,
            gpu_cache: gpu_cache_view,
            gpu_buffer_f: ft_gpu_buf_f_view.as_ref(),
            gpu_buffer_i: ft_gpu_buf_i_view.as_ref(),
            dither: dither_view.as_ref(),
        };

        let mut batches_drawn = 0u32;
        let mut batches_skipped = 0u32;

        for pass in frame.passes.iter() {
            for picture_target in &pass.picture_cache {
                let cache_tex_id = match picture_target.surface {
                    ResolvedSurfaceTexture::TextureCache { ref texture } => {
                        match *texture {
                            TextureSource::TextureCache(id, _) => id,
                            _ => {
                                info!("wgpu: pic target skipped: non-TextureCache texture source");
                                continue;
                            }
                        }
                    }
                    _ => {
                        info!("wgpu: pic target skipped: native surface");
                        continue;
                    }
                };

                let target_wgpu = match self.wgpu_texture_cache.get(&cache_tex_id) {
                    Some(t) => t,
                    None => {
                        info!("wgpu: pic target {:?} skipped: not in wgpu_texture_cache", cache_tex_id);
                        batches_skipped += 1;
                        continue;
                    }
                };

                let target_view = target_wgpu.create_view();
                let target_fmt = target_wgpu.format();
                // Use the full tile texture dimensions for the ortho projection,
                // NOT the dirty rect.  The dirty rect controls the scissor (which
                // limits what we actually draw) but the projection must map the
                // full tile coordinate space so that vertex positions produced by
                // the shader land in the right place.  The GL path does the same:
                // it projects over draw_target.dimensions() (full texture size).
                let target_w = target_wgpu.width;
                let target_h = target_wgpu.height;
                if target_w == 0 || target_h == 0 {
                    info!("wgpu: pic target {:?} skipped: dirty rect {}x{}", cache_tex_id, target_w, target_h);
                    continue;
                }

                // Draw the alpha batch container in this picture target.
                let alpha_batch_container = match picture_target.kind {
                    PictureCacheTargetKind::Draw { ref alpha_batch_container } => {
                        alpha_batch_container
                    }
                    PictureCacheTargetKind::Blit { .. } => continue, // Blits handled separately.
                };

                let scissor = Self::device_rect_to_scissor(
                    alpha_batch_container.task_scissor_rect.as_ref(),
                );

                let has_opaque = !alpha_batch_container.opaque_batches.is_empty();

                // Acquire a depth texture when there are opaque batches.
                // Depth testing prevents overdraw: opaque draws front-to-back
                // with depth write, alpha draws test against the depth buffer.
                let depth_view = if has_opaque {
                    let wgpu_dev = self.wgpu_device.as_mut().unwrap();
                    Some(wgpu_dev.acquire_depth_view(target_wgpu.width, target_wgpu.height))
                } else {
                    None
                };
                let depth_ref = depth_view.as_ref();

                // Use the target's clear_color if available, otherwise
                // transparent black (matching the GL backend's default).
                let tile_clear_color: wgpu::Color = match picture_target.clear_color {
                    Some(c) => wgpu::Color {
                        r: c.r as f64,
                        g: c.g as f64,
                        b: c.b as f64,
                        a: c.a as f64,
                    },
                    None => wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 },
                };
                // Create per-target uniform buffers (shared by all draws to this target).
                let wgpu_dev = self.wgpu_device.as_mut().unwrap();
                let (transform_buf, tex_size_buf) = wgpu_dev.create_target_uniforms(target_w, target_h);

                // Take the encoder so we can create a single render pass for
                // all batches targeting this tile.  While the encoder is out,
                // wgpu_dev remains usable for pipeline / buffer / bind-group work.
                let mut encoder = wgpu_dev.take_encoder();

                let depth_attachment = if has_opaque {
                    depth_ref.map(|dv| wgpu::RenderPassDepthStencilAttachment {
                        view: dv,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    })
                } else {
                    None
                };
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("picture cache pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &target_view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(tile_clear_color),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: depth_attachment,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });

                    // Record all batches into this single render pass.
                    macro_rules! record_batch {
                        ($batch:expr, $is_alpha:expr, $depth_state:expr) => {{
                            let variant = Self::batch_key_to_pipeline_key(&$batch.key, $is_alpha);
                            let instance_bytes = crate::device::as_byte_slice($batch.instances.as_slice());

                            let color_views: [Option<wgpu::TextureView>; 3] = std::array::from_fn(|i| {
                                match $batch.key.textures.input.colors[i] {
                                    TextureSource::TextureCache(id, _swizzle) => {
                                        self.wgpu_texture_cache.get(&id).map(|t| t.create_view())
                                    }
                                    _ => None,
                                }
                            });

                            let textures = TextureBindings {
                                color0: color_views[0].as_ref(),
                                color1: color_views[1].as_ref(),
                                color2: color_views[2].as_ref(),
                                gpu_cache: draw_ctx.gpu_cache,
                                transform_palette: Some(draw_ctx.transform_palette),
                                render_tasks: Some(draw_ctx.render_tasks),
                                prim_headers_f: Some(draw_ctx.prim_headers_f),
                                prim_headers_i: Some(draw_ctx.prim_headers_i),
                                dither: draw_ctx.dither,
                                gpu_buffer_f: draw_ctx.gpu_buffer_f,
                                gpu_buffer_i: draw_ctx.gpu_buffer_i,
                                ..Default::default()
                            };

                            let wgpu_dev = self.wgpu_device.as_mut().unwrap();
                            wgpu_dev.record_draw(
                                &mut pass,
                                variant,
                                Self::blend_mode_to_wgpu(&$batch.key.blend_mode),
                                $depth_state,
                                target_fmt,
                                target_w,
                                target_h,
                                &textures,
                                &transform_buf,
                                &tex_size_buf,
                                instance_bytes,
                                $batch.instances.len() as u32,
                                scissor,
                            );
                            batches_drawn += 1;
                        }};
                    }

                    // Opaque batches: front-to-back (reverse order) with depth write.
                    if has_opaque {
                        for batch in alpha_batch_container.opaque_batches.iter().rev() {
                            record_batch!(batch, false, WgpuDepthState::WriteAndTest);
                        }
                    }

                    // Alpha batches: back-to-front (forward order) with depth test only.
                    let alpha_depth = if has_opaque {
                        WgpuDepthState::TestOnly
                    } else {
                        WgpuDepthState::None
                    };
                    for batch in alpha_batch_container.alpha_batches.iter() {
                        record_batch!(batch, true, alpha_depth);
                    }
                } // render pass dropped here

                // Return the encoder to WgpuDevice.
                let wgpu_dev = self.wgpu_device.as_mut().unwrap();
                wgpu_dev.return_encoder(encoder);

            }

            // Draw cs_* cache tasks, clip masks, quad batches, and
            // alpha_batch_containers in texture_cache/alpha/color targets.
            let all_targets = pass.texture_cache.values()
                .chain(pass.alpha.targets.iter())
                .chain(pass.color.targets.iter());
            for target in all_targets {
                let has_primary = !target.clip_batcher.primary_clips.is_empty();
                let has_secondary = !target.clip_batcher.secondary_clips.is_empty();
                let has_quads = target.prim_instances.iter().any(|m| !m.is_empty())
                    || !target.prim_instances_with_scissor.is_empty();
                let has_cs_tasks = !target.border_segments_solid.is_empty()
                    || !target.border_segments_complex.is_empty()
                    || !target.line_decorations.is_empty()
                    || !target.fast_linear_gradients.is_empty()
                    || !target.linear_gradients.is_empty()
                    || !target.radial_gradients.is_empty()
                    || !target.conic_gradients.is_empty()
                    || !target.horizontal_blurs.is_empty()
                    || !target.vertical_blurs.is_empty()
                    || !target.scalings.is_empty()
                    || !target.svg_filters.is_empty()
                    || !target.svg_nodes.is_empty();
                let has_alpha_batches = !target.alpha_batch_containers.is_empty();
                let has_clip_masks = !target.clip_masks.is_empty();
                let needs_depth = target.needs_depth();

                if !has_primary && !has_secondary && !has_quads && !has_cs_tasks && !has_alpha_batches && !has_clip_masks {
                    continue;
                }

                let target_wgpu = match self.wgpu_texture_cache.get(&target.texture_id) {
                    Some(t) => t,
                    None => {
                        batches_skipped += 1;
                        continue;
                    }
                };
                let target_view = target_wgpu.create_view();
                let target_fmt = target_wgpu.format();
                let target_w = target_wgpu.width;
                let target_h = target_wgpu.height;

                // All draws to this texture cache target share a single render pass.
                let wgpu_dev = self.wgpu_device.as_mut().unwrap();
                let (transform_buf, tex_size_buf) = wgpu_dev.create_target_uniforms(target_w, target_h);
                let depth_view = if needs_depth {
                    Some(wgpu_dev.acquire_depth_view(target_w, target_h))
                } else {
                    None
                };
                let mut encoder = wgpu_dev.take_encoder();

                // Blits: texture-to-texture copies (outside render pass).
                for blit in &target.blits {
                    let src_task = &frame.render_tasks[blit.source];
                    let src_task_rect = src_task.get_target_rect();
                    let src_texture = src_task.get_texture_source();
                    let src_id = match src_texture {
                        TextureSource::TextureCache(id, _) => Some(id),
                        _ => None,
                    };
                    let src_tex = src_id.and_then(|id| self.wgpu_texture_cache.get(&id));
                    if let Some(src_wgpu) = src_tex {
                        // Skip self-blits (same texture) and zero-size copies.
                        let w = blit.target_rect.width() as u32;
                        let h = blit.target_rect.height() as u32;
                        if w == 0 || h == 0 || src_id == Some(target.texture_id) {
                            continue;
                        }
                        // Validate copy fits within both textures.
                        let src_rect = blit.source_rect.translate(src_task_rect.min.to_vector());
                        let sx = src_rect.min.x as u32;
                        let sy = src_rect.min.y as u32;
                        let dx = blit.target_rect.min.x as u32;
                        let dy = blit.target_rect.min.y as u32;
                        if sx + w > src_wgpu.width || sy + h > src_wgpu.height
                            || dx + w > target_w || dy + h > target_h
                        {
                            continue;
                        }
                        encoder.copy_texture_to_texture(
                            wgpu::TexelCopyTextureInfo {
                                texture: &src_wgpu.texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d { x: sx, y: sy, z: 0 },
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::TexelCopyTextureInfo {
                                texture: &target_wgpu.texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d { x: dx, y: dy, z: 0 },
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::Extent3d {
                                width: w,
                                height: h,
                                depth_or_array_layers: 1,
                            },
                        );
                    }
                }

                // Resolve ops: parent-picture to child-target copies (backdrop-filter,
                // picture-cache surface resolution). Like blits, these must happen
                // outside the render pass via copy_texture_to_texture.
                for resolve_op in &target.resolve_ops {
                    let dest_task = &frame.render_tasks[resolve_op.dest_task_id];
                    let dest_info = match dest_task.kind {
                        RenderTaskKind::Picture(ref info) => info,
                        _ => continue,
                    };
                    let dest_task_rect = dest_task.get_target_rect().to_f32();
                    // Use content_size for blur-expanded dest targets.
                    let dest_task_rect = DeviceRect::from_origin_and_size(
                        dest_task_rect.min,
                        dest_info.content_size.to_f32(),
                    );

                    for &src_task_id in &resolve_op.src_task_ids {
                        let src_task = &frame.render_tasks[src_task_id];
                        let src_info = match src_task.kind {
                            RenderTaskKind::Picture(ref info) => info,
                            _ => continue,
                        };
                        let src_task_rect = src_task.get_target_rect().to_f32();

                        // Compute intersection in layout space then scale to device pixels.
                        let wanted = DeviceRect::from_origin_and_size(
                            dest_info.content_origin,
                            dest_task_rect.size().to_f32(),
                        ).cast_unit() * dest_info.device_pixel_scale.inverse();

                        let avail = DeviceRect::from_origin_and_size(
                            src_info.content_origin,
                            src_task_rect.size().to_f32(),
                        ).cast_unit() * src_info.device_pixel_scale.inverse();

                        let int_rect = match wanted.intersection(&avail) {
                            Some(r) => r,
                            None => continue,
                        };

                        let src_int_rect = (int_rect * src_info.device_pixel_scale).cast_unit();
                        let dest_int_rect = (int_rect * dest_info.device_pixel_scale).cast_unit();

                        let src_origin = src_task_rect.min.to_f32()
                            + src_int_rect.min.to_vector()
                            - src_info.content_origin.to_vector();
                        let src = DeviceIntRect::from_origin_and_size(
                            src_origin.to_i32(),
                            src_int_rect.size().round().to_i32(),
                        );

                        let dest_origin = dest_task_rect.min.to_f32()
                            + dest_int_rect.min.to_vector()
                            - dest_info.content_origin.to_vector();
                        let dest = DeviceIntRect::from_origin_and_size(
                            dest_origin.to_i32(),
                            dest_int_rect.size().round().to_i32(),
                        );

                        let w = dest.width() as u32;
                        let h = dest.height() as u32;
                        if w == 0 || h == 0 { continue; }

                        let src_tex_id = src_task.get_target_texture();
                        let dst_tex_id = dest_task.get_target_texture();
                        if src_tex_id == dst_tex_id { continue; }

                        let src_tex = match self.wgpu_texture_cache.get(&src_tex_id) {
                            Some(t) => t,
                            None => continue,
                        };
                        let dst_tex = match self.wgpu_texture_cache.get(&dst_tex_id) {
                            Some(t) => t,
                            None => continue,
                        };

                        let sx = src.min.x.max(0) as u32;
                        let sy = src.min.y.max(0) as u32;
                        let dx = dest.min.x.max(0) as u32;
                        let dy = dest.min.y.max(0) as u32;
                        if sx + w > src_tex.width || sy + h > src_tex.height { continue; }
                        if dx + w > dst_tex.width || dy + h > dst_tex.height { continue; }

                        encoder.copy_texture_to_texture(
                            wgpu::TexelCopyTextureInfo {
                                texture: &src_tex.texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d { x: sx, y: sy, z: 0 },
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::TexelCopyTextureInfo {
                                texture: &dst_tex.texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d { x: dx, y: dy, z: 0 },
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                        );
                    }
                }

                {
                    let depth_attachment = if needs_depth {
                        depth_view.as_ref().map(|dv| wgpu::RenderPassDepthStencilAttachment {
                            view: dv,
                            depth_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(1.0),
                                store: wgpu::StoreOp::Store,
                            }),
                            stencil_ops: None,
                        })
                    } else {
                        None
                    };
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("texture cache pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &target_view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load, // no clear — cache targets are pre-cleared
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: depth_attachment,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });

                    // cs_* cache target tasks: borders, gradients, blurs, etc.
                    if has_cs_tasks {
                        Self::draw_cache_target_tasks_wgpu(
                            self.wgpu_device.as_mut().unwrap(),
                            &draw_ctx,
                            target,
                            &mut pass,
                            target_w,
                            target_h,
                            target_fmt,
                            &transform_buf,
                            &tex_size_buf,
                            &mut batches_drawn,
                        );
                    }

                    // Clip masks: primary (overwrite), then secondary (multiplicative).
                    if has_primary {
                        Self::draw_clip_batch_list_wgpu(
                            self.wgpu_device.as_mut().unwrap(),
                            &draw_ctx,
                            &target.clip_batcher.primary_clips,
                            &mut pass,
                            target_w,
                            target_h,
                            target_fmt,
                            &transform_buf,
                            &tex_size_buf,
                            WgpuBlendMode::None,
                            &mut batches_drawn,
                        );
                    }
                    if has_secondary {
                        Self::draw_clip_batch_list_wgpu(
                            self.wgpu_device.as_mut().unwrap(),
                            &draw_ctx,
                            &target.clip_batcher.secondary_clips,
                            &mut pass,
                            target_w,
                            target_h,
                            target_fmt,
                            &transform_buf,
                            &tex_size_buf,
                            WgpuBlendMode::MultiplyClipMask,
                            &mut batches_drawn,
                        );
                    }

                    // Quad batches (prim_instances indexed by PatternKind).
                    if has_quads {
                        Self::draw_quad_batches_wgpu(
                            self.wgpu_device.as_mut().unwrap(),
                            &draw_ctx,
                            &target.prim_instances,
                            &target.prim_instances_with_scissor,
                            &mut pass,
                            target_w,
                            target_h,
                            target_fmt,
                            &transform_buf,
                            &tex_size_buf,
                            &mut batches_drawn,
                        );
                    }

                    // Alpha batch containers: opaque + alpha batches for offscreen
                    // surfaces (filters, blend modes, isolated stacking contexts).
                    if has_alpha_batches {
                        for alpha_batch_container in &target.alpha_batch_containers {
                            let has_opaque = !alpha_batch_container.opaque_batches.is_empty();

                            // Compute scissor rect from task_scissor_rect if present.
                            let scissor = alpha_batch_container.task_scissor_rect.map(|r| {
                                (r.min.x as u32, r.min.y as u32, r.width() as u32, r.height() as u32)
                            });

                            // Helper closure to record a single batch.
                            macro_rules! record_alpha_batch {
                                ($batch:expr, $is_alpha:expr, $depth_state:expr) => {{
                                    let variant = Self::batch_key_to_pipeline_key(&$batch.key, $is_alpha);
                                    let instance_bytes = crate::device::as_byte_slice($batch.instances.as_slice());

                                    let color_views: [Option<wgpu::TextureView>; 3] = std::array::from_fn(|i| {
                                        match $batch.key.textures.input.colors[i] {
                                            TextureSource::TextureCache(id, _swizzle) => {
                                                self.wgpu_texture_cache.get(&id).map(|t| t.create_view())
                                            }
                                            _ => None,
                                        }
                                    });

                                    let textures = TextureBindings {
                                        color0: color_views[0].as_ref(),
                                        color1: color_views[1].as_ref(),
                                        color2: color_views[2].as_ref(),
                                        gpu_cache: draw_ctx.gpu_cache,
                                        transform_palette: Some(draw_ctx.transform_palette),
                                        render_tasks: Some(draw_ctx.render_tasks),
                                        prim_headers_f: Some(draw_ctx.prim_headers_f),
                                        prim_headers_i: Some(draw_ctx.prim_headers_i),
                                        dither: draw_ctx.dither,
                                        gpu_buffer_f: draw_ctx.gpu_buffer_f,
                                        gpu_buffer_i: draw_ctx.gpu_buffer_i,
                                        ..Default::default()
                                    };

                                    let wgpu_dev = self.wgpu_device.as_mut().unwrap();
                                    wgpu_dev.record_draw(
                                        &mut pass,
                                        variant,
                                        Self::blend_mode_to_wgpu(&$batch.key.blend_mode),
                                        $depth_state,
                                        target_fmt,
                                        target_w,
                                        target_h,
                                        &textures,
                                        &transform_buf,
                                        &tex_size_buf,
                                        instance_bytes,
                                        $batch.instances.len() as u32,
                                        scissor,
                                    );
                                    batches_drawn += 1;
                                }};
                            }

                            // Opaque batches: front-to-back with depth write.
                            if has_opaque {
                                for batch in alpha_batch_container.opaque_batches.iter().rev() {
                                    record_alpha_batch!(batch, false, WgpuDepthState::WriteAndTest);
                                }
                            }

                            // Alpha batches: back-to-front with depth test only.
                            let alpha_depth = if has_opaque {
                                WgpuDepthState::TestOnly
                            } else {
                                WgpuDepthState::None
                            };
                            for batch in alpha_batch_container.alpha_batches.iter() {
                                record_alpha_batch!(batch, true, alpha_depth);
                            }
                        }
                    }

                    // ClipMaskInstanceList: GPU-driven mask instances (ps_quad_mask
                    // and ps_quad_textured with MultiplyClipMask blend).
                    if has_clip_masks {
                        let masks = &target.clip_masks;

                        // Emit one draw for a slice of MaskInstance into either
                        // PsQuadMask or PsQuadMaskFastPath.
                        macro_rules! record_mask_instances {
                            ($instances:expr, $variant:expr) => {{
                                if !$instances.is_empty() {
                                    let instance_bytes = crate::device::as_byte_slice($instances.as_slice());
                                    let textures = TextureBindings {
                                        gpu_cache: draw_ctx.gpu_cache,
                                        transform_palette: Some(draw_ctx.transform_palette),
                                        render_tasks: Some(draw_ctx.render_tasks),
                                        prim_headers_f: Some(draw_ctx.prim_headers_f),
                                        prim_headers_i: Some(draw_ctx.prim_headers_i),
                                        dither: draw_ctx.dither,
                                        gpu_buffer_f: draw_ctx.gpu_buffer_f,
                                        gpu_buffer_i: draw_ctx.gpu_buffer_i,
                                        ..Default::default()
                                    };
                                    let wgpu_dev = self.wgpu_device.as_mut().unwrap();
                                    wgpu_dev.record_draw(
                                        &mut pass,
                                        $variant,
                                        WgpuBlendMode::MultiplyClipMask,
                                        WgpuDepthState::None,
                                        target_fmt,
                                        target_w,
                                        target_h,
                                        &textures,
                                        &transform_buf,
                                        &tex_size_buf,
                                        instance_bytes,
                                        $instances.len() as u32,
                                        None,
                                    );
                                    batches_drawn += 1;
                                }
                            }};
                        }

                        // Fast path (no SDF lookup).
                        record_mask_instances!(&masks.mask_instances_fast, WgpuShaderVariant::PsQuadMaskFastPath);
                        // With per-draw scissor.
                        for (scissor_rect, instances) in &masks.mask_instances_fast_with_scissor {
                            if instances.is_empty() { continue; }
                            let instance_bytes = crate::device::as_byte_slice(instances.as_slice());
                            let textures = TextureBindings {
                                gpu_cache: draw_ctx.gpu_cache,
                                transform_palette: Some(draw_ctx.transform_palette),
                                render_tasks: Some(draw_ctx.render_tasks),
                                prim_headers_f: Some(draw_ctx.prim_headers_f),
                                prim_headers_i: Some(draw_ctx.prim_headers_i),
                                dither: draw_ctx.dither,
                                gpu_buffer_f: draw_ctx.gpu_buffer_f,
                                gpu_buffer_i: draw_ctx.gpu_buffer_i,
                                ..Default::default()
                            };
                            let scissor = Some((
                                scissor_rect.min.x.max(0) as u32,
                                scissor_rect.min.y.max(0) as u32,
                                scissor_rect.width() as u32,
                                scissor_rect.height() as u32,
                            ));
                            let wgpu_dev = self.wgpu_device.as_mut().unwrap();
                            wgpu_dev.record_draw(
                                &mut pass,
                                WgpuShaderVariant::PsQuadMaskFastPath,
                                WgpuBlendMode::MultiplyClipMask,
                                WgpuDepthState::None,
                                target_fmt,
                                target_w,
                                target_h,
                                &textures,
                                &transform_buf,
                                &tex_size_buf,
                                instance_bytes,
                                instances.len() as u32,
                                scissor,
                            );
                            batches_drawn += 1;
                        }

                        // Slow path (SDF / rounded clip).
                        record_mask_instances!(&masks.mask_instances_slow, WgpuShaderVariant::PsQuadMask);
                        // With per-draw scissor.
                        for (scissor_rect, instances) in &masks.mask_instances_slow_with_scissor {
                            if instances.is_empty() { continue; }
                            let instance_bytes = crate::device::as_byte_slice(instances.as_slice());
                            let textures = TextureBindings {
                                gpu_cache: draw_ctx.gpu_cache,
                                transform_palette: Some(draw_ctx.transform_palette),
                                render_tasks: Some(draw_ctx.render_tasks),
                                prim_headers_f: Some(draw_ctx.prim_headers_f),
                                prim_headers_i: Some(draw_ctx.prim_headers_i),
                                dither: draw_ctx.dither,
                                gpu_buffer_f: draw_ctx.gpu_buffer_f,
                                gpu_buffer_i: draw_ctx.gpu_buffer_i,
                                ..Default::default()
                            };
                            let scissor = Some((
                                scissor_rect.min.x.max(0) as u32,
                                scissor_rect.min.y.max(0) as u32,
                                scissor_rect.width() as u32,
                                scissor_rect.height() as u32,
                            ));
                            let wgpu_dev = self.wgpu_device.as_mut().unwrap();
                            wgpu_dev.record_draw(
                                &mut pass,
                                WgpuShaderVariant::PsQuadMask,
                                WgpuBlendMode::MultiplyClipMask,
                                WgpuDepthState::None,
                                target_fmt,
                                target_w,
                                target_h,
                                &textures,
                                &transform_buf,
                                &tex_size_buf,
                                instance_bytes,
                                instances.len() as u32,
                                scissor,
                            );
                            batches_drawn += 1;
                        }

                        // Image-based masks (ps_quad_textured with texture lookup).
                        for (texture_source, prim_instances) in &masks.image_mask_instances {
                            if prim_instances.is_empty() { continue; }
                            let instance_bytes = crate::device::as_byte_slice(prim_instances.as_slice());
                            let color0 = match *texture_source {
                                TextureSource::TextureCache(id, _) => {
                                    self.wgpu_texture_cache.get(&id).map(|t| t.create_view())
                                }
                                _ => None,
                            };
                            let textures = TextureBindings {
                                color0: color0.as_ref(),
                                gpu_cache: draw_ctx.gpu_cache,
                                transform_palette: Some(draw_ctx.transform_palette),
                                render_tasks: Some(draw_ctx.render_tasks),
                                prim_headers_f: Some(draw_ctx.prim_headers_f),
                                prim_headers_i: Some(draw_ctx.prim_headers_i),
                                dither: draw_ctx.dither,
                                gpu_buffer_f: draw_ctx.gpu_buffer_f,
                                gpu_buffer_i: draw_ctx.gpu_buffer_i,
                                ..Default::default()
                            };
                            let wgpu_dev = self.wgpu_device.as_mut().unwrap();
                            wgpu_dev.record_draw(
                                &mut pass,
                                WgpuShaderVariant::PsQuadTextured,
                                WgpuBlendMode::MultiplyClipMask,
                                WgpuDepthState::None,
                                target_fmt,
                                target_w,
                                target_h,
                                &textures,
                                &transform_buf,
                                &tex_size_buf,
                                instance_bytes,
                                prim_instances.len() as u32,
                                None,
                            );
                            batches_drawn += 1;
                        }
                        for ((scissor_rect, texture_source), prim_instances) in &masks.image_mask_instances_with_scissor {
                            if prim_instances.is_empty() { continue; }
                            let instance_bytes = crate::device::as_byte_slice(prim_instances.as_slice());
                            let color0 = match *texture_source {
                                TextureSource::TextureCache(id, _) => {
                                    self.wgpu_texture_cache.get(&id).map(|t| t.create_view())
                                }
                                _ => None,
                            };
                            let textures = TextureBindings {
                                color0: color0.as_ref(),
                                gpu_cache: draw_ctx.gpu_cache,
                                transform_palette: Some(draw_ctx.transform_palette),
                                render_tasks: Some(draw_ctx.render_tasks),
                                prim_headers_f: Some(draw_ctx.prim_headers_f),
                                prim_headers_i: Some(draw_ctx.prim_headers_i),
                                dither: draw_ctx.dither,
                                gpu_buffer_f: draw_ctx.gpu_buffer_f,
                                gpu_buffer_i: draw_ctx.gpu_buffer_i,
                                ..Default::default()
                            };
                            let scissor = Some((
                                scissor_rect.min.x.max(0) as u32,
                                scissor_rect.min.y.max(0) as u32,
                                scissor_rect.width() as u32,
                                scissor_rect.height() as u32,
                            ));
                            let wgpu_dev = self.wgpu_device.as_mut().unwrap();
                            wgpu_dev.record_draw(
                                &mut pass,
                                WgpuShaderVariant::PsQuadTextured,
                                WgpuBlendMode::MultiplyClipMask,
                                WgpuDepthState::None,
                                target_fmt,
                                target_w,
                                target_h,
                                &textures,
                                &transform_buf,
                                &tex_size_buf,
                                instance_bytes,
                                prim_instances.len() as u32,
                                scissor,
                            );
                            batches_drawn += 1;
                        }
                    }
                } // render pass dropped

                let wgpu_dev = self.wgpu_device.as_mut().unwrap();
                wgpu_dev.return_encoder(encoder);
            }
        }

        // Flush the batched encoder so all cache/target textures are written
        // before composite or readback.
        if let Some(dev) = self.wgpu_device.as_mut() {
            dev.flush_encoder();
        }

        if batches_drawn > 0 || batches_skipped > 0 {
            info!(
                "wgpu: drew {} batches, skipped {} in render passes",
                batches_drawn, batches_skipped,
            );
        }

        // Mark the frame as rendered (requires a separate mutable borrow).
        if let Some(doc) = self.active_documents.get_mut(doc_id) {
            doc.frame.has_been_rendered = true;
        }
    }

    /// Map a WebRender batch key to a typed shader variant for the wgpu
    /// pipeline cache.
    ///
    /// The `is_alpha` flag selects the ALPHA_PASS variant for batches from
    /// the alpha (transparent) list.
    #[cfg(feature = "wgpu_backend")]
    fn batch_key_to_pipeline_key(
        key: &crate::batch::BatchKey,
        is_alpha: bool,
    ) -> crate::device::WgpuShaderVariant {
        use crate::batch::BatchKind;
        use crate::batch::BrushBatchKind;
        use crate::device::WgpuShaderVariant;
        use glyph_rasterizer::GlyphFormat;

        match key.kind {
            BatchKind::Brush(BrushBatchKind::Solid) => {
                if is_alpha { WgpuShaderVariant::BrushSolidAlpha } else { WgpuShaderVariant::BrushSolid }
            }
            BatchKind::Brush(BrushBatchKind::Image(..)) => {
                if is_alpha { WgpuShaderVariant::BrushImageAlpha } else { WgpuShaderVariant::BrushImage }
            }
            BatchKind::Brush(BrushBatchKind::Blend) => {
                if is_alpha { WgpuShaderVariant::BrushBlendAlpha } else { WgpuShaderVariant::BrushBlend }
            }
            BatchKind::Brush(BrushBatchKind::MixBlend { .. }) => {
                if is_alpha { WgpuShaderVariant::BrushMixBlendAlpha } else { WgpuShaderVariant::BrushMixBlend }
            }
            BatchKind::Brush(BrushBatchKind::LinearGradient) => {
                if is_alpha { WgpuShaderVariant::BrushLinearGradientAlpha } else { WgpuShaderVariant::BrushLinearGradient }
            }
            BatchKind::Brush(BrushBatchKind::Opacity) => {
                if is_alpha { WgpuShaderVariant::BrushOpacityAlpha } else { WgpuShaderVariant::BrushOpacity }
            }
            BatchKind::Brush(BrushBatchKind::YuvImage(..)) => {
                if is_alpha { WgpuShaderVariant::BrushYuvImageAlpha } else { WgpuShaderVariant::BrushYuvImage }
            }
            BatchKind::TextRun(glyph_format) => {
                match glyph_format {
                    GlyphFormat::TransformedAlpha |
                    GlyphFormat::TransformedSubpixel => WgpuShaderVariant::PsTextRunGlyphTransform,
                    _ => WgpuShaderVariant::PsTextRun,
                }
            }
            BatchKind::Quad(pattern_kind) => {
                use crate::pattern::PatternKind;
                match pattern_kind {
                    PatternKind::ColorOrTexture => WgpuShaderVariant::PsQuadTextured,
                    PatternKind::Gradient => WgpuShaderVariant::PsQuadGradient,
                    PatternKind::RadialGradient => WgpuShaderVariant::PsQuadRadialGradient,
                    PatternKind::ConicGradient => WgpuShaderVariant::PsQuadConicGradient,
                    PatternKind::Mask => WgpuShaderVariant::PsQuadMask,
                }
            }
            BatchKind::SplitComposite => {
                WgpuShaderVariant::PsSplitComposite
            }
        }
    }

    /// Convert a WebRender `BlendMode` to its wgpu equivalent.
    ///
    /// Dual-source and advanced blend modes that have no direct wgpu mapping
    /// fall back to `PremultipliedAlpha`, which gives correct compositing for
    /// the common case.  Proper advanced blend support will require shader-side
    /// emulation or wgpu extensions.
    #[cfg(feature = "wgpu_backend")]
    fn blend_mode_to_wgpu(blend_mode: &BlendMode) -> crate::device::WgpuBlendMode {
        use crate::device::WgpuBlendMode;
        match *blend_mode {
            BlendMode::None => WgpuBlendMode::None,
            BlendMode::Alpha => WgpuBlendMode::Alpha,
            BlendMode::PremultipliedAlpha => WgpuBlendMode::PremultipliedAlpha,
            BlendMode::PremultipliedDestOut => WgpuBlendMode::PremultipliedDestOut,
            BlendMode::Screen => WgpuBlendMode::Screen,
            BlendMode::Exclusion => WgpuBlendMode::Exclusion,
            BlendMode::PlusLighter => WgpuBlendMode::PlusLighter,
            // Dual-source and advanced modes don't have direct wgpu equivalents.
            // Fall back to premultiplied alpha for now.
            BlendMode::SubpixelDualSource
            | BlendMode::MultiplyDualSource
            | BlendMode::Advanced(..) => WgpuBlendMode::PremultipliedAlpha,
        }
    }

    /// Convert a `DeviceIntRect` to a wgpu scissor rect `(x, y, w, h)`.
    ///
    /// wgpu uses top-left origin, same as WebRender's device space for
    /// offscreen render targets, so no Y-flip is needed.  Negative
    /// origins or zero-area rects are clamped to valid values.
    #[cfg(feature = "wgpu_backend")]
    fn device_rect_to_scissor(
        rect: Option<&DeviceIntRect>,
    ) -> Option<(u32, u32, u32, u32)> {
        rect.and_then(|r| {
            let x = r.min.x.max(0) as u32;
            let y = r.min.y.max(0) as u32;
            let w = r.width().max(0) as u32;
            let h = r.height().max(0) as u32;
            if w == 0 || h == 0 { None } else { Some((x, y, w, h)) }
        })
    }

    /// Draw a `ClipBatchList` through the wgpu device.
    ///
    /// Clip shaders use their own instance layouts (ClipMaskInstanceRect for
    /// rectangles, ClipMaskInstanceBoxShadow for box shadows).
    #[cfg(feature = "wgpu_backend")]
    fn draw_clip_batch_list_wgpu(
        wgpu_dev: &mut crate::device::WgpuDevice,
        ctx: &WgpuDrawContext<'_>,
        list: &ClipBatchList,
        pass: &mut wgpu::RenderPass<'_>,
        target_w: u32,
        target_h: u32,
        target_format: wgpu::TextureFormat,
        transform_buf: &wgpu::Buffer,
        tex_size_buf: &wgpu::Buffer,
        blend_mode: crate::device::WgpuBlendMode,
        batches_drawn: &mut u32,
    ) {
        use crate::device::TextureBindings;

        // Slow clip rectangles → cs_clip_rectangle (no FAST_PATH)
        if !list.slow_rectangles.is_empty() {
            let instance_bytes = crate::device::as_byte_slice(&list.slow_rectangles);
            let textures = TextureBindings {
                transform_palette: Some(ctx.transform_palette),
                gpu_cache: ctx.gpu_cache,
                ..Default::default()
            };
            wgpu_dev.record_draw(
                pass,
                crate::device::WgpuShaderVariant::CsClipRectangle,
                blend_mode,
                crate::device::WgpuDepthState::None,
                target_format,
                target_w,
                target_h,
                &textures,
                transform_buf,
                tex_size_buf,
                instance_bytes,
                list.slow_rectangles.len() as u32,
                None,  // no scissor for clip masks
            );
            *batches_drawn += 1;
        }

        // Fast clip rectangles → cs_clip_rectangle with FAST_PATH
        if !list.fast_rectangles.is_empty() {
            let instance_bytes = crate::device::as_byte_slice(&list.fast_rectangles);
            let textures = TextureBindings {
                transform_palette: Some(ctx.transform_palette),
                gpu_cache: ctx.gpu_cache,
                ..Default::default()
            };
            wgpu_dev.record_draw(
                pass,
                crate::device::WgpuShaderVariant::CsClipRectangleFastPath,
                blend_mode,
                crate::device::WgpuDepthState::None,
                target_format,
                target_w,
                target_h,
                &textures,
                transform_buf,
                tex_size_buf,
                instance_bytes,
                list.fast_rectangles.len() as u32,
                None, // no scissor for clip masks
            );
            *batches_drawn += 1;
        }

        // Box shadow clips → cs_clip_box_shadow
        for (mask_texture_source, items) in list.box_shadows.iter() {
            let instance_bytes = crate::device::as_byte_slice(items.as_slice());
            let mask_view = match *mask_texture_source {
                TextureSource::TextureCache(id, _) => {
                    ctx.texture_cache.get(&id).map(|t| t.create_view())
                }
                _ => None,
            };
            let textures = TextureBindings {
                color0: mask_view.as_ref(),
                transform_palette: Some(ctx.transform_palette),
                gpu_cache: ctx.gpu_cache,
                ..Default::default()
            };
            wgpu_dev.record_draw(
                pass,
                crate::device::WgpuShaderVariant::CsClipBoxShadow,
                blend_mode,
                crate::device::WgpuDepthState::None,
                target_format,
                target_w,
                target_h,
                &textures,
                transform_buf,
                tex_size_buf,
                instance_bytes,
                items.len() as u32,
                None, // no scissor for clip masks
            );
            *batches_drawn += 1;
        }
    }

    /// Draw quad batches (ps_quad_* shaders) for a single render target.
    ///
    /// Maps `PatternKind` to the correct shader name and dispatches instances
    /// for both non-scissored and scissored batches.
    #[cfg(feature = "wgpu_backend")]
    fn draw_quad_batches_wgpu(
        wgpu_dev: &mut crate::device::WgpuDevice,
        ctx: &WgpuDrawContext<'_>,
        prim_instances: &[FastHashMap<TextureSource, FrameVec<crate::gpu_types::PrimitiveInstanceData>>],
        prim_instances_with_scissor: &FastHashMap<
            (DeviceIntRect, PatternKind),
            FastHashMap<TextureSource, FrameVec<crate::gpu_types::PrimitiveInstanceData>>,
        >,
        pass: &mut wgpu::RenderPass<'_>,
        target_w: u32,
        target_h: u32,
        target_format: wgpu::TextureFormat,
        transform_buf: &wgpu::Buffer,
        tex_size_buf: &wgpu::Buffer,
        batches_drawn: &mut u32,
    ) {
        use crate::device::{TextureBindings, WgpuBlendMode};

        let pattern_to_shader = |pattern: PatternKind| -> crate::device::WgpuShaderVariant {
            use crate::device::WgpuShaderVariant;
            match pattern {
                PatternKind::ColorOrTexture => WgpuShaderVariant::PsQuadTextured,
                PatternKind::Gradient => WgpuShaderVariant::PsQuadGradient,
                PatternKind::RadialGradient => WgpuShaderVariant::PsQuadRadialGradient,
                PatternKind::ConicGradient => WgpuShaderVariant::PsQuadConicGradient,
                PatternKind::Mask => WgpuShaderVariant::PsQuadMask,
            }
        };

        // Non-scissored quad batches: blend disabled (opaque).
        for (pattern_idx, prim_instances_map) in prim_instances.iter().enumerate() {
            if prim_instances_map.is_empty() {
                continue;
            }
            let pattern = PatternKind::from_u32(pattern_idx as u32);
            let variant = pattern_to_shader(pattern);

            for (texture_source, instances) in prim_instances_map {
                let color0_view = match *texture_source {
                    TextureSource::TextureCache(id, _) => {
                        ctx.texture_cache.get(&id).map(|t| t.create_view())
                    }
                    _ => None,
                };
                let instance_bytes = crate::device::as_byte_slice(instances.as_slice());
                let textures = TextureBindings {
                    color0: color0_view.as_ref(),
                    gpu_cache: ctx.gpu_cache,
                    transform_palette: Some(ctx.transform_palette),
                    render_tasks: Some(ctx.render_tasks),
                    prim_headers_f: Some(ctx.prim_headers_f),
                    prim_headers_i: Some(ctx.prim_headers_i),
                    dither: ctx.dither,
                    gpu_buffer_f: ctx.gpu_buffer_f,
                    gpu_buffer_i: ctx.gpu_buffer_i,
                    ..Default::default()
                };
                wgpu_dev.record_draw(
                    pass,
                    variant,
                    WgpuBlendMode::None, // opaque quads
                    crate::device::WgpuDepthState::None,
                    target_format,
                    target_w,
                    target_h,
                    &textures,
                    transform_buf,
                    tex_size_buf,
                    instance_bytes,
                    instances.len() as u32,
                    None, // no scissor for opaque quads
                );
                *batches_drawn += 1;
            }
        }

        // Scissored quad batches: premultiplied alpha blend.
        for ((scissor_rect, pattern), prim_instances_map) in prim_instances_with_scissor {
            let variant = pattern_to_shader(*pattern);
            let scissor = Self::device_rect_to_scissor(Some(scissor_rect));

            for (texture_source, instances) in prim_instances_map {
                let color0_view = match *texture_source {
                    TextureSource::TextureCache(id, _) => {
                        ctx.texture_cache.get(&id).map(|t| t.create_view())
                    }
                    _ => None,
                };
                let instance_bytes = crate::device::as_byte_slice(instances.as_slice());
                let textures = TextureBindings {
                    color0: color0_view.as_ref(),
                    gpu_cache: ctx.gpu_cache,
                    transform_palette: Some(ctx.transform_palette),
                    render_tasks: Some(ctx.render_tasks),
                    prim_headers_f: Some(ctx.prim_headers_f),
                    prim_headers_i: Some(ctx.prim_headers_i),
                    dither: ctx.dither,
                    gpu_buffer_f: ctx.gpu_buffer_f,
                    gpu_buffer_i: ctx.gpu_buffer_i,
                    ..Default::default()
                };
                wgpu_dev.record_draw(
                    pass,
                    variant,
                    WgpuBlendMode::PremultipliedAlpha,
                    crate::device::WgpuDepthState::None,
                    target_format,
                    target_w,
                    target_h,
                    &textures,
                    transform_buf,
                    tex_size_buf,
                    instance_bytes,
                    instances.len() as u32,
                    scissor,
                );
                *batches_drawn += 1;
            }
        }
    }

    /// Draw cs_* cache target tasks (borders, line decorations, gradients,
    /// blurs, scaling) through the wgpu device for a single render target.
    #[cfg(feature = "wgpu_backend")]
    fn draw_cache_target_tasks_wgpu(
        wgpu_dev: &mut crate::device::WgpuDevice,
        ctx: &WgpuDrawContext<'_>,
        target: &crate::render_target::RenderTarget,
        pass: &mut wgpu::RenderPass<'_>,
        target_w: u32,
        target_h: u32,
        target_format: wgpu::TextureFormat,
        transform_buf: &wgpu::Buffer,
        tex_size_buf: &wgpu::Buffer,
        batches_drawn: &mut u32,
    ) {
        use crate::device::{TextureBindings, WgpuBlendMode};

        let base_textures = TextureBindings {
            gpu_cache: ctx.gpu_cache,
            dither: ctx.dither,
            ..Default::default()
        };

        // Helper: marshal instance data to bytes and record draw with explicit blend mode.
        macro_rules! draw_cs_blend {
            ($variant:expr, $instances:expr, $textures:expr, $blend:expr) => {
                if !$instances.is_empty() {
                    let instance_bytes = crate::device::as_byte_slice($instances.as_slice());
                    wgpu_dev.record_draw(
                        pass,
                        $variant,
                        $blend,
                        crate::device::WgpuDepthState::None,
                        target_format,
                        target_w,
                        target_h,
                        &$textures,
                        transform_buf,
                        tex_size_buf,
                        instance_bytes,
                        $instances.len() as u32,
                        None,
                    );
                    *batches_drawn += 1;
                }
            };
        }

        // Shorthand: draw with no blend (opaque cache writes).
        macro_rules! draw_cs {
            ($variant:expr, $instances:expr, $textures:expr) => {
                draw_cs_blend!($variant, $instances, $textures, WgpuBlendMode::None);
            };
        }

        use crate::device::WgpuShaderVariant;

        // Borders: premultiplied alpha blend (solid, then complex segments).
        draw_cs_blend!(WgpuShaderVariant::CsBorderSolid, target.border_segments_solid,
            base_textures, WgpuBlendMode::PremultipliedAlpha);
        draw_cs_blend!(WgpuShaderVariant::CsBorderSegment, target.border_segments_complex,
            base_textures, WgpuBlendMode::PremultipliedAlpha);

        // Line decorations: premultiplied alpha blend.
        draw_cs_blend!(WgpuShaderVariant::CsLineDecoration, target.line_decorations,
            base_textures, WgpuBlendMode::PremultipliedAlpha);

        // Gradients: no blend (opaque cache writes).
        draw_cs!(WgpuShaderVariant::CsFastLinearGradient, target.fast_linear_gradients, base_textures);
        draw_cs!(WgpuShaderVariant::CsLinearGradient, target.linear_gradients, base_textures);
        draw_cs!(WgpuShaderVariant::CsRadialGradient, target.radial_gradients, base_textures);
        draw_cs!(WgpuShaderVariant::CsConicGradient, target.conic_gradients, base_textures);

        // Blurs: iterate per texture source.
        for (texture_source, blurs) in target.vertical_blurs.iter()
            .chain(target.horizontal_blurs.iter())
        {
            if blurs.is_empty() {
                continue;
            }
            let color0_view = match *texture_source {
                TextureSource::TextureCache(id, _) => {
                    ctx.texture_cache.get(&id).map(|t| t.create_view())
                }
                _ => None,
            };
            let textures = TextureBindings {
                color0: color0_view.as_ref(),
                gpu_cache: ctx.gpu_cache,
                ..Default::default()
            };
            let instance_bytes = crate::device::as_byte_slice(blurs.as_slice());
            wgpu_dev.record_draw(
                pass,
                WgpuShaderVariant::CsBlurColor,
                WgpuBlendMode::None,
                crate::device::WgpuDepthState::None,
                target_format,
                target_w,
                target_h,
                &textures,
                transform_buf,
                tex_size_buf,
                instance_bytes,
                blurs.len() as u32,
                None,
            );
            *batches_drawn += 1;
        }

        // Scaling: iterate per texture source.
        for (texture_source, scalings) in target.scalings.iter() {
            if scalings.is_empty() {
                continue;
            }
            let color0_view = match *texture_source {
                TextureSource::TextureCache(id, _) => {
                    ctx.texture_cache.get(&id).map(|t| t.create_view())
                }
                _ => None,
            };
            let textures = TextureBindings {
                color0: color0_view.as_ref(),
                gpu_cache: ctx.gpu_cache,
                ..Default::default()
            };
            let instance_bytes = crate::device::as_byte_slice(scalings.as_slice());
            wgpu_dev.record_draw(
                pass,
                WgpuShaderVariant::CsScale,
                WgpuBlendMode::None,
                crate::device::WgpuDepthState::None,
                target_format,
                target_w,
                target_h,
                &textures,
                transform_buf,
                tex_size_buf,
                instance_bytes,
                scalings.len() as u32,
                None,
            );
            *batches_drawn += 1;
        }

        // SVG filters: repack u16 fields to i32 for wgpu vertex attributes.
        for (ref textures, ref filters) in &target.svg_filters {
            if filters.is_empty() {
                continue;
            }
            // Repack SvgFilterInstance (24 bytes, u16 fields) to i32 layout (32 bytes).
            let mut repacked = Vec::with_capacity(filters.len() * 8); // 8 x i32 per instance
            for f in filters.iter() {
                repacked.push(f.task_address.0);
                repacked.push(f.input_1_task_address.0);
                repacked.push(f.input_2_task_address.0);
                repacked.push(f.kind as i32);
                repacked.push(f.input_count as i32);
                repacked.push(f.generic_int as i32);
                repacked.push(f.extra_data_address.u as i32);
                repacked.push(f.extra_data_address.v as i32);
            }
            let instance_bytes = crate::device::as_byte_slice(repacked.as_slice());
            // Resolve color texture views from BatchTextures.
            let color_views: [Option<wgpu::TextureView>; 3] = std::array::from_fn(|i| {
                match textures.input.colors[i] {
                    TextureSource::TextureCache(id, _) => {
                        ctx.texture_cache.get(&id).map(|t| t.create_view())
                    }
                    _ => None,
                }
            });
            let tex = TextureBindings {
                color0: color_views[0].as_ref(),
                color1: color_views[1].as_ref(),
                color2: color_views[2].as_ref(),
                gpu_cache: ctx.gpu_cache,
                transform_palette: Some(ctx.transform_palette),
                render_tasks: Some(ctx.render_tasks),
                prim_headers_f: Some(ctx.prim_headers_f),
                prim_headers_i: Some(ctx.prim_headers_i),
                ..Default::default()
            };
            wgpu_dev.record_draw(
                pass,
                WgpuShaderVariant::CsSvgFilter,
                WgpuBlendMode::None,
                crate::device::WgpuDepthState::None,
                target_format,
                target_w,
                target_h,
                &tex,
                transform_buf,
                tex_size_buf,
                instance_bytes,
                filters.len() as u32,
                None,
            );
            *batches_drawn += 1;
        }

        // SVG filter nodes: repack u16 fields to i32 for wgpu vertex attributes.
        for (ref textures, ref filters) in &target.svg_nodes {
            if filters.is_empty() {
                continue;
            }
            // Repack SVGFEFilterInstance (64 bytes, u16 fields) to all-i32/f32 layout.
            // Output: 4xf32 + 4xf32 + 4xf32 + i32 + i32 + i32 + i32 + 2xi32 = 56 bytes
            let mut repacked = Vec::<u8>::with_capacity(filters.len() * 56);
            for f in filters.iter() {
                // target_rect: 4 x f32 (DeviceRect)
                repacked.extend_from_slice(&f.target_rect.min.x.to_le_bytes());
                repacked.extend_from_slice(&f.target_rect.min.y.to_le_bytes());
                repacked.extend_from_slice(&f.target_rect.max.x.to_le_bytes());
                repacked.extend_from_slice(&f.target_rect.max.y.to_le_bytes());
                // input_1_content_scale_and_offset: 4 x f32
                for v in &f.input_1_content_scale_and_offset {
                    repacked.extend_from_slice(&v.to_le_bytes());
                }
                // input_2_content_scale_and_offset: 4 x f32
                for v in &f.input_2_content_scale_and_offset {
                    repacked.extend_from_slice(&v.to_le_bytes());
                }
                // input_1_task_address: i32
                repacked.extend_from_slice(&f.input_1_task_address.0.to_le_bytes());
                // input_2_task_address: i32
                repacked.extend_from_slice(&f.input_2_task_address.0.to_le_bytes());
                // kind: i32
                repacked.extend_from_slice(&(f.kind as i32).to_le_bytes());
                // input_count: i32
                repacked.extend_from_slice(&(f.input_count as i32).to_le_bytes());
                // extra_data_address: 2 x i32
                repacked.extend_from_slice(&(f.extra_data_address.u as i32).to_le_bytes());
                repacked.extend_from_slice(&(f.extra_data_address.v as i32).to_le_bytes());
            }
            let color_views: [Option<wgpu::TextureView>; 3] = std::array::from_fn(|i| {
                match textures.input.colors[i] {
                    TextureSource::TextureCache(id, _) => {
                        ctx.texture_cache.get(&id).map(|t| t.create_view())
                    }
                    _ => None,
                }
            });
            let tex = TextureBindings {
                color0: color_views[0].as_ref(),
                color1: color_views[1].as_ref(),
                color2: color_views[2].as_ref(),
                gpu_cache: ctx.gpu_cache,
                transform_palette: Some(ctx.transform_palette),
                render_tasks: Some(ctx.render_tasks),
                prim_headers_f: Some(ctx.prim_headers_f),
                prim_headers_i: Some(ctx.prim_headers_i),
                ..Default::default()
            };
            wgpu_dev.record_draw(
                pass,
                WgpuShaderVariant::CsSvgFilterNode,
                WgpuBlendMode::None,
                crate::device::WgpuDepthState::None,
                target_format,
                target_w,
                target_h,
                &tex,
                transform_buf,
                tex_size_buf,
                &repacked,
                filters.len() as u32,
                None,
            );
            *batches_drawn += 1;
        }
    }

    /// Resize the wgpu surface when the window size changes.
    /// No-op if this is not a wgpu renderer with a surface.
    #[cfg(feature = "wgpu_backend")]
    pub fn resize_surface(&mut self, width: u32, height: u32) {
        if let Some(ref mut dev) = self.wgpu_device {
            dev.resize_surface(width, height);
        }
    }

    /// Update the current position of the debug cursor.
    pub fn set_cursor_position(
        &mut self,
        position: DeviceIntPoint,
    ) {
        self.cursor_position = position;
    }

    pub fn get_max_texture_size(&self) -> i32 {
        #[cfg(feature = "wgpu_backend")]
        if self.is_wgpu_only() {
            return 16384;
        }
        #[cfg(feature = "gl_backend")]
        { self.device.as_ref().unwrap().max_texture_size() }
        #[cfg(not(feature = "gl_backend"))]
        { 8192 }
    }

    pub fn get_graphics_api_info(&self) -> GraphicsApiInfo {
        #[cfg(feature = "wgpu_backend")]
        if self.is_wgpu_only() {
            return GraphicsApiInfo {
                kind: GraphicsApi::OpenGL, // TODO: add Wgpu variant
                version: "wgpu".to_string(),
                renderer: "wgpu".to_string(),
            };
        }
        #[cfg(feature = "gl_backend")]
        {
            GraphicsApiInfo {
                kind: GraphicsApi::OpenGL,
                version: self.device.as_ref().unwrap().version_string().to_string(),
                renderer: self.device.as_ref().unwrap().renderer_name().to_string(),
            }
        }
        #[cfg(not(feature = "gl_backend"))]
        {
            GraphicsApiInfo {
                kind: GraphicsApi::OpenGL,
                version: "wgpu".to_string(),
                renderer: "wgpu".to_string(),
            }
        }
    }

    pub fn preferred_color_format(&self) -> ImageFormat {
        #[cfg(feature = "wgpu_backend")]
        if self.is_wgpu_only() {
            return ImageFormat::RGBA8;
        }
        #[cfg(feature = "gl_backend")]
        { self.device.as_ref().unwrap().preferred_color_formats().external }
        #[cfg(not(feature = "gl_backend"))]
        { ImageFormat::RGBA8 }
    }

    pub fn required_texture_stride_alignment(&self, format: ImageFormat) -> usize {
        #[cfg(feature = "wgpu_backend")]
        if self.is_wgpu_only() {
            let _ = format;
            return 1;
        }
        #[cfg(feature = "gl_backend")]
        { self.device.as_ref().unwrap().required_pbo_stride().num_bytes(format).get() }
        #[cfg(not(feature = "gl_backend"))]
        { let _ = format; 1 }
    }

    pub fn set_clear_color(&mut self, color: ColorF) {
        self.clear_color = color;
    }

    pub fn flush_pipeline_info(&mut self) -> PipelineInfo {
        mem::replace(&mut self.pipeline_info, PipelineInfo::default())
    }

    /// Returns the Epoch of the current frame in a pipeline.
    pub fn current_epoch(&self, document_id: DocumentId, pipeline_id: PipelineId) -> Option<Epoch> {
        self.pipeline_info.epochs.get(&(pipeline_id, document_id)).cloned()
    }

    fn get_next_result_msg(&mut self) -> Option<ResultMsg> {
        if self.pending_result_msg.is_none() {
            if let Ok(msg) = self.result_rx.try_recv() {
                self.pending_result_msg = Some(msg);
            }
        }

        match (&self.pending_result_msg, &self.target_frame_publish_id) {
          (Some(ResultMsg::PublishDocument(frame_publish_id, _, _, _)), Some(target_id)) => {
            if frame_publish_id > target_id {
              return None;
            }
          }
          _ => {}
        }

        self.pending_result_msg.take()
    }

    /// Processes the result queue.
    ///
    /// Should be called before `render()`, as texture cache updates are done here.
    pub fn update(&mut self) {
        profile_scope!("update");

        // Pull any pending results and return the most recent.
        while let Some(msg) = self.get_next_result_msg() {
            match msg {
                ResultMsg::PublishPipelineInfo(mut pipeline_info) => {
                    for ((pipeline_id, document_id), epoch) in pipeline_info.epochs {
                        self.pipeline_info.epochs.insert((pipeline_id, document_id), epoch);
                    }
                    self.pipeline_info.removed_pipelines.extend(pipeline_info.removed_pipelines.drain(..));
                }
                ResultMsg::PublishDocument(
                    _,
                    document_id,
                    mut doc,
                    resource_update_list,
                ) => {
                    // Add a new document to the active set

                    // If the document we are replacing must be drawn (in order to
                    // update the texture cache), issue a render just to
                    // off-screen targets, ie pass None to render_impl. We do this
                    // because a) we don't need to render to the main framebuffer
                    // so it is cheaper not to, and b) doing so without a
                    // subsequent present would break partial present.
                    let prev_frame_memory = if let Some(mut prev_doc) = self.active_documents.remove(&document_id) {
                        doc.profile.merge(&mut prev_doc.profile);

                        #[cfg(feature = "gl_backend")]
                    if prev_doc.frame.must_be_drawn() && !self.is_wgpu_only() {
                            prev_doc.render_reasons |= RenderReasons::TEXTURE_CACHE_FLUSH;
                            self.render_impl(
                                document_id,
                                &mut prev_doc,
                                None,
                                0,
                            ).ok();
                        }

                        Some(prev_doc.frame.allocator_memory)
                    } else {
                        None
                    };

                    if let Some(memory) = prev_frame_memory {
                        // We just dropped the frame a few lives above. There should be no
                        // live allocations left in the frame's memory.
                        if !self.is_wgpu_only() {
                            memory.assert_memory_reusable();
                        }
                    }

                    self.active_documents.insert(document_id, doc);

                    // IMPORTANT: The pending texture cache updates must be applied
                    //            *after* the previous frame has been rendered above
                    //            (if neceessary for a texture cache update). For
                    //            an example of why this is required:
                    //            1) Previous frame contains a render task that
                    //               targets Texture X.
                    //            2) New frame contains a texture cache update which
                    //               frees Texture X.
                    //            3) bad stuff happens.

                    //TODO: associate `document_id` with target window
                    self.pending_texture_cache_updates |= !resource_update_list.texture_updates.updates.is_empty();
                    self.pending_texture_updates.push(resource_update_list.texture_updates);
                    self.pending_native_surface_updates.extend(resource_update_list.native_surface_updates);
                    self.documents_seen.insert(document_id);
                }
                ResultMsg::UpdateGpuCache(mut list) => {
                    if list.clear {
                        self.pending_gpu_cache_clear = true;
                    }
                    if list.clear {
                        self.gpu_cache_debug_chunks = Vec::new();
                    }
                    for cmd in mem::replace(&mut list.debug_commands, Vec::new()) {
                        match cmd {
                            GpuCacheDebugCmd::Alloc(chunk) => {
                                let row = chunk.address.v as usize;
                                if row >= self.gpu_cache_debug_chunks.len() {
                                    self.gpu_cache_debug_chunks.resize(row + 1, Vec::new());
                                }
                                self.gpu_cache_debug_chunks[row].push(chunk);
                            },
                            GpuCacheDebugCmd::Free(address) => {
                                let chunks = &mut self.gpu_cache_debug_chunks[address.v as usize];
                                let pos = chunks.iter()
                                    .position(|x| x.address == address).unwrap();
                                chunks.remove(pos);
                            },
                        }
                    }
                    self.pending_gpu_cache_updates.push(list);
                }
                ResultMsg::UpdateResources {
                    resource_updates,
                    memory_pressure,
                } => {
                    #[cfg(feature = "gl_backend")]
                    if memory_pressure && !self.is_wgpu_only() {
                        // If a memory pressure event arrives _after_ a new scene has
                        // been published that writes persistent targets (i.e. cached
                        // render tasks to the texture cache, or picture cache tiles)
                        // but _before_ the next update/render loop, those targets
                        // will not be updated due to the active_documents list being
                        // cleared at the end of this message. To work around that,
                        // if any of the existing documents have not rendered yet, and
                        // have picture/texture cache targets, force a render so that
                        // those targets are updated.
                        let active_documents = mem::replace(
                            &mut self.active_documents,
                            FastHashMap::default(),
                        );
                        for (doc_id, mut doc) in active_documents {
                            if doc.frame.must_be_drawn() {
                                // As this render will not be presented, we must pass None to
                                // render_impl. This avoids interfering with partial present
                                // logic, as well as being more efficient.
                                self.render_impl(
                                    doc_id,
                                    &mut doc,
                                    None,
                                    0,
                                ).ok();
                            }
                        }
                    }

                    if self.is_wgpu_only() {
                        #[cfg(feature = "wgpu_backend")]
                        {
                            self.pending_texture_cache_updates |= !resource_updates.texture_updates.updates.is_empty();
                            self.pending_texture_updates.push(resource_updates.texture_updates);
                            self.update_texture_cache_wgpu();
                        }
                    } else {
                        #[cfg(feature = "gl_backend")]
                        {
                            self.pending_texture_cache_updates |= !resource_updates.texture_updates.updates.is_empty();
                            self.pending_texture_updates.push(resource_updates.texture_updates);
                            self.pending_native_surface_updates.extend(resource_updates.native_surface_updates);
                            self.device.as_mut().unwrap().begin_frame();

                            self.update_texture_cache();
                            self.update_native_surfaces();

                            if memory_pressure {
                                self.upload_state.on_memory_pressure(self.device.as_mut().unwrap());
                            }

                            self.device.as_mut().unwrap().end_frame();
                        }
                    }
                }
                ResultMsg::RenderDocumentOffscreen(_document_id, mut _offscreen_doc, _resources) => {
                    #[cfg(feature = "gl_backend")]
                    {
                        if self.is_wgpu_only() {
                            continue;
                        }

                        let prev_doc = self.active_documents.remove(&_document_id);
                        if let Some(mut prev_doc) = prev_doc {
                            if prev_doc.frame.must_be_drawn() {
                                prev_doc.render_reasons |= RenderReasons::TEXTURE_CACHE_FLUSH;
                                self.render_impl(
                                    _document_id,
                                    &mut prev_doc,
                                    None,
                                    0,
                                ).ok();
                            }

                            self.active_documents.insert(_document_id, prev_doc);
                        }

                        self.pending_texture_cache_updates |= !_resources.texture_updates.updates.is_empty();
                        self.pending_texture_updates.push(_resources.texture_updates);
                        self.pending_native_surface_updates.extend(_resources.native_surface_updates);

                        self.render_impl(
                            _document_id,
                            &mut _offscreen_doc,
                            None,
                            0,
                        ).unwrap();
                    }
                    #[cfg(not(feature = "gl_backend"))]
                    { /* skip offscreen rendering in wgpu-only mode */ }
                }
                ResultMsg::AppendNotificationRequests(mut notifications) => {
                    // We need to know specifically if there are any pending
                    // TextureCacheUpdate updates in any of the entries in
                    // pending_texture_updates. They may simply be nops, which do not
                    // need to prevent issuing the notification, and if so, may not
                    // cause a timely frame render to occur to wake up any listeners.
                    if !self.pending_texture_cache_updates {
                        drain_filter(
                            &mut notifications,
                            |n| { n.when() == Checkpoint::FrameTexturesUpdated },
                            |n| { n.notify(); },
                        );
                    }
                    self.notifications.append(&mut notifications);
                }
                ResultMsg::ForceRedraw => {
                    self.force_redraw = true;
                }
                ResultMsg::RefreshShader(path) => {
                    self.pending_shader_updates.push(path);
                }
                ResultMsg::SetParameter(ref param) => {
                    #[cfg(feature = "gl_backend")]
                    if let Some(ref mut device) = self.device {
                        device.set_parameter(param);
                    }
                    self.profiler.set_parameter(param);
                }
                ResultMsg::DebugOutput(output) => match output {
                    #[cfg(feature = "capture")]
                    DebugOutput::SaveCapture(config, deferred) => {
                        self.save_capture(config, deferred);
                    }
                    #[cfg(feature = "replay")]
                    DebugOutput::LoadCapture(config, plain_externals) => {
                        self.active_documents.clear();
                        self.load_capture(config, plain_externals);
                    }
                },
                ResultMsg::DebugCommand(command) => {
                    self.handle_debug_command(command);
                }
            }
        }
    }

    /// update() defers processing of ResultMsg, if frame_publish_id of
    /// ResultMsg::PublishDocument exceeds target_frame_publish_id.
    pub fn set_target_frame_publish_id(&mut self, publish_id: FramePublishId) {
        self.target_frame_publish_id = Some(publish_id);
    }

    fn handle_debug_command(&mut self, command: DebugCommand) {
        match command {
            DebugCommand::SetPictureTileSize(_) |
            DebugCommand::SetMaximumSurfaceSize(_) |
            DebugCommand::GenerateFrame => {
                panic!("Should be handled by render backend");
            }
            #[cfg(feature = "debugger")]
            DebugCommand::Query(ref query) => {
                match query.kind {
                    DebugQueryKind::SpatialTree { .. } => {
                        panic!("Should be handled by render backend");
                    }
                    DebugQueryKind::CompositorConfig { .. } => {
                        let result = match self.active_documents.iter().last() {
                            Some((_, doc)) => {
                                doc.frame.composite_state.print_to_string()
                            }
                            None => {
                                "No active documents".into()
                            }
                        };
                        query.result.send(result).ok();
                    }
                    DebugQueryKind::CompositorView { .. } => {
                        let result = match self.active_documents.iter().last() {
                            Some((_, doc)) => {
                                let info = CompositorDebugInfo::from(&doc.frame.composite_state);
                                serde_json::to_string(&info).unwrap()
                            }
                            None => {
                                "No active documents".into()
                            }
                        };
                        query.result.send(result).ok();
                    }
                }
            }
            DebugCommand::SaveCapture(..) |
            DebugCommand::LoadCapture(..) |
            DebugCommand::StartCaptureSequence(..) |
            DebugCommand::StopCaptureSequence => {
                panic!("Capture commands are not welcome here! Did you build with 'capture' feature?")
            }
            DebugCommand::ClearCaches(_)
            | DebugCommand::SimulateLongSceneBuild(_)
            | DebugCommand::EnableNativeCompositor(_)
            | DebugCommand::SetBatchingLookback(_) => {}
            DebugCommand::InvalidateGpuCache => {
                #[cfg(feature = "gl_backend")]
                self.gpu_cache_texture.invalidate();
            }
            DebugCommand::SetFlags(flags) => {
                self.set_debug_flags(flags);
            }
            DebugCommand::GetDebugFlags(tx) => {
                tx.send(self.debug_flags).unwrap();
            }
            #[cfg(feature = "debugger")]
            DebugCommand::AddDebugClient(client) => {
                self.debugger.add_client(
                    client,
                    self.debug_flags,
                    &self.profiler,
                );
            }
        }
    }

    /// Set a callback for handling external images.
    pub fn set_external_image_handler(&mut self, handler: Box<dyn ExternalImageHandler>) {
        self.external_image_handler = Some(handler);
    }

    /// Retrieve (and clear) the current list of recorded frame profiles.
    pub fn get_frame_profiles(&mut self) -> (Vec<CpuProfile>, Vec<GpuProfile>) {
        let cpu_profiles = self.cpu_profiles.drain(..).collect();
        let gpu_profiles = self.gpu_profiles.drain(..).collect();
        (cpu_profiles, gpu_profiles)
    }

    /// Process texture cache updates in wgpu-only mode.
    /// Creates/deletes wgpu textures, uploads pixel data, and processes copies.
    #[cfg(feature = "wgpu_backend")]
    fn update_texture_cache_wgpu(&mut self) {
        use crate::internal_types::TextureUpdateSource;

        let mut pending_texture_updates = mem::replace(&mut self.pending_texture_updates, vec![]);
        self.pending_texture_cache_updates = false;

        let wgpu_dev = match self.wgpu_device {
            Some(ref dev) => dev,
            None => return,
        };

        for update_list in pending_texture_updates.drain(..) {
            // Process allocations: create or free wgpu textures.
            for allocation in &update_list.allocations {
                match allocation.kind {
                    TextureCacheAllocationKind::Free => {
                        // Remove and drop the wgpu texture.
                        self.wgpu_texture_cache.remove(&allocation.id);
                    }
                    TextureCacheAllocationKind::Alloc(ref info) |
                    TextureCacheAllocationKind::Reset(ref info) => {
                        // For Reset, remove old texture first.
                        if matches!(allocation.kind, TextureCacheAllocationKind::Reset(_)) {
                            self.wgpu_texture_cache.remove(&allocation.id);
                        }
                        let texture = wgpu_dev.create_cache_texture(
                            info.width,
                            info.height,
                            info.format,
                        );
                        self.wgpu_texture_cache.insert(allocation.id, texture);
                    }
                }
            }

            // Process copies first (atlas defragmentation).
            for ((src_id, dst_id), copies) in &update_list.copies {
                let src_tex = match self.wgpu_texture_cache.get(src_id) {
                    Some(t) => t,
                    None => {
                        warn!("wgpu: copy source texture {:?} not found", src_id);
                        continue;
                    }
                };
                let dst_tex = match self.wgpu_texture_cache.get(dst_id) {
                    Some(t) => t,
                    None => {
                        warn!("wgpu: copy dest texture {:?} not found", dst_id);
                        continue;
                    }
                };
                for copy in copies {
                    wgpu_dev.copy_texture_sub_rect(
                        src_tex,
                        copy.src_rect,
                        dst_tex,
                        copy.dst_rect,
                    );
                }
            }

            // Process uploads: write pixel data to wgpu textures.
            for (texture_id, updates) in update_list.updates {
                let texture = match self.wgpu_texture_cache.get(&texture_id) {
                    Some(t) => t,
                    None => {
                        warn!("wgpu: texture cache upload for unknown texture {:?}", texture_id);
                        continue;
                    }
                };
                for update in updates {
                    let dummy_data;
                    let data = match update.source {
                        TextureUpdateSource::Bytes { ref data } => {
                            &data[update.offset as usize ..]
                        }
                        TextureUpdateSource::DebugClear => {
                            // Skip debug clears for now.
                            continue;
                        }
                        TextureUpdateSource::External { id, channel_index } => {
                            let handler = self.external_image_handler
                                .as_mut()
                                .expect("Found external image, but no handler set!");
                            let ext_image = handler.lock(id, channel_index, false);
                            let src = match ext_image.source {
                                ExternalImageSource::RawData(data) => {
                                    &data[update.offset as usize ..]
                                }
                                ExternalImageSource::Invalid => {
                                    let bpp = texture.bytes_per_pixel();
                                    let width = update.stride.map(|s| s as u32)
                                        .unwrap_or(update.rect.width() as u32 * bpp);
                                    let total_size = width * update.rect.height() as u32;
                                    dummy_data = vec![0xFFu8; total_size as usize];
                                    &dummy_data
                                }
                                ExternalImageSource::NativeTexture(eid) => {
                                    panic!("Unexpected external texture {:?} for the texture cache update of {:?}", eid, id);
                                }
                            };
                            wgpu_dev.upload_texture_sub_rect(
                                texture,
                                update.rect,
                                update.stride,
                                src,
                                update.format_override.unwrap_or(api::ImageFormat::BGRA8),
                            );
                            handler.unlock(id, channel_index);
                            continue;
                        }
                    };
                    wgpu_dev.upload_texture_sub_rect(
                        texture,
                        update.rect,
                        update.stride,
                        data,
                        update.format_override.unwrap_or(api::ImageFormat::BGRA8),
                    );
                }
            }
        }

        drain_filter(
            &mut self.notifications,
            |n| { n.when() == Checkpoint::FrameTexturesUpdated },
            |n| { n.notify(); },
        );
    }

    /// Reset the current partial present state. This forces the entire framebuffer
    /// to be refreshed next time `render` is called.
    pub fn force_redraw(&mut self) {
        self.force_redraw = true;
    }

    /// Renders the current frame.
    ///
    /// A Frame is supplied by calling [`generate_frame()`][webrender_api::Transaction::generate_frame].
    /// buffer_age is the age of the current backbuffer. It is only relevant if partial present
    /// is active, otherwise 0 should be passed here.
    pub fn render(
        &mut self,
        device_size: DeviceIntSize,
        buffer_age: usize,
    ) -> Result<RenderResults, Vec<RendererError>> {
        self.device_size = Some(device_size);

        // In wgpu-only mode, use the dedicated wgpu render path.
        #[cfg(feature = "wgpu_backend")]
        if self.is_wgpu_only() {
            return self.render_wgpu(device_size);
        }

        #[cfg(feature = "gl_backend")]
        { self.render_gl(device_size, buffer_age) }

        #[cfg(not(feature = "gl_backend"))]
        { Err(vec![RendererError::UnsupportedBackend("GL backend not compiled")]) }
    }

    pub fn get_debug_flags(&self) -> DebugFlags {
        self.debug_flags
    }

    pub fn set_debug_flags(&mut self, flags: DebugFlags) {
        if let Some(enabled) = flag_changed(self.debug_flags, flags, DebugFlags::GPU_TIME_QUERIES) {
            if enabled {
                self.gpu_profiler.enable_timers();
            } else {
                self.gpu_profiler.disable_timers();
            }
        }
        if let Some(enabled) = flag_changed(self.debug_flags, flags, DebugFlags::GPU_SAMPLE_QUERIES) {
            if enabled {
                self.gpu_profiler.enable_samplers();
            } else {
                self.gpu_profiler.disable_samplers();
            }
        }

        self.debug_flags = flags;
    }

    pub fn set_profiler_ui(&mut self, ui_str: &str) {
        self.profiler.set_ui(ui_str);
    }
}

// ─── GL-only rendering pipeline ────────────────────────────────────────────
#[cfg(feature = "gl_backend")]
impl Renderer {
    fn render_gl(
        &mut self,
        device_size: DeviceIntSize,
        buffer_age: usize,
    ) -> Result<RenderResults, Vec<RendererError>> {
        let doc_id = self.active_documents.keys().last().cloned();

        let result = match doc_id {
            Some(doc_id) => {
                let mut doc = self.active_documents
                    .remove(&doc_id)
                    .unwrap();

                let size = if !device_size.is_empty() {
                    Some(device_size)
                } else {
                    None
                };

                let result = self.render_impl(
                    doc_id,
                    &mut doc,
                    size,
                    buffer_age,
                );

                self.active_documents.insert(doc_id, doc);

                result
            }
            None => {
                self.last_time = zeitstempel::now();
                Ok(RenderResults::default())
            }
        };

        drain_filter(
            &mut self.notifications,
            |n| { n.when() == Checkpoint::FrameRendered },
            |n| { n.notify(); },
        );

        let mut oom = false;
        if let Err(ref errors) = result {
            for error in errors {
                if matches!(error, &RendererError::OutOfMemory) {
                    oom = true;
                    break;
                }
            }
        }

        if oom {
            let _ = self.api_tx.send(ApiMsg::MemoryPressure);
            self.consecutive_oom_frames += 1;
            assert!(self.consecutive_oom_frames < 5, "Renderer out of memory");
        } else {
            self.consecutive_oom_frames = 0;
        }

        self.notifications.clear();

        tracy_frame_marker!();

        result
    }

    /// Update the state of any debug / profiler overlays. This is currently only needed
    /// when running with the native compositor enabled.
    fn update_debug_overlay(
        &mut self,
        framebuffer_size: DeviceIntSize,
        has_debug_items: bool,
    ) {
        // If any of the following debug flags are set, something will be drawn on the debug overlay.
        self.debug_overlay_state.is_enabled = has_debug_items || self.debug_flags.intersects(
            DebugFlags::PROFILER_DBG |
            DebugFlags::RENDER_TARGET_DBG |
            DebugFlags::TEXTURE_CACHE_DBG |
            DebugFlags::EPOCHS |
            DebugFlags::GPU_CACHE_DBG |
            DebugFlags::PICTURE_CACHING_DBG |
            DebugFlags::PRIMITIVE_DBG |
            DebugFlags::ZOOM_DBG |
            DebugFlags::WINDOW_VISIBILITY_DBG
        );

        // Update the debug overlay surface, if we are running in native compositor mode.
        if let CompositorKind::Native { .. } = self.current_compositor_kind {
            let compositor = self.compositor_config.compositor().unwrap();

            // If there is a current surface, destroy it if we don't need it for this frame, or if
            // the size has changed.
            if let Some(current_size) = self.debug_overlay_state.current_size {
                if !self.debug_overlay_state.is_enabled || current_size != framebuffer_size {
                    compositor.destroy_surface(self.device.as_mut().unwrap(), NativeSurfaceId::DEBUG_OVERLAY);
                    self.debug_overlay_state.current_size = None;
                }
            }

            // Allocate a new surface, if we need it and there isn't one.
            if self.debug_overlay_state.is_enabled && self.debug_overlay_state.current_size.is_none() {
                compositor.create_surface(
                    self.device.as_mut().unwrap(),
                    NativeSurfaceId::DEBUG_OVERLAY,
                    DeviceIntPoint::zero(),
                    framebuffer_size,
                    false,
                );
                compositor.create_tile(
                    self.device.as_mut().unwrap(),
                    NativeTileId::DEBUG_OVERLAY,
                );
                self.debug_overlay_state.current_size = Some(framebuffer_size);
            }
        }
    }

    /// Bind a draw target for the debug / profiler overlays, if required.
    fn bind_debug_overlay(&mut self, device_size: DeviceIntSize) -> Option<DrawTarget> {
        // Debug overlay setup are only required in native compositing mode
        if self.debug_overlay_state.is_enabled {
            match self.current_compositor_kind {
                CompositorKind::Native { .. } => {
                    let compositor = self.compositor_config.compositor().unwrap();
                    let surface_size = self.debug_overlay_state.current_size.unwrap();

                    // Ensure old surface is invalidated before binding
                    compositor.invalidate_tile(
                        self.device.as_mut().unwrap(),
                        NativeTileId::DEBUG_OVERLAY,
                        DeviceIntRect::from_size(surface_size),
                    );
                    // Bind the native surface
                    let surface_info = compositor.bind(
                        self.device.as_mut().unwrap(),
                        NativeTileId::DEBUG_OVERLAY,
                        DeviceIntRect::from_size(surface_size),
                        DeviceIntRect::from_size(surface_size),
                    );

                    // Bind the native surface to current FBO target
                    let draw_target = DrawTarget::NativeSurface {
                        offset: surface_info.origin,
                        external_fbo_id: surface_info.fbo_id,
                        dimensions: surface_size,
                    };
                    self.device.as_mut().unwrap().bind_draw_target(draw_target);

                    // When native compositing, clear the debug overlay each frame.
                    self.device.as_mut().unwrap().clear_target(
                        Some([0.0, 0.0, 0.0, 0.0]),
                        None, // debug renderer does not use depth
                        None,
                    );

                    Some(draw_target)
                }
                CompositorKind::Layer { .. } => {
                    let compositor = self.compositor_config.layer_compositor().unwrap();
                    compositor.bind_layer(self.debug_overlay_state.layer_index, &[]);

                    self.device.as_mut().unwrap().clear_target(
                        Some([0.0, 0.0, 0.0, 0.0]),
                        None, // debug renderer does not use depth
                        None,
                    );

                    Some(DrawTarget::new_default(device_size, self.device.as_mut().unwrap().surface_origin_is_top_left()))
                }
                CompositorKind::Draw { .. } => {
                    // If we're not using the native compositor, then the default
                    // frame buffer is already bound. Create a DrawTarget for it and
                    // return it.
                    Some(DrawTarget::new_default(device_size, self.device.as_mut().unwrap().surface_origin_is_top_left()))
                }
            }
        } else {
            None
        }
    }

    /// Unbind the draw target for debug / profiler overlays, if required.
    fn unbind_debug_overlay(&mut self) {
        // Debug overlay setup are only required in native compositing mode
        if self.debug_overlay_state.is_enabled {
            match self.current_compositor_kind {
                CompositorKind::Native { .. } => {
                    let compositor = self.compositor_config.compositor().unwrap();
                    // Unbind the draw target and add it to the visual tree to be composited
                    compositor.unbind(self.device.as_mut().unwrap());

                    let clip_rect = DeviceIntRect::from_size(
                        self.debug_overlay_state.current_size.unwrap(),
                    );

                    compositor.add_surface(
                        self.device.as_mut().unwrap(),
                        NativeSurfaceId::DEBUG_OVERLAY,
                        CompositorSurfaceTransform::identity(),
                        clip_rect,
                        ImageRendering::Auto,
                        clip_rect,
                        ClipRadius::EMPTY,
                    );
                }
                CompositorKind::Draw { .. } => {}
                CompositorKind::Layer { .. } => {
                    let compositor = self.compositor_config.layer_compositor().unwrap();
                    compositor.present_layer(self.debug_overlay_state.layer_index, &[]);
                }
            }
        }
    }

    // If device_size is None, don't render to the main frame buffer. This is useful to
    // update texture cache render tasks but avoid doing a full frame render. If the
    // render is not going to be presented, then this must be set to None, as performing a
    // composite without a present will confuse partial present.
    fn render_impl(
        &mut self,
        doc_id: DocumentId,
        active_doc: &mut RenderedDocument,
        mut device_size: Option<DeviceIntSize>,
        buffer_age: usize,
    ) -> Result<RenderResults, Vec<RendererError>> {
        profile_scope!("render");
        let mut results = RenderResults::default();
        self.profile.end_time_if_started(profiler::FRAME_SEND_TIME);
        self.profile.start_time(profiler::RENDERER_TIME);

        self.upload_state.begin_frame();

        let compositor_kind = active_doc.frame.composite_state.compositor_kind;
        // CompositorKind is updated
        if self.current_compositor_kind != compositor_kind {
            let enable = match (self.current_compositor_kind, compositor_kind) {
                (CompositorKind::Native { .. }, CompositorKind::Draw { .. }) => {
                    if self.debug_overlay_state.current_size.is_some() {
                        self.compositor_config
                            .compositor()
                            .unwrap()
                            .destroy_surface(self.device.as_mut().unwrap(), NativeSurfaceId::DEBUG_OVERLAY);
                        self.debug_overlay_state.current_size = None;
                    }
                    false
                }
                (CompositorKind::Draw { .. }, CompositorKind::Native { .. }) => {
                    true
                }
                (current_compositor_kind, active_doc_compositor_kind) => {
                    warn!("Compositor mismatch, assuming this is Wrench running. Current {:?}, active {:?}",
                        current_compositor_kind, active_doc_compositor_kind);
                    false
                }
            };

            if let Some(config) = self.compositor_config.compositor() {
                config.enable_native_compositor(self.device.as_mut().unwrap(), enable);
            }
            self.current_compositor_kind = compositor_kind;
        }

        // The texture resolver scope should be outside of any rendering, including
        // debug rendering. This ensures that when we return render targets to the
        // pool via glInvalidateFramebuffer, we don't do any debug rendering after
        // that point. Otherwise, the bind / invalidate / bind logic trips up the
        // render pass logic in tiled / mobile GPUs, resulting in an extra copy /
        // resolve step when the debug overlay is enabled.
        self.texture_resolver.begin_frame();

        if let Some(device_size) = device_size {
            self.update_gpu_profile(device_size);
        }

        let cpu_frame_id = {
            let _gm = self.gpu_profiler.start_marker("begin frame");
            let frame_id = self.device.as_mut().unwrap().begin_frame();
            self.gpu_profiler.begin_frame(frame_id);

            self.device.as_mut().unwrap().disable_scissor();
            self.device.as_mut().unwrap().disable_depth();
            self.set_blend(false, FramebufferKind::Main);
            //self.update_shaders();

            self.update_texture_cache();
            self.update_native_surfaces();

            frame_id
        };

        if !active_doc.frame.present {
            // Setting device_size to None is what ensures compositing/presenting
            // the frame is skipped in the rest of this module.
            device_size = None;
        }

        if let Some(device_size) = device_size {
            // Inform the client that we are starting a composition transaction if native
            // compositing is enabled. This needs to be done early in the frame, so that
            // we can create debug overlays after drawing the main surfaces.
            if let CompositorKind::Native { .. } = self.current_compositor_kind {
                let compositor = self.compositor_config.compositor().unwrap();
                compositor.begin_frame(self.device.as_mut().unwrap());
            }

            // Update the state of the debug overlay surface, ensuring that
            // the compositor mode has a suitable surface to draw to, if required.
            self.update_debug_overlay(device_size, !active_doc.frame.debug_items.is_empty());
        }

        let frame = &mut active_doc.frame;
        let profile = &mut active_doc.profile;
        assert!(self.current_compositor_kind == frame.composite_state.compositor_kind);

        if self.shared_texture_cache_cleared {
            assert!(self.documents_seen.contains(&doc_id),
                    "Cleared texture cache without sending new document frame.");
        }

        match self.prepare_gpu_cache(&frame.deferred_resolves) {
            Ok(..) => {
                assert!(frame.gpu_cache_frame_id <= self.gpu_cache_frame_id,
                    "Received frame depends on a later GPU cache epoch ({:?}) than one we received last via `UpdateGpuCache` ({:?})",
                    frame.gpu_cache_frame_id, self.gpu_cache_frame_id);

                self.draw_frame(
                    frame,
                    device_size,
                    buffer_age,
                    &mut results,
                );

                // TODO(nical): do this automatically by selecting counters in the wr profiler
                // Profile marker for the number of invalidated picture cache
                if thread_is_being_profiled() {
                    let duration = Duration::new(0,0);
                    if let Some(n) = self.profile.get(profiler::RENDERED_PICTURE_TILES) {
                        let message = (n as usize).to_string();
                        add_text_marker("NumPictureCacheInvalidated", &message, duration);
                    }
                }

                if device_size.is_some() {
                    self.draw_frame_debug_items(&frame.debug_items);
                }

                self.profile.merge(profile);
            }
            Err(e) => {
                self.renderer_errors.push(e);
            }
        }

        self.unlock_external_images(&frame.deferred_resolves);

        let _gm = self.gpu_profiler.start_marker("end frame");
        self.gpu_profiler.end_frame();

        let t = self.profile.end_time(profiler::RENDERER_TIME);
        self.profile.end_time_if_started(profiler::TOTAL_FRAME_CPU_TIME);

        let current_time = zeitstempel::now();
        if device_size.is_some() {
            let time = profiler::ns_to_ms(current_time - self.last_time);
            self.profile.set(profiler::FRAME_TIME, time);
        }

        let debug_overlay = device_size.and_then(|device_size| {
            // Bind a surface to draw the debug / profiler information to.
            self.bind_debug_overlay(device_size).map(|draw_target| {
                self.draw_render_target_debug(&draw_target);
                self.draw_texture_cache_debug(&draw_target);
                self.draw_gpu_cache_debug(device_size);
                self.draw_zoom_debug(device_size);
                self.draw_epoch_debug();
                self.draw_window_visibility_debug();
                draw_target
            })
        });

        Telemetry::record_renderer_time(Duration::from_micros((t * 1000.00) as u64));
        if self.profile.get(profiler::SHADER_BUILD_TIME).is_none() {
          Telemetry::record_renderer_time_no_sc(Duration::from_micros((t * 1000.00) as u64));
        }

        if self.max_recorded_profiles > 0 {
            while self.cpu_profiles.len() >= self.max_recorded_profiles {
                self.cpu_profiles.pop_front();
            }
            let cpu_profile = CpuProfile::new(
                cpu_frame_id,
                (self.profile.get_or(profiler::FRAME_BUILDING_TIME, 0.0) * 1000000.0) as u64,
                (self.profile.get_or(profiler::RENDERER_TIME, 0.0) * 1000000.0) as u64,
                self.profile.get_or(profiler::DRAW_CALLS, 0.0) as usize,
            );
            self.cpu_profiles.push_back(cpu_profile);
        }

        if thread_is_being_profiled() {
            let duration = Duration::new(0,0);
            let message = (self.profile.get_or(profiler::DRAW_CALLS, 0.0) as usize).to_string();
            add_text_marker("NumDrawCalls", &message, duration);
        }

        let report = self.texture_resolver.report_memory();
        self.profile.set(profiler::RENDER_TARGET_MEM, profiler::bytes_to_mb(report.render_target_textures));
        self.profile.set(profiler::PICTURE_TILES_MEM, profiler::bytes_to_mb(report.picture_tile_textures));
        self.profile.set(profiler::ATLAS_TEXTURES_MEM, profiler::bytes_to_mb(report.atlas_textures));
        self.profile.set(profiler::STANDALONE_TEXTURES_MEM, profiler::bytes_to_mb(report.standalone_textures));

        self.profile.set(profiler::DEPTH_TARGETS_MEM, profiler::bytes_to_mb(self.device.as_mut().unwrap().depth_targets_memory()));

        self.profile.set(profiler::TEXTURES_CREATED, self.device.as_ref().unwrap().textures_created);
        self.profile.set(profiler::TEXTURES_DELETED, self.device.as_ref().unwrap().textures_deleted);

        results.stats.texture_upload_mb = self.profile.get_or(profiler::TEXTURE_UPLOADS_MEM, 0.0);
        self.frame_counter += 1;
        results.stats.resource_upload_time = self.resource_upload_time;
        self.resource_upload_time = 0.0;
        results.stats.gpu_cache_upload_time = self.gpu_cache_upload_time;
        self.gpu_cache_upload_time = 0.0;

        if let Some(stats) = active_doc.frame_stats.take() {
          // Copy the full frame stats to RendererStats
          results.stats.merge(&stats);

          self.profiler.update_frame_stats(stats);
        }

        // Turn the render reasons bitflags into something we can see in the profiler.
        // For now this is just a binary yes/no for each bit, which means that when looking
        // at "Render reasons" in the profiler HUD the average view indicates the proportion
        // of frames that had the bit set over a half second window whereas max shows whether
        // the bit as been set at least once during that time window.
        // We could implement better ways to visualize this information.
        let add_markers = thread_is_being_profiled();
        for i in 0..RenderReasons::NUM_BITS {
            let counter = profiler::RENDER_REASON_FIRST + i as usize;
            let mut val = 0.0;
            let reason_bit = RenderReasons::from_bits_truncate(1 << i);
            if active_doc.render_reasons.contains(reason_bit) {
                val = 1.0;
                if add_markers {
                    let event_str = format!("Render reason {:?}", reason_bit);
                    add_event_marker(&event_str);
                }
            }
            self.profile.set(counter, val);
        }
        active_doc.render_reasons = RenderReasons::empty();


        self.texture_resolver.update_profile(&mut self.profile);

        // Note: this clears the values in self.profile.
        self.profiler.set_counters(&mut self.profile);

        // If debugger is enabled, collect any profiler updates before value is overwritten
        // during update below.
        #[cfg(feature = "debugger")]
        self.debugger.update(
            self.debug_flags,
            &self.profiler,
        );

        // Note: profile counters must be set before this or they will count for next frame.
        self.profiler.update();

        if self.debug_flags.intersects(DebugFlags::PROFILER_DBG | DebugFlags::PROFILER_CAPTURE) {
            if let Some(device_size) = device_size {
                //TODO: take device/pixel ratio into equation?
                if let Some(debug_renderer) = self.debug.get_mut(self.device.as_mut().unwrap()) {
                    self.profiler.draw_profile(
                        self.frame_counter,
                        debug_renderer,
                        device_size,
                    );
                }
            }
        }

        if self.debug_flags.contains(DebugFlags::ECHO_DRIVER_MESSAGES) {
            self.device.as_mut().unwrap().echo_driver_messages();
        }

        if let Some(debug_renderer) = self.debug.try_get_mut() {
            let small_screen = self.debug_flags.contains(DebugFlags::SMALL_SCREEN);
            let scale = if small_screen { 1.6 } else { 1.0 };
            // TODO(gw): Tidy this up so that compositor config integrates better
            //           with the (non-compositor) surface y-flip options.
            let surface_origin_is_top_left = match self.current_compositor_kind {
                CompositorKind::Native { .. } => true,
                CompositorKind::Draw { .. } | CompositorKind::Layer { .. } => self.device.as_mut().unwrap().surface_origin_is_top_left(),
            };
            // If there is a debug overlay, render it. Otherwise, just clear
            // the debug renderer.
            debug_renderer.render(
                self.device.as_mut().unwrap(),
                debug_overlay.and(device_size),
                scale,
                surface_origin_is_top_left,
            );
        }

        self.upload_state.end_frame(self.device.as_mut().unwrap());
        self.device.as_mut().unwrap().end_frame();

        if debug_overlay.is_some() {
            self.last_time = current_time;

            // Unbind the target for the debug overlay. No debug or profiler drawing
            // can occur afer this point.
            self.unbind_debug_overlay();
        }

        if device_size.is_some() {
            // Inform the client that we are finished this composition transaction if native
            // compositing is enabled. This must be called after any debug / profiling compositor
            // surfaces have been drawn and added to the visual tree.
            match self.current_compositor_kind {
                CompositorKind::Layer { .. } => {
                    let compositor = self.compositor_config.layer_compositor().unwrap();
                    compositor.end_frame();
                }
                CompositorKind::Native { .. } => {
                    profile_scope!("compositor.end_frame");
                    let compositor = self.compositor_config.compositor().unwrap();
                    compositor.end_frame(self.device.as_mut().unwrap());
                }
                CompositorKind::Draw { .. } => {}
            }
        }

        self.documents_seen.clear();
        self.shared_texture_cache_cleared = false;

        self.check_gl_errors();

        if self.renderer_errors.is_empty() {
            Ok(results)
        } else {
            Err(mem::replace(&mut self.renderer_errors, Vec::new()))
        }
    }

    fn update_gpu_profile(&mut self, device_size: DeviceIntSize) {
        let _gm = self.gpu_profiler.start_marker("build samples");
        // Block CPU waiting for last frame's GPU profiles to arrive.
        // In general this shouldn't block unless heavily GPU limited.
        let (gpu_frame_id, timers, samplers) = self.gpu_profiler.build_samples();

        if self.max_recorded_profiles > 0 {
            while self.gpu_profiles.len() >= self.max_recorded_profiles {
                self.gpu_profiles.pop_front();
            }

            self.gpu_profiles.push_back(GpuProfile::new(gpu_frame_id, &timers));
        }

        self.profiler.set_gpu_time_queries(timers);

        if !samplers.is_empty() {
            let screen_fraction = 1.0 / device_size.to_f32().area();

            fn accumulate_sampler_value(description: &str, samplers: &[GpuSampler]) -> f32 {
                let mut accum = 0.0;
                for sampler in samplers {
                    if sampler.tag.label != description {
                        continue;
                    }

                    accum += sampler.count as f32;
                }

                accum
            }

            let alpha_targets = accumulate_sampler_value(&"Alpha targets", &samplers) * screen_fraction;
            let transparent_pass = accumulate_sampler_value(&"Transparent pass", &samplers) * screen_fraction;
            let opaque_pass = accumulate_sampler_value(&"Opaque pass", &samplers) * screen_fraction;
            self.profile.set(profiler::ALPHA_TARGETS_SAMPLERS, alpha_targets);
            self.profile.set(profiler::TRANSPARENT_PASS_SAMPLERS, transparent_pass);
            self.profile.set(profiler::OPAQUE_PASS_SAMPLERS, opaque_pass);
            self.profile.set(profiler::TOTAL_SAMPLERS, alpha_targets + transparent_pass + opaque_pass);
        }
    }

    fn update_texture_cache(&mut self) {
        profile_scope!("update_texture_cache");

        let _gm = self.gpu_profiler.start_marker("texture cache update");
        let mut pending_texture_updates = mem::replace(&mut self.pending_texture_updates, vec![]);
        self.pending_texture_cache_updates = false;

        self.profile.start_time(profiler::TEXTURE_CACHE_UPDATE_TIME);

        let mut create_cache_texture_time = 0;
        let mut delete_cache_texture_time = 0;

        for update_list in pending_texture_updates.drain(..) {
            // Handle copies from one texture to another.
            for ((src_tex, dst_tex), copies) in &update_list.copies {

                let dest_texture = &self.texture_resolver.texture_cache_map[&dst_tex].texture;
                let dst_texture_size = dest_texture.get_dimensions().to_f32();

                let mut copy_instances = Vec::new();
                for copy in copies {
                    copy_instances.push(CopyInstance {
                        src_rect: copy.src_rect.to_f32(),
                        dst_rect: copy.dst_rect.to_f32(),
                        dst_texture_size,
                    });
                }

                let draw_target = DrawTarget::from_texture(dest_texture, false);
                self.device.as_mut().unwrap().bind_draw_target(draw_target);

                self.shaders.as_ref().unwrap()
                    .borrow_mut()
                    .ps_copy()
                    .bind(
                        self.device.as_mut().unwrap(),
                        &Transform3D::identity(),
                        None,
                        &mut self.renderer_errors,
                        &mut self.profile,
                    );

                self.draw_instanced_batch(
                    &copy_instances,
                    VertexArrayKind::Copy,
                    &BatchTextures::composite_rgb(
                        TextureSource::TextureCache(*src_tex, Swizzle::default())
                    ),
                    &mut RendererStats::default(),
                );
            }

            // Find any textures that will need to be deleted in this group of allocations.
            let mut pending_deletes = Vec::new();
            for allocation in &update_list.allocations {
                let old = self.texture_resolver.texture_cache_map.remove(&allocation.id);
                match allocation.kind {
                    TextureCacheAllocationKind::Alloc(_) => {
                        assert!(old.is_none(), "Renderer and backend disagree!");
                    }
                    TextureCacheAllocationKind::Reset(_) |
                    TextureCacheAllocationKind::Free => {
                        assert!(old.is_some(), "Renderer and backend disagree!");
                    }
                }
                if let Some(old) = old {

                    // Regenerate the cache allocation info so we can search through deletes for reuse.
                    let size = old.texture.get_dimensions();
                    let info = TextureCacheAllocInfo {
                        width: size.width,
                        height: size.height,
                        format: old.texture.get_format(),
                        filter: old.texture.get_filter(),
                        target: old.texture.get_target(),
                        is_shared_cache: old.texture.flags().contains(TextureFlags::IS_SHARED_TEXTURE_CACHE),
                        has_depth: old.texture.supports_depth(),
                        category: old.category,
                    };
                    pending_deletes.push((old.texture, info));
                }
            }
            // Look for any alloc or reset that has matching alloc info and save it from being deleted.
            let mut reused_textures = VecDeque::with_capacity(pending_deletes.len());
            for allocation in &update_list.allocations {
                match allocation.kind {
                    TextureCacheAllocationKind::Alloc(ref info) |
                    TextureCacheAllocationKind::Reset(ref info) => {
                        reused_textures.push_back(
                            pending_deletes.iter()
                                .position(|(_, old_info)| *old_info == *info)
                                .map(|index| pending_deletes.swap_remove(index).0)
                        );
                    }
                    TextureCacheAllocationKind::Free => {}
                }
            }

            // Now that we've saved as many deletions for reuse as we can, actually delete whatever is left.
            if !pending_deletes.is_empty() {
                let delete_texture_start = zeitstempel::now();
                for (texture, _) in pending_deletes {
                    add_event_marker("TextureCacheFree");
                    self.device.as_mut().unwrap().delete_texture(texture);
                }
                delete_cache_texture_time += zeitstempel::now() - delete_texture_start;
            }

            for allocation in update_list.allocations {
                match allocation.kind {
                    TextureCacheAllocationKind::Alloc(_) => add_event_marker("TextureCacheAlloc"),
                    TextureCacheAllocationKind::Reset(_) => add_event_marker("TextureCacheReset"),
                    TextureCacheAllocationKind::Free => {}
                };
                match allocation.kind {
                    TextureCacheAllocationKind::Alloc(ref info) |
                    TextureCacheAllocationKind::Reset(ref info) => {
                        let create_cache_texture_start = zeitstempel::now();
                        // Create a new native texture, as requested by the texture cache.
                        // If we managed to reuse a deleted texture, then prefer that instead.
                        //
                        // Ensure no PBO is bound when creating the texture storage,
                        // or GL will attempt to read data from there.
                        let mut texture = reused_textures
                            .pop_front()
                            .unwrap_or(None)
                            .unwrap_or_else(|| create_cache_texture(self.device.as_mut().unwrap(), info));

                        if info.is_shared_cache {
                            texture.flags_mut()
                                .insert(TextureFlags::IS_SHARED_TEXTURE_CACHE);

                            // On Mali-Gxx devices we use batched texture uploads as it performs much better.
                            // However, due to another driver bug we must ensure the textures are fully cleared,
                            // otherwise we get visual artefacts when blitting to the texture cache.
                            if self.device.as_mut().unwrap().use_batched_texture_uploads() &&
                                !self.device.as_mut().unwrap().get_capabilities().supports_render_target_partial_update
                            {
                                self.clear_texture(&texture, [0.0; 4]);
                            }

                            // Textures in the cache generally don't need to be cleared,
                            // but we do so if the debug display is active to make it
                            // easier to identify unallocated regions.
                            if self.debug_flags.contains(DebugFlags::TEXTURE_CACHE_DBG) {
                                self.clear_texture(&texture, TEXTURE_CACHE_DBG_CLEAR_COLOR);
                            }
                        }

                        create_cache_texture_time += zeitstempel::now() - create_cache_texture_start;

                        self.texture_resolver.texture_cache_map.insert(allocation.id, CacheTexture {
                            texture,
                            category: info.category,
                        });
                    }
                    TextureCacheAllocationKind::Free => {}
                };
            }

            upload_to_texture_cache(self, update_list.updates);

            self.check_gl_errors();
        }

        if create_cache_texture_time > 0 {
            self.profile.set(
                profiler::CREATE_CACHE_TEXTURE_TIME,
                profiler::ns_to_ms(create_cache_texture_time)
            );
        }
        if delete_cache_texture_time > 0 {
            self.profile.set(
                profiler::DELETE_CACHE_TEXTURE_TIME,
                profiler::ns_to_ms(delete_cache_texture_time)
            )
        }

        let t = self.profile.end_time(profiler::TEXTURE_CACHE_UPDATE_TIME);
        self.resource_upload_time += t;
        Telemetry::record_texture_cache_update_time(Duration::from_micros((t * 1000.00) as u64));

        drain_filter(
            &mut self.notifications,
            |n| { n.when() == Checkpoint::FrameTexturesUpdated },
            |n| { n.notify(); },
        );
    }

    fn check_gl_errors(&mut self) {
        let err = self.device.as_mut().unwrap().get_error();
        if err == gl::OUT_OF_MEMORY {
            self.renderer_errors.push(RendererError::OutOfMemory);
        }

        // Probably should check for other errors?
    }

    fn bind_shader<F>(
        &mut self,
        projection: &default::Transform3D<f32>,
        texture_size: Option<DeviceSize>,
        get_shader: F,
    )
    where
        F: FnOnce(&mut Shaders) -> &mut LazilyCompiledShader,
    {
        let mut shaders = self.shaders.as_ref().unwrap().borrow_mut();
        let shader = get_shader(&mut shaders);
        shader.bind(
            self.device.as_mut().unwrap(),
            projection,
            texture_size,
            &mut self.renderer_errors,
            &mut self.profile,
        );
    }

    fn bind_composite_shader(
        &mut self,
        projection: &default::Transform3D<f32>,
        texture_size: Option<DeviceSize>,
        format: CompositeSurfaceFormat,
        buffer_kind: ImageBufferKind,
        features: CompositeFeatures,
    ) {
        self.bind_shader(projection, texture_size, |shaders| {
            shaders.get_composite_shader(format, buffer_kind, features)
        });
    }

    fn flush_composite_batch(
        &mut self,
        batch: &mut CompositeBatchState,
        stats: &mut RendererStats,
    ) {
        if batch.instances.is_empty() {
            return;
        }

        self.draw_instanced_batch(
            &batch.instances,
            VertexArrayKind::Composite,
            &batch.textures,
            stats,
        );
        batch.instances.clear();
    }

    fn update_composite_batch_state(
        &mut self,
        projection: &default::Transform3D<f32>,
        batch: &mut CompositeBatchState,
        next_textures: BatchTextures,
        next_shader_params: CompositeShaderParams,
        stats: &mut RendererStats,
    ) {
        let flush_batch = !batch.textures.is_compatible_with(&next_textures) ||
            next_shader_params != batch.shader_params;

        if flush_batch {
            self.flush_composite_batch(
                batch,
                stats,
            );
        }

        if next_shader_params != batch.shader_params {
            self.bind_composite_shader(
                projection,
                next_shader_params.3,
                next_shader_params.0,
                next_shader_params.1,
                next_shader_params.2,
            );

            batch.shader_params = next_shader_params;
        }

        batch.textures = next_textures;
    }

    fn build_composite_draw_item(
        &mut self,
        tile: &CompositeTile,
        clip_rect: DeviceRect,
        needs_mask: bool,
        composite_state: &CompositeState,
        external_surfaces: &[ResolvedExternalSurface],
    ) -> (CompositeInstance, BatchTextures, CompositeShaderParams) {
        let tile_rect = composite_state.get_device_rect(&tile.local_rect, tile.transform_index);
        let transform = composite_state.get_device_transform(tile.transform_index);
        let flip = (transform.scale.x < 0.0, transform.scale.y < 0.0);

        let clip = match (needs_mask, tile.clip_index) {
            (true, Some(index)) => Some(composite_state.get_compositor_clip(index)),
            _ => None,
        };

        match tile.surface {
            CompositeTileSurface::Color { color } => {
                let dummy = TextureSource::Dummy;
                let image_buffer_kind = dummy.image_buffer_kind();
                let instance = CompositeInstance::new(
                    tile_rect,
                    clip_rect,
                    color.premultiplied(),
                    flip,
                    clip,
                );
                let features = instance.get_rgb_features();
                (
                    instance,
                    BatchTextures::composite_rgb(dummy),
                    (CompositeSurfaceFormat::Rgba, image_buffer_kind, features, None),
                )
            }
            CompositeTileSurface::Texture { surface: ResolvedSurfaceTexture::TextureCache { texture } } => {
                let instance = CompositeInstance::new(
                    tile_rect,
                    clip_rect,
                    PremultipliedColorF::WHITE,
                    flip,
                    clip,
                );
                let features = instance.get_rgb_features();
                (
                    instance,
                    BatchTextures::composite_rgb(texture),
                    (
                        CompositeSurfaceFormat::Rgba,
                        ImageBufferKind::Texture2D,
                        features,
                        None,
                    ),
                )
            }
            CompositeTileSurface::ExternalSurface { external_surface_index } => {
                let surface = &external_surfaces[external_surface_index.0];

                match surface.color_data {
                    ResolvedExternalSurfaceColorData::Yuv{ ref planes, color_space, format, channel_bit_depth, .. } => {
                        let textures = BatchTextures::composite_yuv(
                            planes[0].texture,
                            planes[1].texture,
                            planes[2].texture,
                        );

                        let uv_rects = [
                            self.texture_resolver.get_uv_rect(&textures.input.colors[0], planes[0].uv_rect),
                            self.texture_resolver.get_uv_rect(&textures.input.colors[1], planes[1].uv_rect),
                            self.texture_resolver.get_uv_rect(&textures.input.colors[2], planes[2].uv_rect),
                        ];

                        let instance = CompositeInstance::new_yuv(
                            tile_rect,
                            clip_rect,
                            color_space,
                            format,
                            channel_bit_depth,
                            uv_rects,
                            flip,
                            clip,
                        );
                        let features = instance.get_yuv_features();

                        (
                            instance,
                            textures,
                            (
                                CompositeSurfaceFormat::Yuv,
                                surface.image_buffer_kind,
                                features,
                                None
                            ),
                        )
                    },
                    ResolvedExternalSurfaceColorData::Rgb { ref plane, .. } => {
                        let uv_rect = self.texture_resolver.get_uv_rect(&plane.texture, plane.uv_rect);
                        let instance = CompositeInstance::new_rgb(
                            tile_rect,
                            clip_rect,
                            PremultipliedColorF::WHITE,
                            uv_rect,
                            plane.texture.uses_normalized_uvs(),
                            flip,
                            clip,
                        );
                        let features = instance.get_rgb_features();
                        (
                            instance,
                            BatchTextures::composite_rgb(plane.texture),
                            (
                                CompositeSurfaceFormat::Rgba,
                                surface.image_buffer_kind,
                                features,
                                Some(self.texture_resolver.get_texture_size(&plane.texture).to_f32()),
                            ),
                        )
                    },
                }
            }
            CompositeTileSurface::Clear => {
                let dummy = TextureSource::Dummy;
                let image_buffer_kind = dummy.image_buffer_kind();
                let instance = CompositeInstance::new(
                    tile_rect,
                    clip_rect,
                    PremultipliedColorF::BLACK,
                    flip,
                    clip,
                );
                let features = instance.get_rgb_features();
                (
                    instance,
                    BatchTextures::composite_rgb(dummy),
                    (CompositeSurfaceFormat::Rgba, image_buffer_kind, features, None),
                )
            }
            CompositeTileSurface::Texture { surface: ResolvedSurfaceTexture::Native { .. } } => {
                unreachable!("bug: found native surface in simple composite path");
            }
        }
    }

    fn draw_instanced_batch<T: Clone>(
        &mut self,
        data: &[T],
        vertex_array_kind: VertexArrayKind,
        textures: &BatchTextures,
        stats: &mut RendererStats,
    ) {
        self.texture_resolver
            .bind_batch_textures(textures, self.device.as_mut().unwrap(), &self.aux_textures);

        // If we end up with an empty draw call here, that means we have
        // probably introduced unnecessary batch breaks during frame
        // building - so we should be catching this earlier and removing
        // the batch.
        debug_assert!(!data.is_empty());

        let chunk_size = if self.debug_flags.contains(DebugFlags::DISABLE_BATCHING) {
            1
        } else if vertex_array_kind == VertexArrayKind::Primitive {
            self.max_primitive_instance_count
        } else {
            data.len()
        };

        let draw_calls = self.vaos.draw_instanced_batch(
            self.device.as_mut().unwrap(),
            data,
            vertex_array_kind,
            self.enable_instancing,
            chunk_size,
        );

        for _ in 0..draw_calls {
            self.profile.inc(profiler::DRAW_CALLS);
        }
        stats.total_draw_calls += draw_calls;

        self.profile.add(profiler::VERTICES, 6 * data.len());
    }

    fn handle_readback_composite(
        &mut self,
        draw_target: DrawTarget,
        uses_scissor: bool,
        backdrop: &RenderTask,
        readback: &RenderTask,
    ) {
        // Extract the rectangle in the backdrop surface's device space of where
        // we need to read from.
        let readback_origin = match readback.kind {
            RenderTaskKind::Readback(ReadbackTask { readback_origin: Some(o), .. }) => o,
            RenderTaskKind::Readback(ReadbackTask { readback_origin: None, .. }) => {
                // If this is a dummy readback, just early out. We know that the
                // clear of the target will ensure the task rect is already zero alpha,
                // so it won't affect the rendering output.
                return;
            }
            _ => unreachable!(),
        };

        if uses_scissor {
            self.device.as_mut().unwrap().disable_scissor();
        }

        let texture_source = TextureSource::TextureCache(
            readback.get_target_texture(),
            Swizzle::default(),
        );
        let (cache_texture, _) = self.texture_resolver
            .resolve(&texture_source).expect("bug: no source texture");

        // Before submitting the composite batch, do the
        // framebuffer readbacks that are needed for each
        // composite operation in this batch.
        let readback_rect = readback.get_target_rect();
        let backdrop_rect = backdrop.get_target_rect();
        let (backdrop_screen_origin, _) = match backdrop.kind {
            RenderTaskKind::Picture(ref task_info) => (task_info.content_origin, task_info.device_pixel_scale),
            _ => panic!("bug: composite on non-picture?"),
        };

        // Bind the FBO to blit the backdrop to.
        // Called per-instance in case the FBO changes. The device will skip
        // the GL call if the requested target is already bound.
        let cache_draw_target = DrawTarget::from_texture(
            cache_texture,
            false,
        );

        // Get the rect that we ideally want, in space of the parent surface
        let wanted_rect = DeviceRect::from_origin_and_size(
            readback_origin,
            readback_rect.size().to_f32(),
        );

        // Get the rect that is available on the parent surface. It may be smaller
        // than desired because this is a picture cache tile covering only part of
        // the wanted rect and/or because the parent surface was clipped.
        let avail_rect = DeviceRect::from_origin_and_size(
            backdrop_screen_origin,
            backdrop_rect.size().to_f32(),
        );

        if let Some(int_rect) = wanted_rect.intersection(&avail_rect) {
            // If there is a valid intersection, work out the correct origins and
            // sizes of the copy rects, and do the blit.
            let copy_size = int_rect.size().to_i32();

            let src_origin = backdrop_rect.min.to_f32() +
                int_rect.min.to_vector() -
                backdrop_screen_origin.to_vector();

            let src = DeviceIntRect::from_origin_and_size(
                src_origin.to_i32(),
                copy_size,
            );

            let dest_origin = readback_rect.min.to_f32() +
                int_rect.min.to_vector() -
                readback_origin.to_vector();

            let dest = DeviceIntRect::from_origin_and_size(
                dest_origin.to_i32(),
                copy_size,
            );

            // Should always be drawing to picture cache tiles or off-screen surface!
            debug_assert!(!draw_target.is_default());
            let device_to_framebuffer = Scale::new(1i32);

            self.device.as_mut().unwrap().blit_render_target(
                draw_target.into(),
                src * device_to_framebuffer,
                cache_draw_target,
                dest * device_to_framebuffer,
                TextureFilter::Linear,
            );
        }

        // Restore draw target to current pass render target, and reset
        // the read target.
        self.device.as_mut().unwrap().bind_draw_target(draw_target);
        self.device.as_mut().unwrap().reset_read_target();

        if uses_scissor {
            self.device.as_mut().unwrap().enable_scissor();
        }
    }

    fn handle_resolves(
        &mut self,
        resolve_ops: &[ResolveOp],
        render_tasks: &RenderTaskGraph,
        draw_target: DrawTarget,
    ) {
        if resolve_ops.is_empty() {
            return;
        }

        let _timer = self.gpu_profiler.start_timer(GPU_TAG_BLIT);

        for resolve_op in resolve_ops {
            self.handle_resolve(
                resolve_op,
                render_tasks,
                draw_target,
            );
        }

        self.device.as_mut().unwrap().reset_read_target();
    }

    fn handle_prims(
        &mut self,
        draw_target: &DrawTarget,
        prim_instances: &[FastHashMap<TextureSource, FrameVec<PrimitiveInstanceData>>],
        prim_instances_with_scissor: &FastHashMap<(DeviceIntRect, PatternKind), FastHashMap<TextureSource, FrameVec<PrimitiveInstanceData>>>,
        projection: &default::Transform3D<f32>,
        stats: &mut RendererStats,
    ) {
        self.device.as_mut().unwrap().disable_depth_write();

        let has_prim_instances = prim_instances.iter().any(|map| !map.is_empty());
        if has_prim_instances || !prim_instances_with_scissor.is_empty() {
            let _timer = self.gpu_profiler.start_timer(GPU_TAG_INDIRECT_PRIM);

            self.set_blend(false, FramebufferKind::Other);

            for (pattern_idx, prim_instances_map) in prim_instances.iter().enumerate() {
                if prim_instances_map.is_empty() {
                    continue;
                }
                let pattern = PatternKind::from_u32(pattern_idx as u32);

                self.shaders.as_ref().unwrap().borrow_mut().get_quad_shader(pattern).bind(
                    self.device.as_mut().unwrap(),
                    projection,
                    None,
                    &mut self.renderer_errors,
                    &mut self.profile,
                );

                for (texture_source, prim_instances) in prim_instances_map {
                    let texture_bindings = BatchTextures::composite_rgb(*texture_source);

                    self.draw_instanced_batch(
                        prim_instances,
                        VertexArrayKind::Primitive,
                        &texture_bindings,
                        stats,
                    );
                }
            }

            if !prim_instances_with_scissor.is_empty() {
                self.set_blend(true, FramebufferKind::Other);
                self.device.as_mut().unwrap().set_blend_mode_premultiplied_alpha();
                self.device.as_mut().unwrap().enable_scissor();

                let mut prev_pattern = None;

                for ((scissor_rect, pattern), prim_instances_map) in prim_instances_with_scissor {
                    if prev_pattern != Some(*pattern) {
                        prev_pattern = Some(*pattern);
                        self.shaders.as_ref().unwrap().borrow_mut().get_quad_shader(*pattern).bind(
                            self.device.as_mut().unwrap(),
                            projection,
                            None,
                            &mut self.renderer_errors,
                            &mut self.profile,
                        );
                    }

                    self.device.as_mut().unwrap().set_scissor_rect(draw_target.to_framebuffer_rect(*scissor_rect));

                    for (texture_source, prim_instances) in prim_instances_map {
                        let texture_bindings = BatchTextures::composite_rgb(*texture_source);

                        self.draw_instanced_batch(
                            prim_instances,
                            VertexArrayKind::Primitive,
                            &texture_bindings,
                            stats,
                        );
                    }
                }

                self.device.as_mut().unwrap().disable_scissor();
            }
        }
    }

    fn handle_clips(
        &mut self,
        draw_target: &DrawTarget,
        masks: &ClipMaskInstanceList,
        projection: &default::Transform3D<f32>,
        stats: &mut RendererStats,
    ) {
        self.device.as_mut().unwrap().disable_depth_write();

        {
            let _timer = self.gpu_profiler.start_timer(GPU_TAG_INDIRECT_MASK);

            self.set_blend(true, FramebufferKind::Other);
            self.set_blend_mode_multiply(FramebufferKind::Other);

            if !masks.mask_instances_fast.is_empty() {
                self.shaders.as_ref().unwrap().borrow_mut().ps_mask_fast().bind(
                    self.device.as_mut().unwrap(),
                    projection,
                    None,
                    &mut self.renderer_errors,
                    &mut self.profile,
                );

                self.draw_instanced_batch(
                    &masks.mask_instances_fast,
                    VertexArrayKind::Mask,
                    &BatchTextures::empty(),
                    stats,
                );
            }

            if !masks.mask_instances_fast_with_scissor.is_empty() {
                self.shaders.as_ref().unwrap().borrow_mut().ps_mask_fast().bind(
                    self.device.as_mut().unwrap(),
                    projection,
                    None,
                    &mut self.renderer_errors,
                    &mut self.profile,
                );

                self.device.as_mut().unwrap().enable_scissor();

                for (scissor_rect, instances) in &masks.mask_instances_fast_with_scissor {
                    self.device.as_mut().unwrap().set_scissor_rect(draw_target.to_framebuffer_rect(*scissor_rect));

                    self.draw_instanced_batch(
                        instances,
                        VertexArrayKind::Mask,
                        &BatchTextures::empty(),
                        stats,
                    );
                }

                self.device.as_mut().unwrap().disable_scissor();
            }

            if !masks.image_mask_instances.is_empty() {
                self.shaders.as_ref().unwrap().borrow_mut().ps_quad_textured().bind(
                    self.device.as_mut().unwrap(),
                    projection,
                    None,
                    &mut self.renderer_errors,
                    &mut self.profile,
                );

                for (texture, prim_instances) in &masks.image_mask_instances {
                    self.draw_instanced_batch(
                        prim_instances,
                        VertexArrayKind::Primitive,
                        &BatchTextures::composite_rgb(*texture),
                        stats,
                    );
                }
            }

            if !masks.image_mask_instances_with_scissor.is_empty() {
                self.device.as_mut().unwrap().enable_scissor();

                self.shaders.as_ref().unwrap().borrow_mut().ps_quad_textured().bind(
                    self.device.as_mut().unwrap(),
                    projection,
                    None,
                    &mut self.renderer_errors,
                    &mut self.profile,
                );

                for ((scissor_rect, texture), prim_instances) in &masks.image_mask_instances_with_scissor {
                    self.device.as_mut().unwrap().set_scissor_rect(draw_target.to_framebuffer_rect(*scissor_rect));

                    self.draw_instanced_batch(
                        prim_instances,
                        VertexArrayKind::Primitive,
                        &BatchTextures::composite_rgb(*texture),
                        stats,
                    );
                }

                self.device.as_mut().unwrap().disable_scissor();
            }

            if !masks.mask_instances_slow.is_empty() {
                self.shaders.as_ref().unwrap().borrow_mut().ps_mask().bind(
                    self.device.as_mut().unwrap(),
                    projection,
                    None,
                    &mut self.renderer_errors,
                    &mut self.profile,
                );

                self.draw_instanced_batch(
                    &masks.mask_instances_slow,
                    VertexArrayKind::Mask,
                    &BatchTextures::empty(),
                    stats,
                );
            }

            if !masks.mask_instances_slow_with_scissor.is_empty() {
                self.shaders.as_ref().unwrap().borrow_mut().ps_mask().bind(
                    self.device.as_mut().unwrap(),
                    projection,
                    None,
                    &mut self.renderer_errors,
                    &mut self.profile,
                );

                self.device.as_mut().unwrap().enable_scissor();

                for (scissor_rect, instances) in &masks.mask_instances_slow_with_scissor {
                    self.device.as_mut().unwrap().set_scissor_rect(draw_target.to_framebuffer_rect(*scissor_rect));

                    self.draw_instanced_batch(
                        instances,
                        VertexArrayKind::Mask,
                        &BatchTextures::empty(),
                        stats,
                    );
                }

                self.device.as_mut().unwrap().disable_scissor();
            }
        }
    }

    fn handle_blits(
        &mut self,
        blits: &[BlitJob],
        render_tasks: &RenderTaskGraph,
        draw_target: DrawTarget,
    ) {
        if blits.is_empty() {
            return;
        }

        let _timer = self.gpu_profiler.start_timer(GPU_TAG_BLIT);

        // TODO(gw): For now, we don't bother batching these by source texture.
        //           If if ever shows up as an issue, we can easily batch them.
        for blit in blits {
            let (source, source_rect) = {
                // A blit from the child render task into this target.
                // TODO(gw): Support R8 format here once we start
                //           creating mips for alpha masks.
                let task = &render_tasks[blit.source];
                let source_rect = blit.source_rect.translate(task.get_target_rect().min.to_vector());
                let source_texture = task.get_texture_source();

                (source_texture, source_rect)
            };

            let (texture, swizzle) = self.texture_resolver
                .resolve(&source)
                .expect("BUG: invalid source texture");

            if swizzle != Swizzle::default() {
                error!("Swizzle {:?} can't be handled by a blit", swizzle);
            }

            let read_target = DrawTarget::from_texture(
                texture,
                false,
            );

            self.device.as_mut().unwrap().blit_render_target(
                read_target.into(),
                read_target.to_framebuffer_rect(source_rect),
                draw_target,
                draw_target.to_framebuffer_rect(blit.target_rect),
                TextureFilter::Linear,
            );
        }
    }

    fn handle_scaling(
        &mut self,
        scalings: &FastHashMap<TextureSource, FrameVec<ScalingInstance>>,
        projection: &default::Transform3D<f32>,
        stats: &mut RendererStats,
    ) {
        if scalings.is_empty() {
            return
        }

        let _timer = self.gpu_profiler.start_timer(GPU_TAG_SCALE);
        for (source, instances) in scalings {
            let buffer_kind = source.image_buffer_kind();

            // When the source texture is an external texture, the UV rect is not known
            // when the external surface descriptor is created, because external textures
            // are not resolved until the lock() callback is invoked at the start of the
            // frame render. We must therefore override the source rects now.
            let uv_override_instances;
            let instances = match source {
                TextureSource::External(..) => {
                    uv_override_instances = instances.iter().map(|instance| {
                        let mut new_instance = instance.clone();
                        let texel_rect: TexelRect = self.texture_resolver.get_uv_rect(
                            &source,
                            instance.source_rect.cast().into()
                        ).into();
                        new_instance.source_rect = DeviceRect::new(texel_rect.uv0, texel_rect.uv1);
                        new_instance
                    }).collect::<Vec<_>>();
                    uv_override_instances.as_slice()
                }
                _ => instances.as_slice()
            };

            self.shaders.as_ref().unwrap()
                .borrow_mut()
                .get_scale_shader(buffer_kind)
                .bind(
                    self.device.as_mut().unwrap(),
                    &projection,
                    Some(self.texture_resolver.get_texture_size(source).to_f32()),
                    &mut self.renderer_errors,
                    &mut self.profile,
                );

            self.draw_instanced_batch(
                instances,
                VertexArrayKind::Scale,
                &BatchTextures::composite_rgb(*source),
                stats,
            );
        }
    }

    fn handle_svg_filters(
        &mut self,
        textures: &BatchTextures,
        svg_filters: &[SvgFilterInstance],
        projection: &default::Transform3D<f32>,
        stats: &mut RendererStats,
    ) {
        if svg_filters.is_empty() {
            return;
        }

        let _timer = self.gpu_profiler.start_timer(GPU_TAG_SVG_FILTER);

        self.shaders.as_ref().unwrap().borrow_mut().cs_svg_filter().bind(
            self.device.as_mut().unwrap(),
            &projection,
            None,
            &mut self.renderer_errors,
            &mut self.profile,
        );

        self.draw_instanced_batch(
            &svg_filters,
            VertexArrayKind::SvgFilter,
            textures,
            stats,
        );
    }

    fn handle_svg_nodes(
        &mut self,
        textures: &BatchTextures,
        svg_filters: &[SVGFEFilterInstance],
        projection: &default::Transform3D<f32>,
        stats: &mut RendererStats,
    ) {
        if svg_filters.is_empty() {
            return;
        }

        let _timer = self.gpu_profiler.start_timer(GPU_TAG_SVG_FILTER_NODES);

        self.shaders.as_ref().unwrap().borrow_mut().cs_svg_filter_node().bind(
            self.device.as_mut().unwrap(),
            &projection,
            None,
            &mut self.renderer_errors,
            &mut self.profile,
        );

        self.draw_instanced_batch(
            &svg_filters,
            VertexArrayKind::SvgFilterNode,
            textures,
            stats,
        );
    }

    fn handle_resolve(
        &mut self,
        resolve_op: &ResolveOp,
        render_tasks: &RenderTaskGraph,
        draw_target: DrawTarget,
    ) {
        for src_task_id in &resolve_op.src_task_ids {
            let src_task = &render_tasks[*src_task_id];
            let src_info = match src_task.kind {
                RenderTaskKind::Picture(ref info) => info,
                _ => panic!("bug: not a picture"),
            };
            let src_task_rect = src_task.get_target_rect().to_f32();

            let dest_task = &render_tasks[resolve_op.dest_task_id];
            let dest_info = match dest_task.kind {
                RenderTaskKind::Picture(ref info) => info,
                _ => panic!("bug: not a picture"),
            };
            let dest_task_rect = dest_task.get_target_rect().to_f32();

            // If the dest picture is going to a blur target, it may have been
            // expanded in size so that the downsampling passes don't introduce
            // sampling error. In this case, we need to ensure we use the
            // content size rather than the render task size to work out
            // the intersecting rect to use for the resolve copy.
            let dest_task_rect = DeviceRect::from_origin_and_size(
                dest_task_rect.min,
                dest_info.content_size.to_f32(),
            );

            // Get the rect that we ideally want, in space of the parent surface
            let wanted_rect = DeviceRect::from_origin_and_size(
                dest_info.content_origin,
                dest_task_rect.size().to_f32(),
            ).cast_unit() * dest_info.device_pixel_scale.inverse();

            // Get the rect that is available on the parent surface. It may be smaller
            // than desired because this is a picture cache tile covering only part of
            // the wanted rect and/or because the parent surface was clipped.
            let avail_rect = DeviceRect::from_origin_and_size(
                src_info.content_origin,
                src_task_rect.size().to_f32(),
            ).cast_unit() * src_info.device_pixel_scale.inverse();

            if let Some(device_int_rect) = wanted_rect.intersection(&avail_rect) {
                let src_int_rect = (device_int_rect * src_info.device_pixel_scale).cast_unit();
                let dest_int_rect = (device_int_rect * dest_info.device_pixel_scale).cast_unit();

                // If there is a valid intersection, work out the correct origins and
                // sizes of the copy rects, and do the blit.

                let src_origin = src_task_rect.min.to_f32() +
                    src_int_rect.min.to_vector() -
                    src_info.content_origin.to_vector();

                let src = DeviceIntRect::from_origin_and_size(
                    src_origin.to_i32(),
                    src_int_rect.size().round().to_i32(),
                );

                let dest_origin = dest_task_rect.min.to_f32() +
                    dest_int_rect.min.to_vector() -
                    dest_info.content_origin.to_vector();

                let dest = DeviceIntRect::from_origin_and_size(
                    dest_origin.to_i32(),
                    dest_int_rect.size().round().to_i32(),
                );

                let texture_source = TextureSource::TextureCache(
                    src_task.get_target_texture(),
                    Swizzle::default(),
                );
                let (cache_texture, _) = self.texture_resolver
                    .resolve(&texture_source).expect("bug: no source texture");

                let read_target = ReadTarget::from_texture(cache_texture);

                // Should always be drawing to picture cache tiles or off-screen surface!
                debug_assert!(!draw_target.is_default());
                let device_to_framebuffer = Scale::new(1i32);

                self.device.as_mut().unwrap().blit_render_target(
                    read_target,
                    src * device_to_framebuffer,
                    draw_target,
                    dest * device_to_framebuffer,
                    TextureFilter::Linear,
                );
            }
        }
    }

    fn draw_picture_cache_target(
        &mut self,
        target: &PictureCacheTarget,
        draw_target: DrawTarget,
        projection: &default::Transform3D<f32>,
        render_tasks: &RenderTaskGraph,
        stats: &mut RendererStats,
    ) {
        profile_scope!("draw_picture_cache_target");

        self.profile.inc(profiler::RENDERED_PICTURE_TILES);
        let _gm = self.gpu_profiler.start_marker("picture cache target");

        {
            let _timer = self.gpu_profiler.start_timer(GPU_TAG_SETUP_TARGET);
            let framebuffer_kind = self.begin_draw_target_pass(
                draw_target,
                true,
                Some(target.dirty_rect),
                0,
            );
            self.set_blend(false, framebuffer_kind);

            let clear_color = target.clear_color.map(|c| c.to_array());
            let scissor_rect = if self.device.as_mut().unwrap().get_capabilities().supports_render_target_partial_update
                && (target.dirty_rect != target.valid_rect
                    || self.device.as_mut().unwrap().get_capabilities().prefers_clear_scissor)
            {
                Some(target.dirty_rect)
            } else {
                None
            };
            match scissor_rect {
                // If updating only a dirty rect within a picture cache target, the
                // clear must also be scissored to that dirty region.
                Some(r) if self.clear_caches_with_quads => {
                    self.device.as_mut().unwrap().enable_depth(DepthFunction::Always);
                    // Save the draw call count so that our reftests don't get confused...
                    let old_draw_call_count = stats.total_draw_calls;
                    if clear_color.is_none() {
                        self.device.as_mut().unwrap().disable_color_write();
                    }
                    let instance = ClearInstance {
                        rect: [
                            r.min.x as f32, r.min.y as f32,
                            r.max.x as f32, r.max.y as f32,
                        ],
                        color: clear_color.unwrap_or([0.0; 4]),
                    };
                    self.shaders.as_ref().unwrap().borrow_mut().ps_clear().bind(
                        self.device.as_mut().unwrap(),
                        &projection,
                        None,
                        &mut self.renderer_errors,
                        &mut self.profile,
                    );
                    self.draw_instanced_batch(
                        &[instance],
                        VertexArrayKind::Clear,
                        &BatchTextures::empty(),
                        stats,
                    );
                    if clear_color.is_none() {
                        self.device.as_mut().unwrap().enable_color_write();
                    }
                    stats.total_draw_calls = old_draw_call_count;
                    self.device.as_mut().unwrap().disable_depth();
                }
                other => {
                    let scissor_rect = other.map(|rect| {
                        draw_target.build_scissor_rect(Some(rect))
                    });
                    self.device.as_mut().unwrap().clear_target(clear_color, Some(1.0), scissor_rect);
                }
            };
            self.device.as_mut().unwrap().disable_depth_write();
        }

        let framebuffer_kind = Self::framebuffer_kind_for(draw_target);

        match target.kind {
            PictureCacheTargetKind::Draw { ref alpha_batch_container } => {
                self.draw_alpha_batch_container(
                    alpha_batch_container,
                    draw_target,
                    framebuffer_kind,
                    projection,
                    render_tasks,
                    stats,
                );
            }
            PictureCacheTargetKind::Blit { task_id, sub_rect_offset } => {
                let src_task = &render_tasks[task_id];
                let (texture, _swizzle) = self.texture_resolver
                    .resolve(&src_task.get_texture_source())
                    .expect("BUG: invalid source texture");

                let src_task_rect = src_task.get_target_rect();

                let p0 = src_task_rect.min + sub_rect_offset;
                let p1 = p0 + target.dirty_rect.size();
                let src_rect = DeviceIntRect::new(p0, p1);

                // TODO(gw): In future, it'd be tidier to have the draw target offset
                //           for DC surfaces handled by `blit_render_target`. However,
                //           for now they are only ever written to here.
                let target_rect = target
                    .dirty_rect
                    .translate(draw_target.offset().to_vector())
                    .cast_unit();

                self.device.as_mut().unwrap().blit_render_target(
                    ReadTarget::from_texture(texture),
                    src_rect.cast_unit(),
                    draw_target,
                    target_rect,
                    TextureFilter::Nearest,
                );
            }
        }

        self.end_draw_target_pass(true);
    }

    /// Draw an alpha batch container into a given draw target. This is used
    /// by both color and picture cache target kinds.
    fn draw_alpha_batch_container(
        &mut self,
        alpha_batch_container: &AlphaBatchContainer,
        draw_target: DrawTarget,
        framebuffer_kind: FramebufferKind,
        projection: &default::Transform3D<f32>,
        render_tasks: &RenderTaskGraph,
        stats: &mut RendererStats,
    ) {
        let uses_scissor = alpha_batch_container.task_scissor_rect.is_some();

        if uses_scissor {
            self.device.as_mut().unwrap().enable_scissor();
            let scissor_rect = draw_target.build_scissor_rect(
                alpha_batch_container.task_scissor_rect,
            );
            self.device.as_mut().unwrap().set_scissor_rect(scissor_rect)
        }

        if !alpha_batch_container.opaque_batches.is_empty()
            && !self.debug_flags.contains(DebugFlags::DISABLE_OPAQUE_PASS) {
            self.draw_opaque_batches(
                alpha_batch_container,
                framebuffer_kind,
                projection,
                stats,
            );
        } else {
            self.device.as_mut().unwrap().disable_depth();
        }

        if !alpha_batch_container.alpha_batches.is_empty()
            && !self.debug_flags.contains(DebugFlags::DISABLE_ALPHA_PASS) {
            self.draw_transparent_batches(
                alpha_batch_container,
                draw_target,
                uses_scissor,
                framebuffer_kind,
                projection,
                render_tasks,
                stats,
            );
            self.set_blend(false, framebuffer_kind);
        }

        self.device.as_mut().unwrap().disable_depth();
        if uses_scissor {
            self.device.as_mut().unwrap().disable_scissor();
        }
    }

    fn draw_opaque_batches(
        &mut self,
        alpha_batch_container: &AlphaBatchContainer,
        framebuffer_kind: FramebufferKind,
        projection: &default::Transform3D<f32>,
        stats: &mut RendererStats,
    ) {
        let _gl = self.gpu_profiler.start_marker("opaque batches");
        let opaque_sampler = self.gpu_profiler.start_sampler(GPU_SAMPLER_TAG_OPAQUE);
        self.set_blend(false, framebuffer_kind);
        self.device.as_mut().unwrap().enable_depth(DepthFunction::LessEqual);
        self.device.as_mut().unwrap().enable_depth_write();

        for batch in alpha_batch_container.opaque_batches.iter().rev() {
            if should_skip_batch(&batch.key.kind, self.debug_flags) {
                continue;
            }

            self.shaders.as_ref().unwrap().borrow_mut()
                .get(&batch.key, batch.features, self.debug_flags, self.device.as_ref().unwrap())
                .bind(
                    self.device.as_mut().unwrap(), projection, None,
                    &mut self.renderer_errors,
                    &mut self.profile,
                );

            let _timer = self.gpu_profiler.start_timer(batch.key.kind.sampler_tag());
            self.draw_instanced_batch(
                &batch.instances,
                VertexArrayKind::Primitive,
                &batch.key.textures,
                stats
            );
        }

        self.device.as_mut().unwrap().disable_depth_write();
        self.gpu_profiler.finish_sampler(opaque_sampler);
    }

    fn apply_alpha_batch_blend_mode(
        &mut self,
        blend_mode: BlendMode,
        framebuffer_kind: FramebufferKind,
    ) {
        match blend_mode {
            _ if self.debug_flags.contains(DebugFlags::SHOW_OVERDRAW) &&
                framebuffer_kind == FramebufferKind::Main => {
                self.device.as_mut().unwrap().set_blend_mode_show_overdraw();
            }
            BlendMode::None => {
                unreachable!("bug: opaque blend in alpha pass");
            }
            BlendMode::Alpha => {
                self.device.as_mut().unwrap().set_blend_mode_alpha();
            }
            BlendMode::PremultipliedAlpha => {
                self.device.as_mut().unwrap().set_blend_mode_premultiplied_alpha();
            }
            BlendMode::PremultipliedDestOut => {
                self.device.as_mut().unwrap().set_blend_mode_premultiplied_dest_out();
            }
            BlendMode::SubpixelDualSource => {
                self.device.as_mut().unwrap().set_blend_mode_subpixel_dual_source();
            }
            BlendMode::Advanced(mode) => {
                if self.enable_advanced_blend_barriers {
                    self.device.as_mut().unwrap().blend_barrier_advanced();
                }
                self.device.as_mut().unwrap().set_blend_mode_advanced(mode);
            }
            BlendMode::MultiplyDualSource => {
                self.device.as_mut().unwrap().set_blend_mode_multiply_dual_source();
            }
            BlendMode::Screen => {
                self.device.as_mut().unwrap().set_blend_mode_screen();
            }
            BlendMode::Exclusion => {
                self.device.as_mut().unwrap().set_blend_mode_exclusion();
            }
            BlendMode::PlusLighter => {
                self.device.as_mut().unwrap().set_blend_mode_plus_lighter();
            }
        }
    }

    fn draw_transparent_batches(
        &mut self,
        alpha_batch_container: &AlphaBatchContainer,
        draw_target: DrawTarget,
        uses_scissor: bool,
        framebuffer_kind: FramebufferKind,
        projection: &default::Transform3D<f32>,
        render_tasks: &RenderTaskGraph,
        stats: &mut RendererStats,
    ) {
        let _gl = self.gpu_profiler.start_marker("alpha batches");
        let transparent_sampler = self.gpu_profiler.start_sampler(GPU_SAMPLER_TAG_TRANSPARENT);
        self.set_blend(true, framebuffer_kind);

        let mut pass_state = AlphaBatchPassState::new();
        let shaders_rc = self.shaders.clone().unwrap();

        for batch in &alpha_batch_container.alpha_batches {
            self.draw_transparent_batch(
                batch,
                draw_target,
                uses_scissor,
                framebuffer_kind,
                projection,
                render_tasks,
                stats,
                &shaders_rc,
                &mut pass_state,
            );
        }

        self.gpu_profiler.finish_sampler(transparent_sampler);
    }

    fn draw_transparent_batch(
        &mut self,
        batch: &PrimitiveBatch,
        draw_target: DrawTarget,
        uses_scissor: bool,
        framebuffer_kind: FramebufferKind,
        projection: &default::Transform3D<f32>,
        render_tasks: &RenderTaskGraph,
        stats: &mut RendererStats,
        shaders_rc: &Rc<RefCell<Shaders>>,
        pass_state: &mut AlphaBatchPassState,
    ) {
        if should_skip_batch(&batch.key.kind, self.debug_flags) {
            return;
        }

        let mut shaders = shaders_rc.borrow_mut();
        let shader = shaders.get(
            &batch.key,
            batch.features | BatchFeatures::ALPHA_PASS,
            self.debug_flags,
            self.device.as_ref().unwrap(),
        );

        if pass_state.transition_blend_mode(batch.key.blend_mode) {
            self.apply_alpha_batch_blend_mode(
                batch.key.blend_mode,
                framebuffer_kind,
            );
        }

        if let BatchKind::Brush(BrushBatchKind::MixBlend { task_id, backdrop_id }) = batch.key.kind {
            debug_assert_eq!(batch.instances.len(), 1);
            self.handle_readback_composite(
                draw_target,
                uses_scissor,
                &render_tasks[task_id],
                &render_tasks[backdrop_id],
            );
        }

        let _timer = self.gpu_profiler.start_timer(batch.key.kind.sampler_tag());
        shader.bind(
            self.device.as_mut().unwrap(),
            projection,
            None,
            &mut self.renderer_errors,
            &mut self.profile,
        );

        self.draw_instanced_batch(
            &batch.instances,
            VertexArrayKind::Primitive,
            &batch.key.textures,
            stats
        );
    }

    /// Rasterize any external compositor surfaces that require updating
    fn update_external_native_surfaces(
        &mut self,
        external_surfaces: &[ResolvedExternalSurface],
        results: &mut RenderResults,
    ) {
        if external_surfaces.is_empty() {
            return;
        }

        let opaque_sampler = self.gpu_profiler.start_sampler(GPU_SAMPLER_TAG_OPAQUE);

        self.device.as_mut().unwrap().disable_depth();
        self.set_blend(false, FramebufferKind::Main);

        for surface in external_surfaces {
            // See if this surface needs to be updated
            let (native_surface_id, surface_size) = match surface.update_params {
                Some(params) => params,
                None => continue,
            };

            // When updating an external surface, the entire surface rect is used
            // for all of the draw, dirty, valid and clip rect parameters.
            let surface_rect = surface_size.into();

            // Bind the native compositor surface to update
            let surface_info = self.compositor_config
                .compositor()
                .unwrap()
                .bind(
                    self.device.as_mut().unwrap(),
                    NativeTileId {
                        surface_id: native_surface_id,
                        x: 0,
                        y: 0,
                    },
                    surface_rect,
                    surface_rect,
                );

            // Bind the native surface to current FBO target
            let draw_target = DrawTarget::NativeSurface {
                offset: surface_info.origin,
                external_fbo_id: surface_info.fbo_id,
                dimensions: surface_size,
            };
            self.device.as_mut().unwrap().bind_draw_target(draw_target);

            let projection = Transform3D::ortho(
                0.0,
                surface_size.width as f32,
                0.0,
                surface_size.height as f32,
                self.device.as_mut().unwrap().ortho_near_plane(),
                self.device.as_mut().unwrap().ortho_far_plane(),
            );

            let ( textures, instance ) = match surface.color_data {
                ResolvedExternalSurfaceColorData::Yuv{
                        ref planes, color_space, format, channel_bit_depth, .. } => {

                    let textures = BatchTextures::composite_yuv(
                        planes[0].texture,
                        planes[1].texture,
                        planes[2].texture,
                    );

                    // When the texture is an external texture, the UV rect is not known when
                    // the external surface descriptor is created, because external textures
                    // are not resolved until the lock() callback is invoked at the start of
                    // the frame render. To handle this, query the texture resolver for the
                    // UV rect if it's an external texture, otherwise use the default UV rect.
                    let uv_rects = [
                        self.texture_resolver.get_uv_rect(&textures.input.colors[0], planes[0].uv_rect),
                        self.texture_resolver.get_uv_rect(&textures.input.colors[1], planes[1].uv_rect),
                        self.texture_resolver.get_uv_rect(&textures.input.colors[2], planes[2].uv_rect),
                    ];

                    let instance = CompositeInstance::new_yuv(
                        surface_rect.to_f32(),
                        surface_rect.to_f32(),
                        // z-id is not relevant when updating a native compositor surface.
                        // TODO(gw): Support compositor surfaces without z-buffer, for memory / perf win here.
                        color_space,
                        format,
                        channel_bit_depth,
                        uv_rects,
                        (false, false),
                        None,
                    );

                    // Bind an appropriate YUV shader for the texture format kind
                    self.bind_composite_shader(
                        &projection,
                        None,
                        CompositeSurfaceFormat::Yuv,
                        surface.image_buffer_kind,
                        instance.get_yuv_features(),
                    );

                    ( textures, instance )
                },
                ResolvedExternalSurfaceColorData::Rgb{ ref plane, .. } => {
                    let textures = BatchTextures::composite_rgb(plane.texture);
                    let uv_rect = self.texture_resolver.get_uv_rect(&textures.input.colors[0], plane.uv_rect);
                    let instance = CompositeInstance::new_rgb(
                        surface_rect.to_f32(),
                        surface_rect.to_f32(),
                        PremultipliedColorF::WHITE,
                        uv_rect,
                        plane.texture.uses_normalized_uvs(),
                        (false, false),
                        None,
                    );
                    let features = instance.get_rgb_features();

                    self.bind_composite_shader(
                        &projection,
                        None,
                        CompositeSurfaceFormat::Rgba,
                        surface.image_buffer_kind,
                        features,
                    );

                    ( textures, instance )
                },
            };

            self.draw_instanced_batch(
                &[instance],
                VertexArrayKind::Composite,
                &textures,
                &mut results.stats,
            );

            self.compositor_config
                .compositor()
                .unwrap()
                .unbind(self.device.as_mut().unwrap());
        }

        self.gpu_profiler.finish_sampler(opaque_sampler);
    }

    /// Draw a list of tiles to the framebuffer
    fn draw_tile_list<'a, I: Iterator<Item = &'a occlusion::Item<OcclusionItemKey>>>(
        &mut self,
        tiles_iter: I,
        composite_state: &CompositeState,
        external_surfaces: &[ResolvedExternalSurface],
        projection: &default::Transform3D<f32>,
        stats: &mut RendererStats,
    ) {
        let mut batch = CompositeBatchState::new();

        self.bind_composite_shader(
            projection,
            None,
            batch.shader_params.0,
            batch.shader_params.1,
            batch.shader_params.2,
        );

        for item in tiles_iter {
            let tile = &composite_state.tiles[item.key.tile_index];
            let (instance, textures, shader_params) = self.build_composite_draw_item(
                tile,
                item.rectangle,
                item.key.needs_mask,
                composite_state,
                external_surfaces,
            );

            self.update_composite_batch_state(
                projection,
                &mut batch,
                textures,
                shader_params,
                stats,
            );

            // Add instance to current batch
            batch.instances.push(instance);
        }

        // Flush the last batch
        self.flush_composite_batch(
            &mut batch,
            stats,
        );
    }

    // Composite tiles in a swapchain. When using LayerCompositor, we may
    // split the compositing in to multiple swapchains.
    fn composite_pass(
        &mut self,
        composite_state: &CompositeState,
        draw_target: DrawTarget,
        clear_color: ColorF,
        projection: &default::Transform3D<f32>,
        results: &mut RenderResults,
        partial_present_mode: Option<PartialPresentMode>,
        layer: &SwapChainLayer,
    ) {
        self.begin_composite_pass(
            draw_target,
            clear_color,
            partial_present_mode,
            layer,
        );

        // Draw opaque tiles
        let opaque_items = layer.occlusion.opaque_items();
        if !opaque_items.is_empty() {
            self.draw_composite_tile_group(
                GPU_SAMPLER_TAG_OPAQUE,
                false,
                |_| {},
                |renderer| {
                    renderer.draw_tile_list(
                        opaque_items.iter(),
                        &composite_state,
                        &composite_state.external_surfaces,
                        projection,
                        &mut results.stats,
                    );
                },
            );
        }

        // Draw clear tiles
        if !layer.clear_tiles.is_empty() {
            self.draw_composite_tile_group(
                GPU_SAMPLER_TAG_TRANSPARENT,
                true,
                |renderer| renderer.device.as_mut().unwrap().set_blend_mode_premultiplied_dest_out(),
                |renderer| {
                    renderer.draw_tile_list(
                        layer.clear_tiles.iter(),
                        &composite_state,
                        &composite_state.external_surfaces,
                        projection,
                        &mut results.stats,
                    );
                },
            );
        }

        // Draw alpha tiles
        let alpha_items = layer.occlusion.alpha_items();
        if !alpha_items.is_empty() {
            self.draw_composite_tile_group(
                GPU_SAMPLER_TAG_TRANSPARENT,
                true,
                |renderer| renderer.set_blend_mode_premultiplied_alpha(FramebufferKind::Main),
                |renderer| {
                    renderer.draw_tile_list(
                        alpha_items.iter().rev(),
                        &composite_state,
                        &composite_state.external_surfaces,
                        projection,
                        &mut results.stats,
                    );
                },
            );
        }
    }

    fn begin_composite_pass(
        &mut self,
        draw_target: DrawTarget,
        clear_color: ColorF,
        partial_present_mode: Option<PartialPresentMode>,
        layer: &SwapChainLayer,
    ) {
        self.device.as_mut().unwrap().bind_draw_target(draw_target);
        self.device.as_mut().unwrap().disable_depth_write();
        self.device.as_mut().unwrap().disable_depth();

        // If using KHR_partial_update, call eglSetDamageRegion.
        // This must be called exactly once per frame, and prior to any rendering to the main
        // framebuffer. Additionally, on Mali-G77 we encountered rendering issues when calling
        // this earlier in the frame, during offscreen render passes. So call it now, immediately
        // before rendering to the main framebuffer. See bug 1685276 for details.
        if let Some(partial_present) = self.compositor_config.partial_present() {
            if let Some(PartialPresentMode::Single { dirty_rect }) = partial_present_mode {
                partial_present.set_buffer_damage_region(&[dirty_rect.to_i32()]);
            }
        }

        // Clear the framebuffer
        let clear_color = Some(clear_color.to_array());

        match partial_present_mode {
            Some(PartialPresentMode::Single { dirty_rect }) => {
                // There is no need to clear if the dirty rect is occluded. Additionally,
                // on Mali-G77 we have observed artefacts when calling glClear (even with
                // the empty scissor rect set) after calling eglSetDamageRegion with an
                // empty damage region. So avoid clearing in that case. See bug 1709548.
                if !dirty_rect.is_empty() && layer.occlusion.test(&dirty_rect) {
                    // We have a single dirty rect, so clear only that
                    self.device.as_mut().unwrap().clear_target(clear_color,
                                             None,
                                             Some(draw_target.to_framebuffer_rect(dirty_rect.to_i32())));
                }
            }
            None => {
                // Partial present is disabled, so clear the entire framebuffer
                self.device.as_mut().unwrap().clear_target(clear_color,
                                         None,
                                         None);
            }
        }
    }

    fn draw_composite_tile_group<FB, FD>(
        &mut self,
        sampler_tag: GpuProfileTag,
        blend: bool,
        configure_blend: FB,
        draw_tiles: FD,
    )
    where
        FB: FnOnce(&mut Self),
        FD: FnOnce(&mut Self),
    {
        let sampler = self.gpu_profiler.start_sampler(sampler_tag);
        self.set_blend(blend, FramebufferKind::Main);
        configure_blend(self);
        draw_tiles(self);
        self.gpu_profiler.finish_sampler(sampler);
    }

    /// Composite picture cache tiles into the framebuffer. This is currently
    /// the only way that picture cache tiles get drawn. In future, the tiles
    /// will often be handed to the OS compositor, and this method will be
    /// rarely used.
    fn composite_simple(
        &mut self,
        composite_state: &CompositeState,
        frame_device_size: DeviceIntSize,
        fb_draw_target: DrawTarget,
        projection: &default::Transform3D<f32>,
        results: &mut RenderResults,
        partial_present_mode: Option<PartialPresentMode>,
        device_size: DeviceIntSize,
    ) {
        let _gm = self.gpu_profiler.start_marker("framebuffer");
        let _timer = self.gpu_profiler.start_timer(GPU_TAG_COMPOSITE);

        // We are only interested in tiles backed with actual cached pixels so we don't
        // count clear tiles here.
        let num_tiles = composite_state.tiles
            .iter()
            .filter(|tile| tile.kind != TileKind::Clear).count();
        self.profile.set(profiler::PICTURE_TILES, num_tiles);

        let (window_is_opaque, enable_screenshot)  = match self.compositor_config.layer_compositor() {
            Some(ref compositor) => {
                let props = compositor.get_window_properties();
                (props.is_opaque, props.enable_screenshot)
            }
            None => (true, true)
        };

        let mut input_layers: Vec<CompositorInputLayer> = Vec::new();
        let mut swapchain_layers = Vec::new();
        let cap = composite_state.tiles.len();
        let mut segment_builder = SegmentBuilder::new();
        let mut tile_index_to_layer_index = vec![None; composite_state.tiles.len()];
        let mut full_render_occlusion = occlusion::FrontToBackBuilder::with_capacity(cap, cap);
        let mut layer_compositor_frame_state = LayerCompositorFrameState{
            tile_states: FastHashMap::default(),
            rects_without_id: Vec::new(),
        };

        // Calculate layers with full device rect

        // Add a debug overlay request if enabled
        if self.debug_overlay_state.is_enabled {
            self.debug_overlay_state.layer_index = input_layers.len();

            input_layers.push(CompositorInputLayer {
                usage: CompositorSurfaceUsage::DebugOverlay,
                is_opaque: false,
                offset: DeviceIntPoint::zero(),
                clip_rect: device_size.into(),
            });

            swapchain_layers.push(SwapChainLayer {
                clear_tiles: Vec::new(),
                occlusion: occlusion::FrontToBackBuilder::with_capacity(cap, cap),
            });
        }

        // NOTE: Tiles here are being iterated in front-to-back order by
        //       z-id, due to the sort in composite_state.end_frame()
        for (idx, tile) in composite_state.tiles.iter().enumerate() {
            let device_tile_box = composite_state.get_device_rect(
                &tile.local_rect,
                tile.transform_index
            );

            if let Some(ref _compositor) = self.compositor_config.layer_compositor() {
                match tile.tile_id {
                    Some(tile_id) => {
                        layer_compositor_frame_state.
                            tile_states
                            .insert(
                            tile_id,
                            CompositeTileState {
                                local_rect: tile.local_rect,
                                local_valid_rect: tile.local_valid_rect,
                                device_clip_rect: tile.device_clip_rect,
                                z_id: tile.z_id,
                                device_tile_box: device_tile_box,
                                visible_rects: Vec::new(),
                            },
                        );
                    }
                    None => {}
                }
            }

            // Simple compositor needs the valid rect in device space to match clip rect
            let device_valid_rect = composite_state
                .get_device_rect(&tile.local_valid_rect, tile.transform_index);

            let rect = device_tile_box
                .intersection_unchecked(&tile.device_clip_rect)
                .intersection_unchecked(&device_valid_rect);

            if rect.is_empty() {
                continue;
            }

            // Determine if the tile is an external surface or content
            let usage = match tile.surface {
                CompositeTileSurface::Texture { .. } |
                CompositeTileSurface::Color { .. } |
                CompositeTileSurface::Clear => {
                    CompositorSurfaceUsage::Content
                }
                CompositeTileSurface::ExternalSurface { external_surface_index } => {
                    match (self.current_compositor_kind, enable_screenshot) {
                        (CompositorKind::Native { .. }, _) | (CompositorKind::Draw { .. }, _) => {
                            CompositorSurfaceUsage::Content
                        }
                        (CompositorKind::Layer { .. }, true) => {
                            CompositorSurfaceUsage::Content
                        }
                        (CompositorKind::Layer { .. }, false) => {
                            let surface = &composite_state.external_surfaces[external_surface_index.0];

                            // TODO(gwc): For now, we only select a hardware overlay swapchain if we
                            // have an external image, but it may make sense to do for compositor
                            // surfaces without in future.
                            match surface.external_image_id {
                                Some(external_image_id) => {
                                    let image_key = match surface.color_data {
                                        ResolvedExternalSurfaceColorData::Rgb { image_dependency, .. } => image_dependency.key,
                                        ResolvedExternalSurfaceColorData::Yuv { image_dependencies, .. } => image_dependencies[0].key,
                                    };

                                    CompositorSurfaceUsage::External {
                                        image_key,
                                        external_image_id,
                                        transform_index: tile.transform_index,
                                    }
                                }
                                None => {
                                    CompositorSurfaceUsage::Content
                                }
                            }
                        }
                    }
                }
            };

            if let Some(ref _compositor) = self.compositor_config.layer_compositor() {
                if let CompositeTileSurface::ExternalSurface { .. } = tile.surface {
                    assert!(tile.tile_id.is_none());
                    // ExternalSurface is not promoted to external composite.
                    if let CompositorSurfaceUsage::Content = usage {
                        layer_compositor_frame_state.rects_without_id.push(rect);
                    }
                } else {
                    assert!(tile.tile_id.is_some());
                }
            }

            // Determine whether we need a new layer, and if so, what kind
            let new_layer_kind = match input_layers.last() {
                Some(curr_layer) => {
                    match (curr_layer.usage, usage) {
                        // Content -> content, composite in to same layer
                        (CompositorSurfaceUsage::Content, CompositorSurfaceUsage::Content) => None,
                        (CompositorSurfaceUsage::External { .. }, CompositorSurfaceUsage::Content) => Some(usage),

                        // Switch of layer type, or video -> video, need new swapchain
                        (CompositorSurfaceUsage::Content, CompositorSurfaceUsage::External { .. }) |
                        (CompositorSurfaceUsage::External { .. }, CompositorSurfaceUsage::External { .. }) => {
                            // Only create a new layer if we're using LayerCompositor
                            match self.compositor_config {
                                CompositorConfig::Draw { .. } | CompositorConfig::Native { .. } => None,
                                CompositorConfig::Layer { .. } => {
                                    Some(usage)
                                }
                            }
                        }
                        (CompositorSurfaceUsage::DebugOverlay, _) => {
                            Some(usage)
                        }
                        // Should not encounter debug layers as new layer
                        (_, CompositorSurfaceUsage::DebugOverlay) => {
                            unreachable!();
                        }
                    }
                }
                None => {
                    // No layers yet, so we need a new one
                    Some(usage)
                }
            };

            if let Some(new_layer_kind) = new_layer_kind {
                let (offset, clip_rect, is_opaque) = match usage {
                    CompositorSurfaceUsage::Content => {
                        (
                            DeviceIntPoint::zero(),
                            device_size.into(),
                            false,      // Assume not opaque, we'll calculate this later
                        )
                    }
                    CompositorSurfaceUsage::External { .. } => {
                        let rect = composite_state.get_device_rect(
                            &tile.local_rect,
                            tile.transform_index
                        );

                        let clip_rect = tile.device_clip_rect.to_i32();
                        let is_opaque = tile.kind != TileKind::Alpha;

                        (rect.min.to_i32(), clip_rect, is_opaque)
                    }
                    CompositorSurfaceUsage::DebugOverlay => unreachable!(),
                };

                input_layers.push(CompositorInputLayer {
                    usage: new_layer_kind,
                    is_opaque,
                    offset,
                    clip_rect,
                });

                swapchain_layers.push(SwapChainLayer {
                    clear_tiles: Vec::new(),
                    occlusion: occlusion::FrontToBackBuilder::with_capacity(cap, cap),
                })
            }
            tile_index_to_layer_index[idx] = Some(input_layers.len() - 1);

            // Caluclate actual visible tile's rects

            match tile.kind {
                TileKind::Opaque | TileKind::Alpha => {
                    let is_opaque = tile.kind != TileKind::Alpha;

                    match tile.clip_index {
                        Some(clip_index) => {
                            let clip = composite_state.get_compositor_clip(clip_index);

                            // TODO(gw): Make segment builder generic on unit to avoid casts below.
                            segment_builder.initialize(
                                rect.cast_unit(),
                                None,
                                rect.cast_unit(),
                            );
                            segment_builder.push_clip_rect(
                                clip.rect.cast_unit(),
                                Some(clip.radius),
                                ClipMode::Clip,
                            );
                            segment_builder.build(|segment| {
                                let key = OcclusionItemKey { tile_index: idx, needs_mask: segment.has_mask };

                                full_render_occlusion.add(
                                    &segment.rect.cast_unit(),
                                    is_opaque && !segment.has_mask,
                                    key,
                                );
                            });
                        }
                        None => {
                            full_render_occlusion.add(&rect, is_opaque, OcclusionItemKey {
                                tile_index: idx,
                                needs_mask: false,
                            });
                        }
                    }
                }
                TileKind::Clear => {}
            }
        }

        assert_eq!(swapchain_layers.len(), input_layers.len());

        if window_is_opaque {
            match input_layers.last_mut() {
                Some(_layer) => {
                    // If the window is opaque, and the last(back) layer is
                    //  a content layer then mark that as opaque.
                    // TODO: This causes talos performance regressions.
                    // if let CompositorSurfaceUsage::Content = layer.usage {
                    //     layer.is_opaque = true;
                    // }
                }
                None => {
                    // If no tiles were present, and we expect an opaque window,
                    // add an empty layer to force a composite that clears the screen,
                    // to match existing semantics.
                    input_layers.push(CompositorInputLayer {
                        usage: CompositorSurfaceUsage::Content,
                        is_opaque: true,
                        offset: DeviceIntPoint::zero(),
                        clip_rect: device_size.into(),
                    });

                    swapchain_layers.push(SwapChainLayer {
                        clear_tiles: Vec::new(),
                        occlusion: occlusion::FrontToBackBuilder::with_capacity(cap, cap),
                    });
                }
            }
        }

        let mut full_render = false;

        // Start compositing if using OS compositor
        if let Some(ref mut compositor) = self.compositor_config.layer_compositor() {
            let input = CompositorInputConfig {
                enable_screenshot,
                layers: &input_layers,
            };
            full_render = compositor.begin_frame(&input);
        }

        // Full render is requested when layer tree is updated.
        let mut partial_present_mode = if full_render {
            None
        } else {
            partial_present_mode
        };

        assert_eq!(swapchain_layers.len(), input_layers.len());

        // Recalculate dirty rect for layer compositor
        if let Some(ref _compositor) = self.compositor_config.layer_compositor() {
            // Set visible rests of current frame to each tile's CompositeTileState.
            for item in full_render_occlusion
            .opaque_items()
            .iter()
            .chain(full_render_occlusion.alpha_items().iter()) {
                let tile = &composite_state.tiles[item.key.tile_index];
                match tile.tile_id {
                    Some(tile_id) => {
                        if let Some(tile_state) = layer_compositor_frame_state.tile_states.get_mut(&tile_id) {
                            tile_state.visible_rects.push(item.rectangle);
                        } else {
                            unreachable!();
                        }
                    }
                    None => {}
                }
            }

            let can_use_partial_present =
                !self.force_redraw && !full_render &&
                self.layer_compositor_frame_state_in_prev_frame.is_some();

            if can_use_partial_present {
                let mut combined_dirty_rect = DeviceRect::zero();

                for tile in composite_state.tiles.iter() {
                    if tile.kind == TileKind::Clear {
                        continue;
                    }

                    if tile.tile_id.is_none() {
                        match tile.surface {
                            CompositeTileSurface::ExternalSurface { .. } => {}
                            CompositeTileSurface::Texture { .. }  |
                            CompositeTileSurface::Color { .. } |
                            CompositeTileSurface::Clear => {
                                unreachable!();
                            },
                        }
                        continue;
                    }

                    assert!(tile.tile_id.is_some());

                    let tiles_exists_in_prev_frame =
                        self.layer_compositor_frame_state_in_prev_frame
                        .as_ref()
                        .unwrap()
                        .tile_states
                        .contains_key(&tile.tile_id.unwrap());
                    let tile_id = tile.tile_id.unwrap();
                    let tile_state = layer_compositor_frame_state.tile_states.get(&tile_id).unwrap();

                    if tiles_exists_in_prev_frame {
                        let prev_tile_state = self.layer_compositor_frame_state_in_prev_frame
                            .as_ref()
                            .unwrap()
                            .tile_states
                            .get(&tile_id)
                            .unwrap();

                        if tile_state.same_state(prev_tile_state) {
                            // Case that tile is same state in previous frame and current frame.
                            // Intersection of tile's dirty rect and tile's visible rects are actual dirty rects.
                            let dirty_rect = composite_state.get_device_rect(
                                &tile.local_dirty_rect,
                                tile.transform_index,
                            );
                            for rect in tile_state.visible_rects.iter()  {
                                let visible_dirty_rect = rect.intersection(&dirty_rect);
                                if visible_dirty_rect.is_some() {
                                    combined_dirty_rect = combined_dirty_rect.union(&visible_dirty_rect.unwrap());
                                }
                            }
                        } else {
                            // If tile is rendered in previous frame, but its state is different,
                            // both visible rects in previous frame and current frame are dirty rects.
                            for rect in tile_state.visible_rects
                                .iter()
                                .chain(prev_tile_state.visible_rects.iter())  {
                                combined_dirty_rect = combined_dirty_rect.union(&rect);
                            }
                        }
                    } else {
                        // If tile is not rendered in previous frame, its all visible rects are dirty rects.
                        for rect in &tile_state.visible_rects {
                            combined_dirty_rect = combined_dirty_rect.union(&rect);
                        }
                    }
                }

                // Case that tile is rendered in pervious frame, but not in current frame.
                for (tile_id, tile_state) in self.layer_compositor_frame_state_in_prev_frame
                    .as_ref()
                    .unwrap()
                    .tile_states
                    .iter() {
                    if !layer_compositor_frame_state.tile_states.contains_key(&tile_id) {
                        for rect in tile_state.visible_rects.iter()  {
                            combined_dirty_rect = combined_dirty_rect.union(&rect);
                        }
                    }
                }

                // Case that ExternalSurface is not promoted to external composite.
                for rect in layer_compositor_frame_state
                    .rects_without_id
                    .iter()
                    .chain(self.layer_compositor_frame_state_in_prev_frame.as_ref().unwrap().rects_without_id.iter())  {
                    combined_dirty_rect = combined_dirty_rect.union(&rect);
                }

                partial_present_mode = Some(PartialPresentMode::Single {
                    dirty_rect: combined_dirty_rect,
                });
            } else {
                partial_present_mode = None;
            }

            self.layer_compositor_frame_state_in_prev_frame = Some(layer_compositor_frame_state);
        }

        // Check tiles handling with partial_present_mode

        // NOTE: Tiles here are being iterated in front-to-back order by
        //       z-id, due to the sort in composite_state.end_frame()
        for (idx, tile) in composite_state.tiles.iter().enumerate() {
            let device_tile_box = composite_state.get_device_rect(
                &tile.local_rect,
                tile.transform_index
            );

            // Determine a clip rect to apply to this tile, depending on what
            // the partial present mode is.
            let partial_clip_rect = match partial_present_mode {
                Some(PartialPresentMode::Single { dirty_rect }) => dirty_rect,
                None => device_tile_box,
            };

            // Simple compositor needs the valid rect in device space to match clip rect
            let device_valid_rect = composite_state
                .get_device_rect(&tile.local_valid_rect, tile.transform_index);

            let rect = device_tile_box
                .intersection_unchecked(&tile.device_clip_rect)
                .intersection_unchecked(&partial_clip_rect)
                .intersection_unchecked(&device_valid_rect);

            if rect.is_empty() {
                continue;
            }

            let layer_index = match tile_index_to_layer_index[idx] {
                None => {
                    // The rect of partial present should be subset of the rect of full render.
                    error!("rect {:?} should have valid layer index", rect);
                    continue;
                }
                Some(layer_index) => layer_index,
            };

            // For normal tiles, add to occlusion tracker. For clear tiles, add directly
            // to the swapchain tile list
            let layer = &mut swapchain_layers[layer_index];

            // Clear tiles overwrite whatever is under them, so they are treated as opaque.
            match tile.kind {
                TileKind::Opaque | TileKind::Alpha => {
                    let is_opaque = tile.kind != TileKind::Alpha;

                    match tile.clip_index {
                        Some(clip_index) => {
                            let clip = composite_state.get_compositor_clip(clip_index);

                                // TODO(gw): Make segment builder generic on unit to avoid casts below.
                            segment_builder.initialize(
                                rect.cast_unit(),
                                None,
                                rect.cast_unit(),
                            );
                            segment_builder.push_clip_rect(
                                clip.rect.cast_unit(),
                                Some(clip.radius),
                                ClipMode::Clip,
                            );
                            segment_builder.build(|segment| {
                                let key = OcclusionItemKey { tile_index: idx, needs_mask: segment.has_mask };

                                layer.occlusion.add(
                                    &segment.rect.cast_unit(),
                                    is_opaque && !segment.has_mask,
                                    key,
                                );
                            });
                        }
                        None => {
                            layer.occlusion.add(&rect, is_opaque, OcclusionItemKey {
                                tile_index: idx,
                                needs_mask: false,
                            });
                        }
                    }
                }
                TileKind::Clear => {
                    // Clear tiles are specific to how we render the window buttons on
                    // Windows 8. They clobber what's under them so they can be treated as opaque,
                    // but require a different blend state so they will be rendered after the opaque
                    // tiles and before transparent ones.
                    layer.clear_tiles.push(occlusion::Item { rectangle: rect, key: OcclusionItemKey { tile_index: idx, needs_mask: false } });
                }
            }
        }

        assert_eq!(swapchain_layers.len(), input_layers.len());
        let mut content_clear_color = Some(self.clear_color);

        for (layer_index, (layer, swapchain_layer)) in input_layers.iter().zip(swapchain_layers.iter()).enumerate() {
            self.device.as_mut().unwrap().reset_state();

            // Skip compositing external images or debug layers here
            match layer.usage {
                CompositorSurfaceUsage::Content => {}
                CompositorSurfaceUsage::External { .. } | CompositorSurfaceUsage::DebugOverlay => {
                    continue;
                }
            }

            // Only use supplied clear color for first content layer we encounter
            let clear_color = content_clear_color.take().unwrap_or(ColorF::TRANSPARENT);

            if let Some(ref mut _compositor) = self.compositor_config.layer_compositor() {
                if let Some(PartialPresentMode::Single { dirty_rect }) = partial_present_mode {
                    if dirty_rect.is_empty() {
                        continue;
                    }
                }
            }

            let draw_target = match self.compositor_config {
                CompositorConfig::Layer { ref mut compositor } => {
                    match partial_present_mode {
                        Some(PartialPresentMode::Single { dirty_rect }) => {
                            compositor.bind_layer(layer_index, &[dirty_rect.to_i32()]);
                        }
                        None => {
                            compositor.bind_layer(layer_index, &[]);
                        }
                    };

                    DrawTarget::NativeSurface {
                        offset: -layer.offset,
                        external_fbo_id: 0,
                        dimensions: frame_device_size,
                    }
                }
                // Native can be hit when switching compositors (disable when using Layer)
                CompositorConfig::Draw { .. } | CompositorConfig::Native { .. } => {
                    fb_draw_target
                }
            };

            // TODO(gwc): When supporting external attached swapchains, need to skip the composite pass here

            // Draw each compositing pass in to a swap chain
            self.composite_pass(
                composite_state,
                draw_target,
                clear_color,
                projection,
                results,
                partial_present_mode,
                swapchain_layer,
            );

            if let Some(ref mut compositor) = self.compositor_config.layer_compositor() {
                match partial_present_mode {
                    Some(PartialPresentMode::Single { dirty_rect }) => {
                        compositor.present_layer(layer_index, &[dirty_rect.to_i32()]);
                    }
                    None => {
                        compositor.present_layer(layer_index, &[]);
                    }
                };
            }
        }

        // End frame notify for experimental compositor
        if let Some(ref mut compositor) = self.compositor_config.layer_compositor() {
            for (layer_index, layer) in input_layers.iter().enumerate() {
                // External surfaces need transform applied, but content
                // surfaces are always at identity
                let transform = match layer.usage {
                    CompositorSurfaceUsage::Content => CompositorSurfaceTransform::identity(),
                    CompositorSurfaceUsage::External { transform_index, .. } => composite_state.get_compositor_transform(transform_index),
                    CompositorSurfaceUsage::DebugOverlay => CompositorSurfaceTransform::identity(),
                };

                compositor.add_surface(
                    layer_index,
                    transform,
                    layer.clip_rect,
                    ImageRendering::Auto,
                );
            }
        }
    }

    fn clear_render_target(
        &mut self,
        target: &RenderTarget,
        draw_target: DrawTarget,
        framebuffer_kind: FramebufferKind,
        projection: &default::Transform3D<f32>,
        stats: &mut RendererStats,
    ) {
        let needs_depth = target.needs_depth();

        let clear_depth = if needs_depth {
            Some(1.0)
        } else {
            None
        };

        let _timer = self.gpu_profiler.start_timer(GPU_TAG_SETUP_TARGET);

        self.device.as_mut().unwrap().disable_depth();
        self.set_blend(false, framebuffer_kind);

        let is_alpha = target.target_kind == RenderTargetKind::Alpha;
        let require_precise_clear = target.cached;

        // On some Mali-T devices we have observed crashes in subsequent draw calls
        // immediately after clearing the alpha render target regions with glClear().
        // Using the shader to clear the regions avoids the crash. See bug 1638593.
        let clear_with_quads = (target.cached && self.clear_caches_with_quads)
            || (is_alpha && self.clear_alpha_targets_with_quads);

        let favor_partial_updates = self.device.as_mut().unwrap().get_capabilities().supports_render_target_partial_update
            && self.enable_clear_scissor;

        // On some Adreno 4xx devices we have seen render tasks to alpha targets have no
        // effect unless the target is fully cleared prior to rendering. See bug 1714227.
        let full_clears_on_adreno = is_alpha && self.device.as_mut().unwrap().get_capabilities().requires_alpha_target_full_clear;
        let require_full_clear = !require_precise_clear
            && (full_clears_on_adreno || !favor_partial_updates);

        let clear_color = target
            .clear_color
            .map(|color| color.to_array());

        let mut cleared_depth = false;
        if clear_with_quads {
            // Will be handled last. Only specific rects will be cleared.
        } else if require_precise_clear {
            // Only clear specific rects
            for (rect, color) in &target.clears {
                self.device.as_mut().unwrap().clear_target(
                    Some(color.to_array()),
                    None,
                    Some(draw_target.to_framebuffer_rect(*rect)),
                );
            }
        } else {
            // At this point we know we don't require precise clears for correctness.
            // We may still attempt to restruct the clear rect as an optimization on
            // some configurations.
            let clear_rect = if require_full_clear {
                None
            } else {
                match draw_target {
                    DrawTarget::Default { rect, total_size, .. } => {
                        if rect.min == FramebufferIntPoint::zero() && rect.size() == total_size {
                            // Whole screen is covered, no need for scissor
                            None
                        } else {
                            Some(rect)
                        }
                    }
                    DrawTarget::Texture { .. } => {
                        // TODO(gw): Applying a scissor rect and minimal clear here
                        // is a very large performance win on the Intel and nVidia
                        // GPUs that I have tested with. It's possible it may be a
                        // performance penalty on other GPU types - we should test this
                        // and consider different code paths.
                        //
                        // Note: The above measurements were taken when render
                        // target slices were minimum 2048x2048. Now that we size
                        // them adaptively, this may be less of a win (except perhaps
                        // on a mostly-unused last slice of a large texture array).
                        target.used_rect.map(|rect| draw_target.to_framebuffer_rect(rect))
                    }
                    // Full clear.
                    _ => None,
                }
            };

            self.device.as_mut().unwrap().clear_target(
                clear_color,
                clear_depth,
                clear_rect,
            );
            cleared_depth = true;
        }

        // Make sure to clear the depth buffer if it is used.
        if needs_depth && !cleared_depth {
            // TODO: We could also clear the depth buffer via ps_clear. This
            // is done by picture cache targets in some cases.
            self.device.as_mut().unwrap().clear_target(None, clear_depth, None);
        }

        // Finally, if we decided to clear with quads or if we need to clear
        // some areas with specific colors that don't match the global clear
        // color, clear more areas using a draw call.

        let mut clear_instances = Vec::with_capacity(target.clears.len());
        for (rect, color) in &target.clears {
            if clear_with_quads || (!require_precise_clear && target.clear_color != Some(*color)) {
                let rect = rect.to_f32();
                clear_instances.push(ClearInstance {
                    rect: [
                        rect.min.x, rect.min.y,
                        rect.max.x, rect.max.y,
                    ],
                    color: color.to_array(),
                })
            }
        }

        if !clear_instances.is_empty() {
            self.shaders.as_ref().unwrap().borrow_mut().ps_clear().bind(
                self.device.as_mut().unwrap(),
                &projection,
                None,
                &mut self.renderer_errors,
                &mut self.profile,
            );
            self.draw_instanced_batch(
                &clear_instances,
                VertexArrayKind::Clear,
                &BatchTextures::empty(),
                stats,
            );
        }
    }

    fn draw_render_target(
        &mut self,
        texture_id: CacheTextureId,
        target: &RenderTarget,
        render_tasks: &RenderTaskGraph,
        stats: &mut RendererStats,
    ) {
        let needs_depth = target.needs_depth();

        let texture = self.texture_resolver.get_cache_texture_mut(&texture_id);
        if needs_depth {
            self.device.as_mut().unwrap().reuse_render_target::<u8>(
                texture,
                RenderTargetInfo { has_depth: needs_depth },
            );
        }

        let draw_target = DrawTarget::from_texture(
            texture,
            needs_depth,
        );

        let projection = Transform3D::ortho(
            0.0,
            draw_target.dimensions().width as f32,
            0.0,
            draw_target.dimensions().height as f32,
            self.device.as_mut().unwrap().ortho_near_plane(),
            self.device.as_mut().unwrap().ortho_far_plane(),
        );

        profile_scope!("draw_render_target");
        let _gm = self.gpu_profiler.start_marker("render target");

        let counter = match target.target_kind {
            RenderTargetKind::Color => profiler::COLOR_PASSES,
            RenderTargetKind::Alpha => profiler::ALPHA_PASSES,
        };
        self.profile.inc(counter);

        let sampler_query = match target.target_kind {
            RenderTargetKind::Color => None,
            RenderTargetKind::Alpha => Some(self.gpu_profiler.start_sampler(GPU_SAMPLER_TAG_ALPHA)),
        };

        // sanity check for the depth buffer
        if let DrawTarget::Texture { with_depth, .. } = draw_target {
            assert!(with_depth >= target.needs_depth());
        }

        let preserve_mask = match target.clear_color {
            Some(_) => 0,
            None => gl::COLOR_BUFFER_BIT0_QCOM,
        };
        let framebuffer_kind = self.begin_draw_target_pass(
            draw_target,
            needs_depth,
            target.used_rect,
            preserve_mask,
        );

        self.clear_render_target(
            target,
            draw_target,
            framebuffer_kind,
            &projection,
            stats,
        );

        if needs_depth {
            self.device.as_mut().unwrap().disable_depth_write();
        }

        // Handle any resolves from parent pictures to this target
        self.handle_resolves(
            &target.resolve_ops,
            render_tasks,
            draw_target,
        );

        // Handle any blits from the texture cache to this target.
        self.handle_blits(
            &target.blits,
            render_tasks,
            draw_target,
        );

        // Draw any borders for this target.
        if !target.border_segments_solid.is_empty() ||
           !target.border_segments_complex.is_empty()
        {
            let _timer = self.gpu_profiler.start_timer(GPU_TAG_CACHE_BORDER);

            self.set_blend(true, FramebufferKind::Other);
            self.set_blend_mode_premultiplied_alpha(FramebufferKind::Other);

            if !target.border_segments_solid.is_empty() {
                self.bind_shader(&projection, None, |shaders| shaders.cs_border_solid());

                self.draw_instanced_batch(
                    &target.border_segments_solid,
                    VertexArrayKind::Border,
                    &BatchTextures::empty(),
                    stats,
                );
            }

            if !target.border_segments_complex.is_empty() {
                self.bind_shader(&projection, None, |shaders| shaders.cs_border_segment());

                self.draw_instanced_batch(
                    &target.border_segments_complex,
                    VertexArrayKind::Border,
                    &BatchTextures::empty(),
                    stats,
                );
            }

            self.set_blend(false, FramebufferKind::Other);
        }

        // Draw any line decorations for this target.
        if !target.line_decorations.is_empty() {
            let _timer = self.gpu_profiler.start_timer(GPU_TAG_CACHE_LINE_DECORATION);

            self.set_blend(true, FramebufferKind::Other);
            self.set_blend_mode_premultiplied_alpha(FramebufferKind::Other);

            self.bind_shader(&projection, None, |shaders| shaders.cs_line_decoration());

            self.draw_instanced_batch(
                &target.line_decorations,
                VertexArrayKind::LineDecoration,
                &BatchTextures::empty(),
                stats,
            );

            self.set_blend(false, FramebufferKind::Other);
        }

        // Draw any fast path linear gradients for this target.
        if !target.fast_linear_gradients.is_empty() {
            let _timer = self.gpu_profiler.start_timer(GPU_TAG_CACHE_FAST_LINEAR_GRADIENT);

            self.set_blend(false, FramebufferKind::Other);

            self.bind_shader(&projection, None, |shaders| shaders.cs_fast_linear_gradient());

            self.draw_instanced_batch(
                &target.fast_linear_gradients,
                VertexArrayKind::FastLinearGradient,
                &BatchTextures::empty(),
                stats,
            );
        }

        // Draw any linear gradients for this target.
        if !target.linear_gradients.is_empty() {
            let _timer = self.gpu_profiler.start_timer(GPU_TAG_CACHE_LINEAR_GRADIENT);

            self.set_blend(false, FramebufferKind::Other);

            self.bind_shader(&projection, None, |shaders| shaders.cs_linear_gradient());

            self.draw_instanced_batch(
                &target.linear_gradients,
                VertexArrayKind::LinearGradient,
                &BatchTextures::empty(),
                stats,
            );
        }

        // Draw any radial gradients for this target.
        if !target.radial_gradients.is_empty() {
            let _timer = self.gpu_profiler.start_timer(GPU_TAG_RADIAL_GRADIENT);

            self.set_blend(false, FramebufferKind::Other);

            self.bind_shader(&projection, None, |shaders| shaders.cs_radial_gradient());

            self.draw_instanced_batch(
                &target.radial_gradients,
                VertexArrayKind::RadialGradient,
                &BatchTextures::empty(),
                stats,
            );
        }

        // Draw any conic gradients for this target.
        if !target.conic_gradients.is_empty() {
            let _timer = self.gpu_profiler.start_timer(GPU_TAG_CONIC_GRADIENT);

            self.set_blend(false, FramebufferKind::Other);

            self.bind_shader(&projection, None, |shaders| shaders.cs_conic_gradient());

            self.draw_instanced_batch(
                &target.conic_gradients,
                VertexArrayKind::ConicGradient,
                &BatchTextures::empty(),
                stats,
            );
        }

        // Draw any blurs for this target.
        // Blurs are rendered as a standard 2-pass
        // separable implementation.
        // TODO(gw): In the future, consider having
        //           fast path blur shaders for common
        //           blur radii with fixed weights.
        if !target.vertical_blurs.is_empty() || !target.horizontal_blurs.is_empty() {
            let _timer = self.gpu_profiler.start_timer(GPU_TAG_BLUR);

            self.set_blend(false, framebuffer_kind);
            self.bind_shader(&projection, None, |shaders| shaders.cs_blur_rgba8());

            if !target.vertical_blurs.is_empty() {
                self.draw_blurs(
                    &target.vertical_blurs,
                    stats,
                );
            }

            if !target.horizontal_blurs.is_empty() {
                self.draw_blurs(
                    &target.horizontal_blurs,
                    stats,
                );
            }
        }

        self.handle_scaling(
            &target.scalings,
            &projection,
            stats,
        );

        for (ref textures, ref filters) in &target.svg_filters {
            self.handle_svg_filters(
                textures,
                filters,
                &projection,
                stats,
            );
        }

        for (ref textures, ref filters) in &target.svg_nodes {
            self.handle_svg_nodes(textures, filters, &projection, stats);
        }

        for alpha_batch_container in &target.alpha_batch_containers {
            self.draw_alpha_batch_container(
                alpha_batch_container,
                draw_target,
                framebuffer_kind,
                &projection,
                render_tasks,
                stats,
            );
        }

        self.handle_prims(
            &draw_target,
            &target.prim_instances,
            &target.prim_instances_with_scissor,
            &projection,
            stats,
        );

        // Draw the clip items into the tiled alpha mask.
        let has_primary_clips = !target.clip_batcher.primary_clips.is_empty();
        let has_secondary_clips = !target.clip_batcher.secondary_clips.is_empty();
        let has_clip_masks = !target.clip_masks.is_empty();
        if has_primary_clips | has_secondary_clips | has_clip_masks {
            let _timer = self.gpu_profiler.start_timer(GPU_TAG_CACHE_CLIP);

            // TODO(gw): Consider grouping multiple clip masks per shader
            //           invocation here to reduce memory bandwith further?

            if has_primary_clips {
                // Draw the primary clip mask - since this is the first mask
                // for the task, we can disable blending, knowing that it will
                // overwrite every pixel in the mask area.
                self.set_blend(false, FramebufferKind::Other);
                self.draw_clip_batch_list(
                    &target.clip_batcher.primary_clips,
                    &projection,
                    stats,
                );
            }

            if has_secondary_clips {
                // switch to multiplicative blending for secondary masks, using
                // multiplicative blending to accumulate clips into the mask.
                self.set_blend(true, FramebufferKind::Other);
                self.set_blend_mode_multiply(FramebufferKind::Other);
                self.draw_clip_batch_list(
                    &target.clip_batcher.secondary_clips,
                    &projection,
                    stats,
                );
            }

            if has_clip_masks {
                self.handle_clips(
                    &draw_target,
                    &target.clip_masks,
                    &projection,
                    stats,
                );
            }
        }

        self.end_draw_target_pass(needs_depth);

        if let Some(sampler) = sampler_query {
            self.gpu_profiler.finish_sampler(sampler);
        }
    }

    fn draw_blurs(
        &mut self,
        blurs: &FastHashMap<TextureSource, FrameVec<BlurInstance>>,
        stats: &mut RendererStats,
    ) {
        for (texture, blurs) in blurs {
            let textures = BatchTextures::composite_rgb(
                *texture,
            );

            self.draw_instanced_batch(
                blurs,
                VertexArrayKind::Blur,
                &textures,
                stats,
            );
        }
    }

    /// Draw all the instances in a clip batcher list to the current target.
    fn draw_clip_batch_list(
        &mut self,
        list: &ClipBatchList,
        projection: &default::Transform3D<f32>,
        stats: &mut RendererStats,
    ) {
        if self.debug_flags.contains(DebugFlags::DISABLE_CLIP_MASKS) {
            return;
        }

        // draw rounded cornered rectangles
        if !list.slow_rectangles.is_empty() {
            let _gm2 = self.gpu_profiler.start_marker("slow clip rectangles");
            self.bind_shader(projection, None, |shaders| shaders.cs_clip_rectangle_slow());
            self.draw_instanced_batch(
                &list.slow_rectangles,
                VertexArrayKind::ClipRect,
                &BatchTextures::empty(),
                stats,
            );
        }
        if !list.fast_rectangles.is_empty() {
            let _gm2 = self.gpu_profiler.start_marker("fast clip rectangles");
            self.bind_shader(projection, None, |shaders| shaders.cs_clip_rectangle_fast());
            self.draw_instanced_batch(
                &list.fast_rectangles,
                VertexArrayKind::ClipRect,
                &BatchTextures::empty(),
                stats,
            );
        }

        // draw box-shadow clips
        for (mask_texture_id, items) in list.box_shadows.iter() {
            let _gm2 = self.gpu_profiler.start_marker("box-shadows");
            let textures = BatchTextures::composite_rgb(*mask_texture_id);
            self.bind_shader(projection, None, |shaders| shaders.cs_clip_box_shadow());
            self.draw_instanced_batch(
                items,
                VertexArrayKind::ClipBoxShadow,
                &textures,
                stats,
            );
        }
    }

    fn update_deferred_resolves(&mut self, deferred_resolves: &[DeferredResolve]) -> Option<GpuCacheUpdateList> {
        // The first thing we do is run through any pending deferred
        // resolves, and use a callback to get the UV rect for this
        // custom item. Then we patch the resource_rects structure
        // here before it's uploaded to the GPU.
        if deferred_resolves.is_empty() {
            return None;
        }

        let handler = self.external_image_handler
            .as_mut()
            .expect("Found external image, but no handler set!");

        let mut list = GpuCacheUpdateList {
            frame_id: FrameId::INVALID,
            clear: false,
            height: self.gpu_cache_texture.get_height(),
            blocks: Vec::new(),
            updates: Vec::new(),
            debug_commands: Vec::new(),
        };

        for (i, deferred_resolve) in deferred_resolves.iter().enumerate() {
            self.gpu_profiler.place_marker("deferred resolve");
            let props = &deferred_resolve.image_properties;
            let ext_image = props
                .external_image
                .expect("BUG: Deferred resolves must be external images!");
            // Provide rendering information for NativeTexture external images.
            let image = handler.lock(ext_image.id, ext_image.channel_index, deferred_resolve.is_composited);
            let texture_target = match ext_image.image_type {
                ExternalImageType::TextureHandle(target) => target,
                ExternalImageType::Buffer => {
                    panic!("not a suitable image type in update_deferred_resolves()");
                }
            };

            // In order to produce the handle, the external image handler may call into
            // the GL context and change some states.
            self.device.as_mut().unwrap().reset_state();

            let texture = match image.source {
                ExternalImageSource::NativeTexture(texture_id) => {
                    ExternalTexture::new(
                        texture_id,
                        texture_target,
                        image.uv,
                        deferred_resolve.rendering,
                    )
                }
                ExternalImageSource::Invalid => {
                    warn!("Invalid ext-image");
                    debug!(
                        "For ext_id:{:?}, channel:{}.",
                        ext_image.id,
                        ext_image.channel_index
                    );
                    // Just use 0 as the gl handle for this failed case.
                    ExternalTexture::new(
                        0,
                        texture_target,
                        image.uv,
                        deferred_resolve.rendering,
                    )
                }
                ExternalImageSource::RawData(_) => {
                    panic!("Raw external data is not expected for deferred resolves!");
                }
            };

            self.texture_resolver
                .external_images
                .insert(DeferredResolveIndex(i as u32), texture);

            list.updates.push(GpuCacheUpdate::Copy {
                block_index: list.blocks.len(),
                block_count: BLOCKS_PER_UV_RECT,
                address: deferred_resolve.address,
            });
            list.blocks.push(image.uv.into());
            list.blocks.push([0f32; 4].into());
        }

        Some(list)
    }

    fn unlock_external_images(
        &mut self,
        deferred_resolves: &[DeferredResolve],
    ) {
        if !self.texture_resolver.external_images.is_empty() {
            let handler = self.external_image_handler
                .as_mut()
                .expect("Found external image, but no handler set!");

            for (index, _) in self.texture_resolver.external_images.drain() {
                let props = &deferred_resolves[index.0 as usize].image_properties;
                let ext_image = props
                    .external_image
                    .expect("BUG: Deferred resolves must be external images!");
                handler.unlock(ext_image.id, ext_image.channel_index);
            }
        }
    }

    /// Update the dirty rects based on current compositing mode and config
    // TODO(gw): This can be tidied up significantly once the Draw compositor
    //           is implemented in terms of the compositor trait.
    fn calculate_dirty_rects(
        &mut self,
        buffer_age: usize,
        composite_state: &CompositeState,
        draw_target_dimensions: DeviceIntSize,
        results: &mut RenderResults,
    ) -> Option<PartialPresentMode> {

        if let Some(ref _compositor) = self.compositor_config.layer_compositor() {
            // Calculate dirty rects of layer compositor in composite_simple()
            return None;
        }

        let mut partial_present_mode = None;

        let (max_partial_present_rects, draw_previous_partial_present_regions) = match self.current_compositor_kind {
            CompositorKind::Native { .. } => {
                // Assume that we can return a single dirty rect for native
                // compositor for now, and that there is no buffer-age functionality.
                // These params can be exposed by the compositor capabilities struct
                // as the Draw compositor is ported to use it.
                (1, false)
            }
            CompositorKind::Draw { draw_previous_partial_present_regions, max_partial_present_rects } => {
                (max_partial_present_rects, draw_previous_partial_present_regions)
            }
            CompositorKind::Layer { .. } => {
                unreachable!();
            }
        };

        if max_partial_present_rects > 0 {
            let prev_frames_damage_rect = if let Some(..) = self.compositor_config.partial_present() {
                self.buffer_damage_tracker
                    .get_damage_rect(buffer_age)
                    .or_else(|| Some(DeviceRect::from_size(draw_target_dimensions.to_f32())))
            } else {
                None
            };

            let can_use_partial_present =
                composite_state.dirty_rects_are_valid &&
                !self.force_redraw &&
                !(prev_frames_damage_rect.is_none() && draw_previous_partial_present_regions) &&
                !self.debug_overlay_state.is_enabled;

            if can_use_partial_present {
                let mut combined_dirty_rect = DeviceRect::zero();
                let fb_rect = DeviceRect::from_size(draw_target_dimensions.to_f32());

                // Work out how many dirty rects WR produced, and if that's more than
                // what the device supports.
                for tile in &composite_state.tiles {
                    if tile.kind == TileKind::Clear {
                        continue;
                    }
                    let dirty_rect = composite_state.get_device_rect(
                        &tile.local_dirty_rect,
                        tile.transform_index,
                    );

                    // In pathological cases where a tile is extremely zoomed, it
                    // may end up with device coords outside the range of an i32,
                    // so clamp it to the frame buffer rect here, before it gets
                    // casted to an i32 rect below.
                    if let Some(dirty_rect) = dirty_rect.intersection(&fb_rect) {
                        combined_dirty_rect = combined_dirty_rect.union(&dirty_rect);
                    }
                }

                let combined_dirty_rect = combined_dirty_rect.round();
                let combined_dirty_rect_i32 = combined_dirty_rect.to_i32();
                // Return this frame's dirty region. If nothing has changed, don't return any dirty
                // rects at all (the client can use this as a signal to skip present completely).
                if !combined_dirty_rect.is_empty() {
                    results.dirty_rects.push(combined_dirty_rect_i32);
                }

                // Track this frame's dirty region, for calculating subsequent frames' damage.
                if draw_previous_partial_present_regions {
                    self.buffer_damage_tracker.push_dirty_rect(&combined_dirty_rect);
                }

                // If the implementation requires manually keeping the buffer consistent,
                // then we must combine this frame's dirty region with that of previous frames
                // to determine the total_dirty_rect. The is used to determine what region we
                // render to, and is what we send to the compositor as the buffer damage region
                // (eg for KHR_partial_update).
                let total_dirty_rect = if draw_previous_partial_present_regions {
                    combined_dirty_rect.union(&prev_frames_damage_rect.unwrap())
                } else {
                    combined_dirty_rect
                };

                partial_present_mode = Some(PartialPresentMode::Single {
                    dirty_rect: total_dirty_rect,
                });
            } else {
                // If we don't have a valid partial present scenario, return a single
                // dirty rect to the client that covers the entire framebuffer.
                let fb_rect = DeviceIntRect::from_size(
                    draw_target_dimensions,
                );
                results.dirty_rects.push(fb_rect);

                if draw_previous_partial_present_regions {
                    self.buffer_damage_tracker.push_dirty_rect(&fb_rect.to_f32());
                }
            }
        }

        partial_present_mode
    }

    fn bind_frame_data(&mut self, frame: &mut Frame) {
        profile_scope!("bind_frame_data");

        let _timer = self.gpu_profiler.start_timer(GPU_TAG_SETUP_DATA);

        self.vertex_data_textures.bind_frame_data(
            self.device.as_mut().unwrap(),
            self.upload_state.gl_pools_mut().0,
            frame,
        );
    }

    fn update_native_surfaces(&mut self) {
        profile_scope!("update_native_surfaces");

        match self.compositor_config {
            CompositorConfig::Native { ref mut compositor, .. } => {
                for op in self.pending_native_surface_updates.drain(..) {
                    match op.details {
                        NativeSurfaceOperationDetails::CreateSurface { id, virtual_offset, tile_size, is_opaque } => {
                            let _inserted = self.allocated_native_surfaces.insert(id);
                            debug_assert!(_inserted, "bug: creating existing surface");
                            compositor.create_surface(
                                    self.device.as_mut().unwrap(),
                                    id,
                                    virtual_offset,
                                    tile_size,
                                    is_opaque,
                            );
                        }
                        NativeSurfaceOperationDetails::CreateExternalSurface { id, is_opaque } => {
                            let _inserted = self.allocated_native_surfaces.insert(id);
                            debug_assert!(_inserted, "bug: creating existing surface");
                            compositor.create_external_surface(
                                self.device.as_mut().unwrap(),
                                id,
                                is_opaque,
                            );
                        }
                        NativeSurfaceOperationDetails::CreateBackdropSurface { id, color } => {
                            let _inserted = self.allocated_native_surfaces.insert(id);
                            debug_assert!(_inserted, "bug: creating existing surface");
                            compositor.create_backdrop_surface(
                                self.device.as_mut().unwrap(),
                                id,
                                color,
                            );
                        }
                        NativeSurfaceOperationDetails::DestroySurface { id } => {
                            let _existed = self.allocated_native_surfaces.remove(&id);
                            debug_assert!(_existed, "bug: removing unknown surface");
                            compositor.destroy_surface(self.device.as_mut().unwrap(), id);
                        }
                        NativeSurfaceOperationDetails::CreateTile { id } => {
                            compositor.create_tile(self.device.as_mut().unwrap(), id);
                        }
                        NativeSurfaceOperationDetails::DestroyTile { id } => {
                            compositor.destroy_tile(self.device.as_mut().unwrap(), id);
                        }
                        NativeSurfaceOperationDetails::AttachExternalImage { id, external_image } => {
                            compositor.attach_external_image(self.device.as_mut().unwrap(), id, external_image);
                        }
                    }
                }
            }
            CompositorConfig::Draw { .. } | CompositorConfig::Layer { .. } => {
                // Ensure nothing is added in simple composite mode, since otherwise
                // memory will leak as this doesn't get drained
                debug_assert!(self.pending_native_surface_updates.is_empty());
            }
        }
    }

    fn draw_frame(
        &mut self,
        frame: &mut Frame,
        device_size: Option<DeviceIntSize>,
        buffer_age: usize,
        results: &mut RenderResults,
    ) {
        profile_scope!("draw_frame");

        // These markers seem to crash a lot on Android, see bug 1559834
        #[cfg(not(target_os = "android"))]
        let _gm = self.gpu_profiler.start_marker("draw frame");

        if frame.passes.is_empty() {
            frame.has_been_rendered = true;
            return;
        }

        self.device.as_mut().unwrap().disable_depth_write();
        self.set_blend(false, FramebufferKind::Other);
        self.device.as_mut().unwrap().disable_stencil();

        self.bind_frame_data(frame);

        // Upload experimental GPU buffer texture if there is any data present
        // TODO: Recycle these textures, upload via PBO or best approach for platform
        let gpu_buffer_texture_f = create_gpu_buffer_texture(self.device.as_mut().unwrap(), &frame.gpu_buffer_f);
        if let Some(ref texture) = gpu_buffer_texture_f {
            self.device.as_mut().unwrap()
                .bind_texture(TextureSampler::GpuBufferF, texture, Swizzle::default());
        }

        let gpu_buffer_texture_i = create_gpu_buffer_texture(self.device.as_mut().unwrap(), &frame.gpu_buffer_i);
        if let Some(ref texture) = gpu_buffer_texture_i {
            self.device.as_mut().unwrap()
                .bind_texture(TextureSampler::GpuBufferI, texture, Swizzle::default());
        }

        let bytes_to_mb = 1.0 / 1000000.0;
        let gpu_buffer_bytes_f = gpu_buffer_texture_f
            .as_ref()
            .map(|tex| tex.size_in_bytes())
            .unwrap_or(0);
        let gpu_buffer_bytes_i = gpu_buffer_texture_i
            .as_ref()
            .map(|tex| tex.size_in_bytes())
            .unwrap_or(0);
        let gpu_buffer_mb = (gpu_buffer_bytes_f + gpu_buffer_bytes_i) as f32 * bytes_to_mb;
        self.profile.set(profiler::GPU_BUFFER_MEM, gpu_buffer_mb);

        let gpu_cache_bytes = self.gpu_cache_texture.gpu_size_in_bytes();
        let gpu_cache_mb = gpu_cache_bytes as f32 * bytes_to_mb;
        self.profile.set(profiler::GPU_CACHE_MEM, gpu_cache_mb);

        // Determine the present mode and dirty rects, if device_size
        // is Some(..). If it's None, no composite will occur and only
        // picture cache and texture cache targets will be updated.
        // TODO(gw): Split Frame so that it's clearer when a composite
        //           is occurring.
        let present_mode = device_size.and_then(|device_size| {
            self.calculate_dirty_rects(
                buffer_age,
                &frame.composite_state,
                device_size,
                results,
            )
        });

        // If we have a native OS compositor, then make use of that interface to
        // specify how to composite each of the picture cache surfaces. First, we
        // need to find each tile that may be bound and updated later in the frame
        // and invalidate it so that the native render compositor knows that these
        // tiles can't be composited early. Next, after all such tiles have been
        // invalidated, then we queue surfaces for native composition by the render
        // compositor before we actually update the tiles. This allows the render
        // compositor to start early composition while the tiles are updating.
        if let CompositorKind::Native { .. } = self.current_compositor_kind {
            let compositor = self.compositor_config.compositor().unwrap();
            // Invalidate any native surface tiles that might be updated by passes.
            if !frame.has_been_rendered {
                for tile in &frame.composite_state.tiles {
                    if tile.kind == TileKind::Clear {
                        continue;
                    }
                    if !tile.local_dirty_rect.is_empty() {
                        if let CompositeTileSurface::Texture { surface: ResolvedSurfaceTexture::Native { id, .. } } = tile.surface {
                            let valid_rect = frame.composite_state.get_surface_rect(
                                &tile.local_valid_rect,
                                &tile.local_rect,
                                tile.transform_index,
                            ).to_i32();

                            compositor.invalidate_tile(self.device.as_mut().unwrap(), id, valid_rect);
                        }
                    }
                }
            }
            // Ensure any external surfaces that might be used during early composition
            // are invalidated first so that the native compositor can properly schedule
            // composition to happen only when the external surface is updated.
            // See update_external_native_surfaces for more details.
            for surface in &frame.composite_state.external_surfaces {
                if let Some((native_surface_id, size)) = surface.update_params {
                    let surface_rect = size.into();
                    compositor.invalidate_tile(self.device.as_mut().unwrap(), NativeTileId { surface_id: native_surface_id, x: 0, y: 0 }, surface_rect);
                }
            }
            // Finally queue native surfaces for early composition, if applicable. By now,
            // we have already invalidated any tiles that such surfaces may depend upon, so
            // the native render compositor can keep track of when to actually schedule
            // composition as surfaces are updated.
            if device_size.is_some() {
                frame.composite_state.composite_native(
                    self.clear_color,
                    &results.dirty_rects,
                    self.device.as_mut().unwrap(),
                    &mut **compositor,
                );
            }
        }

        for (_pass_index, pass) in frame.passes.iter_mut().enumerate() {
            #[cfg(not(target_os = "android"))]
            let _gm = self.gpu_profiler.start_marker(&format!("pass {}", _pass_index));

            profile_scope!("offscreen target");

            // If this frame has already been drawn, then any texture
            // cache targets have already been updated and can be
            // skipped this time.
            if !frame.has_been_rendered {
                for (&texture_id, target) in &pass.texture_cache {
                    self.draw_render_target(
                        texture_id,
                        target,
                        &frame.render_tasks,
                        &mut results.stats,
                    );
                }

                if !pass.picture_cache.is_empty() {
                    self.profile.inc(profiler::COLOR_PASSES);
                }

                // Draw picture caching tiles for this pass.
                for picture_target in &pass.picture_cache {
                    results.stats.color_target_count += 1;

                    let draw_target = match picture_target.surface {
                        ResolvedSurfaceTexture::TextureCache { ref texture } => {
                            let (texture, _) = self.texture_resolver
                                .resolve(texture)
                                .expect("bug");

                            DrawTarget::from_texture(
                                texture,
                                true,
                            )
                        }
                        ResolvedSurfaceTexture::Native { id, size } => {
                            let surface_info = match self.current_compositor_kind {
                                CompositorKind::Native { .. } => {
                                    let compositor = self.compositor_config.compositor().unwrap();
                                    compositor.bind(
                                        self.device.as_mut().unwrap(),
                                        id,
                                        picture_target.dirty_rect,
                                        picture_target.valid_rect,
                                    )
                                }
                                CompositorKind::Draw { .. } | CompositorKind::Layer { .. } => {
                                    unreachable!();
                                }
                            };

                            DrawTarget::NativeSurface {
                                offset: surface_info.origin,
                                external_fbo_id: surface_info.fbo_id,
                                dimensions: size,
                            }
                        }
                    };

                    let projection = Transform3D::ortho(
                        0.0,
                        draw_target.dimensions().width as f32,
                        0.0,
                        draw_target.dimensions().height as f32,
                        self.device.as_mut().unwrap().ortho_near_plane(),
                        self.device.as_mut().unwrap().ortho_far_plane(),
                    );

                    self.draw_picture_cache_target(
                        picture_target,
                        draw_target,
                        &projection,
                        &frame.render_tasks,
                        &mut results.stats,
                    );

                    // Native OS surfaces must be unbound at the end of drawing to them
                    if let ResolvedSurfaceTexture::Native { .. } = picture_target.surface {
                        match self.current_compositor_kind {
                            CompositorKind::Native { .. } => {
                                let compositor = self.compositor_config.compositor().unwrap();
                                compositor.unbind(self.device.as_mut().unwrap());
                            }
                            CompositorKind::Draw { .. } | CompositorKind::Layer { .. } => {
                                unreachable!();
                            }
                        }
                    }
                }
            }

            for target in &pass.alpha.targets {
                results.stats.alpha_target_count += 1;
                self.draw_render_target(
                    target.texture_id(),
                    target,
                    &frame.render_tasks,
                    &mut results.stats,
                );
            }

            for target in &pass.color.targets {
                results.stats.color_target_count += 1;
                self.draw_render_target(
                    target.texture_id(),
                    target,
                    &frame.render_tasks,
                    &mut results.stats,
                );
            }

            // Only end the pass here and invalidate previous textures for
            // off-screen targets. Deferring return of the inputs to the
            // frame buffer until the implicit end_pass in end_frame allows
            // debug draw overlays to be added without triggering a copy
            // resolve stage in mobile / tiled GPUs.
            self.texture_resolver.end_pass(
                self.device.as_mut().unwrap(),
                &pass.textures_to_invalidate,
            );
        }

        self.composite_frame(
            frame,
            device_size,
            results,
            present_mode,
        );

        if let Some(gpu_buffer_texture_f) = gpu_buffer_texture_f {
            self.device.as_mut().unwrap().delete_texture(gpu_buffer_texture_f);
        }
        if let Some(gpu_buffer_texture_i) = gpu_buffer_texture_i {
            self.device.as_mut().unwrap().delete_texture(gpu_buffer_texture_i);
        }

        frame.has_been_rendered = true;
    }

    fn composite_frame(
        &mut self,
        frame: &mut Frame,
        device_size: Option<DeviceIntSize>,
        results: &mut RenderResults,
        present_mode: Option<PartialPresentMode>,
    ) {
        profile_scope!("main target");

        if let Some(device_size) = device_size {
            results.stats.color_target_count += 1;
            results.picture_cache_debug = mem::replace(
                &mut frame.composite_state.picture_cache_debug,
                PictureCacheDebugInfo::new(),
            );

            let size = frame.device_rect.size().to_f32();
            let surface_origin_is_top_left = self.device.as_mut().unwrap().surface_origin_is_top_left();
            let (bottom, top) = if surface_origin_is_top_left {
              (0.0, size.height)
            } else {
              (size.height, 0.0)
            };

            let projection = Transform3D::ortho(
                0.0,
                size.width,
                bottom,
                top,
                self.device.as_mut().unwrap().ortho_near_plane(),
                self.device.as_mut().unwrap().ortho_far_plane(),
            );

            let fb_scale = Scale::<_, _, FramebufferPixel>::new(1i32);
            let mut fb_rect = frame.device_rect * fb_scale;

            if !surface_origin_is_top_left {
                let h = fb_rect.height();
                fb_rect.min.y = device_size.height - fb_rect.max.y;
                fb_rect.max.y = fb_rect.min.y + h;
            }

            let draw_target = DrawTarget::Default {
                rect: fb_rect,
                total_size: device_size * fb_scale,
                surface_origin_is_top_left,
            };

            // If we have a native OS compositor, then make use of that interface
            // to specify how to composite each of the picture cache surfaces.
            match self.current_compositor_kind {
                CompositorKind::Native { .. } => {
                    // We have already queued surfaces for early native composition by this point.
                    // All that is left is to finally update any external native surfaces that were
                    // invalidated so that composition can complete.
                    self.update_external_native_surfaces(
                        &frame.composite_state.external_surfaces,
                        results,
                    );
                }
                CompositorKind::Draw { .. } | CompositorKind::Layer { .. } => {
                    self.composite_simple(
                        &frame.composite_state,
                        frame.device_rect.size(),
                        draw_target,
                        &projection,
                        results,
                        present_mode,
                        device_size,
                    );
                }
            }
            // Reset force_redraw. It was used in composite_simple() with layer compositor.
            self.force_redraw = false;
        } else {
            // Rendering a frame without presenting it will confuse the partial
            // present logic, so force a full present for the next frame.
            self.force_redraw = true;
        }
    }

    pub fn debug_renderer(&mut self) -> Option<&mut DebugRenderer> {
        self.debug.get_mut(self.device.as_mut().unwrap())
    }

    fn draw_frame_debug_items(&mut self, items: &[DebugItem]) {
        if items.is_empty() {
            return;
        }

        let debug_renderer = match self.debug.get_mut(self.device.as_mut().unwrap()) {
            Some(render) => render,
            None => return,
        };

        for item in items {
            match item {
                DebugItem::Rect { rect, outer_color, inner_color, thickness } => {
                    if inner_color.a > 0.001 {
                        let rect = rect.inflate(-thickness as f32, -thickness as f32);
                        debug_renderer.add_quad(
                            rect.min.x,
                            rect.min.y,
                            rect.max.x,
                            rect.max.y,
                            (*inner_color).into(),
                            (*inner_color).into(),
                        );
                    }

                    if outer_color.a > 0.001 {
                        debug_renderer.add_rect(
                            &rect.to_i32(),
                            *thickness,
                            (*outer_color).into(),
                        );
                    }
                }
                DebugItem::Text { ref msg, position, color } => {
                    debug_renderer.add_text(
                        position.x,
                        position.y,
                        msg,
                        (*color).into(),
                        None,
                    );
                }
            }
        }
    }

    fn draw_render_target_debug(&mut self, draw_target: &DrawTarget) {
        if !self.debug_flags.contains(DebugFlags::RENDER_TARGET_DBG) {
            return;
        }

        let debug_renderer = match self.debug.get_mut(self.device.as_mut().unwrap()) {
            Some(render) => render,
            None => return,
        };

        let textures = self.texture_resolver
            .texture_cache_map
            .values()
            .filter(|item| item.category == TextureCacheCategory::RenderTarget)
            .map(|item| &item.texture)
            .collect::<Vec<&Texture>>();

        Self::do_debug_blit(
            self.device.as_mut().unwrap(),
            debug_renderer,
            textures,
            draw_target,
            0,
            &|_| [0.0, 1.0, 0.0, 1.0], // Use green for all RTs.
        );
    }

    fn draw_zoom_debug(
        &mut self,
        device_size: DeviceIntSize,
    ) {
        if !self.debug_flags.contains(DebugFlags::ZOOM_DBG) {
            return;
        }

        let debug_renderer = match self.debug.get_mut(self.device.as_mut().unwrap()) {
            Some(render) => render,
            None => return,
        };

        let source_size = DeviceIntSize::new(64, 64);
        let target_size = DeviceIntSize::new(1024, 1024);

        let source_origin = DeviceIntPoint::new(
            (self.cursor_position.x - source_size.width / 2)
                .min(device_size.width - source_size.width)
                .max(0),
            (self.cursor_position.y - source_size.height / 2)
                .min(device_size.height - source_size.height)
                .max(0),
        );

        let source_rect = DeviceIntRect::from_origin_and_size(
            source_origin,
            source_size,
        );

        let target_rect = DeviceIntRect::from_origin_and_size(
            DeviceIntPoint::new(
                device_size.width - target_size.width - 64,
                device_size.height - target_size.height - 64,
            ),
            target_size,
        );

        let texture_rect = FramebufferIntRect::from_size(
            source_rect.size().cast_unit(),
        );

        debug_renderer.add_rect(
            &target_rect.inflate(1, 1),
            1,
            debug_colors::RED.into(),
        );

        let zoom_texture = self.aux_textures.ensure_zoom_texture(self.device.as_mut().unwrap(), source_rect);

        // Copy frame buffer into the zoom texture
        let read_target = DrawTarget::new_default(device_size, self.device.as_mut().unwrap().surface_origin_is_top_left());
        self.device.as_mut().unwrap().blit_render_target(
            read_target.into(),
            read_target.to_framebuffer_rect(source_rect),
            DrawTarget::from_texture(
                zoom_texture,
                false,
            ),
            texture_rect,
            TextureFilter::Nearest,
        );

        // Draw the zoom texture back to the framebuffer
        self.device.as_mut().unwrap().blit_render_target(
            ReadTarget::from_texture(
                self.aux_textures.zoom_texture(),
            ),
            texture_rect,
            read_target,
            read_target.to_framebuffer_rect(target_rect),
            TextureFilter::Nearest,
        );
    }

    fn draw_texture_cache_debug(&mut self, draw_target: &DrawTarget) {
        if !self.debug_flags.contains(DebugFlags::TEXTURE_CACHE_DBG) {
            return;
        }

        let debug_renderer = match self.debug.get_mut(self.device.as_mut().unwrap()) {
            Some(render) => render,
            None => return,
        };

        let textures = self.texture_resolver
            .texture_cache_map
            .values()
            .filter(|item| item.category == TextureCacheCategory::Atlas)
            .map(|item| &item.texture)
            .collect::<Vec<&Texture>>();

        fn select_color(texture: &Texture) -> [f32; 4] {
            if texture.flags().contains(TextureFlags::IS_SHARED_TEXTURE_CACHE) {
                [1.0, 0.5, 0.0, 1.0] // Orange for shared.
            } else {
                [1.0, 0.0, 1.0, 1.0] // Fuchsia for standalone.
            }
        }

        Self::do_debug_blit(
            self.device.as_mut().unwrap(),
            debug_renderer,
            textures,
            draw_target,
            if self.debug_flags.contains(DebugFlags::RENDER_TARGET_DBG) { 544 } else { 0 },
            &select_color,
        );
    }

    fn do_debug_blit(
        device: &mut Device,
        debug_renderer: &mut DebugRenderer,
        mut textures: Vec<&Texture>,
        draw_target: &DrawTarget,
        bottom: i32,
        select_color: &dyn Fn(&Texture) -> [f32; 4],
    ) {
        let mut spacing = 16;
        let mut size = 512;

        let device_size = draw_target.dimensions();
        let fb_width = device_size.width;
        let fb_height = device_size.height;
        let surface_origin_is_top_left = draw_target.surface_origin_is_top_left();

        let num_textures = textures.len() as i32;

        if num_textures * (size + spacing) > fb_width {
            let factor = fb_width as f32 / (num_textures * (size + spacing)) as f32;
            size = (size as f32 * factor) as i32;
            spacing = (spacing as f32 * factor) as i32;
        }

        let text_height = 14; // Visually approximated.
        let text_margin = 1;
        let tag_height = text_height + text_margin * 2;
        let tag_y = fb_height - (bottom + spacing + tag_height);
        let image_y = tag_y - size;

        // Sort the display by size (in bytes), so that left-to-right is
        // largest-to-smallest.
        //
        // Note that the vec here is in increasing order, because the elements
        // get drawn right-to-left.
        textures.sort_by_key(|t| t.size_in_bytes());

        let mut i = 0;
        for texture in textures.iter() {
            let dimensions = texture.get_dimensions();
            let src_rect = FramebufferIntRect::from_size(
                FramebufferIntSize::new(dimensions.width as i32, dimensions.height as i32),
            );

            let x = fb_width - (spacing + size) * (i as i32 + 1);

            // If we have more targets than fit on one row in screen, just early exit.
            if x > fb_width {
                return;
            }

            // Draw the info tag.
            let tag_rect = rect(x, tag_y, size, tag_height).to_box2d();
            let tag_color = select_color(texture);
            device.clear_target(
                Some(tag_color),
                None,
                Some(draw_target.to_framebuffer_rect(tag_rect)),
            );

            // Draw the dimensions onto the tag.
            let dim = texture.get_dimensions();
            let text_rect = tag_rect.inflate(-text_margin, -text_margin);
            debug_renderer.add_text(
                text_rect.min.x as f32,
                text_rect.max.y as f32, // Top-relative.
                &format!("{}x{}", dim.width, dim.height),
                ColorU::new(0, 0, 0, 255),
                Some(tag_rect.to_f32())
            );

            // Blit the contents of the texture.
            let dest_rect = draw_target.to_framebuffer_rect(rect(x, image_y, size, size).to_box2d());
            let read_target = ReadTarget::from_texture(texture);

            if surface_origin_is_top_left {
                device.blit_render_target(
                    read_target,
                    src_rect,
                    *draw_target,
                    dest_rect,
                    TextureFilter::Linear,
                );
            } else {
                 // Invert y.
                 device.blit_render_target_invert_y(
                    read_target,
                    src_rect,
                    *draw_target,
                    dest_rect,
                );
            }
            i += 1;
        }
    }

    fn draw_epoch_debug(&mut self) {
        if !self.debug_flags.contains(DebugFlags::EPOCHS) {
            return;
        }

        let debug_renderer = match self.debug.get_mut(self.device.as_mut().unwrap()) {
            Some(render) => render,
            None => return,
        };

        let dy = debug_renderer.line_height();
        let x0: f32 = 30.0;
        let y0: f32 = 30.0;
        let mut y = y0;
        let mut text_width = 0.0;
        for ((pipeline, document_id), epoch) in  &self.pipeline_info.epochs {
            y += dy;
            let w = debug_renderer.add_text(
                x0, y,
                &format!("({:?}, {:?}): {:?}", pipeline, document_id, epoch),
                ColorU::new(255, 255, 0, 255),
                None,
            ).size.width;
            text_width = f32::max(text_width, w);
        }

        let margin = 10.0;
        debug_renderer.add_quad(
            x0 - margin,
            y0 - margin,
            x0 + text_width + margin,
            y + margin,
            ColorU::new(25, 25, 25, 200),
            ColorU::new(51, 51, 51, 200),
        );
    }

    fn draw_window_visibility_debug(&mut self) {
        if !self.debug_flags.contains(DebugFlags::WINDOW_VISIBILITY_DBG) {
            return;
        }

        let debug_renderer = match self.debug.get_mut(self.device.as_mut().unwrap()) {
            Some(render) => render,
            None => return,
        };

        let x: f32 = 30.0;
        let y: f32 = 40.0;

        if let CompositorConfig::Native { ref mut compositor, .. } = self.compositor_config {
            let visibility = compositor.get_window_visibility(self.device.as_mut().unwrap());
            let color = if visibility.is_fully_occluded {
                ColorU::new(255, 0, 0, 255)

            } else {
                ColorU::new(0, 0, 255, 255)
            };

            debug_renderer.add_text(
                x, y,
                &format!("{:?}", visibility),
                color,
                None,
            );
        }


    }

    fn draw_gpu_cache_debug(&mut self, device_size: DeviceIntSize) {
        if !self.debug_flags.contains(DebugFlags::GPU_CACHE_DBG) {
            return;
        }

        let debug_renderer = match self.debug.get_mut(self.device.as_mut().unwrap()) {
            Some(render) => render,
            None => return,
        };

        let (x_off, y_off) = (30f32, 30f32);
        let height = self.gpu_cache_texture.get_height()
            .min(device_size.height - (y_off as i32) * 2) as usize;
        debug_renderer.add_quad(
            x_off,
            y_off,
            x_off + MAX_VERTEX_TEXTURE_WIDTH as f32,
            y_off + height as f32,
            ColorU::new(80, 80, 80, 80),
            ColorU::new(80, 80, 80, 80),
        );

        let upper = self.gpu_cache_debug_chunks.len().min(height);
        for chunk in self.gpu_cache_debug_chunks[0..upper].iter().flatten() {
            let color = ColorU::new(250, 0, 0, 200);
            debug_renderer.add_quad(
                x_off + chunk.address.u as f32,
                y_off + chunk.address.v as f32,
                x_off + chunk.address.u as f32 + chunk.size as f32,
                y_off + chunk.address.v as f32 + 1.0,
                color,
                color,
            );
        }
    }

    /// Pass-through to `Device::read_pixels_into`, used by Gecko's WR bindings.
    pub fn read_pixels_into(&mut self, rect: FramebufferIntRect, format: ImageFormat, output: &mut [u8]) {
        #[cfg(feature = "wgpu_backend")]
        if self.is_wgpu_only() {
            // For wgpu, delegate to read_pixels_rgba8_wgpu and truncate/convert.
            let rgba = self.read_pixels_rgba8_wgpu(rect);
            let len = output.len().min(rgba.len());
            output[..len].copy_from_slice(&rgba[..len]);
            return;
        }
        self.device.as_mut().unwrap().read_pixels_into(rect, format, output);
    }

    pub fn read_pixels_rgba8(&mut self, rect: FramebufferIntRect) -> Vec<u8> {
        // wgpu path: read from the offscreen readback texture.
        #[cfg(feature = "wgpu_backend")]
        if self.is_wgpu_only() {
            return self.read_pixels_rgba8_wgpu(rect);
        }

        let mut pixels = vec![0; (rect.area() * 4) as usize];
        self.device.as_mut().unwrap().read_pixels_into(rect, ImageFormat::RGBA8, &mut pixels);
        pixels
    }

    /// Read back pixels from the wgpu readback texture in RGBA8 format.
    /// The rect uses GL conventions (origin at bottom-left), so we flip Y.
    #[cfg(feature = "wgpu_backend")]
    fn read_pixels_rgba8_wgpu(&mut self, rect: FramebufferIntRect) -> Vec<u8> {
        if self.wgpu_readback_texture.is_none() || self.wgpu_device.is_none() {
            return vec![0; (rect.area() * 4) as usize];
        }

        // Take the readback texture out to avoid borrow conflict with wgpu_device.
        let rt = self.wgpu_readback_texture.take().unwrap();
        let wgpu_dev = self.wgpu_device.as_mut().unwrap();

        // Read the full texture in BGRA format.
        let full_w = rt.width as usize;
        let full_h = rt.height as usize;
        let mut full_pixels = vec![0u8; full_w * full_h * 4];
        wgpu_dev.read_texture_pixels(&rt, &mut full_pixels);

        // Put the readback texture back.
        self.wgpu_readback_texture = Some(rt);

        // Extract the requested rect, flipping Y (GL has origin at bottom-left,
        // wgpu texture has origin at top-left).
        let rx = rect.min.x as usize;
        let ry = rect.min.y as usize;
        let rw = rect.width() as usize;
        let rh = rect.height() as usize;
        let mut output = vec![0u8; rw * rh * 4];

        for row in 0..rh {
            // GL rect Y=0 is bottom of framebuffer. In the texture, bottom row
            // is at index (full_h - 1). So GL row `ry + row` maps to texture
            // row `full_h - 1 - (ry + row)`.
            let src_row = full_h - 1 - (ry + row);
            let src_start = (src_row * full_w + rx) * 4;
            let dst_start = row * rw * 4;
            output[dst_start..dst_start + rw * 4]
                .copy_from_slice(&full_pixels[src_start..src_start + rw * 4]);
        }

        // Convert BGRA → RGBA in place.
        for pixel in output.chunks_exact_mut(4) {
            pixel.swap(0, 2); // swap B and R
        }

        output
    }

    // De-initialize the Renderer safely, assuming the GL is still alive and active.
    pub fn deinit(mut self) {
        #[cfg(feature = "wgpu_backend")]
        drop(self.wgpu_device.take());

        if let Some(mut device) = self.device.take() {
            //Note: this is a fake frame, only needed because texture deletion is require to happen inside a frame
            device.begin_frame();
            // If we are using a native compositor, ensure that any remaining native
            // surfaces are freed.
            if let CompositorConfig::Native { mut compositor, .. } = self.compositor_config {
                for id in self.allocated_native_surfaces.drain() {
                    compositor.destroy_surface(&mut device, id);
                }
                // Destroy the debug overlay surface, if currently allocated.
                if self.debug_overlay_state.current_size.is_some() {
                    compositor.destroy_surface(&mut device, NativeSurfaceId::DEBUG_OVERLAY);
                }
                compositor.deinit(&mut device);
            }
            self.gpu_cache_texture.deinit(&mut device);
            self.vertex_data_textures.deinit(&mut device);
            self.upload_state.deinit(&mut device);
            self.texture_resolver.deinit(&mut device);
            self.vaos.deinit(&mut device);
            self.aux_textures.deinit(&mut device);
            self.debug.deinit(&mut device);

            if let Some(shaders_rc) = self.shaders.take() {
                if let Ok(shaders) = Rc::try_unwrap(shaders_rc) {
                    shaders.into_inner().deinit(&mut device);
                }
            }

            if let Some(async_screenshots) = self.async_screenshots.take() {
                async_screenshots.deinit(&mut device);
            }

            if let Some(async_frame_recorder) = self.async_frame_recorder.take() {
                async_frame_recorder.deinit(&mut device);
            }

            #[cfg(feature = "capture")]
            device.delete_fbo(self.read_fbo);
            #[cfg(feature = "replay")]
            for (_, ext) in self.owned_external_images {
                device.delete_external_texture(ext);
            }
            device.end_frame();
        }
    }

    /// Collects a memory report.
    pub fn report_memory(&self, swgl: *mut c_void) -> MemoryReport {
        let mut report = MemoryReport::default();

        #[cfg(feature = "wgpu_backend")]
        if self.is_wgpu_only() {
            // Render task CPU memory is available in wgpu mode.
            for (_id, doc) in &self.active_documents {
                let frame_alloc_stats = doc.frame.allocator_memory.get_stats();
                report.frame_allocator += frame_alloc_stats.reserved_bytes;
                report.render_tasks += doc.frame.render_tasks.report_memory();
            }
            return report;
        }

        // GPU cache CPU memory.
        self.gpu_cache_texture.report_memory_to(&mut report, self.size_of_ops.as_ref().unwrap());

        // Render task CPU memory.
        for (_id, doc) in &self.active_documents {
            let frame_alloc_stats = doc.frame.allocator_memory.get_stats();
            report.frame_allocator += frame_alloc_stats.reserved_bytes;
            report.render_tasks += doc.frame.render_tasks.report_memory();
        }

        // Vertex data GPU memory.
        self.vertex_data_textures.report_memory_to(&mut report);

        // Texture cache and render target GPU memory.
        report += self.texture_resolver.report_memory();

        self.upload_state.report_memory_to(&mut report, self.size_of_ops.as_ref().unwrap());

        // Textures held internally within the device layer.
        report += self.device.as_ref().unwrap().report_memory(self.size_of_ops.as_ref().unwrap(), swgl);

        report
    }

    fn framebuffer_kind_for(draw_target: DrawTarget) -> FramebufferKind {
        if draw_target.is_default() {
            FramebufferKind::Main
        } else {
            FramebufferKind::Other
        }
    }

    fn begin_draw_target_pass(
        &mut self,
        draw_target: DrawTarget,
        needs_depth: bool,
        tiled_rect: Option<DeviceIntRect>,
        tiled_preserve_mask: u32,
    ) -> FramebufferKind {
        let framebuffer_kind = Self::framebuffer_kind_for(draw_target);

        self.device.as_mut().unwrap().bind_draw_target(draw_target);

        if let Some(tiled_rect) = tiled_rect {
            if self.device.as_mut().unwrap().get_capabilities().supports_qcom_tiled_rendering {
                self.device.as_mut().unwrap().start_tiling_qcom(tiled_rect, tiled_preserve_mask);
            }
        }

        if needs_depth {
            self.device.as_mut().unwrap().enable_depth_write();
        } else {
            self.device.as_mut().unwrap().disable_depth_write();
        }

        framebuffer_kind
    }

    fn end_draw_target_pass(&mut self, needs_depth: bool) {
        if needs_depth {
            self.device.as_mut().unwrap().invalidate_depth_target();
        }

        if self.device.as_mut().unwrap().get_capabilities().supports_qcom_tiled_rendering {
            self.device.as_mut().unwrap().end_tiling_qcom(gl::COLOR_BUFFER_BIT0_QCOM);
        }
    }

    // Sets the blend mode. Blend is unconditionally set if the "show overdraw" debugging mode is
    // enabled.
    fn set_blend(&mut self, mut blend: bool, framebuffer_kind: FramebufferKind) {
        if framebuffer_kind == FramebufferKind::Main &&
                self.debug_flags.contains(DebugFlags::SHOW_OVERDRAW) {
            blend = true
        }
        self.device.as_mut().unwrap().set_blend(blend)
    }

    fn set_blend_mode_multiply(&mut self, framebuffer_kind: FramebufferKind) {
        if framebuffer_kind == FramebufferKind::Main &&
                self.debug_flags.contains(DebugFlags::SHOW_OVERDRAW) {
            self.device.as_mut().unwrap().set_blend_mode_show_overdraw();
        } else {
            self.device.as_mut().unwrap().set_blend_mode_multiply();
        }
    }

    fn set_blend_mode_premultiplied_alpha(&mut self, framebuffer_kind: FramebufferKind) {
        if framebuffer_kind == FramebufferKind::Main &&
                self.debug_flags.contains(DebugFlags::SHOW_OVERDRAW) {
            self.device.as_mut().unwrap().set_blend_mode_show_overdraw();
        } else {
            self.device.as_mut().unwrap().set_blend_mode_premultiplied_alpha();
        }
    }

    /// Clears the texture with a given color.
    fn clear_texture(&mut self, texture: &Texture, color: [f32; 4]) {
        self.device.as_mut().unwrap().bind_draw_target(DrawTarget::from_texture(
            &texture,
            false,
        ));
        self.device.as_mut().unwrap().clear_target(Some(color), None, None);
    }
}

bitflags! {
    /// Flags that control how shaders are pre-cached, if at all.
    #[derive(Default, Debug, Copy, PartialEq, Eq, Clone, PartialOrd, Ord, Hash)]
    pub struct ShaderPrecacheFlags: u32 {
        /// Needed for const initialization
        const EMPTY                 = 0;

        /// Only start async compile
        const ASYNC_COMPILE         = 1 << 2;

        /// Do a full compile/link during startup
        const FULL_COMPILE          = 1 << 3;
    }
}

/// The cumulative times spent in each painting phase to generate this frame.
#[derive(Debug, Default)]
pub struct FullFrameStats {
    pub full_display_list: bool,
    pub gecko_display_list_time: f64,
    pub wr_display_list_time: f64,
    pub scene_build_time: f64,
    pub frame_build_time: f64,
}

impl FullFrameStats {
    pub fn merge(&self, other: &FullFrameStats) -> Self {
        Self {
            full_display_list: self.full_display_list || other.full_display_list,
            gecko_display_list_time: self.gecko_display_list_time + other.gecko_display_list_time,
            wr_display_list_time: self.wr_display_list_time + other.wr_display_list_time,
            scene_build_time: self.scene_build_time + other.scene_build_time,
            frame_build_time: self.frame_build_time + other.frame_build_time
        }
    }

    pub fn total(&self) -> f64 {
      self.gecko_display_list_time + self.wr_display_list_time + self.scene_build_time + self.frame_build_time
    }
}

/// Some basic statistics about the rendered scene, used in Gecko, as
/// well as in wrench reftests to ensure that tests are batching and/or
/// allocating on render targets as we expect them to.
#[repr(C)]
#[derive(Debug, Default)]
pub struct RendererStats {
    pub total_draw_calls: usize,
    pub alpha_target_count: usize,
    pub color_target_count: usize,
    pub texture_upload_mb: f64,
    pub resource_upload_time: f64,
    pub gpu_cache_upload_time: f64,
    pub gecko_display_list_time: f64,
    pub wr_display_list_time: f64,
    pub scene_build_time: f64,
    pub frame_build_time: f64,
    pub full_display_list: bool,
    pub full_paint: bool,
}

impl RendererStats {
    pub fn merge(&mut self, stats: &FullFrameStats) {
        self.gecko_display_list_time = stats.gecko_display_list_time;
        self.wr_display_list_time = stats.wr_display_list_time;
        self.scene_build_time = stats.scene_build_time;
        self.frame_build_time = stats.frame_build_time;
        self.full_display_list = stats.full_display_list;
        self.full_paint = true;
    }
}

/// Return type from render(), which contains some repr(C) statistics as well as
/// some non-repr(C) data.
#[derive(Debug, Default)]
pub struct RenderResults {
    /// Statistics about the frame that was rendered.
    pub stats: RendererStats,

    /// A list of the device dirty rects that were updated
    /// this frame.
    /// TODO(gw): This is an initial interface, likely to change in future.
    /// TODO(gw): The dirty rects here are currently only useful when scrolling
    ///           is not occurring. They are still correct in the case of
    ///           scrolling, but will be very large (until we expose proper
    ///           OS compositor support where the dirty rects apply to a
    ///           specific picture cache slice / OS compositor surface).
    pub dirty_rects: Vec<DeviceIntRect>,

    /// Information about the state of picture cache tiles. This is only
    /// allocated and stored if config.testing is true (such as wrench)
    pub picture_cache_debug: PictureCacheDebugInfo,
}

#[cfg(any(feature = "capture", feature = "replay"))]
#[cfg_attr(feature = "capture", derive(Serialize))]
#[cfg_attr(feature = "replay", derive(Deserialize))]
struct PlainTexture {
    data: String,
    size: DeviceIntSize,
    format: ImageFormat,
    filter: TextureFilter,
    has_depth: bool,
    category: Option<TextureCacheCategory>,
}


#[cfg(any(feature = "capture", feature = "replay"))]
#[cfg_attr(feature = "capture", derive(Serialize))]
#[cfg_attr(feature = "replay", derive(Deserialize))]
struct PlainRenderer {
    device_size: Option<DeviceIntSize>,
    gpu_cache: PlainTexture,
    gpu_cache_frame_id: FrameId,
    textures: FastHashMap<CacheTextureId, PlainTexture>,
}

#[cfg(any(feature = "capture", feature = "replay"))]
#[cfg_attr(feature = "capture", derive(Serialize))]
#[cfg_attr(feature = "replay", derive(Deserialize))]
struct PlainExternalResources {
    images: Vec<ExternalCaptureImage>
}

#[cfg(feature = "replay")]
enum CapturedExternalImageData {
    NativeTexture(gl::GLuint),
    Buffer(Arc<Vec<u8>>),
}

#[cfg(feature = "replay")]
struct DummyExternalImageHandler {
    data: FastHashMap<(ExternalImageId, u8), (CapturedExternalImageData, TexelRect)>,
}

#[cfg(feature = "replay")]
impl ExternalImageHandler for DummyExternalImageHandler {
    fn lock(&mut self, key: ExternalImageId, channel_index: u8, _is_composited: bool) -> ExternalImage {
        let (ref captured_data, ref uv) = self.data[&(key, channel_index)];
        ExternalImage {
            uv: *uv,
            source: match *captured_data {
                CapturedExternalImageData::NativeTexture(tid) => ExternalImageSource::NativeTexture(tid),
                CapturedExternalImageData::Buffer(ref arc) => ExternalImageSource::RawData(&*arc),
            }
        }
    }
    fn unlock(&mut self, _key: ExternalImageId, _channel_index: u8) {}
}

#[derive(Default)]
pub struct PipelineInfo {
    pub epochs: FastHashMap<(PipelineId, DocumentId), Epoch>,
    pub removed_pipelines: Vec<(PipelineId, DocumentId)>,
}

impl Renderer {
    #[cfg(feature = "capture")]
    fn save_texture(
        texture: &Texture, category: Option<TextureCacheCategory>, name: &str, root: &PathBuf, device: &mut Device
    ) -> PlainTexture {
        use std::fs;
        use std::io::Write;

        let short_path = format!("textures/{}.raw", name);

        let bytes_per_pixel = texture.get_format().bytes_per_pixel();
        let read_format = texture.get_format();
        let rect_size = texture.get_dimensions();

        let mut file = fs::File::create(root.join(&short_path))
            .expect(&format!("Unable to create {}", short_path));
        let bytes_per_texture = (rect_size.width * rect_size.height * bytes_per_pixel) as usize;
        let mut data = vec![0; bytes_per_texture];

        //TODO: instead of reading from an FBO with `read_pixels*`, we could
        // read from textures directly with `get_tex_image*`.

        let rect = device_size_as_framebuffer_size(rect_size).into();

        device.attach_read_texture(texture);
        #[cfg(feature = "png")]
        {
            let mut png_data;
            let (data_ref, format) = match texture.get_format() {
                ImageFormat::RGBAF32 => {
                    png_data = vec![0; (rect_size.width * rect_size.height * 4) as usize];
                    device.read_pixels_into(rect, ImageFormat::RGBA8, &mut png_data);
                    (&png_data, ImageFormat::RGBA8)
                }
                fm => (&data, fm),
            };
            CaptureConfig::save_png(
                root.join(format!("textures/{}-{}.png", name, 0)),
                rect_size, format,
                None,
                data_ref,
            );
        }
        device.read_pixels_into(rect, read_format, &mut data);
        file.write_all(&data)
            .unwrap();

        PlainTexture {
            data: short_path,
            size: rect_size,
            format: texture.get_format(),
            filter: texture.get_filter(),
            has_depth: texture.supports_depth(),
            category,
        }
    }

    #[cfg(feature = "replay")]
    fn load_texture<D: GpuDevice<Texture = Texture>>(
        target: ImageBufferKind,
        plain: &PlainTexture,
        rt_info: Option<RenderTargetInfo>,
        root: &PathBuf,
        device: &mut D
    ) -> (Texture, Vec<u8>)
    {
        use std::fs::File;
        use std::io::Read;

        let mut texels = Vec::new();
        File::open(root.join(&plain.data))
            .expect(&format!("Unable to open texture at {}", plain.data))
            .read_to_end(&mut texels)
            .unwrap();

        let texture = device.create_texture(
            target,
            plain.format,
            plain.size.width,
            plain.size.height,
            plain.filter,
            rt_info,
        );
        device.upload_texture_immediate(&texture, &texels);

        (texture, texels)
    }

    #[cfg(feature = "capture")]
    fn save_capture(
        &mut self,
        config: CaptureConfig,
        deferred_images: Vec<ExternalCaptureImage>,
    ) {
        use std::fs;
        use std::io::Write;
        use api::ExternalImageData;
        use crate::render_api::CaptureBits;

        if self.device.is_none() {
            warn!("save_capture: GL device not available (wgpu mode), skipping");
            return;
        }

        let root = config.resource_root();

        self.device.as_mut().unwrap().begin_frame();
        let _gm = self.gpu_profiler.start_marker("read GPU data");
        self.device.as_mut().unwrap().bind_read_target_impl(self.read_fbo, DeviceIntPoint::zero());

        if config.bits.contains(CaptureBits::EXTERNAL_RESOURCES) && !deferred_images.is_empty() {
            info!("saving external images");
            let mut arc_map = FastHashMap::<*const u8, String>::default();
            let mut tex_map = FastHashMap::<u32, String>::default();
            let handler = self.external_image_handler
                .as_mut()
                .expect("Unable to lock the external image handler!");
            for def in &deferred_images {
                info!("\t{}", def.short_path);
                let ExternalImageData { id, channel_index, image_type, .. } = def.external;
                // The image rendering parameter is irrelevant because no filtering happens during capturing.
                let ext_image = handler.lock(id, channel_index, false);
                let (data, short_path) = match ext_image.source {
                    ExternalImageSource::RawData(data) => {
                        let arc_id = arc_map.len() + 1;
                        match arc_map.entry(data.as_ptr()) {
                            Entry::Occupied(e) => {
                                (None, e.get().clone())
                            }
                            Entry::Vacant(e) => {
                                let short_path = format!("externals/d{}.raw", arc_id);
                                (Some(data.to_vec()), e.insert(short_path).clone())
                            }
                        }
                    }
                    ExternalImageSource::NativeTexture(gl_id) => {
                        let tex_id = tex_map.len() + 1;
                        match tex_map.entry(gl_id) {
                            Entry::Occupied(e) => {
                                (None, e.get().clone())
                            }
                            Entry::Vacant(e) => {
                                let target = match image_type {
                                    ExternalImageType::TextureHandle(target) => target,
                                    ExternalImageType::Buffer => unreachable!(),
                                };
                                info!("\t\tnative texture of target {:?}", target);
                                self.device.as_mut().unwrap().attach_read_texture_external(gl_id, target);
                                let data = self.device.as_mut().unwrap().read_pixels(&def.descriptor);
                                let short_path = format!("externals/t{}.raw", tex_id);
                                (Some(data), e.insert(short_path).clone())
                            }
                        }
                    }
                    ExternalImageSource::Invalid => {
                        info!("\t\tinvalid source!");
                        (None, String::new())
                    }
                };
                if let Some(bytes) = data {
                    fs::File::create(root.join(&short_path))
                        .expect(&format!("Unable to create {}", short_path))
                        .write_all(&bytes)
                        .unwrap();
                    #[cfg(feature = "png")]
                    CaptureConfig::save_png(
                        root.join(&short_path).with_extension("png"),
                        def.descriptor.size,
                        def.descriptor.format,
                        def.descriptor.stride,
                        &bytes,
                    );
                }
                let plain = PlainExternalImage {
                    data: short_path,
                    external: def.external,
                    uv: ext_image.uv,
                };
                config.serialize_for_resource(&plain, &def.short_path);
            }
            for def in &deferred_images {
                handler.unlock(def.external.id, def.external.channel_index);
            }
            let plain_external = PlainExternalResources {
                images: deferred_images,
            };
            config.serialize_for_resource(&plain_external, "external_resources");
        }

        if config.bits.contains(CaptureBits::FRAME) {
            let path_textures = root.join("textures");
            if !path_textures.is_dir() {
                fs::create_dir(&path_textures).unwrap();
            }

            info!("saving GPU cache");
            self.update_gpu_cache(); // flush pending updates
            let mut plain_self = PlainRenderer {
                device_size: self.device_size,
                gpu_cache: Self::save_texture(
                    self.gpu_cache_texture.get_texture(),
                    None, "gpu", &root, self.device.as_mut().unwrap(),
                ),
                gpu_cache_frame_id: self.gpu_cache_frame_id,
                textures: FastHashMap::default(),
            };

            info!("saving cached textures");
            for (id, item) in &self.texture_resolver.texture_cache_map {
                let file_name = format!("cache-{}", plain_self.textures.len() + 1);
                info!("\t{}", file_name);
                let plain = Self::save_texture(&item.texture, Some(item.category), &file_name, &root, self.device.as_mut().unwrap());
                plain_self.textures.insert(*id, plain);
            }

            config.serialize_for_resource(&plain_self, "renderer");
        }

        self.device.as_mut().unwrap().reset_read_target();
        self.device.as_mut().unwrap().end_frame();

        let mut stats_file = fs::File::create(config.root.join("profiler-stats.txt"))
            .expect(&format!("Unable to create profiler-stats.txt"));
        if self.debug_flags.intersects(DebugFlags::PROFILER_DBG | DebugFlags::PROFILER_CAPTURE) {
            self.profiler.dump_stats(&mut stats_file).unwrap();
        } else {
            writeln!(stats_file, "Turn on PROFILER_DBG or PROFILER_CAPTURE to get stats here!").unwrap();
        }

        info!("done.");
    }

    #[cfg(feature = "replay")]
    fn load_capture(
        &mut self,
        config: CaptureConfig,
        plain_externals: Vec<PlainExternalImage>,
    ) {
        use std::{fs::File, io::Read};

        if self.device.is_none() {
            warn!("load_capture: GL device not available (wgpu mode), skipping");
            return;
        }

        info!("loading external buffer-backed images");
        assert!(self.texture_resolver.external_images.is_empty());
        let mut raw_map = FastHashMap::<String, Arc<Vec<u8>>>::default();
        let mut image_handler = DummyExternalImageHandler {
            data: FastHashMap::default(),
        };

        let root = config.resource_root();

        // Note: this is a `SCENE` level population of the external image handlers
        // It would put both external buffers and texture into the map.
        // But latter are going to be overwritten later in this function
        // if we are in the `FRAME` level.
        for plain_ext in plain_externals {
            let data = match raw_map.entry(plain_ext.data) {
                Entry::Occupied(e) => e.get().clone(),
                Entry::Vacant(e) => {
                    let mut buffer = Vec::new();
                    File::open(root.join(e.key()))
                        .expect(&format!("Unable to open {}", e.key()))
                        .read_to_end(&mut buffer)
                        .unwrap();
                    e.insert(Arc::new(buffer)).clone()
                }
            };
            let ext = plain_ext.external;
            let value = (CapturedExternalImageData::Buffer(data), plain_ext.uv);
            image_handler.data.insert((ext.id, ext.channel_index), value);
        }

        if let Some(external_resources) = config.deserialize_for_resource::<PlainExternalResources, _>("external_resources") {
            info!("loading external texture-backed images");
            let mut native_map = FastHashMap::<String, gl::GLuint>::default();
            for ExternalCaptureImage { short_path, external, descriptor } in external_resources.images {
                let target = match external.image_type {
                    ExternalImageType::TextureHandle(target) => target,
                    ExternalImageType::Buffer => continue,
                };
                let plain_ext = config.deserialize_for_resource::<PlainExternalImage, _>(&short_path)
                    .expect(&format!("Unable to read {}.ron", short_path));
                let key = (external.id, external.channel_index);

                let tid = match native_map.entry(plain_ext.data) {
                    Entry::Occupied(e) => e.get().clone(),
                    Entry::Vacant(e) => {
                        let plain_tex = PlainTexture {
                            data: e.key().clone(),
                            size: descriptor.size,
                            format: descriptor.format,
                            filter: TextureFilter::Linear,
                            has_depth: false,
                            category: None,
                        };
                        let t = Self::load_texture(
                            target,
                            &plain_tex,
                            None,
                            &root,
                            self.device.as_mut().unwrap()
                        );
                        let extex = t.0.into_external();
                        self.owned_external_images.insert(key, extex.clone());
                        e.insert(extex.internal_id()).clone()
                    }
                };

                let value = (CapturedExternalImageData::NativeTexture(tid), plain_ext.uv);
                image_handler.data.insert(key, value);
            }
        }

        self.device.as_mut().unwrap().begin_frame();
        self.gpu_cache_texture.remove_texture(self.device.as_mut().unwrap());

        if let Some(renderer) = config.deserialize_for_resource::<PlainRenderer, _>("renderer") {
            info!("loading cached textures");
            self.device_size = renderer.device_size;

            for (_id, item) in self.texture_resolver.texture_cache_map.drain() {
                self.device.as_mut().unwrap().delete_texture(item.texture);
            }
            for (id, texture) in renderer.textures {
                info!("\t{}", texture.data);
                let target = ImageBufferKind::Texture2D;
                let t = Self::load_texture(
                    target,
                    &texture,
                    Some(RenderTargetInfo { has_depth: texture.has_depth }),
                    &root,
                    self.device.as_mut().unwrap()
                );
                self.texture_resolver.texture_cache_map.insert(id, CacheTexture {
                    texture: t.0,
                    category: texture.category.unwrap_or(TextureCacheCategory::Standalone),
                });
            }

            info!("loading gpu cache");
            let (t, gpu_cache_data) = Self::load_texture(
                ImageBufferKind::Texture2D,
                &renderer.gpu_cache,
                Some(RenderTargetInfo { has_depth: false }),
                &root,
                self.device.as_mut().unwrap(),
            );
            self.gpu_cache_texture.load_from_data(t, gpu_cache_data);
            self.gpu_cache_frame_id = renderer.gpu_cache_frame_id;
        } else {
            info!("loading cached textures");
            self.device.as_mut().unwrap().begin_frame();
            for (_id, item) in self.texture_resolver.texture_cache_map.drain() {
                self.device.as_mut().unwrap().delete_texture(item.texture);
            }
        }
        self.device.as_mut().unwrap().end_frame();

        self.external_image_handler = Some(Box::new(image_handler) as Box<_>);
        info!("done.");
    }
}

#[derive(Clone, Copy, PartialEq)]
enum FramebufferKind {
    Main,
    Other,
}

fn should_skip_batch(kind: &BatchKind, flags: DebugFlags) -> bool {
    match kind {
        BatchKind::TextRun(_) => {
            flags.contains(DebugFlags::DISABLE_TEXT_PRIMS)
        }
        BatchKind::Brush(BrushBatchKind::LinearGradient) => {
            flags.contains(DebugFlags::DISABLE_GRADIENT_PRIMS)
        }
        _ => false,
    }
}

#[cfg(feature = "gl_backend")]
impl CompositeState {
    /// Use the client provided native compositor interface to add all picture
    /// cache tiles to the OS compositor
    fn composite_native(
        &self,
        clear_color: ColorF,
        dirty_rects: &[DeviceIntRect],
        device: &mut Device,
        compositor: &mut dyn Compositor,
    ) {
        // Add each surface to the visual tree. z-order is implicit based on
        // order added. Offset and clip rect apply to all tiles within this
        // surface.
        for surface in &self.descriptor.surfaces {
            compositor.add_surface(
                device,
                surface.surface_id.expect("bug: no native surface allocated"),
                surface.transform,
                surface.clip_rect.to_i32(),
                surface.image_rendering,
                surface.rounded_clip_rect.to_i32(),
                surface.rounded_clip_radii,
            );
        }
        compositor.start_compositing(device, clear_color, dirty_rects, &[]);
    }
}

mod tests {
    #[test]
    fn test_buffer_damage_tracker() {
        use super::BufferDamageTracker;
        use api::units::{DevicePoint, DeviceRect, DeviceSize};

        let mut tracker = BufferDamageTracker::default();
        assert_eq!(tracker.get_damage_rect(0), None);
        assert_eq!(tracker.get_damage_rect(1), Some(DeviceRect::zero()));
        assert_eq!(tracker.get_damage_rect(2), Some(DeviceRect::zero()));
        assert_eq!(tracker.get_damage_rect(3), Some(DeviceRect::zero()));

        let damage1 = DeviceRect::from_origin_and_size(DevicePoint::new(10.0, 10.0), DeviceSize::new(10.0, 10.0));
        let damage2 = DeviceRect::from_origin_and_size(DevicePoint::new(20.0, 20.0), DeviceSize::new(10.0, 10.0));
        let combined = damage1.union(&damage2);

        tracker.push_dirty_rect(&damage1);
        assert_eq!(tracker.get_damage_rect(0), None);
        assert_eq!(tracker.get_damage_rect(1), Some(DeviceRect::zero()));
        assert_eq!(tracker.get_damage_rect(2), Some(damage1));
        assert_eq!(tracker.get_damage_rect(3), Some(damage1));

        tracker.push_dirty_rect(&damage2);
        assert_eq!(tracker.get_damage_rect(0), None);
        assert_eq!(tracker.get_damage_rect(1), Some(DeviceRect::zero()));
        assert_eq!(tracker.get_damage_rect(2), Some(damage2));
        assert_eq!(tracker.get_damage_rect(3), Some(combined));
    }
}
