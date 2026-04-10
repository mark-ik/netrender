/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use api::{BlobImageHandler, ColorF, CrashAnnotator, DocumentId, IdNamespace};
use api::{VoidPtrToSizeFn, FontRenderMode, ImageFormat};
use api::{RenderNotifier, ImageBufferKind};
use api::units::*;
use api::channel::unbounded_channel;
pub use api::DebugFlags;

use crate::bump_allocator::ChunkPool;
use crate::render_api::{RenderApiSender, FrameMsg};
use crate::composite::{CompositorKind, CompositorConfig};
use crate::device::{GpuDevice, RendererBackend, TextureFilter};
#[cfg(feature = "gl_backend")]
use crate::device::{DeviceConfig, UploadMethod, UploadPBOPool, VertexUsageHint, Device, ProgramCache};
#[cfg(feature = "gl_backend")]
use crate::device::query::GpuProfiler;
use crate::frame_builder::FrameBuilderConfig;
use crate::glyph_cache::GlyphCache;
use glyph_rasterizer::{GlyphRasterThread, GlyphRasterizer, SharedFontResources};
use crate::gpu_types::PrimitiveInstanceData;
use crate::internal_types::{FastHashMap, FastHashSet, FrameId};
use crate::picture;
use crate::profiler::{self, Profiler, TransactionProfile};
use crate::render_backend::RenderBackend;
use crate::resource_cache::ResourceCache;
use crate::scene_builder_thread::{SceneBuilderThread, SceneBuilderThreadChannels, LowPrioritySceneBuilderThread};
use crate::texture_cache::{TextureCache, TextureCacheConfig};
use crate::picture_textures::PictureTextures;
use crate::renderer::{
    Renderer, DebugOverlayState, BufferDamageTracker, PipelineInfo,
    RendererError,
};
use crate::renderer::{ShaderPrecacheFlags, SharedShaders};
#[cfg(feature = "gl_backend")]
use crate::renderer::{
    debug, gpu_cache, vertex, gl,
    TextureResolver,
    upload::{RendererUploadState, UploadTexturePool},
    shade::Shaders,
};
#[cfg(feature = "debugger")]
use crate::debugger::Debugger;

use std::{
    mem,
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::{Arc, atomic::{AtomicBool, Ordering}},
    num::NonZeroUsize,
    path::PathBuf,
};

use tracy_rs::register_thread_with_profiler;
use rayon::{ThreadPool, ThreadPoolBuilder};
use malloc_size_of::MallocSizeOfOps;

/// Use this hint for all vertex data re-initialization. This allows
/// the driver to better re-use RBOs internally.
#[cfg(feature = "gl_backend")]
pub const ONE_TIME_USAGE_HINT: VertexUsageHint = VertexUsageHint::Stream;

/// Is only false if no WR instances have ever been created.
static HAS_BEEN_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Returns true if a WR instance has ever been initialized in this process.
pub fn wr_has_been_initialized() -> bool {
    HAS_BEEN_INITIALIZED.load(Ordering::SeqCst)
}

fn create_dither_matrix_texture<D: GpuDevice>(device: &mut D) -> D::Texture {
    let dither_matrix: [u8; 64] = [
        0, 48, 12, 60, 3, 51, 15, 63,
        32, 16, 44, 28, 35, 19, 47, 31,
        8, 56, 4, 52, 11, 59, 7, 55,
        40, 24, 36, 20, 43, 27, 39, 23,
        2, 50, 14, 62, 1, 49, 13, 61,
        34, 18, 46, 30, 33, 17, 45, 29,
        10, 58, 6, 54, 9, 57, 5, 53,
        42, 26, 38, 22, 41, 25, 37, 21,
    ];

    let texture = device.create_texture(
        ImageBufferKind::Texture2D,
        ImageFormat::R8,
        8,
        8,
        TextureFilter::Nearest,
        None,
    );
    device.upload_texture_immediate(&texture, &dither_matrix);

    texture
}

/// Allows callers to hook in at certain points of the async scene build. These
/// functions are all called from the scene builder thread.
pub trait SceneBuilderHooks {
    /// This is called exactly once, when the scene builder thread is started
    /// and before it processes anything.
    fn register(&self);
    /// This is called before each scene build starts.
    fn pre_scene_build(&self);
    /// This is called before each scene swap occurs.
    fn pre_scene_swap(&self);
    /// This is called after each scene swap occurs. The PipelineInfo contains
    /// the updated epochs and pipelines removed in the new scene compared to
    /// the old scene.
    fn post_scene_swap(&self, document_id: &Vec<DocumentId>, info: PipelineInfo, schedule_frame: bool);
    /// This is called after a resource update operation on the scene builder
    /// thread, in the case where resource updates were applied without a scene
    /// build.
    fn post_resource_update(&self, document_ids: &Vec<DocumentId>);
    /// This is called after a scene build completes without any changes being
    /// made. We guarantee that each pre_scene_build call will be matched with
    /// exactly one of post_scene_swap, post_resource_update or
    /// post_empty_scene_build.
    fn post_empty_scene_build(&self);
    /// This is a generic callback which provides an opportunity to run code
    /// on the scene builder thread. This is called as part of the main message
    /// loop of the scene builder thread, but outside of any specific message
    /// handler.
    fn poke(&self);
    /// This is called exactly once, when the scene builder thread is about to
    /// terminate.
    fn deregister(&self);
}

/// Allows callers to hook into the main render_backend loop and provide
/// additional frame ops for generate_frame transactions. These functions
/// are all called from the render backend thread.
pub trait AsyncPropertySampler {
    /// This is called exactly once, when the render backend thread is started
    /// and before it processes anything.
    fn register(&self);
    /// This is called for each transaction with the generate_frame flag set
    /// (i.e. that will trigger a render). The list of frame messages returned
    /// are processed as though they were part of the original transaction.
    fn sample(&self, document_id: DocumentId, generated_frame_id: Option<u64>) -> Vec<FrameMsg>;
    /// This is called exactly once, when the render backend thread is about to
    /// terminate.
    fn deregister(&self);
}

pub trait RenderBackendHooks {
    fn init_thread(&self);
}

pub struct WebRenderOptions {
    pub resource_override_path: Option<PathBuf>,
    /// Whether to use shaders that have been optimized at build time.
    pub use_optimized_shaders: bool,
    pub enable_aa: bool,
    pub enable_dithering: bool,
    pub max_recorded_profiles: usize,
    #[cfg(feature = "gl_backend")]
    pub precache_flags: ShaderPrecacheFlags,
    /// Enable sub-pixel anti-aliasing if a fast implementation is available.
    pub enable_subpixel_aa: bool,
    pub clear_color: ColorF,
    pub enable_clear_scissor: Option<bool>,
    pub max_internal_texture_size: Option<i32>,
    pub image_tiling_threshold: i32,
    #[cfg(feature = "gl_backend")]
    pub upload_method: UploadMethod,
    /// The default size in bytes for PBOs used to upload texture data.
    pub upload_pbo_default_size: usize,
    pub batched_upload_threshold: i32,
    pub workers: Option<Arc<ThreadPool>>,
    /// A pool of large memory chunks used by the per-frame allocators.
    /// Providing the pool here makes it possible to share a single pool for
    /// all WebRender instances.
    pub chunk_pool: Option<Arc<ChunkPool>>,
    pub dedicated_glyph_raster_thread: Option<GlyphRasterThread>,
    pub enable_multithreading: bool,
    pub blob_image_handler: Option<Box<dyn BlobImageHandler>>,
    pub crash_annotator: Option<Box<dyn CrashAnnotator>>,
    pub size_of_op: Option<VoidPtrToSizeFn>,
    pub enclosing_size_of_op: Option<VoidPtrToSizeFn>,
    #[cfg(feature = "gl_backend")]
    pub cached_programs: Option<Rc<ProgramCache>>,
    pub debug_flags: DebugFlags,
    pub renderer_id: Option<u64>,
    pub scene_builder_hooks: Option<Box<dyn SceneBuilderHooks + Send>>,
    pub render_backend_hooks: Option<Box<dyn RenderBackendHooks + Send>>,
    pub sampler: Option<Box<dyn AsyncPropertySampler + Send>>,
    pub support_low_priority_transactions: bool,
    pub namespace_alloc_by_client: bool,
    /// If namespaces are allocated by the client, then the namespace for fonts
    /// must also be allocated by the client to avoid namespace collisions with
    /// the backend.
    pub shared_font_namespace: Option<IdNamespace>,
    pub testing: bool,
    /// Set to true if this GPU supports hardware fast clears as a performance
    /// optimization. Likely requires benchmarking on various GPUs to see if
    /// it is a performance win. The default is false, which tends to be best
    /// performance on lower end / integrated GPUs.
    pub gpu_supports_fast_clears: bool,
    pub allow_dual_source_blending: bool,
    pub allow_advanced_blend_equation: bool,
    /// If true, allow textures to be initialized with glTexStorage.
    /// This affects VRAM consumption and data upload paths.
    pub allow_texture_storage_support: bool,
    /// If true, we allow the data uploaded in a different format from the
    /// one expected by the driver, pretending the format is matching, and
    /// swizzling the components on all the shader sampling.
    pub allow_texture_swizzling: bool,
    /// Use `ps_clear` shader with batched quad rendering to clear the rects
    /// in texture cache and picture cache tasks.
    /// This helps to work around some Intel drivers
    /// that incorrectly synchronize clears to following draws.
    pub clear_caches_with_quads: bool,
    /// Output the source of the shader with the given name.
    pub dump_shader_source: Option<String>,
    pub surface_origin_is_top_left: bool,
    /// The configuration options defining how WR composites the final scene.
    pub compositor_config: CompositorConfig,
    pub enable_gpu_markers: bool,
    /// If true, panic whenever a GL error occurs. This has a significant
    /// performance impact, so only use when debugging specific problems!
    pub panic_on_gl_error: bool,
    pub picture_tile_size: Option<DeviceIntSize>,
    pub texture_cache_config: TextureCacheConfig,
    /// If true, we'll use instanced vertex attributes. Each instace is a quad.
    /// If false, we'll duplicate the instance attributes per vertex and issue
    /// regular indexed draws instead.
    pub enable_instancing: bool,
    /// If true, we'll reject contexts backed by a software rasterizer, except
    /// Software WebRender.
    pub reject_software_rasterizer: bool,
    /// If enabled, pinch-zoom will apply the zoom factor during compositing
    /// of picture cache tiles. This is higher performance (tiles are not
    /// re-rasterized during zoom) but lower quality result. For most display
    /// items, if the zoom factor is relatively small, bilinear filtering should
    /// make the result look quite close to the high-quality zoom, except for glyphs.
    pub low_quality_pinch_zoom: bool,
    pub max_shared_surface_size: i32,
    /// If true, open a debug socket to listen for remote debugger.
    /// Relies on `debugger` cargo feature being enabled.
    pub enable_debugger: bool,

    /// Use a more precise method for sampling gradients.
    pub precise_linear_gradients: bool,
    pub precise_radial_gradients: bool,
    pub precise_conic_gradients: bool,

    /// Directory where the wgpu pipeline cache blob is loaded from and saved
    /// to.  `None` disables disk persistence.  Only effective on Vulkan; on
    /// other backends wgpu's pipeline cache is a no-op.
    #[cfg(feature = "wgpu_backend")]
    pub pipeline_cache_dir: Option<PathBuf>,

    /// Presentation mode for the wgpu surface.
    ///
    /// Only has an effect when using `RendererBackend::Wgpu` with a surface
    /// (i.e. not headless or shared-device mode).  Defaults to `Fifo`
    /// (vsync).  Common alternatives: `AutoNoVsync` (uncapped), `Mailbox`
    /// (low-latency on supported hardware).
    ///
    /// The adapter's supported present modes are queried at init time; if the
    /// requested mode is unsupported, `Fifo` is used as a fallback.
    #[cfg(feature = "wgpu_backend")]
    pub wgpu_present_mode: wgpu::PresentMode,
}

impl WebRenderOptions {
    /// Number of batches to look back in history for adding the current
    /// transparent instance into.
    const BATCH_LOOKBACK_COUNT: usize = 10;

    /// Since we are re-initializing the instance buffers on every draw call,
    /// the driver has to internally manage PBOs in flight.
    /// It's typically done by bucketing up to a specific limit, and then
    /// just individually managing the largest buffers.
    /// Having a limit here allows the drivers to more easily manage
    /// the PBOs for us.
    const MAX_INSTANCE_BUFFER_SIZE: usize = 0x20000; // actual threshold in macOS GL drivers

    #[cfg(feature = "gl_backend")]
    pub(crate) fn prepare_for_device_creation(&mut self) {
        match self.compositor_config {
            CompositorConfig::Draw { .. } | CompositorConfig::Native { .. } => {}
            CompositorConfig::Layer { .. } => {
                self.surface_origin_is_top_left = true;
            }
        }
    }

    #[cfg(feature = "gl_backend")]
    pub(crate) fn take_device_config(&mut self) -> DeviceConfig {
        DeviceConfig {
            crash_annotator: self.crash_annotator.take(),
            resource_override_path: self.resource_override_path.clone(),
            use_optimized_shaders: self.use_optimized_shaders,
            upload_method: self.upload_method.clone(),
            batched_upload_threshold: self.batched_upload_threshold,
            cached_programs: self.cached_programs.take(),
            allow_texture_storage_support: self.allow_texture_storage_support,
            allow_texture_swizzling: self.allow_texture_swizzling,
            dump_shader_source: self.dump_shader_source.take(),
            surface_origin_is_top_left: self.surface_origin_is_top_left,
            panic_on_gl_error: self.panic_on_gl_error,
        }
    }
}

impl Default for WebRenderOptions {
    fn default() -> Self {
        WebRenderOptions {
            resource_override_path: None,
            use_optimized_shaders: false,
            enable_aa: true,
            enable_dithering: false,
            debug_flags: DebugFlags::empty(),
            max_recorded_profiles: 0,
            #[cfg(feature = "gl_backend")]
            precache_flags: ShaderPrecacheFlags::empty(),
            enable_subpixel_aa: false,
            clear_color: ColorF::new(1.0, 1.0, 1.0, 1.0),
            enable_clear_scissor: None,
            max_internal_texture_size: None,
            image_tiling_threshold: 4096,
            // This is best as `Immediate` on Angle, or `Pixelbuffer(Dynamic)` on GL,
            // but we are unable to make this decision here, so picking the reasonable medium.
            #[cfg(feature = "gl_backend")]
            upload_method: UploadMethod::PixelBuffer(ONE_TIME_USAGE_HINT),
            upload_pbo_default_size: 512 * 512 * 4,
            batched_upload_threshold: 512 * 512,
            workers: None,
            chunk_pool: None,
            dedicated_glyph_raster_thread: None,
            enable_multithreading: true,
            blob_image_handler: None,
            crash_annotator: None,
            size_of_op: None,
            enclosing_size_of_op: None,
            renderer_id: None,
            #[cfg(feature = "gl_backend")]
            cached_programs: None,
            scene_builder_hooks: None,
            render_backend_hooks: None,
            sampler: None,
            support_low_priority_transactions: false,
            namespace_alloc_by_client: false,
            shared_font_namespace: None,
            testing: false,
            gpu_supports_fast_clears: false,
            allow_dual_source_blending: true,
            allow_advanced_blend_equation: false,
            allow_texture_storage_support: true,
            allow_texture_swizzling: true,
            clear_caches_with_quads: true,
            dump_shader_source: None,
            surface_origin_is_top_left: false,
            compositor_config: CompositorConfig::default(),
            enable_gpu_markers: true,
            panic_on_gl_error: false,
            picture_tile_size: None,
            texture_cache_config: TextureCacheConfig::DEFAULT,
            // Disabling instancing means more vertex data to upload and potentially
            // process by the vertex shaders.
            enable_instancing: true,
            reject_software_rasterizer: false,
            low_quality_pinch_zoom: false,
            max_shared_surface_size: 2048,
            enable_debugger: true,
            precise_linear_gradients: false,
            precise_radial_gradients: false,
            precise_conic_gradients: false,
            #[cfg(feature = "wgpu_backend")]
            pipeline_cache_dir: None,
            #[cfg(feature = "wgpu_backend")]
            wgpu_present_mode: wgpu::PresentMode::Fifo,
        }
    }
}

/// Initializes WebRender and creates a `Renderer` and `RenderApiSender`.
///
/// # Examples
/// Initializes a `Renderer` with some reasonable values. For more information see
/// [`WebRenderOptions`][WebRenderOptions].
///
/// ```rust,ignore
/// # use webrender::renderer::Renderer;
/// # use std::path::PathBuf;
/// let opts = webrender::WebRenderOptions {
///    device_pixel_ratio: 1.0,
///    resource_override_path: None,
///    enable_aa: false,
/// };
/// let (renderer, sender) = Renderer::new(opts);
/// ```
/// [WebRenderOptions]: struct.WebRenderOptions.html
#[cfg(feature = "gl_backend")]
pub fn create_webrender_instance(
    gl: Rc<dyn gleam::gl::Gl>,
    notifier: Box<dyn RenderNotifier>,
    options: WebRenderOptions,
    shaders: Option<&SharedShaders>,
) -> Result<(Renderer, RenderApiSender), RendererError> {
    create_webrender_instance_with_backend(
        RendererBackend::Gl { gl },
        notifier,
        options,
        shaders,
    )
}

pub fn create_webrender_instance_with_backend(
    backend: RendererBackend,
    notifier: Box<dyn RenderNotifier>,
    mut options: WebRenderOptions,
    shaders: Option<&SharedShaders>,
) -> Result<(Renderer, RenderApiSender), RendererError> {
    #[cfg(feature = "wgpu_native")]
    if let RendererBackend::Wgpu { instance, surface, width, height } = backend {
        let present_mode = options.wgpu_present_mode;
        return create_webrender_instance_wgpu(notifier, options, WgpuInit::CreateDevice { instance, surface, width, height, present_mode });
    }

    #[cfg(feature = "wgpu_backend")]
    if let RendererBackend::WgpuShared { device, queue } = backend {
        return create_webrender_instance_wgpu(notifier, options, WgpuInit::SharedDevice { device, queue });
    }

    #[cfg(feature = "wgpu_backend")]
    if let RendererBackend::WgpuHal { device_factory } = backend {
        let (device, queue) = device_factory();
        return create_webrender_instance_wgpu(notifier, options, WgpuInit::SharedDevice { device, queue });
    }

    #[cfg(feature = "gl_backend")]
    {
        let device = backend.create_gl_device(&mut options);
        create_webrender_instance_with_device(device, notifier, options, shaders)
    }
    #[cfg(not(feature = "gl_backend"))]
    Err(RendererError::UnsupportedBackend("GL backend not compiled"))
}

#[cfg(feature = "gl_backend")]
fn create_webrender_instance_with_device(
    mut device: Device,
    notifier: Box<dyn RenderNotifier>,
    mut options: WebRenderOptions,
    shaders: Option<&SharedShaders>,
) -> Result<(Renderer, RenderApiSender), RendererError> {
    if !wr_has_been_initialized() {
        // If the profiler feature is enabled, try to load the profiler shared library
        // if the path was provided.
        #[cfg(feature = "profiler")]
        unsafe {
            if let Ok(ref tracy_path) = std::env::var("WR_TRACY_PATH") {
                let ok = tracy_rs::load(tracy_path);
                info!("Load tracy from {} -> {}", tracy_path, ok);
            }
        }

        register_thread_with_profiler("Compositor".to_owned());
    }

    HAS_BEEN_INITIALIZED.store(true, Ordering::SeqCst);

    let (api_tx, api_rx) = unbounded_channel();
    let (result_tx, result_rx) = unbounded_channel();
    let gl_type = device.gl_type();

    let color_cache_formats = device.preferred_color_formats();
    let swizzle_settings = device.swizzle_settings();
    let use_dual_source_blending =
        device.dual_source_blending_is_supported(options.allow_dual_source_blending);
    let ext_blend_equation_advanced =
        device.advanced_blend_equation_is_supported(options.allow_advanced_blend_equation);
    let ext_blend_equation_advanced_coherent = device.advanced_blend_is_coherent();

    let enable_clear_scissor = device.resolve_clear_scissor(options.enable_clear_scissor);

    // 2048 is the minimum that the texture cache can work with.
    const MIN_TEXTURE_SIZE: i32 = 2048;
    let mut max_internal_texture_size = device.max_texture_size();
    if max_internal_texture_size < MIN_TEXTURE_SIZE {
        // Broken GL contexts can return a max texture size of zero (See #1260).
        // Better to gracefully fail now than panic as soon as a texture is allocated.
        error!(
            "Device reporting insufficient max texture size ({})",
            max_internal_texture_size
        );
        return Err(RendererError::MaxTextureSize);
    }
    if let Some(internal_limit) = options.max_internal_texture_size {
        assert!(internal_limit >= MIN_TEXTURE_SIZE);
        max_internal_texture_size = max_internal_texture_size.min(internal_limit);
    }

    if options.reject_software_rasterizer && device.is_software_rasterizer() {
        return Err(RendererError::SoftwareRasterizer);
    }

    let image_tiling_threshold = options.image_tiling_threshold
        .min(max_internal_texture_size);

    device.begin_frame();

    let shaders = match shaders {
        Some(shaders) => Rc::clone(shaders),
        None => {
            let mut shaders = Shaders::new(&mut device, &options)?;
            if options.precache_flags.intersects(ShaderPrecacheFlags::ASYNC_COMPILE | ShaderPrecacheFlags::FULL_COMPILE) {
                let mut pending_shaders = shaders.precache_all(options.precache_flags);
                while shaders.resume_precache(&mut device, &mut pending_shaders)? {}
            }
            Rc::new(RefCell::new(shaders))
        }
    };

    let dither_matrix_texture = if options.enable_dithering {
        Some(create_dither_matrix_texture(&mut device))
    } else {
        None
    };
    let aux_textures = super::RendererAuxTextures::new_gl(dither_matrix_texture);

    let max_primitive_instance_count =
        WebRenderOptions::MAX_INSTANCE_BUFFER_SIZE / mem::size_of::<PrimitiveInstanceData>();
    let vaos = vertex::RendererVaoState::new_gl(
        &mut device,
        if options.enable_instancing { None } else { NonZeroUsize::new(max_primitive_instance_count) },
    );

    let texture_upload_pbo_pool = UploadPBOPool::new(&mut device, options.upload_pbo_default_size);
    let staging_texture_pool = UploadTexturePool::new();
    let upload_state = RendererUploadState::new_gl(texture_upload_pbo_pool, staging_texture_pool);
    let texture_resolver = TextureResolver::new(&mut device);

    let vertex_data_textures = vertex::RendererVertexData::new_gl();

    // On some (mostly older, integrated) GPUs, the normal GPU texture cache update path
    // doesn't work well when running on ANGLE, causing CPU stalls inside D3D and/or the
    // GPU driver. See https://bugzilla.mozilla.org/show_bug.cgi?id=1576637 for much
    // more detail. To reduce the number of code paths we have active that require testing,
    // we will enable the GPU cache scatter update path on all devices running with ANGLE.
    // We want a better solution long-term, but for now this is a significant performance
    // improvement on HD4600 era GPUs, and shouldn't hurt performance in a noticeable
    // way on other systems running under ANGLE.
    let is_software = device.is_software_webrender();

    // On other GL platforms, like macOS or Android, creating many PBOs is very inefficient.
    // This is what happens in GPU cache updates in PBO path. Instead, we switch everything
    // except software GL to use the GPU scattered updates.
    let supports_scatter = device.get_capabilities().supports_color_buffer_float;
    let gpu_cache_texture = gpu_cache::RendererGpuCache::new_gl(
        &mut device,
        supports_scatter && !is_software,
    )?;

    device.end_frame();

    let backend_notifier = notifier.clone();

    let clear_alpha_targets_with_quads = device.requires_quad_target_clears();

    let prefer_subpixel_aa = options.enable_subpixel_aa && use_dual_source_blending;
    let default_font_render_mode = match (options.enable_aa, prefer_subpixel_aa) {
        (true, true) => FontRenderMode::Subpixel,
        (true, false) => FontRenderMode::Alpha,
        (false, _) => FontRenderMode::Mono,
    };

    let compositor_kind = match options.compositor_config {
        CompositorConfig::Draw { max_partial_present_rects, draw_previous_partial_present_regions, .. } => {
            CompositorKind::Draw { max_partial_present_rects, draw_previous_partial_present_regions }
        }
        CompositorConfig::Native { ref compositor } => {
            let capabilities = compositor.get_capabilities(&mut device);

            CompositorKind::Native {
                capabilities,
            }
        }
        CompositorConfig::Layer { .. } => {
            CompositorKind::Layer {
            }
        }
    };

    let config = FrameBuilderConfig {
        default_font_render_mode,
        dual_source_blending_is_supported: use_dual_source_blending,
        dual_source_mix_blend_supported: use_dual_source_blending,
        testing: options.testing,
        gpu_supports_fast_clears: options.gpu_supports_fast_clears,
        gpu_supports_advanced_blend: ext_blend_equation_advanced,
        advanced_blend_is_coherent: ext_blend_equation_advanced_coherent,
        gpu_supports_render_target_partial_update: device.supports_render_target_partial_update(),
        external_images_require_copy: device.external_images_require_copy(),
        batch_lookback_count: WebRenderOptions::BATCH_LOOKBACK_COUNT,
        background_color: Some(options.clear_color),
        compositor_kind,
        tile_size_override: None,
        max_surface_override: None,
        max_depth_ids: device.max_depth_ids(),
        max_target_size: max_internal_texture_size,
        force_invalidation: false,
        is_software,
        low_quality_pinch_zoom: options.low_quality_pinch_zoom,
        max_shared_surface_size: options.max_shared_surface_size,
        enable_dithering: options.enable_dithering,
        precise_linear_gradients: options.precise_linear_gradients,
        precise_radial_gradients: options.precise_radial_gradients,
        precise_conic_gradients: options.precise_conic_gradients,
    };
    info!("WR {:?}", config);

    let debug_flags = options.debug_flags;
    let size_of_op = options.size_of_op;
    let enclosing_size_of_op = options.enclosing_size_of_op;
    let make_size_of_ops =
        move || size_of_op.map(|o| MallocSizeOfOps::new(o, enclosing_size_of_op));
    let workers = options
        .workers
        .take()
        .unwrap_or_else(|| {
            let worker = ThreadPoolBuilder::new()
                .thread_name(|idx|{ format!("WRWorker#{}", idx) })
                .start_handler(move |idx| {
                    register_thread_with_profiler(format!("WRWorker#{}", idx));
                    profiler::register_thread(&format!("WRWorker#{}", idx));
                })
                .exit_handler(move |_idx| {
                    profiler::unregister_thread();
                })
                .build();
            Arc::new(worker.unwrap())
        });
    let sampler = options.sampler;
    let namespace_alloc_by_client = options.namespace_alloc_by_client;

    // Ensure shared font keys exist within their own unique namespace so
    // that they don't accidentally collide across Renderer instances.
    let font_namespace = if namespace_alloc_by_client {
        options.shared_font_namespace.expect("Shared font namespace must be allocated by client")
    } else {
        RenderBackend::next_namespace_id()
    };
    let fonts = SharedFontResources::new(font_namespace);

    let blob_image_handler = options.blob_image_handler.take();
    let scene_builder_hooks = options.scene_builder_hooks;
    let rb_thread_name = format!("WRRenderBackend#{}", options.renderer_id.unwrap_or(0));
    let scene_thread_name = format!("WRSceneBuilder#{}", options.renderer_id.unwrap_or(0));
    let lp_scene_thread_name = format!("WRSceneBuilderLP#{}", options.renderer_id.unwrap_or(0));

    let glyph_rasterizer = GlyphRasterizer::new(
        workers,
        options.dedicated_glyph_raster_thread,
        device.supports_r8_texture_upload(),
    );

    let (scene_builder_channels, scene_tx) =
        SceneBuilderThreadChannels::new(api_tx.clone());

    let sb_fonts = fonts.clone();

    crate::thread_util::spawn_named(scene_thread_name.clone(), move || {
        register_thread_with_profiler(scene_thread_name.clone());
        profiler::register_thread(&scene_thread_name);

        let mut scene_builder = SceneBuilderThread::new(
            config,
            sb_fonts,
            make_size_of_ops(),
            scene_builder_hooks,
            scene_builder_channels,
        );
        scene_builder.run();

        profiler::unregister_thread();
    })?;

    let low_priority_scene_tx = if options.support_low_priority_transactions {
        let (low_priority_scene_tx, low_priority_scene_rx) = unbounded_channel();
        let lp_builder = LowPrioritySceneBuilderThread {
            rx: low_priority_scene_rx,
            tx: scene_tx.clone(),
            tile_pool: api::BlobTilePool::new(),
        };

        crate::thread_util::spawn_named(lp_scene_thread_name.clone(), move || {
            register_thread_with_profiler(lp_scene_thread_name.clone());
            profiler::register_thread(&lp_scene_thread_name);

            let mut scene_builder = lp_builder;
            scene_builder.run();

            profiler::unregister_thread();
        })?;

        low_priority_scene_tx
    } else {
        scene_tx.clone()
    };

    let rb_blob_handler = blob_image_handler
        .as_ref()
        .map(|handler| handler.create_similar());

    let texture_cache_config = options.texture_cache_config.clone();
    let mut picture_tile_size = options.picture_tile_size.unwrap_or(picture::TILE_SIZE_DEFAULT);
    // Clamp the picture tile size to reasonable values.
    picture_tile_size.width = picture_tile_size.width.max(128).min(4096);
    picture_tile_size.height = picture_tile_size.height.max(128).min(4096);

    let picture_texture_filter = if options.low_quality_pinch_zoom {
        TextureFilter::Linear
    } else {
        TextureFilter::Nearest
    };

    let render_backend_hooks = options.render_backend_hooks.take();

    let chunk_pool = options.chunk_pool.take().unwrap_or_else(|| {
        Arc::new(ChunkPool::new())
    });

    let rb_scene_tx = scene_tx.clone();
    let rb_fonts = fonts.clone();
    let enable_multithreading = options.enable_multithreading;
    crate::thread_util::spawn_named(rb_thread_name.clone(), move || {
        if let Some(hooks) = render_backend_hooks {
            hooks.init_thread();
        }
        register_thread_with_profiler(rb_thread_name.clone());
        profiler::register_thread(&rb_thread_name);

        let texture_cache = TextureCache::new(
            max_internal_texture_size,
            image_tiling_threshold,
            color_cache_formats,
            swizzle_settings,
            &texture_cache_config,
        );

        let picture_textures = PictureTextures::new(
            picture_tile_size,
            picture_texture_filter,
        );

        let glyph_cache = GlyphCache::new();

        let mut resource_cache = ResourceCache::new(
            texture_cache,
            picture_textures,
            glyph_rasterizer,
            glyph_cache,
            rb_fonts,
            rb_blob_handler,
        );

        resource_cache.enable_multithreading(enable_multithreading);

        let mut backend = RenderBackend::new(
            api_rx,
            result_tx,
            rb_scene_tx,
            resource_cache,
            chunk_pool,
            backend_notifier,
            config,
            sampler,
            make_size_of_ops(),
            debug_flags,
            namespace_alloc_by_client,
        );
        backend.run();
        profiler::unregister_thread();
    })?;

    let gpu_profiler = device.create_gpu_profiler(options.enable_gpu_markers);
    #[cfg(feature = "capture")]
    let read_fbo = device.create_fbo();

    let mut renderer = Renderer {
        result_rx,
        api_tx: api_tx.clone(),
        device: Some(device),
        active_documents: FastHashMap::default(),
        pending_texture_updates: Vec::new(),
        pending_texture_cache_updates: false,
        pending_native_surface_updates: Vec::new(),
        pending_gpu_cache_updates: Vec::new(),
        pending_gpu_cache_clear: false,
        pending_shader_updates: Vec::new(),
        shaders: Some(shaders),
        debug: debug::LazyInitializedDebugRenderer::new(),
        debug_flags: DebugFlags::empty(),
        profile: TransactionProfile::new(),
        frame_counter: 0,
        resource_upload_time: 0.0,
        gpu_cache_upload_time: 0.0,
        profiler: Profiler::new(),
        max_recorded_profiles: options.max_recorded_profiles,
        clear_color: options.clear_color,
        enable_clear_scissor,
        enable_advanced_blend_barriers: !ext_blend_equation_advanced_coherent,
        clear_caches_with_quads: options.clear_caches_with_quads,
        clear_alpha_targets_with_quads,
        last_time: 0,
        gpu_profiler,
        vaos,
        vertex_data_textures,
        pipeline_info: PipelineInfo::default(),
        aux_textures,
        external_image_handler: None,
        size_of_ops: make_size_of_ops(),
        cpu_profiles: VecDeque::new(),
        gpu_profiles: VecDeque::new(),
        gpu_cache_texture,
        gpu_cache_debug_chunks: Vec::new(),
        gpu_cache_frame_id: FrameId::INVALID,
        gpu_cache_overflow: false,
        upload_state,
        texture_resolver,
        renderer_errors: Vec::new(),
        async_frame_recorder: None,
        async_screenshots: None,
        #[cfg(feature = "capture")]
        read_fbo,
        #[cfg(feature = "replay")]
        owned_external_images: FastHashMap::default(),
        notifications: Vec::new(),
        device_size: None,
        cursor_position: DeviceIntPoint::zero(),
        shared_texture_cache_cleared: false,
        documents_seen: FastHashSet::default(),
        force_redraw: true,
        compositor_config: options.compositor_config,
        current_compositor_kind: compositor_kind,
        allocated_native_surfaces: FastHashSet::default(),
        debug_overlay_state: DebugOverlayState::new(),
        buffer_damage_tracker: BufferDamageTracker::default(),
        max_primitive_instance_count,
        enable_instancing: options.enable_instancing,
        consecutive_oom_frames: 0,
        target_frame_publish_id: None,
        pending_result_msg: None,
        layer_compositor_frame_state_in_prev_frame: None,
        #[cfg(feature = "debugger")]
        debugger: Debugger::new(),
        #[cfg(feature = "wgpu_backend")]
        wgpu_device: None,
        #[cfg(feature = "wgpu_backend")]
        wgpu_texture_cache: FastHashMap::default(),
        #[cfg(feature = "wgpu_backend")]
        wgpu_gpu_cache: super::WgpuGpuCacheState::new(),
        #[cfg(feature = "wgpu_backend")]
        wgpu_dither_texture: None,
        #[cfg(feature = "wgpu_backend")]
        wgpu_frame_data: None,
        #[cfg(feature = "wgpu_backend")]
        wgpu_readback_texture: None,
        #[cfg(feature = "wgpu_backend")]
        wgpu_host_render_target: None,
        #[cfg(feature = "wgpu_backend")]
        wgpu_frame_view_override: None,
        #[cfg(feature = "wgpu_backend")]
        wgpu_external_image_handler: None,
        #[cfg(feature = "wgpu_backend")]
        wgpu_external_texture_id_counter: 0xF000_0000,
    };

    // We initially set the flags to default and then now call set_debug_flags
    // to ensure any potential transition when enabling a flag is run.
    renderer.set_debug_flags(debug_flags);
    renderer.profiler.set_ui("Default");

    let sender = RenderApiSender::new(
        api_tx,
        scene_tx,
        low_priority_scene_tx,
        blob_image_handler,
        fonts,
    );

    #[cfg(feature = "debugger")]
    if options.enable_debugger {
        let api = if namespace_alloc_by_client {
            sender.create_api_by_client(IdNamespace::DEBUGGER)
        } else {
            sender.create_api()
        };
        crate::debugger::start(api);
    }

    Ok((renderer, sender))
}

/// How the wgpu device should be obtained.
#[cfg(feature = "wgpu_backend")]
pub(crate) enum WgpuInit {
    /// WebRender creates its own adapter + device, optionally with a surface.
    /// Not available on wasm — use `SharedDevice` instead.
    #[cfg(feature = "wgpu_native")]
    CreateDevice {
        instance: Option<wgpu::Instance>,
        surface: Option<wgpu::Surface<'static>>,
        width: u32,
        height: u32,
        present_mode: wgpu::PresentMode,
    },
    /// The host application provides a pre-existing device + queue.
    SharedDevice {
        device: wgpu::Device,
        queue: wgpu::Queue,
    },
}

/// Creates a wgpu-only Renderer instance with no GL device.
///
/// The GL draw loop is not used; the Renderer's `device` field is `None`.
/// All subsystems use their wgpu enum variants (no-op stubs for now).
/// The backend threads (scene builder, render backend) are set up identically
/// to the GL path — they are device-independent.
#[cfg(feature = "wgpu_backend")]
pub fn create_webrender_instance_wgpu(
    notifier: Box<dyn RenderNotifier>,
    mut options: WebRenderOptions,
    init: WgpuInit,
) -> Result<(Renderer, RenderApiSender), RendererError> {
    use crate::device::WgpuDevice;

    if !wr_has_been_initialized() {
        #[cfg(feature = "profiler")]
        unsafe {
            if let Ok(ref tracy_path) = std::env::var("WR_TRACY_PATH") {
                let ok = tracy_rs::load(tracy_path);
                info!("Load tracy from {} -> {}", tracy_path, ok);
            }
        }

        register_thread_with_profiler("Compositor".to_owned());
    }

    HAS_BEEN_INITIALIZED.store(true, Ordering::SeqCst);

    let (api_tx, api_rx) = unbounded_channel();
    let (result_tx, result_rx) = unbounded_channel();

    // Create or adopt the wgpu device.
    let mut wgpu_device = match init {
        #[cfg(feature = "wgpu_native")]
        WgpuInit::CreateDevice { instance, surface, width, height, present_mode } => {
            if let Some(surface) = surface {
                let inst = instance.as_ref()
                    .expect("wgpu Instance must be provided when surface is Some");
                WgpuDevice::new_with_surface(inst, surface, width, height, present_mode, options.pipeline_cache_dir.as_deref())
                    .ok_or(RendererError::UnsupportedBackend("no wgpu adapter available for surface"))?
            } else {
                WgpuDevice::new_headless(options.pipeline_cache_dir.as_deref())
                    .ok_or(RendererError::UnsupportedBackend("no wgpu adapter available"))?
            }
        }
        WgpuInit::SharedDevice { device, queue } => {
            WgpuDevice::from_shared_device(device, queue)
        }
    };

    // Query real device limits from the wgpu device.
    let max_texture_size: i32 = wgpu_device.max_texture_size();
    // Enable subpixel AA only when the adapter supports dual-source blending
    // and the caller has not disabled AA. DUAL_SOURCE_BLENDING is a standard
    // wgpu feature (available on Vulkan, DX12, Metal) — not nightly.
    let use_dual_source_blending = options.enable_aa
        && wgpu_device.supports_dual_source_blending();
    let ext_blend_equation_advanced = false;
    let ext_blend_equation_advanced_coherent = false;
    let enable_clear_scissor = options.enable_clear_scissor.unwrap_or(false);
    let is_software = false;

    const MIN_TEXTURE_SIZE: i32 = 2048;
    let mut max_internal_texture_size = max_texture_size;
    if let Some(internal_limit) = options.max_internal_texture_size {
        assert!(internal_limit >= MIN_TEXTURE_SIZE);
        max_internal_texture_size = max_internal_texture_size.min(internal_limit);
    }

    let image_tiling_threshold = options.image_tiling_threshold
        .min(max_internal_texture_size);

    // When gl_backend is also compiled alongside wgpu_backend, the Renderer
    // struct still has GL-carrier fields.  Initialize them to the wgpu no-op
    // variant so the struct literal compiles; these fields are never accessed
    // on the wgpu render path (render() returns early via render_wgpu()).
    #[cfg(feature = "gl_backend")]
    let vaos = vertex::RendererVaoState::new_wgpu();
    #[cfg(feature = "gl_backend")]
    let vertex_data_textures = vertex::RendererVertexData::new_wgpu();
    #[cfg(feature = "gl_backend")]
    let upload_state = RendererUploadState::new_wgpu();
    #[cfg(feature = "gl_backend")]
    let gpu_cache_texture = gpu_cache::RendererGpuCache::new_wgpu();
    let dither_tex = if options.enable_dithering {
        Some(create_dither_matrix_texture(&mut wgpu_device))
    } else {
        None
    };
    #[cfg(feature = "gl_backend")]
    let aux_textures = super::RendererAuxTextures::Wgpu(super::WgpuRendererAuxTextures);
    #[cfg(feature = "gl_backend")]
    let texture_resolver = TextureResolver::new_without_gl();
    #[cfg(feature = "gl_backend")]
    let gpu_profiler = GpuProfiler::new_noop();

    let color_cache_formats = crate::device::TextureFormatPair {
        internal: ImageFormat::BGRA8,
        external: ImageFormat::BGRA8,
    };
    let swizzle_settings = None; // wgpu handles format conversion natively

    let max_primitive_instance_count =
        WebRenderOptions::MAX_INSTANCE_BUFFER_SIZE / mem::size_of::<PrimitiveInstanceData>();

    let backend_notifier = notifier.clone();

    let prefer_subpixel_aa = options.enable_subpixel_aa && use_dual_source_blending;
    let default_font_render_mode = match (options.enable_aa, prefer_subpixel_aa) {
        (true, true) => FontRenderMode::Subpixel,
        (true, false) => FontRenderMode::Alpha,
        (false, _) => FontRenderMode::Mono,
    };

    let compositor_kind = match options.compositor_config {
        CompositorConfig::Draw { max_partial_present_rects, draw_previous_partial_present_regions, .. } => {
            CompositorKind::Draw { max_partial_present_rects, draw_previous_partial_present_regions }
        }
        #[cfg(feature = "gl_backend")]
        CompositorConfig::Native { .. } => {
            // The native OS compositor (DirectComposition / Core Animation) is
            // currently GL-only: every Compositor trait method takes &mut Device
            // (the GL device).  A proper wgpu implementation would need either:
            //   a) A parallel WgpuCompositor trait with &mut WgpuDevice methods, plus
            //      platform interop to share wgpu HAL textures with the OS compositor.
            //   b) A generic Compositor trait refactor that abstracts over both backends.
            //
            // Until that work is done, fall back to the software draw compositor so
            // the caller gets a functional renderer rather than a hard error.  The
            // native compositor box is dropped here; if it implements Drop it should
            // release any OS resources it holds.
            warn!(
                "CompositorConfig::Native is not yet supported in wgpu-only mode; \
                 falling back to CompositorKind::Draw. Native OS compositing (DirectComposition, \
                 Core Animation) requires GL device interop that has not been ported to wgpu."
            );
            CompositorKind::Draw {
                max_partial_present_rects: 0,
                draw_previous_partial_present_regions: false,
            }
        }
        CompositorConfig::Layer { .. } => {
            CompositorKind::Layer {}
        }
    };

    let config = FrameBuilderConfig {
        default_font_render_mode,
        dual_source_blending_is_supported: use_dual_source_blending,
        // wgpu path: BrushImage has no dual-source shader variant yet,
        // so Multiply must use the two-pass brush_mix_blend fallback.
        dual_source_mix_blend_supported: false,
        testing: options.testing,
        gpu_supports_fast_clears: options.gpu_supports_fast_clears,
        gpu_supports_advanced_blend: ext_blend_equation_advanced,
        advanced_blend_is_coherent: ext_blend_equation_advanced_coherent,
        gpu_supports_render_target_partial_update: true,
        external_images_require_copy: false,
        batch_lookback_count: WebRenderOptions::BATCH_LOOKBACK_COUNT,
        background_color: Some(options.clear_color),
        compositor_kind,
        tile_size_override: None,
        max_surface_override: None,
        max_depth_ids: 1 << 22, // 24-bit depth minus 2 reserve bits, matching GL path
        max_target_size: max_internal_texture_size,
        force_invalidation: false,
        is_software,
        low_quality_pinch_zoom: options.low_quality_pinch_zoom,
        max_shared_surface_size: options.max_shared_surface_size,
        enable_dithering: options.enable_dithering,
        precise_linear_gradients: options.precise_linear_gradients,
        precise_radial_gradients: options.precise_radial_gradients,
        precise_conic_gradients: options.precise_conic_gradients,
    };
    info!("WR (wgpu) {:?}", config);

    let debug_flags = options.debug_flags;
    let size_of_op = options.size_of_op;
    let enclosing_size_of_op = options.enclosing_size_of_op;
    let make_size_of_ops =
        move || size_of_op.map(|o| MallocSizeOfOps::new(o, enclosing_size_of_op));
    let workers = options
        .workers
        .take()
        .unwrap_or_else(|| {
            let worker = ThreadPoolBuilder::new()
                .thread_name(|idx| format!("WRWorker#{}", idx))
                .start_handler(move |idx| {
                    register_thread_with_profiler(format!("WRWorker#{}", idx));
                    profiler::register_thread(&format!("WRWorker#{}", idx));
                })
                .exit_handler(move |_idx| {
                    profiler::unregister_thread();
                })
                .build();
            Arc::new(worker.unwrap())
        });
    let sampler = options.sampler;
    let namespace_alloc_by_client = options.namespace_alloc_by_client;

    let font_namespace = if namespace_alloc_by_client {
        options.shared_font_namespace.expect("Shared font namespace must be allocated by client")
    } else {
        RenderBackend::next_namespace_id()
    };
    let fonts = SharedFontResources::new(font_namespace);

    let blob_image_handler = options.blob_image_handler.take();
    let scene_builder_hooks = options.scene_builder_hooks;
    let rb_thread_name = format!("WRRenderBackend#{}", options.renderer_id.unwrap_or(0));
    let scene_thread_name = format!("WRSceneBuilder#{}", options.renderer_id.unwrap_or(0));
    let lp_scene_thread_name = format!("WRSceneBuilderLP#{}", options.renderer_id.unwrap_or(0));

    let glyph_rasterizer = GlyphRasterizer::new(
        workers,
        options.dedicated_glyph_raster_thread,
        true, // wgpu supports R8 texture upload
    );

    let (scene_builder_channels, scene_tx) =
        SceneBuilderThreadChannels::new(api_tx.clone());

    let sb_fonts = fonts.clone();

    crate::thread_util::spawn_named(scene_thread_name.clone(), move || {
        register_thread_with_profiler(scene_thread_name.clone());
        profiler::register_thread(&scene_thread_name);

        let mut scene_builder = SceneBuilderThread::new(
            config,
            sb_fonts,
            make_size_of_ops(),
            scene_builder_hooks,
            scene_builder_channels,
        );
        scene_builder.run();

        profiler::unregister_thread();
    })?;

    let low_priority_scene_tx = if options.support_low_priority_transactions {
        let (low_priority_scene_tx, low_priority_scene_rx) = unbounded_channel();
        let lp_builder = LowPrioritySceneBuilderThread {
            rx: low_priority_scene_rx,
            tx: scene_tx.clone(),
            tile_pool: api::BlobTilePool::new(),
        };

        crate::thread_util::spawn_named(lp_scene_thread_name.clone(), move || {
            register_thread_with_profiler(lp_scene_thread_name.clone());
            profiler::register_thread(&lp_scene_thread_name);

            let mut scene_builder = lp_builder;
            scene_builder.run();

            profiler::unregister_thread();
        })?;

        low_priority_scene_tx
    } else {
        scene_tx.clone()
    };

    let rb_blob_handler = blob_image_handler
        .as_ref()
        .map(|handler| handler.create_similar());

    let texture_cache_config = options.texture_cache_config.clone();
    let mut picture_tile_size = options.picture_tile_size.unwrap_or(picture::TILE_SIZE_DEFAULT);
    picture_tile_size.width = picture_tile_size.width.max(128).min(4096);
    picture_tile_size.height = picture_tile_size.height.max(128).min(4096);

    let picture_texture_filter = if options.low_quality_pinch_zoom {
        TextureFilter::Linear
    } else {
        TextureFilter::Nearest
    };

    let render_backend_hooks = options.render_backend_hooks.take();

    let chunk_pool = options.chunk_pool.take().unwrap_or_else(|| {
        Arc::new(ChunkPool::new())
    });

    let rb_scene_tx = scene_tx.clone();
    let rb_fonts = fonts.clone();
    let enable_multithreading = options.enable_multithreading;
    crate::thread_util::spawn_named(rb_thread_name.clone(), move || {
        if let Some(hooks) = render_backend_hooks {
            hooks.init_thread();
        }
        register_thread_with_profiler(rb_thread_name.clone());
        profiler::register_thread(&rb_thread_name);

        let texture_cache = TextureCache::new(
            max_internal_texture_size,
            image_tiling_threshold,
            color_cache_formats,
            swizzle_settings,
            &texture_cache_config,
        );

        let picture_textures = PictureTextures::new(
            picture_tile_size,
            picture_texture_filter,
        );

        let glyph_cache = GlyphCache::new();

        let mut resource_cache = ResourceCache::new(
            texture_cache,
            picture_textures,
            glyph_rasterizer,
            glyph_cache,
            rb_fonts,
            rb_blob_handler,
        );

        resource_cache.enable_multithreading(enable_multithreading);

        let mut backend = RenderBackend::new(
            api_rx,
            result_tx,
            rb_scene_tx,
            resource_cache,
            chunk_pool,
            backend_notifier,
            config,
            sampler,
            make_size_of_ops(),
            debug_flags,
            namespace_alloc_by_client,
        );
        backend.run();
        profiler::unregister_thread();
    })?;

    let mut renderer = Renderer {
        result_rx,
        api_tx: api_tx.clone(),
        #[cfg(feature = "gl_backend")]
        device: None,
        active_documents: FastHashMap::default(),
        pending_texture_updates: Vec::new(),
        pending_texture_cache_updates: false,
        pending_native_surface_updates: Vec::new(),
        pending_gpu_cache_updates: Vec::new(),
        pending_gpu_cache_clear: false,
        pending_shader_updates: Vec::new(),
        #[cfg(feature = "gl_backend")]
        shaders: None,
        #[cfg(feature = "gl_backend")]
        debug: debug::LazyInitializedDebugRenderer::new(),
        debug_flags: DebugFlags::empty(),
        profile: TransactionProfile::new(),
        frame_counter: 0,
        resource_upload_time: 0.0,
        gpu_cache_upload_time: 0.0,
        profiler: Profiler::new(),
        max_recorded_profiles: options.max_recorded_profiles,
        clear_color: options.clear_color,
        enable_clear_scissor,
        enable_advanced_blend_barriers: !ext_blend_equation_advanced_coherent,
        clear_caches_with_quads: options.clear_caches_with_quads,
        clear_alpha_targets_with_quads: false,
        last_time: 0,
        #[cfg(feature = "gl_backend")]
        gpu_profiler,
        #[cfg(feature = "gl_backend")]
        vaos,
        #[cfg(feature = "gl_backend")]
        vertex_data_textures,
        pipeline_info: PipelineInfo::default(),
        #[cfg(feature = "gl_backend")]
        aux_textures,
        external_image_handler: None,
        size_of_ops: make_size_of_ops(),
        cpu_profiles: VecDeque::new(),
        gpu_profiles: VecDeque::new(),
        #[cfg(feature = "gl_backend")]
        gpu_cache_texture,
        gpu_cache_debug_chunks: Vec::new(),
        gpu_cache_frame_id: FrameId::INVALID,
        gpu_cache_overflow: false,
        #[cfg(feature = "gl_backend")]
        upload_state,
        #[cfg(feature = "gl_backend")]
        texture_resolver,
        renderer_errors: Vec::new(),
        #[cfg(feature = "gl_backend")]
        async_frame_recorder: None,
        #[cfg(feature = "gl_backend")]
        async_screenshots: None,
        #[cfg(feature = "capture")]
        read_fbo: crate::device::FBOId::default(),
        #[cfg(feature = "replay")]
        owned_external_images: FastHashMap::default(),
        notifications: Vec::new(),
        device_size: None,
        cursor_position: DeviceIntPoint::zero(),
        shared_texture_cache_cleared: false,
        documents_seen: FastHashSet::default(),
        force_redraw: true,
        compositor_config: options.compositor_config,
        current_compositor_kind: compositor_kind,
        allocated_native_surfaces: FastHashSet::default(),
        debug_overlay_state: DebugOverlayState::new(),
        buffer_damage_tracker: BufferDamageTracker::default(),
        max_primitive_instance_count,
        enable_instancing: options.enable_instancing,
        consecutive_oom_frames: 0,
        target_frame_publish_id: None,
        pending_result_msg: None,
        layer_compositor_frame_state_in_prev_frame: None,
        #[cfg(feature = "debugger")]
        debugger: Debugger::new(),
        wgpu_device: Some(wgpu_device),
        wgpu_texture_cache: FastHashMap::default(),
        wgpu_gpu_cache: super::WgpuGpuCacheState::new(),
        wgpu_dither_texture: dither_tex,
        wgpu_frame_data: None,
        wgpu_readback_texture: None,
        wgpu_host_render_target: None,
        wgpu_frame_view_override: None,
        #[cfg(feature = "wgpu_backend")]
        wgpu_external_image_handler: None,
        #[cfg(feature = "wgpu_backend")]
        wgpu_external_texture_id_counter: 0xF000_0000,
    };

    renderer.set_debug_flags(debug_flags);
    renderer.profiler.set_ui("Default");

    let sender = RenderApiSender::new(
        api_tx,
        scene_tx,
        low_priority_scene_tx,
        blob_image_handler,
        fonts,
    );

    Ok((renderer, sender))
}
