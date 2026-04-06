/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! wgpu backend device — Stages 5-6a.
//!
//! Stage 5: creates `wgpu::RenderPipeline` objects for all generated WGSL
//! shader variants.
//! Stage 6a: proves end-to-end rendering with bind groups, vertex/index
//! buffers, render pass encoding, and pixel readback.

use std::collections::HashMap;

use api::{ImageBufferKind, ImageFormat};
use api::units::DeviceIntRect;

use super::{GpuDevice, GpuFrameId, Texel, TextureFilter};
use crate::internal_types::RenderTargetInfo;
use crate::shader_source::WGSL_SHADERS;

/// Reinterpret a typed slice as raw bytes.
///
/// Safe for WebRender GPU types — they are all `repr(C)` structs of f32/i32
/// with no padding, or plain `[f32; N]` / `[i32; N]` arrays.
pub(crate) fn as_byte_slice<T>(data: &[T]) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(
            data.as_ptr() as *const u8,
            std::mem::size_of_val(data),
        )
    }
}

/// A wgpu-backed texture handle.
pub struct WgpuTexture {
    pub(crate) texture: wgpu::Texture,
    format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
}

impl WgpuTexture {
    /// Create a default texture view for this texture.
    pub fn create_view(&self) -> wgpu::TextureView {
        self.texture.create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// Bytes per pixel for this texture's format.
    pub fn bytes_per_pixel(&self) -> u32 {
        wgpu_format_bytes_per_pixel(self.format)
    }

    /// The texture format.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }
}

/// A wgpu-backed shader pipeline.
pub struct WgpuProgram {
    pipeline: wgpu::RenderPipeline,
}

/// Typed shader variant key for the wgpu pipeline cache.
///
/// Each variant maps to exactly one `(shader_name, config)` string pair in
/// `WGSL_SHADERS`.  Using a flat enum instead of stringly-typed keys gives us
/// compiler exhaustiveness checks and prevents the class of bugs where a
/// wrong config string silently selects the wrong shader (e.g. the DPR=2
/// `GLYPH_TRANSFORM` bug).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WgpuShaderVariant {
    // -- Brush shaders (opaque + alpha) --
    BrushSolid,
    BrushSolidAlpha,
    BrushImage,
    BrushImageAlpha,
    BrushImageRepeat,
    BrushImageRepeatAlpha,
    BrushBlend,
    BrushBlendAlpha,
    BrushMixBlend,
    BrushMixBlendAlpha,
    BrushLinearGradient,
    BrushLinearGradientAlpha,
    BrushOpacity,
    BrushOpacityAlpha,
    BrushYuvImage,
    BrushYuvImageAlpha,

    // -- Text --
    PsTextRun,
    PsTextRunGlyphTransform,

    // -- Quad shaders --
    PsQuadTextured,
    PsQuadGradient,
    PsQuadRadialGradient,
    PsQuadConicGradient,
    PsQuadMask,
    PsQuadMaskFastPath,

    // -- Prim --
    PsSplitComposite,

    // -- Clip shaders --
    CsClipRectangle,
    CsClipRectangleFastPath,
    CsClipBoxShadow,

    // -- Cache task shaders --
    CsBorderSolid,
    CsBorderSegment,
    CsLineDecoration,
    CsFastLinearGradient,
    CsLinearGradient,
    CsRadialGradient,
    CsConicGradient,
    CsBlurColor,
    CsBlurAlpha,
    CsScale,
    CsSvgFilter,
    CsSvgFilterNode,

    // -- Composite --
    Composite,
    CompositeFastPath,
    CompositeYuv,
    CompositeFastPathYuv,

    // -- Debug --
    DebugColor,
    DebugFont,

    // -- Utility --
    PsClear,
    PsCopy,
}

impl WgpuShaderVariant {
    /// Map this variant to the `(shader_name, config)` string pair used by
    /// the build-time `WGSL_SHADERS` HashMap.
    pub fn shader_key(self) -> (&'static str, &'static str) {
        match self {
            Self::BrushSolid                => ("brush_solid", ""),
            Self::BrushSolidAlpha           => ("brush_solid", "ALPHA_PASS"),
            Self::BrushImage                => ("brush_image", "TEXTURE_2D"),
            Self::BrushImageAlpha           => ("brush_image", "ALPHA_PASS,TEXTURE_2D"),
            Self::BrushImageRepeat          => ("brush_image", "ANTIALIASING,REPETITION,TEXTURE_2D"),
            Self::BrushImageRepeatAlpha     => ("brush_image", "ALPHA_PASS,ANTIALIASING,REPETITION,TEXTURE_2D"),
            Self::BrushBlend                => ("brush_blend", ""),
            Self::BrushBlendAlpha           => ("brush_blend", "ALPHA_PASS"),
            Self::BrushMixBlend             => ("brush_mix_blend", ""),
            Self::BrushMixBlendAlpha        => ("brush_mix_blend", "ALPHA_PASS"),
            Self::BrushLinearGradient       => ("brush_linear_gradient", "DITHERING"),
            Self::BrushLinearGradientAlpha  => ("brush_linear_gradient", "ALPHA_PASS,DITHERING"),
            Self::BrushOpacity              => ("brush_opacity", ""),
            Self::BrushOpacityAlpha         => ("brush_opacity", "ALPHA_PASS"),
            Self::BrushYuvImage             => ("brush_yuv_image", "TEXTURE_2D,YUV"),
            Self::BrushYuvImageAlpha        => ("brush_yuv_image", "ALPHA_PASS,TEXTURE_2D,YUV"),
            Self::PsTextRun                 => ("ps_text_run", "ALPHA_PASS,TEXTURE_2D"),
            Self::PsTextRunGlyphTransform   => ("ps_text_run", "ALPHA_PASS,GLYPH_TRANSFORM,TEXTURE_2D"),
            Self::PsQuadTextured            => ("ps_quad_textured", ""),
            Self::PsQuadGradient            => ("ps_quad_gradient", "DITHERING"),
            Self::PsQuadRadialGradient      => ("ps_quad_radial_gradient", "DITHERING"),
            Self::PsQuadConicGradient       => ("ps_quad_conic_gradient", "DITHERING"),
            Self::PsQuadMask                => ("ps_quad_mask", ""),
            Self::PsQuadMaskFastPath        => ("ps_quad_mask", "FAST_PATH"),
            Self::PsSplitComposite          => ("ps_split_composite", ""),
            Self::CsClipRectangle           => ("cs_clip_rectangle", ""),
            Self::CsClipRectangleFastPath   => ("cs_clip_rectangle", "FAST_PATH"),
            Self::CsClipBoxShadow           => ("cs_clip_box_shadow", "TEXTURE_2D"),
            Self::CsBorderSolid             => ("cs_border_solid", ""),
            Self::CsBorderSegment           => ("cs_border_segment", ""),
            Self::CsLineDecoration          => ("cs_line_decoration", ""),
            Self::CsFastLinearGradient      => ("cs_fast_linear_gradient", ""),
            Self::CsLinearGradient          => ("cs_linear_gradient", "DITHERING"),
            Self::CsRadialGradient          => ("cs_radial_gradient", "DITHERING"),
            Self::CsConicGradient           => ("cs_conic_gradient", "DITHERING"),
            Self::CsBlurColor               => ("cs_blur", "COLOR_TARGET"),
            Self::CsBlurAlpha               => ("cs_blur", "ALPHA_TARGET"),
            Self::CsScale                   => ("cs_scale", "TEXTURE_2D"),
            Self::CsSvgFilter              => ("cs_svg_filter", ""),
            Self::CsSvgFilterNode          => ("cs_svg_filter_node", ""),
            Self::Composite                 => ("composite", "TEXTURE_2D"),
            Self::CompositeFastPath         => ("composite", "FAST_PATH,TEXTURE_2D"),
            Self::CompositeYuv              => ("composite", "TEXTURE_2D,YUV"),
            Self::CompositeFastPathYuv      => ("composite", "FAST_PATH,TEXTURE_2D,YUV"),
            Self::DebugColor                => ("debug_color", ""),
            Self::DebugFont                 => ("debug_font", ""),
            Self::PsClear                   => ("ps_clear", ""),
            Self::PsCopy                    => ("ps_copy", ""),
        }
    }

    /// Reverse-map a `(shader_name, config)` string pair from `WGSL_SHADERS`
    /// back to a typed variant.  Returns `None` for debug-overdraw variants
    /// and other configs not used at runtime by the wgpu path.
    pub fn from_shader_key(name: &str, config: &str) -> Option<Self> {
        Some(match (name, config) {
            ("brush_solid", "")                                     => Self::BrushSolid,
            ("brush_solid", "ALPHA_PASS")                           => Self::BrushSolidAlpha,
            ("brush_image", "TEXTURE_2D")                           => Self::BrushImage,
            ("brush_image", "ALPHA_PASS,TEXTURE_2D")                => Self::BrushImageAlpha,
            ("brush_image", "ANTIALIASING,REPETITION,TEXTURE_2D")   => Self::BrushImageRepeat,
            ("brush_image", "ALPHA_PASS,ANTIALIASING,REPETITION,TEXTURE_2D") => Self::BrushImageRepeatAlpha,
            ("brush_blend", "")                                     => Self::BrushBlend,
            ("brush_blend", "ALPHA_PASS")                           => Self::BrushBlendAlpha,
            ("brush_mix_blend", "")                                 => Self::BrushMixBlend,
            ("brush_mix_blend", "ALPHA_PASS")                       => Self::BrushMixBlendAlpha,
            ("brush_linear_gradient", "DITHERING")                  => Self::BrushLinearGradient,
            ("brush_linear_gradient", "ALPHA_PASS,DITHERING")       => Self::BrushLinearGradientAlpha,
            ("brush_opacity", "")                                   => Self::BrushOpacity,
            ("brush_opacity", "ALPHA_PASS")                         => Self::BrushOpacityAlpha,
            ("brush_yuv_image", "TEXTURE_2D,YUV")                   => Self::BrushYuvImage,
            ("brush_yuv_image", "ALPHA_PASS,TEXTURE_2D,YUV")        => Self::BrushYuvImageAlpha,
            ("ps_text_run", "ALPHA_PASS,TEXTURE_2D")                => Self::PsTextRun,
            ("ps_text_run", "ALPHA_PASS,GLYPH_TRANSFORM,TEXTURE_2D") => Self::PsTextRunGlyphTransform,
            ("ps_quad_textured", "")                                => Self::PsQuadTextured,
            ("ps_quad_gradient", "DITHERING")                       => Self::PsQuadGradient,
            ("ps_quad_radial_gradient", "DITHERING")                => Self::PsQuadRadialGradient,
            ("ps_quad_conic_gradient", "DITHERING")                 => Self::PsQuadConicGradient,
            ("ps_quad_mask", "")                                    => Self::PsQuadMask,
            ("ps_quad_mask", "FAST_PATH")                           => Self::PsQuadMaskFastPath,
            ("ps_split_composite", "")                              => Self::PsSplitComposite,
            ("cs_clip_rectangle", "")                               => Self::CsClipRectangle,
            ("cs_clip_rectangle", "FAST_PATH")                      => Self::CsClipRectangleFastPath,
            ("cs_clip_box_shadow", "TEXTURE_2D")                    => Self::CsClipBoxShadow,
            ("cs_border_solid", "")                                 => Self::CsBorderSolid,
            ("cs_border_segment", "")                               => Self::CsBorderSegment,
            ("cs_line_decoration", "")                              => Self::CsLineDecoration,
            ("cs_fast_linear_gradient", "")                         => Self::CsFastLinearGradient,
            ("cs_linear_gradient", "DITHERING")                     => Self::CsLinearGradient,
            ("cs_radial_gradient", "DITHERING")                     => Self::CsRadialGradient,
            ("cs_conic_gradient", "DITHERING")                      => Self::CsConicGradient,
            ("cs_blur", "COLOR_TARGET")                             => Self::CsBlurColor,
            ("cs_blur", "ALPHA_TARGET")                             => Self::CsBlurAlpha,
            ("cs_scale", "TEXTURE_2D")                              => Self::CsScale,
            ("cs_svg_filter", "")                                      => Self::CsSvgFilter,
            ("cs_svg_filter_node", "")                                 => Self::CsSvgFilterNode,
            ("composite", "TEXTURE_2D")                             => Self::Composite,
            ("composite", "FAST_PATH,TEXTURE_2D")                      => Self::CompositeFastPath,
            ("composite", "TEXTURE_2D,YUV")                            => Self::CompositeYuv,
            ("composite", "FAST_PATH,TEXTURE_2D,YUV")                  => Self::CompositeFastPathYuv,
            ("debug_color", "")                                     => Self::DebugColor,
            ("debug_font", "")                                      => Self::DebugFont,
            ("ps_clear", "")                                        => Self::PsClear,
            ("ps_copy", "")                                         => Self::PsCopy,
            _ => return None,
        })
    }

    /// Returns the instance data layout for this shader variant, or `None`
    /// for shaders that use a single vertex buffer (debug shaders, etc.).
    fn instance_layout(self) -> Option<&'static [(&'static str, wgpu::VertexFormat)]> {
        match self {
            Self::Composite | Self::CompositeFastPath
            | Self::CompositeYuv | Self::CompositeFastPathYuv => Some(COMPOSITE_INSTANCE_LAYOUT),
            Self::CsClipRectangle | Self::CsClipRectangleFastPath => Some(CLIP_RECT_INSTANCE_LAYOUT),
            Self::CsClipBoxShadow => Some(CLIP_BOX_SHADOW_INSTANCE_LAYOUT),
            Self::CsBlurColor | Self::CsBlurAlpha => Some(BLUR_INSTANCE_LAYOUT),
            Self::CsScale => Some(SCALE_INSTANCE_LAYOUT),
            Self::CsSvgFilter => Some(SVG_FILTER_INSTANCE_LAYOUT),
            Self::CsSvgFilterNode => Some(SVG_FILTER_NODE_INSTANCE_LAYOUT),
            Self::CsBorderSolid | Self::CsBorderSegment => Some(BORDER_INSTANCE_LAYOUT),
            Self::CsLineDecoration => Some(LINE_DECORATION_INSTANCE_LAYOUT),
            Self::CsFastLinearGradient => Some(FAST_LINEAR_GRADIENT_INSTANCE_LAYOUT),
            Self::CsLinearGradient => Some(LINEAR_GRADIENT_INSTANCE_LAYOUT),
            Self::CsRadialGradient => Some(RADIAL_GRADIENT_INSTANCE_LAYOUT),
            Self::CsConicGradient => Some(CONIC_GRADIENT_INSTANCE_LAYOUT),
            Self::PsQuadMask | Self::PsQuadMaskFastPath => Some(MASK_INSTANCE_LAYOUT),
            // All brush_*, ps_text_run*, ps_quad_* (non-mask), ps_split_composite
            Self::BrushSolid | Self::BrushSolidAlpha
            | Self::BrushImage | Self::BrushImageAlpha
            | Self::BrushImageRepeat | Self::BrushImageRepeatAlpha
            | Self::BrushBlend | Self::BrushBlendAlpha
            | Self::BrushMixBlend | Self::BrushMixBlendAlpha
            | Self::BrushLinearGradient | Self::BrushLinearGradientAlpha
            | Self::BrushOpacity | Self::BrushOpacityAlpha
            | Self::BrushYuvImage | Self::BrushYuvImageAlpha
            | Self::PsTextRun | Self::PsTextRunGlyphTransform
            | Self::PsQuadTextured | Self::PsQuadGradient
            | Self::PsQuadRadialGradient | Self::PsQuadConicGradient
            | Self::PsSplitComposite => Some(PRIMITIVE_INSTANCE_LAYOUT),
            // Debug and utility shaders use a single vertex buffer
            Self::DebugColor | Self::DebugFont | Self::PsClear | Self::PsCopy => None,
        }
    }
}

/// Blend mode key for wgpu pipeline cache.
///
/// This is a simplified version of the WebRender `BlendMode` enum, used as a
/// pipeline cache key. Each variant maps to a specific `wgpu::BlendState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WgpuBlendMode {
    /// Blending disabled — writes RGB directly.
    None,
    /// Standard alpha: src*alpha + dst*(1-alpha).
    Alpha,
    /// Pre-multiplied alpha: src + dst*(1-src_alpha).
    PremultipliedAlpha,
    /// Pre-multiplied dest-out: dst*(1-src_alpha).
    PremultipliedDestOut,
    /// Screen: src + dst*(1-src_color) for color, premultiplied alpha for alpha.
    Screen,
    /// Exclusion: src*(1-dst) + dst*(1-src) for color, premultiplied alpha for alpha.
    Exclusion,
    /// Plus-lighter: src + dst (clamped).
    PlusLighter,
    /// Multiplicative clip mask: dst * src (for accumulating secondary clip masks).
    MultiplyClipMask,
}

impl WgpuBlendMode {
    /// Convert to the corresponding `wgpu::BlendState`.
    fn to_wgpu_blend_state(self) -> Option<wgpu::BlendState> {
        use wgpu::BlendComponent;
        use wgpu::BlendFactor::*;
        use wgpu::BlendOperation::Add;

        match self {
            WgpuBlendMode::None => None,
            WgpuBlendMode::Alpha => Some(wgpu::BlendState {
                color: BlendComponent {
                    src_factor: SrcAlpha,
                    dst_factor: OneMinusSrcAlpha,
                    operation: Add,
                },
                alpha: BlendComponent {
                    src_factor: One,
                    dst_factor: OneMinusSrcAlpha,
                    operation: Add,
                },
            }),
            WgpuBlendMode::PremultipliedAlpha => {
                Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING)
            }
            WgpuBlendMode::PremultipliedDestOut => Some(wgpu::BlendState {
                color: BlendComponent {
                    src_factor: Zero,
                    dst_factor: OneMinusSrcAlpha,
                    operation: Add,
                },
                alpha: BlendComponent {
                    src_factor: Zero,
                    dst_factor: OneMinusSrcAlpha,
                    operation: Add,
                },
            }),
            WgpuBlendMode::Screen => Some(wgpu::BlendState {
                color: BlendComponent {
                    src_factor: One,
                    dst_factor: OneMinusSrc,
                    operation: Add,
                },
                alpha: BlendComponent {
                    src_factor: One,
                    dst_factor: OneMinusSrcAlpha,
                    operation: Add,
                },
            }),
            WgpuBlendMode::Exclusion => Some(wgpu::BlendState {
                color: BlendComponent {
                    src_factor: OneMinusDst,
                    dst_factor: OneMinusSrc,
                    operation: Add,
                },
                alpha: BlendComponent {
                    src_factor: One,
                    dst_factor: OneMinusSrcAlpha,
                    operation: Add,
                },
            }),
            WgpuBlendMode::PlusLighter => Some(wgpu::BlendState {
                color: BlendComponent {
                    src_factor: One,
                    dst_factor: One,
                    operation: Add,
                },
                alpha: BlendComponent {
                    src_factor: One,
                    dst_factor: One,
                    operation: Add,
                },
            }),
            WgpuBlendMode::MultiplyClipMask => Some(wgpu::BlendState {
                color: BlendComponent {
                    src_factor: Zero,
                    dst_factor: Src,
                    operation: Add,
                },
                alpha: BlendComponent {
                    src_factor: Zero,
                    dst_factor: SrcAlpha,
                    operation: Add,
                },
            }),
        }
    }
}

/// Depth testing mode for wgpu pipeline cache.
///
/// In wgpu, depth/stencil state is baked into the pipeline at creation time.
/// This enum is part of the pipeline cache key alongside blend mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WgpuDepthState {
    /// No depth testing or writing.
    None,
    /// Depth test (LessEqual) + depth write — for opaque batches drawn front-to-back.
    WriteAndTest,
    /// Depth test (LessEqual) + no depth write — for alpha batches behind opaque geometry.
    TestOnly,
    /// Depth format present but always passes — for MixBlend pass 2 where the render
    /// pass has a depth attachment but we don't want depth rejection.
    AlwaysPass,
}

impl WgpuDepthState {
    /// Convert to the corresponding `wgpu::DepthStencilState`.
    fn to_wgpu_depth_stencil(self) -> Option<wgpu::DepthStencilState> {
        match self {
            WgpuDepthState::None => None,
            WgpuDepthState::WriteAndTest => Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            WgpuDepthState::TestOnly => Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            WgpuDepthState::AlwaysPass => Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
        }
    }
}

/// Cached shader modules and vertex layout info for a shader variant.
struct ShaderEntry {
    vs_module: wgpu::ShaderModule,
    fs_module: wgpu::ShaderModule,
    vertex_layouts: ShaderVertexLayouts,
}

/// Pre-computed vertex layout data for a shader variant.
enum ShaderVertexLayouts {
    /// Single per-vertex buffer (debug_color, debug_font, cs_* fallback).
    SingleBuffer {
        attrs: Vec<wgpu::VertexAttribute>,
        stride: u64,
    },
    /// Two buffers: unit quad vertex + instance data (brush_*, ps_text_run, composite, quad).
    Instanced {
        vertex_attrs: Vec<wgpu::VertexAttribute>,
        vertex_stride: u64,
        instance_attrs: Vec<wgpu::VertexAttribute>,
        instance_stride: u64,
    },
}

pub struct WgpuDevice {
    device: wgpu::Device,
    queue: wgpu::Queue,
    #[allow(dead_code)]
    features: wgpu::Features,
    frame_id: GpuFrameId,
    /// Compiled shader modules + vertex layout info, keyed by typed variant.
    shaders: HashMap<WgpuShaderVariant, ShaderEntry>,
    /// Render pipelines, keyed by (variant, blend_mode, depth_state, target_format).
    pipelines: HashMap<(WgpuShaderVariant, WgpuBlendMode, WgpuDepthState, wgpu::TextureFormat), WgpuProgram>,
    #[allow(dead_code)]
    pipeline_layout: wgpu::PipelineLayout,
    bind_group_layout_0: wgpu::BindGroupLayout,
    bind_group_layout_1: wgpu::BindGroupLayout,
    global_sampler: wgpu::Sampler,
    dummy_texture_f32: wgpu::TextureView,
    dummy_texture_i32: wgpu::TextureView,
    /// Maximum depth IDs for orthographic z mapping (matches scene config).
    max_depth_ids: i32,
    /// Pooled depth textures keyed by (width, height).
    depth_textures: HashMap<(u32, u32), wgpu::Texture>,
    /// Window surface for presentation. None in headless mode.
    surface: Option<wgpu::Surface<'static>>,
    surface_config: Option<wgpu::SurfaceConfiguration>,
    /// Batched command encoder — shared across draws to the same target.
    /// Call `ensure_encoder()` to lazily create, `flush_encoder()` to submit.
    pending_encoder: Option<wgpu::CommandEncoder>,
    /// Pre-allocated unit quad vertex buffer (4 corners, Unorm8x2, 4-byte stride).
    pub(crate) unit_quad_vb: wgpu::Buffer,
    /// Pre-allocated unit quad index buffer ([0,1,2, 2,1,3]).
    pub(crate) unit_quad_ib: wgpu::Buffer,
    /// Pre-allocated mali workaround uniform (constant 0u32).
    pub(crate) mali_workaround_buf: wgpu::Buffer,
}

/// Texture bindings for a general-purpose draw call.
///
/// Each field corresponds to a fixed binding slot in the WGSL shader binding
/// table (see `FIXED_BINDINGS` in wgsl.rs).  `None` means "use the dummy
/// texture for that slot".
#[derive(Default)]
pub struct TextureBindings<'a> {
    /// binding 0: sColor0
    pub color0: Option<&'a wgpu::TextureView>,
    /// binding 1: sColor1
    pub color1: Option<&'a wgpu::TextureView>,
    /// binding 2: sColor2
    pub color2: Option<&'a wgpu::TextureView>,
    /// binding 3: sGpuCache
    pub gpu_cache: Option<&'a wgpu::TextureView>,
    /// binding 4: sTransformPalette
    pub transform_palette: Option<&'a wgpu::TextureView>,
    /// binding 5: sRenderTasks
    pub render_tasks: Option<&'a wgpu::TextureView>,
    /// binding 6: sDither
    pub dither: Option<&'a wgpu::TextureView>,
    /// binding 7: sPrimitiveHeadersF
    pub prim_headers_f: Option<&'a wgpu::TextureView>,
    /// binding 8: sPrimitiveHeadersI (sint)
    pub prim_headers_i: Option<&'a wgpu::TextureView>,
    /// binding 9: sClipMask
    pub clip_mask: Option<&'a wgpu::TextureView>,
    /// binding 10: sGpuBufferF
    pub gpu_buffer_f: Option<&'a wgpu::TextureView>,
    /// binding 11: sGpuBufferI (sint)
    pub gpu_buffer_i: Option<&'a wgpu::TextureView>,
}

/// Create the constant unit quad vertex buffer, index buffer, and mali
/// workaround uniform. Called once during device init.
fn create_constant_buffers(device: &wgpu::Device) -> (wgpu::Buffer, wgpu::Buffer, wgpu::Buffer) {
    use wgpu::util::DeviceExt;

    // Unit quad: 4 corners as Unorm8x2, padded to 4-byte stride.
    let quad_verts: [[u8; 4]; 4] = [
        [0, 0, 0, 0],
        [0xFF, 0, 0, 0],
        [0, 0xFF, 0, 0],
        [0xFF, 0xFF, 0, 0],
    ];
    let vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("unit quad vb"),
        contents: as_byte_slice(&quad_verts),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let indices: [u16; 6] = [0, 1, 2, 2, 1, 3];
    let ib = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("unit quad ib"),
        contents: as_byte_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });

    let mali = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mali workaround"),
        contents: &0u32.to_le_bytes(),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    (vb, ib, mali)
}

impl WgpuDevice {
    /// Create a headless device (no surface/window required).
    pub fn new_headless() -> Option<Self> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::None,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok()?;

        let wanted = wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;
        let required_features = adapter.features() & wanted;

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("WebRender wgpu device"),
                required_features,
                ..Default::default()
            },
        ))
        .ok()?;

        let bind_group_layout_0 = create_resource_bind_group_layout(&device);
        let bind_group_layout_1 = create_sampler_bind_group_layout(&device);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("WR pipeline layout"),
            bind_group_layouts: &[&bind_group_layout_0, &bind_group_layout_1],
            push_constant_ranges: &[],
        });

        let global_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("global_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        device.on_uncaptured_error(Box::new(|err| {
            log::warn!("wgpu uncaptured error: {}", err);
        }));

        let dummy_texture_f32 =
            create_dummy_texture(&device, &queue, wgpu::TextureFormat::Rgba8Unorm);
        let dummy_texture_i32 =
            create_dummy_texture(&device, &queue, wgpu::TextureFormat::Rgba32Sint);
        let (shaders, pipelines) = create_all_pipelines_threaded(&device, &pipeline_layout);
        let (unit_quad_vb, unit_quad_ib, mali_workaround_buf) = create_constant_buffers(&device);

        Some(WgpuDevice {
            device,
            queue,
            features: required_features,
            frame_id: GpuFrameId::new(0),
            shaders,
            pipelines,
            pipeline_layout,
            bind_group_layout_0,
            bind_group_layout_1,
            global_sampler,
            dummy_texture_f32,
            dummy_texture_i32,
            max_depth_ids: 1 << 22,
            depth_textures: HashMap::new(),
            surface: None,
            surface_config: None,
            pending_encoder: None,
            unit_quad_vb,
            unit_quad_ib,
            mali_workaround_buf,
        })
    }

    /// Create a device with a window surface for presentation.
    ///
    /// The caller creates the `wgpu::Instance` and `wgpu::Surface<'static>` from
    /// its window handle and passes them here along with the initial framebuffer
    /// size. The instance must be the same one that created the surface.
    pub fn new_with_surface(
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .ok()?;

        let wanted = wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;
        let required_features = adapter.features() & wanted;

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("WebRender wgpu device"),
                required_features,
                ..Default::default()
            },
        ))
        .ok()?;

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .find(|f| matches!(f, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm))
            .copied()
            .unwrap_or(surface_caps.formats[0]);

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            format: surface_format,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &surface_config);

        let bind_group_layout_0 = create_resource_bind_group_layout(&device);
        let bind_group_layout_1 = create_sampler_bind_group_layout(&device);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("WR pipeline layout"),
            bind_group_layouts: &[&bind_group_layout_0, &bind_group_layout_1],
            push_constant_ranges: &[],
        });

        let global_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("global_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        device.on_uncaptured_error(Box::new(|err| {
            log::warn!("wgpu uncaptured error: {}", err);
        }));

        let dummy_texture_f32 =
            create_dummy_texture(&device, &queue, wgpu::TextureFormat::Rgba8Unorm);
        let dummy_texture_i32 =
            create_dummy_texture(&device, &queue, wgpu::TextureFormat::Rgba32Sint);
        let (shaders, pipelines) = create_all_pipelines_threaded(&device, &pipeline_layout);
        let (unit_quad_vb, unit_quad_ib, mali_workaround_buf) = create_constant_buffers(&device);

        Some(WgpuDevice {
            device,
            queue,
            features: required_features,
            frame_id: GpuFrameId::new(0),
            shaders,
            pipelines,
            pipeline_layout,
            bind_group_layout_0,
            bind_group_layout_1,
            global_sampler,
            dummy_texture_f32,
            dummy_texture_i32,
            max_depth_ids: 1 << 22,
            depth_textures: HashMap::new(),
            surface: Some(surface),
            surface_config: Some(surface_config),
            pending_encoder: None,
            unit_quad_vb,
            unit_quad_ib,
            mali_workaround_buf,
        })
    }

    /// Returns true if this device has a presentation surface.
    pub fn has_surface(&self) -> bool {
        self.surface.is_some()
    }

    /// Submit the pending command encoder, if any.
    /// Call between render targets, before surface present, and at frame end.
    pub fn flush_encoder(&mut self) {
        if let Some(encoder) = self.pending_encoder.take() {
            self.device.push_error_scope(wgpu::ErrorFilter::Validation);
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                encoder.finish()
            })) {
                Ok(cmd) => {
                    self.queue.submit([cmd]);
                }
                Err(_) => {
                    log::error!("wgpu flush_encoder: encoder.finish() panicked (invalid encoder)");
                }
            }
            let err = pollster::block_on(self.device.pop_error_scope());
            if let Some(e) = err {
                log::error!("wgpu flush_encoder validation error: {}", e);
            }
        }
    }

    /// Take the pending encoder (or create a new one).
    /// While the encoder is out, the caller can create render passes from it
    /// while still calling other WgpuDevice methods (pipeline lookup, buffer
    /// creation, bind groups) — since the encoder is no longer on self.
    /// Caller must return it via `return_encoder()`.
    pub fn take_encoder(&mut self) -> wgpu::CommandEncoder {
        self.pending_encoder.take().unwrap_or_else(|| {
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame encoder"),
            })
        })
    }

    /// Return an encoder after use. Typically called after dropping a render pass.
    pub fn return_encoder(&mut self, encoder: wgpu::CommandEncoder) {
        debug_assert!(self.pending_encoder.is_none(), "return_encoder called with encoder already present");
        self.pending_encoder = Some(encoder);
    }

    /// Create per-target projection and texture-size uniform buffers.
    /// These are constant for all draws to the same render target, so they
    /// should be created once and shared across all draws.
    pub fn create_target_uniforms(&self, width: u32, height: u32) -> (wgpu::Buffer, wgpu::Buffer) {
        let projection = ortho(width as f32, height as f32, self.max_depth_ids as f32);
        let mut transform_data = Vec::with_capacity(64);
        for f in &projection {
            transform_data.extend_from_slice(&f.to_le_bytes());
        }
        let transform_buf = self.create_uniform_buffer("target transform", &transform_data);

        let mut tex_size_data = Vec::with_capacity(8);
        tex_size_data.extend_from_slice(&(width as f32).to_le_bytes());
        tex_size_data.extend_from_slice(&(height as f32).to_le_bytes());
        let tex_size_buf = self.create_uniform_buffer("target texture size", &tex_size_data);

        (transform_buf, tex_size_buf)
    }

    /// Look up or lazily create a render pipeline for the given key.
    /// Returns None if the shader variant is not loaded.
    pub fn ensure_pipeline(
        &mut self,
        variant: WgpuShaderVariant,
        blend_mode: WgpuBlendMode,
        depth_state: WgpuDepthState,
        target_format: wgpu::TextureFormat,
    ) -> Option<&wgpu::RenderPipeline> {
        let pipeline_key = (variant, blend_mode, depth_state, target_format);
        if !self.pipelines.contains_key(&pipeline_key) {
            let shader = self.shaders.get(&variant)?;
            let pipeline = create_pipeline_for_blend(
                &self.device,
                &self.pipeline_layout,
                &shader.vs_module,
                &shader.fs_module,
                &shader.vertex_layouts,
                variant,
                blend_mode,
                depth_state,
                target_format,
            );
            self.pipelines.insert(pipeline_key, WgpuProgram { pipeline });
        }
        self.pipelines.get(&pipeline_key).map(|p| &p.pipeline)
    }

    /// Record a single instanced quad draw into an existing render pass.
    ///
    /// The caller is responsible for creating the render pass and managing the
    /// encoder. This method handles pipeline lookup, bind group creation,
    /// buffer upload, and the actual draw_indexed call.
    ///
    /// `transform_buf` and `tex_size_buf` are shared per-target uniforms
    /// (from `create_target_uniforms`).
    pub fn record_draw(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        variant: WgpuShaderVariant,
        blend_mode: WgpuBlendMode,
        depth_state: WgpuDepthState,
        target_format: wgpu::TextureFormat,
        target_width: u32,
        target_height: u32,
        textures: &TextureBindings<'_>,
        transform_buf: &wgpu::Buffer,
        tex_size_buf: &wgpu::Buffer,
        instance_bytes: &[u8],
        instance_count: u32,
        scissor_rect: Option<(u32, u32, u32, u32)>,
    ) {
        let pipeline = match self.ensure_pipeline(variant, blend_mode, depth_state, target_format) {
            Some(p) => p,
            None => {
                log::warn!("wgpu: pipeline not found for {:?}, skipping draw", variant);
                return;
            }
        };
        pass.set_pipeline(pipeline);

        let (bg0, bg1) = self.create_bind_groups_full(
            textures,
            transform_buf,
            tex_size_buf,
            &self.mali_workaround_buf,
        );
        let instance_buf = self.create_vertex_buffer("draw instances", instance_bytes);

        if let Some((x, y, w, h)) = scissor_rect {
            pass.set_scissor_rect(x, y, w, h);
        } else {
            pass.set_scissor_rect(0, 0, target_width, target_height);
        }
        pass.set_bind_group(0, &bg0, &[]);
        pass.set_bind_group(1, &bg1, &[]);
        pass.set_vertex_buffer(0, self.unit_quad_vb.slice(..));
        pass.set_vertex_buffer(1, instance_buf.slice(..));
        pass.set_index_buffer(self.unit_quad_ib.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..6, 0, 0..instance_count);
    }

    /// Acquire the current surface texture for rendering.
    /// Returns None if no surface is configured or acquisition fails.
    pub fn acquire_surface_texture(&self) -> Option<wgpu::SurfaceTexture> {
        let surface = self.surface.as_ref()?;
        match surface.get_current_texture() {
            Ok(tex) => Some(tex),
            Err(e) => {
                warn!("wgpu: failed to acquire surface texture: {:?}", e);
                None
            }
        }
    }

    /// Get the surface texture format, if a surface is configured.
    pub fn surface_format(&self) -> Option<wgpu::TextureFormat> {
        self.surface_config.as_ref().map(|c| c.format)
    }

    /// Resize the surface. Called when the window size changes.
    pub fn resize_surface(&mut self, width: u32, height: u32) {
        if let Some(ref mut config) = self.surface_config {
            config.width = width.max(1);
            config.height = height.max(1);
            if let Some(ref surface) = self.surface {
                surface.configure(&self.device, config);
            }
        }
    }

    /// Acquire (or create) a depth texture for the given render target dimensions.
    /// Returns a view into the depth texture suitable for render pass attachment.
    pub fn acquire_depth_view(&mut self, width: u32, height: u32) -> wgpu::TextureView {
        let key = (width, height);
        if !self.depth_textures.contains_key(&key) {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("depth target"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            self.depth_textures.insert(key, tex);
        }
        self.depth_textures[&key].create_view(&wgpu::TextureViewDescriptor::default())
    }

    pub fn begin_frame(&mut self) -> GpuFrameId {
        self.frame_id = self.frame_id + 1;
        self.frame_id
    }

    pub fn end_frame(&mut self) {
        let _ = self.device.poll(wgpu::PollType::Wait);
    }

    pub fn create_texture(
        &mut self,
        target: ImageBufferKind,
        format: ImageFormat,
        width: i32,
        height: i32,
        _filter: TextureFilter,
        render_target: Option<RenderTargetInfo>,
    ) -> WgpuTexture {
        let wgpu_format = image_format_to_wgpu(format, self.features);
        let mut usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
        if render_target.is_some() {
            usage |= wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
        }

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: width as u32,
                height: height as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: image_buffer_kind_to_texture_dimension(target),
            format: wgpu_format,
            usage,
            view_formats: &[],
        });

        WgpuTexture {
            texture,
            format: wgpu_format,
            width: width as u32,
            height: height as u32,
        }
    }

    pub fn upload_texture_immediate(&mut self, texture: &WgpuTexture, pixels: &[u8]) {
        let bpp = wgpu_format_bytes_per_pixel(texture.format);
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(texture.width * bpp),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: texture.width,
                height: texture.height,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Upload pixel data to a sub-rectangle of a wgpu texture.
    pub fn upload_texture_sub_rect(
        &self,
        texture: &WgpuTexture,
        rect: DeviceIntRect,
        stride: Option<i32>,
        data: &[u8],
        format: ImageFormat,
    ) {
        let bpp = wgpu_format_bytes_per_pixel(texture.format);
        let row_bytes = rect.width() as u32 * bpp;
        let src_stride = stride.map(|s| s as u32).unwrap_or(row_bytes);

        // wgpu requires bytes_per_row to be aligned to 256 for buffer copies,
        // but write_texture from CPU data has no such constraint.
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rect.min.x as u32,
                    y: rect.min.y as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(src_stride),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: rect.width() as u32,
                height: rect.height() as u32,
                depth_or_array_layers: 1,
            },
        );
        let _ = format; // kept for future format conversion if needed
    }

    /// Create a wgpu texture suitable for use as a texture cache entry.
    pub fn create_cache_texture(
        &self,
        width: i32,
        height: i32,
        format: ImageFormat,
    ) -> WgpuTexture {
        let wgpu_format = image_format_to_wgpu(format, self.features);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgpu cache texture"),
            size: wgpu::Extent3d {
                width: width as u32,
                height: height as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        WgpuTexture {
            texture,
            width: width as u32,
            height: height as u32,
            format: wgpu_format,
        }
    }

    /// Copy a sub-rectangle from one wgpu texture to another.
    /// Used for texture cache atlas defragmentation.
    pub fn copy_texture_sub_rect(
        &self,
        src: &WgpuTexture,
        src_rect: DeviceIntRect,
        dst: &WgpuTexture,
        dst_rect: DeviceIntRect,
    ) {
        debug_assert_eq!(src_rect.size(), dst_rect.size());
        let size = wgpu::Extent3d {
            width: src_rect.width() as u32,
            height: src_rect.height() as u32,
            depth_or_array_layers: 1,
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("texture cache copy"),
            });
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &src.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: src_rect.min.x as u32,
                    y: src_rect.min.y as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &dst.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: dst_rect.min.x as u32,
                    y: dst_rect.min.y as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            size,
        );
        self.queue.submit([encoder.finish()]);
    }

    pub fn clear_texture(&self, texture: &WgpuTexture, color: [f64; 4]) {
        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("clear_texture"),
            });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: color[0],
                            g: color[1],
                            b: color[2],
                            a: color[3],
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        self.queue.submit([encoder.finish()]);
    }

    pub fn delete_texture(&mut self, texture: WgpuTexture) {
        drop(texture);
    }

    /// Return a reference to the underlying wgpu device.
    pub fn wgpu_device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Return a reference to the wgpu queue.
    pub fn wgpu_queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Create a data texture from raw bytes and upload the data.
    ///
    /// WebRender uses "data textures" (RGBA32F, RGBA16F, RGBA32Sint, etc.)
    /// to pass per-frame data to shaders: GPU cache, transform palette,
    /// render tasks, primitive headers, and GPU buffers. This method creates
    /// a texture of the given wgpu format and uploads the data in one call.
    ///
    /// The texture is created with TEXTURE_BINDING usage so it can be sampled
    /// by shaders, and COPY_DST so it can be updated.
    pub fn create_data_texture(
        &self,
        label: &str,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        data: &[u8],
    ) -> WgpuTexture {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let bpp = wgpu_format_bytes_per_pixel(format);
        let w = width.max(1);
        let h = height.max(1);
        let expected = (w * h * bpp) as usize;

        if !data.is_empty() {
            // Pad data to fill the full texture if needed.  The raw data
            // is a tightly-packed array of items that may not fill the last
            // texture row.  Zero-padding is safe because shaders only
            // access indices within the valid item range.
            let upload_data: std::borrow::Cow<'_, [u8]> = if data.len() >= expected {
                std::borrow::Cow::Borrowed(&data[..expected])
            } else {
                let mut padded = Vec::with_capacity(expected);
                padded.extend_from_slice(data);
                padded.resize(expected, 0u8);
                std::borrow::Cow::Owned(padded)
            };
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &upload_data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * bpp),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }

        WgpuTexture {
            texture,
            format,
            width: w,
            height: h,
        }
    }

    /// Update an existing data texture with new data, reallocating if the
    /// dimensions have changed.
    pub fn update_data_texture(
        &self,
        existing: &mut WgpuTexture,
        width: u32,
        height: u32,
        data: &[u8],
    ) {
        let w = width.max(1);
        let h = height.max(1);
        if existing.width != w || existing.height != h {
            // Reallocate — dimensions changed.
            let label = "data texture (resized)";
            *existing = self.create_data_texture(label, w, h, existing.format, data);
            return;
        }

        let bpp = wgpu_format_bytes_per_pixel(existing.format);
        let expected = (w * h * bpp) as usize;
        if !data.is_empty() {
            let upload_data: std::borrow::Cow<'_, [u8]> = if data.len() >= expected {
                std::borrow::Cow::Borrowed(&data[..expected])
            } else {
                let mut padded = Vec::with_capacity(expected);
                padded.extend_from_slice(data);
                padded.resize(expected, 0u8);
                std::borrow::Cow::Owned(padded)
            };
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &existing.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &upload_data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * bpp),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    fn create_uniform_buffer(&self, label: &str, data: &[u8]) -> wgpu::Buffer {
        use wgpu::util::DeviceExt;
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: data,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            })
    }

    pub fn create_vertex_buffer(&self, label: &str, data: &[u8]) -> wgpu::Buffer {
        use wgpu::util::DeviceExt;
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: data,
                usage: wgpu::BufferUsages::VERTEX,
            })
    }

    fn create_index_buffer(&self, label: &str, data: &[u8]) -> wgpu::Buffer {
        use wgpu::util::DeviceExt;
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: data,
                usage: wgpu::BufferUsages::INDEX,
            })
    }

    fn create_bind_groups(
        &self,
        transform_buf: &wgpu::Buffer,
        tex_size_buf: &wgpu::Buffer,
        mali_buf: &wgpu::Buffer,
    ) -> (wgpu::BindGroup, wgpu::BindGroup) {
        self.create_bind_groups_with_color0(None, transform_buf, tex_size_buf, mali_buf)
    }

    pub fn create_bind_groups_with_color0(
        &self,
        color0_view: Option<&wgpu::TextureView>,
        transform_buf: &wgpu::Buffer,
        tex_size_buf: &wgpu::Buffer,
        mali_buf: &wgpu::Buffer,
    ) -> (wgpu::BindGroup, wgpu::BindGroup) {
        let color0_view = color0_view.unwrap_or(&self.dummy_texture_f32);
        let group_0 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("WR group 0"),
            layout: &self.bind_group_layout_0,
            entries: &[
                tex_entry(0, color0_view),
                tex_entry(1, &self.dummy_texture_f32),
                tex_entry(2, &self.dummy_texture_f32),
                tex_entry(3, &self.dummy_texture_f32),
                tex_entry(4, &self.dummy_texture_f32),
                tex_entry(5, &self.dummy_texture_f32),
                tex_entry(6, &self.dummy_texture_f32),
                tex_entry(7, &self.dummy_texture_f32),
                tex_entry(8, &self.dummy_texture_i32),
                tex_entry(9, &self.dummy_texture_f32),
                tex_entry(10, &self.dummy_texture_f32),
                tex_entry(11, &self.dummy_texture_i32),
                wgpu::BindGroupEntry {
                    binding: 12,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: transform_buf,
                        offset: 0,
                        size: wgpu::BufferSize::new(64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 13,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: tex_size_buf,
                        offset: 0,
                        size: wgpu::BufferSize::new(8),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 14,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: mali_buf,
                        offset: 0,
                        size: wgpu::BufferSize::new(4),
                    }),
                },
            ],
        });

        let group_1 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("WR group 1 (sampler)"),
            layout: &self.bind_group_layout_1,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Sampler(&self.global_sampler),
            }],
        });

        (group_0, group_1)
    }

    pub fn read_texture_pixels(&mut self, texture: &WgpuTexture, output: &mut [u8]) {
        // Flush any pending draw commands so results are visible to the copy.
        self.flush_encoder();
        let bpp = wgpu_format_bytes_per_pixel(texture.format);
        let bytes_per_row_unaligned = texture.width * bpp;
        let bytes_per_row = (bytes_per_row_unaligned + 255) & !255;

        let buf_size = (bytes_per_row as u64) * (texture.height as u64);
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback staging"),
            size: buf_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: texture.width,
                height: texture.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::Wait).unwrap();

        let mapped = slice.get_mapped_range();
        let dst_stride = (texture.width * bpp) as usize;
        let src_stride = bytes_per_row as usize;
        for row in 0..texture.height as usize {
            let src_start = row * src_stride;
            let dst_start = row * dst_stride;
            output[dst_start..dst_start + dst_stride]
                .copy_from_slice(&mapped[src_start..src_start + dst_stride]);
        }
        drop(mapped);
        staging.unmap();
    }

    /// Read back pixels from a `wgpu::Texture` (e.g. a surface texture) into `output`.
    /// The texture must have COPY_SRC usage.  Output is tightly-packed BGRA rows.
    pub fn read_surface_texture_pixels(
        &self,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
        output: &mut [u8],
    ) {
        let bpp = 4u32; // Bgra8Unorm
        let bytes_per_row_unaligned = width * bpp;
        let bytes_per_row = (bytes_per_row_unaligned + 255) & !255;

        let buf_size = (bytes_per_row as u64) * (height as u64);
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("surface readback staging"),
            size: buf_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = self.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor { label: Some("surface readback") },
        );
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);

        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::Wait).unwrap();

        let mapped = slice.get_mapped_range();
        let dst_stride = (width * bpp) as usize;
        let src_stride = bytes_per_row as usize;
        for row in 0..height as usize {
            let src_start = row * src_stride;
            let dst_start = row * dst_stride;
            output[dst_start..dst_start + dst_stride]
                .copy_from_slice(&mapped[src_start..src_start + dst_stride]);
        }
        drop(mapped);
        staging.unmap();
    }

    pub fn pipeline_count(&self) -> usize {
        self.pipelines.len()
    }

    pub fn shader_count(&self) -> usize {
        self.shaders.len()
    }

    pub fn render_debug_color_quad(&mut self, target: &WgpuTexture, color: [u8; 4]) {
        let target_view = target
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let projection = ortho(target.width as f32, target.height as f32, self.max_depth_ids as f32);
        let mut transform_data = Vec::with_capacity(64);
        for f in &projection {
            transform_data.extend_from_slice(&f.to_le_bytes());
        }
        let transform_buf = self.create_uniform_buffer("debug_color transform", &transform_data);

        let mut tex_size_data = Vec::with_capacity(8);
        tex_size_data.extend_from_slice(&(target.width as f32).to_le_bytes());
        tex_size_data.extend_from_slice(&(target.height as f32).to_le_bytes());
        let tex_size_buf = self.create_uniform_buffer("debug_color texture size", &tex_size_data);
        let (bg0, bg1) = self.create_bind_groups(&transform_buf, &tex_size_buf, &self.mali_workaround_buf);

        #[repr(C)]
        #[derive(Copy, Clone)]
        struct Vert {
            pos: [f32; 2],
            color: [u8; 4],
        }

        let verts = [
            Vert {
                pos: [0.0, 0.0],
                color,
            },
            Vert {
                pos: [target.width as f32, 0.0],
                color,
            },
            Vert {
                pos: [0.0, target.height as f32],
                color,
            },
            Vert {
                pos: [target.width as f32, target.height as f32],
                color,
            },
        ];
        let vert_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                verts.as_ptr() as *const u8,
                std::mem::size_of_val(&verts),
            )
        };
        let vb = self.create_vertex_buffer("debug_color verts", vert_bytes);

        let indices: [u16; 6] = [0, 1, 2, 2, 1, 3];
        let idx_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                indices.as_ptr() as *const u8,
                std::mem::size_of_val(&indices),
            )
        };
        let ib = self.create_index_buffer("debug_color indices", idx_bytes);

        let surface_fmt = self.surface_config.as_ref()
            .map(|c| c.format)
            .unwrap_or(wgpu::TextureFormat::Bgra8Unorm);
        let pipeline = &self.pipelines[&(WgpuShaderVariant::DebugColor, WgpuBlendMode::PremultipliedAlpha, WgpuDepthState::None, surface_fmt)].pipeline;
        let device = &self.device;
        let encoder = self.pending_encoder.get_or_insert_with(|| {
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame encoder"),
            })
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("debug_color pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bg0, &[]);
            pass.set_bind_group(1, &bg1, &[]);
            pass.set_vertex_buffer(0, vb.slice(..));
            pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..6, 0, 0..1);
        }
    }

    pub fn render_debug_font_quad(
        &mut self,
        target: &WgpuTexture,
        source: &WgpuTexture,
        color: [u8; 4],
    ) {
        let target_view = target
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let source_view = source
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let projection = ortho(target.width as f32, target.height as f32, self.max_depth_ids as f32);
        let mut transform_data = Vec::with_capacity(64);
        for f in &projection {
            transform_data.extend_from_slice(&f.to_le_bytes());
        }
        let transform_buf = self.create_uniform_buffer("debug_font transform", &transform_data);

        let mut tex_size_data = Vec::with_capacity(8);
        tex_size_data.extend_from_slice(&(target.width as f32).to_le_bytes());
        tex_size_data.extend_from_slice(&(target.height as f32).to_le_bytes());
        let tex_size_buf = self.create_uniform_buffer("debug_font texture size", &tex_size_data);
        let (bg0, bg1) =
            self.create_bind_groups_with_color0(Some(&source_view), &transform_buf, &tex_size_buf, &self.mali_workaround_buf);

        #[repr(C)]
        #[derive(Copy, Clone)]
        struct Vert {
            pos: [f32; 2],
            color: [u8; 4],
            uv: [f32; 2],
        }

        let verts = [
            Vert {
                pos: [0.0, 0.0],
                color,
                uv: [0.0, 0.0],
            },
            Vert {
                pos: [target.width as f32, 0.0],
                color,
                uv: [1.0, 0.0],
            },
            Vert {
                pos: [0.0, target.height as f32],
                color,
                uv: [0.0, 1.0],
            },
            Vert {
                pos: [target.width as f32, target.height as f32],
                color,
                uv: [1.0, 1.0],
            },
        ];
        let vert_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                verts.as_ptr() as *const u8,
                std::mem::size_of_val(&verts),
            )
        };
        let vb = self.create_vertex_buffer("debug_font verts", vert_bytes);

        let indices: [u16; 6] = [0, 1, 2, 2, 1, 3];
        let idx_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                indices.as_ptr() as *const u8,
                std::mem::size_of_val(&indices),
            )
        };
        let ib = self.create_index_buffer("debug_font indices", idx_bytes);

        let surface_fmt = self.surface_config.as_ref()
            .map(|c| c.format)
            .unwrap_or(wgpu::TextureFormat::Bgra8Unorm);
        let pipeline = &self.pipelines[&(WgpuShaderVariant::DebugFont, WgpuBlendMode::PremultipliedAlpha, WgpuDepthState::None, surface_fmt)].pipeline;
        let device = &self.device;
        let encoder = self.pending_encoder.get_or_insert_with(|| {
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame encoder"),
            })
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("debug_font pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bg0, &[]);
            pass.set_bind_group(1, &bg1, &[]);
            pass.set_vertex_buffer(0, vb.slice(..));
            pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..6, 0, 0..1);
        }
    }

    /// Create a render target texture suitable for wgpu composite rendering.
    /// Uses the internal wgpu device directly (no &mut self needed).
    pub fn create_render_target(&self, width: u32, height: u32) -> WgpuTexture {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("wgpu composite RT"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        WgpuTexture {
            texture,
            width,
            height,
            format: wgpu::TextureFormat::Bgra8Unorm,
        }
    }

    /// Render composite tile instances through the composite pipeline.
    ///
    /// This is the first real draw path that exercises instanced rendering
    /// with the same data layout that the GL renderer uses. The caller
    /// provides raw `CompositeInstance` bytes and a pipeline config key.
    pub fn render_composite_instances(
        &mut self,
        target: &WgpuTexture,
        source_texture: Option<&WgpuTexture>,
        instance_bytes: &[u8],
        instance_count: u32,
        variant: WgpuShaderVariant,
        clear_color: Option<wgpu::Color>,
    ) {
        let target_view = target
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.render_composite_instances_to_view(
            &target_view,
            target.width,
            target.height,
            source_texture,
            instance_bytes,
            instance_count,
            variant,
            clear_color,
        );
    }

    /// Render composite tile instances to an arbitrary texture view.
    ///
    /// This is the core rendering method — `render_composite_instances` delegates
    /// here. Used directly when rendering to a surface texture view.
    pub fn render_composite_instances_to_view(
        &mut self,
        target_view: &wgpu::TextureView,
        target_width: u32,
        target_height: u32,
        source_texture: Option<&WgpuTexture>,
        instance_bytes: &[u8],
        instance_count: u32,
        variant: WgpuShaderVariant,
        clear_color: Option<wgpu::Color>,
    ) {
        let surface_fmt = self.surface_config.as_ref()
            .map(|c| c.format)
            .unwrap_or(wgpu::TextureFormat::Bgra8Unorm);
        let pipeline_key = (variant, WgpuBlendMode::PremultipliedAlpha, WgpuDepthState::None, surface_fmt);
        let program = self
            .pipelines
            .get(&pipeline_key)
            .unwrap_or_else(|| panic!("composite pipeline not found for variant {:?}", variant));

        // Transform: orthographic projection matching the target dimensions
        let projection = ortho(target_width as f32, target_height as f32, self.max_depth_ids as f32);
        let mut transform_data = Vec::with_capacity(64);
        for f in &projection {
            transform_data.extend_from_slice(&f.to_le_bytes());
        }
        let transform_buf = self.create_uniform_buffer("composite transform", &transform_data);

        let mut tex_size_data = Vec::with_capacity(8);
        tex_size_data.extend_from_slice(&(target_width as f32).to_le_bytes());
        tex_size_data.extend_from_slice(&(target_height as f32).to_le_bytes());
        let tex_size_buf = self.create_uniform_buffer("composite texture size", &tex_size_data);
        let source_view = source_texture.map(|t| {
            t.texture
                .create_view(&wgpu::TextureViewDescriptor::default())
        });
        let (bg0, bg1) = self.create_bind_groups_with_color0(
            source_view.as_ref(),
            &transform_buf,
            &tex_size_buf,
            &self.mali_workaround_buf,
        );

        let instance_buf = self.create_vertex_buffer("composite instances", instance_bytes);

        let load = match clear_color {
            Some(c) => wgpu::LoadOp::Clear(c),
            None => wgpu::LoadOp::Load,
        };

        let device = &self.device;
        let encoder = self.pending_encoder.get_or_insert_with(|| {
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame encoder"),
            })
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&program.pipeline);
            pass.set_bind_group(0, &bg0, &[]);
            pass.set_bind_group(1, &bg1, &[]);
            pass.set_vertex_buffer(0, self.unit_quad_vb.slice(..));
            pass.set_vertex_buffer(1, instance_buf.slice(..));
            pass.set_index_buffer(self.unit_quad_ib.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..6, 0, 0..instance_count);
        }
    }

    // ── General-purpose instanced draw ──────────────────────────────────
    //
    // These methods extend the composite-only rendering to support arbitrary
    // WebRender shader pipelines (alpha batches, picture cache targets, etc.)
    // by allowing callers to specify per-binding texture views.

    /// Create bind groups with caller-specified texture views at each slot.
    pub fn create_bind_groups_full(
        &self,
        textures: &TextureBindings<'_>,
        transform_buf: &wgpu::Buffer,
        tex_size_buf: &wgpu::Buffer,
        mali_buf: &wgpu::Buffer,
    ) -> (wgpu::BindGroup, wgpu::BindGroup) {
        let df = &self.dummy_texture_f32;
        let di = &self.dummy_texture_i32;

        let group_0 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("WR group 0 (full)"),
            layout: &self.bind_group_layout_0,
            entries: &[
                tex_entry(0,  textures.color0.unwrap_or(df)),
                tex_entry(1,  textures.color1.unwrap_or(df)),
                tex_entry(2,  textures.color2.unwrap_or(df)),
                tex_entry(3,  textures.gpu_cache.unwrap_or(df)),
                tex_entry(4,  textures.transform_palette.unwrap_or(df)),
                tex_entry(5,  textures.render_tasks.unwrap_or(df)),
                tex_entry(6,  textures.dither.unwrap_or(df)),
                tex_entry(7,  textures.prim_headers_f.unwrap_or(df)),
                tex_entry(8,  textures.prim_headers_i.unwrap_or(di)),
                tex_entry(9,  textures.clip_mask.unwrap_or(df)),
                tex_entry(10, textures.gpu_buffer_f.unwrap_or(df)),
                tex_entry(11, textures.gpu_buffer_i.unwrap_or(di)),
                wgpu::BindGroupEntry {
                    binding: 12,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: transform_buf,
                        offset: 0,
                        size: wgpu::BufferSize::new(64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 13,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: tex_size_buf,
                        offset: 0,
                        size: wgpu::BufferSize::new(8),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 14,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: mali_buf,
                        offset: 0,
                        size: wgpu::BufferSize::new(4),
                    }),
                },
            ],
        });

        let group_1 = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("WR group 1 (sampler)"),
            layout: &self.bind_group_layout_1,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Sampler(&self.global_sampler),
            }],
        });

        (group_0, group_1)
    }

    /// Draw instanced quads for a given shader pipeline on a render target.
    ///
    /// This is the general-purpose wgpu draw path for WebRender batches.
    /// The caller supplies:
    /// - `variant`: typed shader variant key
    /// - `target_view` / dimensions: where to render
    /// - `textures`: per-binding texture views (data textures, color sources)
    /// - `instance_bytes`: raw instance data, same layout as the GL path
    /// - `instance_count`: number of instances
    /// - `clear`: whether to clear the color (and depth, if attached) target before drawing
    /// - `depth_state`: depth testing mode for this draw
    /// - `depth_view`: depth texture view (required when depth_state is not None)
    pub fn draw_instanced(
        &mut self,
        variant: WgpuShaderVariant,
        blend_mode: WgpuBlendMode,
        depth_state: WgpuDepthState,
        target_view: &wgpu::TextureView,
        target_width: u32,
        target_height: u32,
        target_format: wgpu::TextureFormat,
        textures: &TextureBindings<'_>,
        instance_bytes: &[u8],
        instance_count: u32,
        clear_color: Option<wgpu::Color>,
        scissor_rect: Option<(u32, u32, u32, u32)>,
        depth_view: Option<&wgpu::TextureView>,
    ) {
        // Lazily create a pipeline for this (variant, blend_mode, depth_state, format) if needed.
        let pipeline_key = (variant, blend_mode, depth_state, target_format);
        if !self.pipelines.contains_key(&pipeline_key) {
            let shader = match self.shaders.get(&variant) {
                Some(s) => s,
                None => {
                    log::warn!(
                        "wgpu: shader not found for {:?}, skipping draw",
                        variant,
                    );
                    return;
                }
            };
            let pipeline = create_pipeline_for_blend(
                &self.device,
                &self.pipeline_layout,
                &shader.vs_module,
                &shader.fs_module,
                &shader.vertex_layouts,
                variant,
                blend_mode,
                depth_state,
                target_format,
            );
            self.pipelines.insert(pipeline_key, WgpuProgram { pipeline });
        }

        let program = self.pipelines.get(&pipeline_key).unwrap();

        let projection = ortho(target_width as f32, target_height as f32, self.max_depth_ids as f32);
        let mut transform_data = Vec::with_capacity(64);
        for f in &projection {
            transform_data.extend_from_slice(&f.to_le_bytes());
        }
        let transform_buf = self.create_uniform_buffer("draw transform", &transform_data);

        let mut tex_size_data = Vec::with_capacity(8);
        tex_size_data.extend_from_slice(&(target_width as f32).to_le_bytes());
        tex_size_data.extend_from_slice(&(target_height as f32).to_le_bytes());
        let tex_size_buf = self.create_uniform_buffer("draw texture size", &tex_size_data);
        let (bg0, bg1) = self.create_bind_groups_full(
            textures,
            &transform_buf,
            &tex_size_buf,
            &self.mali_workaround_buf,
        );

        let instance_buf = self.create_vertex_buffer("draw instances", instance_bytes);

        let color_load = match clear_color {
            Some(c) => wgpu::LoadOp::Clear(c),
            None => wgpu::LoadOp::Load,
        };

        let depth_attachment = if depth_state != WgpuDepthState::None {
            depth_view.map(|dv| wgpu::RenderPassDepthStencilAttachment {
                view: dv,
                depth_ops: Some(wgpu::Operations {
                    load: if clear_color.is_some() {
                        wgpu::LoadOp::Clear(1.0)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            })
        } else {
            None
        };

        let device = &self.device;
        let encoder = self.pending_encoder.get_or_insert_with(|| {
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame encoder"),
            })
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("draw_instanced pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: color_load, store: wgpu::StoreOp::Store },
                    depth_slice: None,
                })],
                depth_stencil_attachment: depth_attachment,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&program.pipeline);
            if let Some((x, y, w, h)) = scissor_rect {
                pass.set_scissor_rect(x, y, w, h);
            }
            pass.set_bind_group(0, &bg0, &[]);
            pass.set_bind_group(1, &bg1, &[]);
            pass.set_vertex_buffer(0, self.unit_quad_vb.slice(..));
            pass.set_vertex_buffer(1, instance_buf.slice(..));
            pass.set_index_buffer(self.unit_quad_ib.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..6, 0, 0..instance_count);
        }
    }
}

impl GpuDevice for WgpuDevice {
    type Texture = WgpuTexture;

    fn create_texture(
        &mut self,
        target: ImageBufferKind,
        format: ImageFormat,
        width: i32,
        height: i32,
        filter: TextureFilter,
        render_target: Option<RenderTargetInfo>,
    ) -> Self::Texture {
        WgpuDevice::create_texture(self, target, format, width, height, filter, render_target)
    }

    fn upload_texture_immediate<T: Texel>(&mut self, texture: &Self::Texture, pixels: &[T]) {
        let byte_len = std::mem::size_of_val(pixels);
        let bytes = unsafe { std::slice::from_raw_parts(pixels.as_ptr() as *const u8, byte_len) };
        WgpuDevice::upload_texture_immediate(self, texture, bytes)
    }

    fn delete_texture(&mut self, texture: Self::Texture) {
        WgpuDevice::delete_texture(self, texture)
    }
}

/// Build an orthographic projection matrix for wgpu (NDC z range [0, 1]).
///
/// `max_depth` controls the z mapping:
/// - z=0 → depth=1.0 (back), z=max_depth → depth=0.0 (front)
/// - With LessEqual depth test, higher z values (closer) win.
/// - For draws without depth testing, the z value is still mapped into [0,1]
///   to avoid clipping. z=0 maps to 1.0 which is valid.
fn ortho(w: f32, h: f32, max_depth: f32) -> [f32; 16] {
    let z_scale = if max_depth > 0.0 { -1.0 / max_depth } else { 0.0 };
    let z_offset = if max_depth > 0.0 { 1.0 } else { 0.0 };
    [
        2.0 / w,
        0.0,
        0.0,
        0.0,
        0.0,
        -2.0 / h,
        0.0,
        0.0,
        0.0,
        0.0,
        z_scale,
        0.0,
        -1.0,
        1.0,
        z_offset,
        1.0,
    ]
}

fn tex_entry(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}

fn create_resource_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    use wgpu::{
        BindGroupLayoutEntry, BindingType, BufferBindingType, ShaderStages, TextureSampleType,
        TextureViewDimension,
    };

    let vis = ShaderStages::VERTEX_FRAGMENT;
    // Color/dither/clip textures: sampled with filtering (textureSample).
    let float_tex = |binding: u32| BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: BindingType::Texture {
            multisampled: false,
            view_dimension: TextureViewDimension::D2,
            sample_type: TextureSampleType::Float { filterable: true },
        },
        count: None,
    };
    // Data textures (GPU cache, transforms, render tasks, prim headers,
    // GPU buffers): accessed with textureLoad, not filtered. Must be
    // non-filterable to accept Rgba32Float views.
    let unfilt_tex = |binding: u32| BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: BindingType::Texture {
            multisampled: false,
            view_dimension: TextureViewDimension::D2,
            sample_type: TextureSampleType::Float { filterable: false },
        },
        count: None,
    };
    let sint_tex = |binding: u32| BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: BindingType::Texture {
            multisampled: false,
            view_dimension: TextureViewDimension::D2,
            sample_type: TextureSampleType::Sint,
        },
        count: None,
    };
    let uniform_buf = |binding: u32, min_size: u64| BindGroupLayoutEntry {
        binding,
        visibility: ShaderStages::VERTEX_FRAGMENT,
        ty: BindingType::Buffer {
            ty: BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(min_size),
        },
        count: None,
    };

    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("WR resources (group 0)"),
        entries: &[
            float_tex(0),   // sColor0 — filterable (textureSample)
            float_tex(1),   // sColor1 — filterable (textureSample)
            float_tex(2),   // sColor2 — filterable (textureSample)
            unfilt_tex(3),  // sGpuCache — Rgba32Float, textureLoad only
            unfilt_tex(4),  // sTransformPalette — Rgba32Float, textureLoad only
            unfilt_tex(5),  // sRenderTasks — Rgba32Float, textureLoad only
            float_tex(6),   // sDither — Rgba8, textureSample
            unfilt_tex(7),  // sPrimitiveHeadersF — Rgba32Float, textureLoad only
            sint_tex(8),    // sPrimitiveHeadersI — Rgba32Sint, textureLoad only
            float_tex(9),   // sClipMask — R8/Rgba8, textureLoad only (but filterable-compatible)
            unfilt_tex(10), // sGpuBufferF — Rgba32Float, textureLoad only
            sint_tex(11),   // sGpuBufferI — Rgba32Sint, textureLoad only
            uniform_buf(12, 64),
            uniform_buf(13, 8),
            uniform_buf(14, 4),
        ],
    })
}

fn create_sampler_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("WR sampler (group 1)"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        }],
    })
}

fn create_dummy_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dummy 1x1"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    // Initialize to opaque white for f32 textures (so that the FAST_PATH
    // composite shader — which outputs the texture sample directly — produces
    // white when sampling the dummy, and the non-FAST_PATH shader multiplies
    // by vColor × white = vColor).  Integer textures stay zeroed.
    let (bpp, pixels): (usize, Vec<u8>) = match format {
        wgpu::TextureFormat::Rgba8Unorm => (4, vec![255u8; 4]),
        wgpu::TextureFormat::Rgba32Sint => (16, vec![0u8; 16]),
        _ => (4, vec![255u8; 4]),
    };
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bpp as u32),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

fn vertex_format_from_wgsl_type(ty: &str) -> wgpu::VertexFormat {
    match ty {
        "f32" => wgpu::VertexFormat::Float32,
        "i32" => wgpu::VertexFormat::Sint32,
        "u32" => wgpu::VertexFormat::Uint32,
        "vec2<f32>" => wgpu::VertexFormat::Float32x2,
        "vec3<f32>" => wgpu::VertexFormat::Float32x3,
        "vec4<f32>" => wgpu::VertexFormat::Float32x4,
        "vec2<i32>" => wgpu::VertexFormat::Sint32x2,
        "vec3<i32>" => wgpu::VertexFormat::Sint32x3,
        "vec4<i32>" => wgpu::VertexFormat::Sint32x4,
        "vec2<u32>" => wgpu::VertexFormat::Uint32x2,
        "vec3<u32>" => wgpu::VertexFormat::Uint32x3,
        "vec4<u32>" => wgpu::VertexFormat::Uint32x4,
        other => unreachable!("WGSL vertex input type not in WebRender's set: {}", other),
    }
}

fn format_size(fmt: wgpu::VertexFormat) -> u64 {
    match fmt {
        wgpu::VertexFormat::Float32 | wgpu::VertexFormat::Sint32 | wgpu::VertexFormat::Uint32 => 4,
        wgpu::VertexFormat::Float32x2 | wgpu::VertexFormat::Sint32x2 | wgpu::VertexFormat::Uint32x2 => 8,
        wgpu::VertexFormat::Float32x3 | wgpu::VertexFormat::Sint32x3 => 12,
        wgpu::VertexFormat::Float32x4 | wgpu::VertexFormat::Sint32x4 | wgpu::VertexFormat::Uint32x4 => 16,
        wgpu::VertexFormat::Unorm8x2 => 2,
        wgpu::VertexFormat::Unorm8x4 => 4,
        wgpu::VertexFormat::Unorm16x2 | wgpu::VertexFormat::Uint16x2 | wgpu::VertexFormat::Sint16x2 => 4,
        wgpu::VertexFormat::Unorm16x4 | wgpu::VertexFormat::Uint16x4 | wgpu::VertexFormat::Sint16x4 => 8,
        _ => unreachable!("format_size: format not in WebRender's set: {:?}", fmt),
    }
}

/// A parsed vertex/instance input from a WGSL entry point.
struct WgslVertexInput {
    shader_location: u32,
    name: String,
    format: wgpu::VertexFormat,
}

/// Parse all `@location(N)` vertex inputs from a WGSL vertex entry point.
fn parse_wgsl_vertex_inputs(vertex_wgsl: &str) -> Vec<WgslVertexInput> {
    let vertex_line = vertex_wgsl
        .lines()
        .find(|line| line.contains("fn main("))
        .expect("WGSL vertex entry point not found");
    let params_start = vertex_line
        .find("fn main(")
        .map(|idx| idx + "fn main(".len())
        .unwrap();
    let params_end = vertex_line
        .rfind(") ->")
        .expect("WGSL vertex params terminator not found");
    let params_src = &vertex_line[params_start..params_end];

    let mut inputs = Vec::new();

    for param in params_src.split(", @").map(|part| {
        if part.starts_with('@') {
            part.to_string()
        } else {
            format!("@{}", part)
        }
    }) {
        if !param.contains("@location(") {
            continue;
        }
        let loc_start = param.find("@location(").unwrap() + "@location(".len();
        let loc_end = param[loc_start..].find(')').unwrap() + loc_start;
        let shader_location: u32 = param[loc_start..loc_end].parse().unwrap();
        // Extract "name: type" — strip any extra qualifiers like @interpolate(flat)
        let mut rest = param[loc_end + 1..].trim();
        while rest.starts_with('@') {
            // Skip @interpolate(...) or similar
            if let Some(paren_start) = rest.find('(') {
                if let Some(paren_end) = rest[paren_start..].find(')') {
                    rest = rest[paren_start + paren_end + 1..].trim();
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        let (name, ty) = rest
            .rsplit_once(": ")
            .expect("WGSL vertex input name:type not found");
        let name = name.trim().to_string();
        let format = vertex_format_from_wgsl_type(ty.trim());
        inputs.push(WgslVertexInput { shader_location, name, format });
    }

    inputs
}

/// Build vertex attributes treating all inputs as a single per-vertex buffer.
/// Used for debug_color / debug_font which have no instancing.
fn build_all_as_vertex_attrs(inputs: &[WgslVertexInput]) -> (Vec<wgpu::VertexAttribute>, u64) {
    let mut attrs = Vec::new();
    let mut stride: u64 = 0;
    for input in inputs {
        attrs.push(wgpu::VertexAttribute {
            format: input.format,
            offset: stride,
            shader_location: input.shader_location,
        });
        stride += format_size(input.format);
    }
    let stride = align_vertex_stride(stride);
    (attrs, stride)
}

/// Build two buffer layouts for instanced shaders: buffer 0 is the unit-quad
/// vertex (location 0), buffer 1 is the instance data.
///
/// The instance layout is specified by name→(format, byte_size) in struct
/// memory order, because the WGSL `@location(N)` numbers are assigned
/// sequentially per-variant and can differ for the same field across variants.
/// The name is used to look up the actual location from the parsed WGSL inputs.
fn build_instanced_layouts(
    inputs: &[WgslVertexInput],
    instance_struct: &[(&str, wgpu::VertexFormat)],
) -> (Vec<wgpu::VertexAttribute>, u64, Vec<wgpu::VertexAttribute>, u64) {
    // Buffer 0: vertex position (the input named "aPosition")
    // The WGSL declares vec2<f32> but the actual vertex data is U8Norm
    // (matching the GL VAO: [[0,0],[0xFF,0],[0,0xFF],[0xFF,0xFF]]).
    // wgpu auto-converts Unorm8x2 → vec2<f32> in the shader.
    let vertex_input = inputs
        .iter()
        .find(|i| i.name == "aPosition")
        .expect("instanced shader must have aPosition");
    let vertex_format = wgpu::VertexFormat::Unorm8x2;
    let vertex_attrs = vec![wgpu::VertexAttribute {
        format: vertex_format,
        offset: 0,
        shader_location: vertex_input.shader_location,
    }];
    let vertex_stride = align_vertex_stride(format_size(vertex_format));

    // Build a name → location map from the shader's actual inputs
    let name_to_loc: HashMap<&str, u32> = inputs
        .iter()
        .map(|i| (i.name.as_str(), i.shader_location))
        .collect();

    // Buffer 1: instance data, laid out per the struct memory order.
    // Only emit attributes for fields the shader actually reads.
    let mut instance_attrs = Vec::new();
    let mut instance_offset: u64 = 0;
    for &(field_name, format) in instance_struct {
        if let Some(&loc) = name_to_loc.get(field_name) {
            instance_attrs.push(wgpu::VertexAttribute {
                format,
                offset: instance_offset,
                shader_location: loc,
            });
        }
        // Always advance offset — the struct field exists in memory
        // even if this shader variant doesn't read it.
        instance_offset += format_size(format);
    }
    let instance_stride = align_vertex_stride(instance_offset);

    (vertex_attrs, vertex_stride, instance_attrs, instance_stride)
}

/// Instance struct layout for `CompositeInstance` (gpu_types.rs).
/// Listed in struct field order with the WGSL attribute name used by the
/// shader (after naga translation, field names may have a trailing `_`).
const COMPOSITE_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    ("aDeviceRect",             wgpu::VertexFormat::Float32x4),
    ("aDeviceClipRect",         wgpu::VertexFormat::Float32x4),
    ("aColor",                  wgpu::VertexFormat::Float32x4),
    ("aParams",                 wgpu::VertexFormat::Float32x4),
    ("aUvRect0_",               wgpu::VertexFormat::Float32x4),
    ("aUvRect1_",               wgpu::VertexFormat::Float32x4),
    ("aUvRect2_",               wgpu::VertexFormat::Float32x4),
    ("aFlip",                   wgpu::VertexFormat::Float32x2),
    ("aDeviceRoundedClipRect",  wgpu::VertexFormat::Float32x4),
    ("aDeviceRoundedClipRadii", wgpu::VertexFormat::Float32x4),
];

/// Instance struct layout for `PrimitiveInstanceData` (gpu_types.rs).
const PRIMITIVE_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    ("aData", wgpu::VertexFormat::Sint32x4),
];

/// Instance layout for `ps_quad_mask` (PrimitiveInstanceData + ClipData).
/// Total stride: 32 bytes.
const MASK_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    ("aData",     wgpu::VertexFormat::Sint32x4),   // 16
    ("aClipData", wgpu::VertexFormat::Sint32x4),   // 16
];

/// Instance layout for `ClipMaskInstanceRect` (gpu_types.rs).
/// Used by `cs_clip_rectangle` (both default and FAST_PATH variants).
/// Total stride: 200 bytes.
const CLIP_RECT_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    // ClipMaskInstanceCommon (44 bytes)
    ("aClipDeviceArea",   wgpu::VertexFormat::Float32x4),   // 16
    ("aClipOrigins",      wgpu::VertexFormat::Float32x4),   // 16
    ("aDevicePixelScale", wgpu::VertexFormat::Float32),      // 4
    ("aTransformIds",     wgpu::VertexFormat::Sint32x2),     // 8
    // ClipMaskInstanceRect specific (156 bytes)
    ("aClipLocalPos",     wgpu::VertexFormat::Float32x2),   // 8
    ("aClipLocalRect",    wgpu::VertexFormat::Float32x4),   // 16
    ("aClipMode",         wgpu::VertexFormat::Float32),      // 4
    ("aClipRect_TL",      wgpu::VertexFormat::Float32x4),   // 16
    ("aClipRadii_TL",     wgpu::VertexFormat::Float32x4),   // 16
    ("aClipRect_TR",      wgpu::VertexFormat::Float32x4),   // 16
    ("aClipRadii_TR",     wgpu::VertexFormat::Float32x4),   // 16
    ("aClipRect_BL",      wgpu::VertexFormat::Float32x4),   // 16
    ("aClipRadii_BL",     wgpu::VertexFormat::Float32x4),   // 16
    ("aClipRect_BR",      wgpu::VertexFormat::Float32x4),   // 16
    ("aClipRadii_BR",     wgpu::VertexFormat::Float32x4),   // 16
];

/// Instance layout for `ClipMaskInstanceBoxShadow` (gpu_types.rs).
/// Used by `cs_clip_box_shadow`.
/// Total stride: 84 bytes.
const CLIP_BOX_SHADOW_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    // ClipMaskInstanceCommon (44 bytes)
    ("aClipDeviceArea",         wgpu::VertexFormat::Float32x4),   // 16
    ("aClipOrigins",            wgpu::VertexFormat::Float32x4),   // 16
    ("aDevicePixelScale",       wgpu::VertexFormat::Float32),      // 4
    ("aTransformIds",           wgpu::VertexFormat::Sint32x2),     // 8
    // ClipMaskInstanceBoxShadow specific (40 bytes)
    ("aClipDataResourceAddress", wgpu::VertexFormat::Sint16x2),    // 4
    ("aClipSrcRectSize",        wgpu::VertexFormat::Float32x2),   // 8
    ("aClipMode",               wgpu::VertexFormat::Sint32),       // 4
    ("aStretchMode",            wgpu::VertexFormat::Sint32x2),     // 8
    ("aClipDestRect",           wgpu::VertexFormat::Float32x4),   // 16
];

/// Instance layout for `BlurInstance` (gpu_types.rs).
/// Used by `cs_blur` (both COLOR_TARGET and ALPHA_TARGET variants).
/// Total stride: 28 bytes.
const BLUR_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    ("aBlurRenderTaskAddress", wgpu::VertexFormat::Sint32),    // 4
    ("aBlurSourceTaskAddress", wgpu::VertexFormat::Sint32),    // 4
    ("aBlurDirection",         wgpu::VertexFormat::Sint32),    // 4
    ("aBlurEdgeMode",          wgpu::VertexFormat::Sint32),    // 4
    ("aBlurParams",            wgpu::VertexFormat::Float32x3), // 12
];

/// Instance layout for `ScalingInstance` (gpu_types.rs).
/// Used by `cs_scale`.
/// Total stride: 36 bytes.
const SCALE_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    ("aScaleTargetRect", wgpu::VertexFormat::Float32x4), // 16
    ("aScaleSourceRect", wgpu::VertexFormat::Float32x4), // 16
    ("aSourceRectType",  wgpu::VertexFormat::Float32),   // 4
];

/// Instance layout for `BorderInstance` (gpu_types.rs).
/// Used by `cs_border_solid` and `cs_border_segment`.
/// Total stride: 108 bytes.
const BORDER_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    ("aTaskOrigin",   wgpu::VertexFormat::Float32x2), // 8
    ("aRect",         wgpu::VertexFormat::Float32x4), // 16
    ("aColor0_",      wgpu::VertexFormat::Float32x4), // 16
    ("aColor1_",      wgpu::VertexFormat::Float32x4), // 16
    ("aFlags",        wgpu::VertexFormat::Sint32),    // 4
    ("aWidths",       wgpu::VertexFormat::Float32x2), // 8
    ("aRadii",        wgpu::VertexFormat::Float32x2), // 8
    ("aClipParams1_", wgpu::VertexFormat::Float32x4), // 16
    ("aClipParams2_", wgpu::VertexFormat::Float32x4), // 16
];

/// Instance layout for `LineDecorationJob` (render_target.rs).
/// Used by `cs_line_decoration`.
/// Total stride: 36 bytes.
const LINE_DECORATION_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    ("aTaskRect",           wgpu::VertexFormat::Float32x4), // 16
    ("aLocalSize",          wgpu::VertexFormat::Float32x2), // 8
    ("aWavyLineThickness",  wgpu::VertexFormat::Float32),   // 4
    ("aStyle",              wgpu::VertexFormat::Sint32),     // 4
    ("aAxisSelect",         wgpu::VertexFormat::Float32),    // 4
];

/// Instance layout for `FastLinearGradientInstance`.
/// Used by `cs_fast_linear_gradient`.
/// Total stride: 52 bytes.
const FAST_LINEAR_GRADIENT_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    ("aTaskRect",   wgpu::VertexFormat::Float32x4), // 16
    ("aColor0_",    wgpu::VertexFormat::Float32x4), // 16
    ("aColor1_",    wgpu::VertexFormat::Float32x4), // 16
    ("aAxisSelect", wgpu::VertexFormat::Float32),   // 4
];

/// Instance layout for `LinearGradientInstance`.
/// Used by `cs_linear_gradient`.
/// Total stride: 48 bytes.
const LINEAR_GRADIENT_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    ("aTaskRect",              wgpu::VertexFormat::Float32x4), // 16
    ("aStartPoint",            wgpu::VertexFormat::Float32x2), // 8
    ("aEndPoint",              wgpu::VertexFormat::Float32x2), // 8
    ("aScale",                 wgpu::VertexFormat::Float32x2), // 8
    ("aExtendMode",            wgpu::VertexFormat::Sint32),    // 4
    ("aGradientStopsAddress",  wgpu::VertexFormat::Sint32),    // 4
];

/// Instance layout for `RadialGradientInstance`.
/// Used by `cs_radial_gradient`.
/// Total stride: 52 bytes.
const RADIAL_GRADIENT_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    ("aTaskRect",              wgpu::VertexFormat::Float32x4), // 16
    ("aCenter",                wgpu::VertexFormat::Float32x2), // 8
    ("aScale",                 wgpu::VertexFormat::Float32x2), // 8
    ("aStartRadius",           wgpu::VertexFormat::Float32),   // 4
    ("aEndRadius",             wgpu::VertexFormat::Float32),   // 4
    ("aXYRatio",               wgpu::VertexFormat::Float32),   // 4
    ("aExtendMode",            wgpu::VertexFormat::Sint32),    // 4
    ("aGradientStopsAddress",  wgpu::VertexFormat::Sint32),    // 4
];

/// Instance layout for `ConicGradientInstance`.
/// Used by `cs_conic_gradient`.
/// Total stride: 52 bytes.
const CONIC_GRADIENT_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    ("aTaskRect",              wgpu::VertexFormat::Float32x4), // 16
    ("aCenter",                wgpu::VertexFormat::Float32x2), // 8
    ("aScale",                 wgpu::VertexFormat::Float32x2), // 8
    ("aStartOffset",           wgpu::VertexFormat::Float32),   // 4
    ("aEndOffset",             wgpu::VertexFormat::Float32),   // 4
    ("aAngle",                 wgpu::VertexFormat::Float32),   // 4
    ("aExtendMode",            wgpu::VertexFormat::Sint32),    // 4
    ("aGradientStopsAddress",  wgpu::VertexFormat::Sint32),    // 4
];

/// Instance layout for repacked `SvgFilterInstance`.
/// The original struct uses u16 fields which can't be directly mapped to
/// wgpu vertex attributes (no single-u16 format exists). We repack the
/// instance data to i32 fields on the CPU side before upload.
/// Used by `cs_svg_filter`.
/// Repacked stride: 32 bytes (8 x i32).
const SVG_FILTER_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    ("aFilterRenderTaskAddress",  wgpu::VertexFormat::Sint32),     // 4
    ("aFilterInput1TaskAddress",  wgpu::VertexFormat::Sint32),     // 4
    ("aFilterInput2TaskAddress",  wgpu::VertexFormat::Sint32),     // 4
    ("aFilterKind",               wgpu::VertexFormat::Sint32),     // 4
    ("aFilterInputCount",         wgpu::VertexFormat::Sint32),     // 4
    ("aFilterGenericInt",         wgpu::VertexFormat::Sint32),     // 4
    ("aFilterExtraDataAddress",   wgpu::VertexFormat::Sint32x2),   // 8
];

/// Instance layout for repacked `SVGFEFilterInstance`.
/// Same u16-to-i32 repacking as SVG_FILTER_INSTANCE_LAYOUT.
/// Used by `cs_svg_filter_node`.
/// Repacked stride: 56 bytes.
const SVG_FILTER_NODE_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    ("aFilterTargetRect",                    wgpu::VertexFormat::Float32x4), // 16
    ("aFilterInput1ContentScaleAndOffset",   wgpu::VertexFormat::Float32x4), // 16
    ("aFilterInput2ContentScaleAndOffset",   wgpu::VertexFormat::Float32x4), // 16
    ("aFilterInput1TaskAddress",             wgpu::VertexFormat::Sint32),    // 4
    ("aFilterInput2TaskAddress",             wgpu::VertexFormat::Sint32),    // 4
    ("aFilterKind",                          wgpu::VertexFormat::Sint32),    // 4
    ("aFilterInputCount",                    wgpu::VertexFormat::Sint32),    // 4
    ("aFilterExtraDataAddress",              wgpu::VertexFormat::Sint32x2),  // 8
];

fn build_debug_color_attrs() -> (Vec<wgpu::VertexAttribute>, u64) {
    let attrs = vec![
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Unorm8x4,
            offset: 8,
            shader_location: 1,
        },
    ];
    (attrs, 12)
}

fn build_debug_font_attrs() -> (Vec<wgpu::VertexAttribute>, u64) {
    let attrs = vec![
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Unorm8x4,
            offset: 8,
            shader_location: 1,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x2,
            offset: 12,
            shader_location: 2,
        },
    ];
    (attrs, 20)
}

fn align_vertex_stride(stride: u64) -> u64 {
    let align = wgpu::VERTEX_STRIDE_ALIGNMENT;
    stride.div_ceil(align) * align
}

/// Create a render pipeline for a specific blend mode and depth state from cached shader modules.
fn create_pipeline_for_blend(
    device: &wgpu::Device,
    pipeline_layout: &wgpu::PipelineLayout,
    vs_module: &wgpu::ShaderModule,
    fs_module: &wgpu::ShaderModule,
    vertex_layouts: &ShaderVertexLayouts,
    variant: WgpuShaderVariant,
    blend_mode: WgpuBlendMode,
    depth_state: WgpuDepthState,
    target_format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let pipeline_label = format!("{:?}#{:?}#{:?}#{:?}", variant, blend_mode, depth_state, target_format);

    // Build wgpu vertex buffer layout references from our cached data.
    let vbl_single;
    let vbl_instanced;
    let buffers: &[wgpu::VertexBufferLayout] = match vertex_layouts {
        ShaderVertexLayouts::SingleBuffer { attrs, stride } => {
            vbl_single = [wgpu::VertexBufferLayout {
                array_stride: *stride,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: attrs,
            }];
            &vbl_single
        }
        ShaderVertexLayouts::Instanced {
            vertex_attrs,
            vertex_stride,
            instance_attrs,
            instance_stride,
        } => {
            vbl_instanced = [
                wgpu::VertexBufferLayout {
                    array_stride: *vertex_stride,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: vertex_attrs,
                },
                wgpu::VertexBufferLayout {
                    array_stride: *instance_stride,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: instance_attrs,
                },
            ];
            &vbl_instanced
        }
    };

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(&pipeline_label),
        layout: Some(pipeline_layout),
        vertex: wgpu::VertexState {
            module: vs_module,
            entry_point: Some("main"),
            buffers,
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: fs_module,
            entry_point: Some("main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: blend_mode.to_wgpu_blend_state(),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: depth_state.to_wgpu_depth_stencil(),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    })
}

/// Wrapper that runs `create_all_pipelines` on a thread with a 16 MB stack.
///
/// Naga's WGSL parser uses recursive descent and overflows the default thread
/// stack (~1-2 MB) for large transpiled shaders such as `cs_svg_filter_node`
/// (~1600 lines of WGSL).
fn create_all_pipelines_threaded(
    device: &wgpu::Device,
    pipeline_layout: &wgpu::PipelineLayout,
) -> (
    HashMap<WgpuShaderVariant, ShaderEntry>,
    HashMap<(WgpuShaderVariant, WgpuBlendMode, WgpuDepthState, wgpu::TextureFormat), WgpuProgram>,
) {
    // SAFETY: `device` and `pipeline_layout` are borrowed from the caller's
    // stack frame. We transmute them to 'static so they can cross the thread
    // boundary, then join the thread before this function returns — so the
    // references are always valid.
    struct SendPtr<T>(*const T);
    unsafe impl<T> Send for SendPtr<T> {}

    let device_ptr = SendPtr(device as *const wgpu::Device);
    let layout_ptr = SendPtr(pipeline_layout as *const wgpu::PipelineLayout);
    let handle = std::thread::Builder::new()
        .name("wgpu-shader-compile".into())
        .stack_size(16 * 1024 * 1024) // 16 MB
        .spawn(move || {
            let device = unsafe { &*device_ptr.0 };
            let layout = unsafe { &*layout_ptr.0 };
            create_all_pipelines(device, layout)
        })
        .expect("failed to spawn shader compile thread");
    handle.join().expect("shader compile thread panicked")
}

fn create_all_pipelines(
    device: &wgpu::Device,
    pipeline_layout: &wgpu::PipelineLayout,
) -> (
    HashMap<WgpuShaderVariant, ShaderEntry>,
    HashMap<(WgpuShaderVariant, WgpuBlendMode, WgpuDepthState, wgpu::TextureFormat), WgpuProgram>,
) {
    let mut shaders = HashMap::new();
    let mut pipelines = HashMap::new();

    for (&(name, config), source) in WGSL_SHADERS.iter() {
        // Map the string key to a typed variant.  Shaders that don't have a
        // typed variant (e.g. DEBUG_OVERDRAW configs) are still compiled but
        // not indexed — they can be added to the enum later if needed.
        let variant = match WgpuShaderVariant::from_shader_key(name, config) {
            Some(v) => v,
            None => {
                log::debug!("wgpu: skipping untyped shader ({:?}, {:?})", name, config);
                continue;
            }
        };

        let vs_label = format!("{:?} (VS)", variant);
        let fs_label = format!("{:?} (FS)", variant);

        let vs_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&vs_label),
            source: wgpu::ShaderSource::Wgsl(source.vert_source.into()),
        });
        let fs_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&fs_label),
            source: wgpu::ShaderSource::Wgsl(source.frag_source.into()),
        });

        // Determine buffer layout(s) from the typed variant.
        let inputs = parse_wgsl_vertex_inputs(source.vert_source);
        let instance_layout = variant.instance_layout();

        // Build the cached vertex layout info.
        let vertex_layouts = match (variant, instance_layout) {
            (WgpuShaderVariant::DebugColor, _) => {
                let (attrs, stride) = build_debug_color_attrs();
                ShaderVertexLayouts::SingleBuffer { attrs, stride }
            }
            (WgpuShaderVariant::DebugFont, _) => {
                let (attrs, stride) = build_debug_font_attrs();
                ShaderVertexLayouts::SingleBuffer { attrs, stride }
            }
            (_, Some(inst_layout)) => {
                let (va, vs, ia, is) = build_instanced_layouts(&inputs, inst_layout);
                ShaderVertexLayouts::Instanced {
                    vertex_attrs: va,
                    vertex_stride: vs,
                    instance_attrs: ia,
                    instance_stride: is,
                }
            }
            _ => {
                let (attrs, stride) = build_all_as_vertex_attrs(&inputs);
                ShaderVertexLayouts::SingleBuffer { attrs, stride }
            }
        };

        // Create the default pipeline with PremultipliedAlpha blend, no depth,
        // targeting the surface format (Bgra8Unorm).  Pipelines for other formats
        // (e.g. R8Unorm for clip masks) are created lazily in draw_instanced.
        let default_blend = WgpuBlendMode::PremultipliedAlpha;
        let default_depth = WgpuDepthState::None;
        let default_format = wgpu::TextureFormat::Bgra8Unorm;
        let pipeline = create_pipeline_for_blend(
            device,
            pipeline_layout,
            &vs_module,
            &fs_module,
            &vertex_layouts,
            variant,
            default_blend,
            default_depth,
            default_format,
        );

        pipelines.insert(
            (variant, default_blend, default_depth, default_format),
            WgpuProgram { pipeline },
        );
        shaders.insert(
            variant,
            ShaderEntry { vs_module, fs_module, vertex_layouts },
        );
    }

    (shaders, pipelines)
}

fn image_format_to_wgpu(format: ImageFormat, features: wgpu::Features) -> wgpu::TextureFormat {
    match format {
        ImageFormat::R8 => wgpu::TextureFormat::R8Unorm,
        ImageFormat::BGRA8 => wgpu::TextureFormat::Bgra8Unorm,
        ImageFormat::RGBA8 => wgpu::TextureFormat::Rgba8Unorm,
        ImageFormat::RG8 => wgpu::TextureFormat::Rg8Unorm,
        ImageFormat::RGBAF32 => wgpu::TextureFormat::Rgba32Float,
        ImageFormat::R16 => {
            assert!(
                features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM),
                "ImageFormat::R16 requires wgpu::Features::TEXTURE_FORMAT_16BIT_NORM"
            );
            wgpu::TextureFormat::R16Unorm
        }
        ImageFormat::RG16 => {
            assert!(
                features.contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM),
                "ImageFormat::RG16 requires wgpu::Features::TEXTURE_FORMAT_16BIT_NORM"
            );
            wgpu::TextureFormat::Rg16Unorm
        }
        ImageFormat::RGBAI32 => wgpu::TextureFormat::Rgba32Sint,
    }
}

fn image_buffer_kind_to_texture_dimension(kind: ImageBufferKind) -> wgpu::TextureDimension {
    match kind {
        ImageBufferKind::Texture2D
        | ImageBufferKind::TextureRect
        | ImageBufferKind::TextureExternal
        | ImageBufferKind::TextureExternalBT709 => wgpu::TextureDimension::D2,
    }
}

fn wgpu_format_bytes_per_pixel(format: wgpu::TextureFormat) -> u32 {
    match format {
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm => 4,
        wgpu::TextureFormat::R8Unorm => 1,
        wgpu::TextureFormat::R16Unorm => 2,
        wgpu::TextureFormat::Rg8Unorm => 2,
        wgpu::TextureFormat::Rg16Unorm => 4,
        wgpu::TextureFormat::Rgba32Float | wgpu::TextureFormat::Rgba32Sint => 16,
        other => unreachable!("wgpu_format_bytes_per_pixel: format not in WebRender's set: {:?}", other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn try_device() -> Option<WgpuDevice> {
        let dev = WgpuDevice::new_headless();
        if dev.is_none() {
            eprintln!("wgpu: no adapter available — skipping test");
        }
        dev
    }

    #[test]
    fn headless_init_and_frame_lifecycle() {
        let Some(mut dev) = try_device() else { return };
        let id1 = dev.begin_frame();
        dev.end_frame();
        let id2 = dev.begin_frame();
        assert!(id2 > id1);
        dev.end_frame();
    }

    #[test]
    fn texture_create_and_upload() {
        let Some(mut dev) = try_device() else { return };
        let tex = dev.create_texture(
            ImageBufferKind::Texture2D,
            ImageFormat::BGRA8,
            4,
            4,
            TextureFilter::Linear,
            None,
        );
        let pixels = vec![0xffu8; 4 * 4 * 4];
        dev.upload_texture_immediate(&tex, &pixels);
        dev.delete_texture(tex);
    }

    #[test]
    fn clear_render_target() {
        let Some(mut dev) = try_device() else { return };
        let tex = dev.create_texture(
            ImageBufferKind::Texture2D,
            ImageFormat::BGRA8,
            32,
            32,
            TextureFilter::Nearest,
            Some(RenderTargetInfo { has_depth: false }),
        );
        dev.clear_texture(&tex, [1.0, 0.0, 0.0, 1.0]);
        dev.delete_texture(tex);
    }

    #[test]
    fn create_all_shader_pipelines() {
        let Some(dev) = try_device() else { return };
        // shader_count() = typed variants actually compiled (from_shader_key matched).
        // pipeline_count() = pipelines created (one per variant × default blend/depth/format).
        // These should match — every compiled shader gets a default pipeline.
        assert_eq!(
            dev.pipeline_count(),
            dev.shader_count(),
            "Expected one pipeline per typed shader variant: {} shaders, {} pipelines",
            dev.shader_count(),
            dev.pipeline_count()
        );
    }

    #[test]
    fn render_solid_quad_debug_color() {
        let Some(mut dev) = try_device() else { return };
        let size: u32 = 64;

        let rt = dev.create_texture(
            ImageBufferKind::Texture2D,
            ImageFormat::BGRA8,
            size as i32,
            size as i32,
            TextureFilter::Nearest,
            Some(RenderTargetInfo { has_depth: false }),
        );
        dev.render_debug_color_quad(&rt, [255, 0, 0, 255]);

        let mut pixels = vec![0u8; (size * size * 4) as usize];
        dev.read_texture_pixels(&rt, &mut pixels);

        let cx = size / 2;
        let cy = size / 2;
        let idx = ((cy * size + cx) * 4) as usize;
        let b = pixels[idx];
        let g = pixels[idx + 1];
        let r = pixels[idx + 2];
        let a = pixels[idx + 3];

        assert!(r > 250, "Red channel should be ~255, got {}", r);
        assert!(g < 5, "Green channel should be ~0, got {}", g);
        assert!(b < 5, "Blue channel should be ~0, got {}", b);
        assert!(a > 250, "Alpha channel should be ~255, got {}", a);
    }

    #[test]
    fn render_sampled_quad_debug_font() {
        let Some(mut dev) = try_device() else { return };
        let size: u32 = 64;

        let rt = dev.create_texture(
            ImageBufferKind::Texture2D,
            ImageFormat::BGRA8,
            size as i32,
            size as i32,
            TextureFilter::Nearest,
            Some(RenderTargetInfo { has_depth: false }),
        );
        let src = dev.create_texture(
            ImageBufferKind::Texture2D,
            ImageFormat::R8,
            1,
            1,
            TextureFilter::Nearest,
            None,
        );
        dev.upload_texture_immediate(&src, &[255]);
        dev.render_debug_font_quad(&rt, &src, [0, 255, 0, 255]);

        let mut pixels = vec![0u8; (size * size * 4) as usize];
        dev.read_texture_pixels(&rt, &mut pixels);

        let idx = (((size / 2) * size + (size / 2)) * 4) as usize;
        let b = pixels[idx];
        let g = pixels[idx + 1];
        let r = pixels[idx + 2];
        let a = pixels[idx + 3];

        assert!(g > 250, "Green channel should be ~255, got {}", g);
        assert!(r < 5, "Red channel should be ~0, got {}", r);
        assert!(b < 5, "Blue channel should be ~0, got {}", b);
        assert!(a > 250, "Alpha channel should be ~255, got {}", a);
    }

    #[test]
    fn render_composite_instance() {
        let Some(mut dev) = try_device() else { return };
        // Skip if the CompositeFastPath pipeline wasn't compiled (headless
        // devices may not compile all shader variants).
        if !dev.shaders.contains_key(&WgpuShaderVariant::CompositeFastPath) {
            eprintln!("wgpu: CompositeFastPath shader not available — skipping test");
            return;
        }
        let size: u32 = 64;

        // Render target
        let rt = dev.create_texture(
            ImageBufferKind::Texture2D,
            ImageFormat::BGRA8,
            size as i32,
            size as i32,
            TextureFilter::Nearest,
            Some(RenderTargetInfo { has_depth: false }),
        );

        // Source texture: solid green (BGRA8 = B,G,R,A)
        let src = dev.create_texture(
            ImageBufferKind::Texture2D,
            ImageFormat::BGRA8,
            1,
            1,
            TextureFilter::Nearest,
            None,
        );
        dev.upload_texture_immediate(&src, &[0, 255, 0, 255]); // BGRA: green

        // Build a CompositeInstance as raw bytes.
        // Struct layout (all f32 unless noted):
        //   rect(4), clip_rect(4), color(4), params(4),
        //   uv_rects[0](4), uv_rects[1](4), uv_rects[2](4),
        //   flip(2), rounded_clip_rect(4), rounded_clip_radii(4)
        // = 38 floats = 152 bytes
        let s = size as f32;
        let floats: [f32; 38] = [
            // rect: device-space destination (x0, y0, x1, y1)
            0.0, 0.0, s, s,
            // clip_rect
            0.0, 0.0, s, s,
            // color (white, not used for texture sampling)
            1.0, 1.0, 1.0, 1.0,
            // params: _padding, UV_TYPE_NORMALIZED=0, yuv_format=0, yuv_channel_bit_depth=0
            0.0, 0.0, 0.0, 0.0,
            // uv_rects[0]: normalized UV rect covering full source
            0.0, 0.0, 1.0, 1.0,
            // uv_rects[1]: unused
            0.0, 0.0, 0.0, 0.0,
            // uv_rects[2]: unused
            0.0, 0.0, 0.0, 0.0,
            // flip: (0, 0) = no flip
            0.0, 0.0,
            // rounded_clip_rect: unused
            0.0, 0.0, 0.0, 0.0,
            // rounded_clip_radii: unused
            0.0, 0.0, 0.0, 0.0,
        ];
        let instance_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                floats.as_ptr() as *const u8,
                std::mem::size_of_val(&floats),
            )
        };
        assert_eq!(instance_bytes.len(), 152);

        dev.render_composite_instances(
            &rt,
            Some(&src),
            instance_bytes,
            1,
            WgpuShaderVariant::CompositeFastPath,
            Some(wgpu::Color::BLACK),
        );

        // Read back and verify center pixel is green
        let mut pixels = vec![0u8; (size * size * 4) as usize];
        dev.read_texture_pixels(&rt, &mut pixels);

        let cx = size / 2;
        let cy = size / 2;
        let idx = ((cy * size + cx) * 4) as usize;
        let b = pixels[idx];
        let g = pixels[idx + 1];
        let r = pixels[idx + 2];
        let a = pixels[idx + 3];

        assert!(g > 250, "Green channel should be ~255, got {}", g);
        assert!(r < 5, "Red channel should be ~0, got {}", r);
        assert!(b < 5, "Blue channel should be ~0, got {}", b);
        assert!(a > 250, "Alpha channel should be ~255, got {}", a);
    }

    /// Smoke test: `draw_instanced` with `brush_solid` pipeline runs without
    /// GPU validation errors.  Uses dummy data textures (all zeros) so the
    /// shader will produce degenerate output — the test only verifies that the
    /// draw completes and readback works.
    #[test]
    fn draw_instanced_brush_solid_smoke() {
        let Some(mut dev) = try_device() else { return };
        let size: u32 = 32;

        // Render target
        let rt = dev.create_data_texture(
            "brush_solid RT",
            size,
            size,
            wgpu::TextureFormat::Bgra8Unorm,
            &vec![0u8; (size * size * 4) as usize],
        );
        // Ensure it has RENDER_ATTACHMENT usage — create_data_texture only sets
        // TEXTURE_BINDING | COPY_DST.  Use create_cache_texture instead.
        drop(rt);
        let rt = {
            let tex = dev.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("brush_solid RT"),
                size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            WgpuTexture { texture: tex, format: wgpu::TextureFormat::Bgra8Unorm, width: size, height: size }
        };
        let rt_view = rt.create_view();

        // Minimal data textures (1x1 RGBA32Float, all zeros) for the data
        // texture slots. The shader will sample address 0 and get zeros — the
        // draw will produce degenerate output but should not crash.
        let zero_f32 = [0u8; 16]; // 1 texel of Rgba32Float
        let gpu_cache = dev.create_data_texture(
            "gpu_cache", 1, 1, wgpu::TextureFormat::Rgba32Float, &zero_f32,
        );
        let transforms = dev.create_data_texture(
            "transforms", 1, 1, wgpu::TextureFormat::Rgba32Float, &zero_f32,
        );
        let render_tasks = dev.create_data_texture(
            "render_tasks", 1, 1, wgpu::TextureFormat::Rgba32Float, &zero_f32,
        );
        let prim_headers_f = dev.create_data_texture(
            "prim_headers_f", 1, 1, wgpu::TextureFormat::Rgba32Float, &zero_f32,
        );
        let zero_i32 = [0u8; 16]; // 1 texel of Rgba32Sint
        let prim_headers_i = dev.create_data_texture(
            "prim_headers_i", 1, 1, wgpu::TextureFormat::Rgba32Sint, &zero_i32,
        );

        let gc_view = gpu_cache.create_view();
        let tf_view = transforms.create_view();
        let rt_tasks_view = render_tasks.create_view();
        let phf_view = prim_headers_f.create_view();
        let phi_view = prim_headers_i.create_view();

        let textures = TextureBindings {
            gpu_cache: Some(&gc_view),
            transform_palette: Some(&tf_view),
            render_tasks: Some(&rt_tasks_view),
            prim_headers_f: Some(&phf_view),
            prim_headers_i: Some(&phi_view),
            ..Default::default()
        };

        // PrimitiveInstanceData: aData = ivec4(0, 0, 0, 0)
        // All addresses zero → samples row 0 of every data texture.
        let instance_data: [i32; 4] = [0, 0, 0, 0];
        let instance_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                instance_data.as_ptr() as *const u8,
                std::mem::size_of_val(&instance_data),
            )
        };

        // This is the critical call: draw through the brush_solid pipeline.
        dev.draw_instanced(
            WgpuShaderVariant::BrushSolid,
            WgpuBlendMode::None,
            WgpuDepthState::None,
            &rt_view,
            size,
            size,
            wgpu::TextureFormat::Bgra8Unorm,
            &textures,
            instance_bytes,
            1,
            Some(wgpu::Color::BLACK), // clear to black
            None, // no scissor
            None, // no depth
        );
        dev.flush_encoder();

        // Read back — we don't check specific pixel values (degenerate data
        // means output is undefined), just that readback succeeds without panic.
        let mut pixels = vec![0u8; (size * size * 4) as usize];
        dev.read_texture_pixels(&rt, &mut pixels);

        // If we got here without a GPU validation error or panic, the pipeline
        // layout, vertex format, bind group, and draw submission are all valid.
    }

    /// Full data test: `draw_instanced` with `brush_solid` rendering a red
    /// rectangle by constructing valid WebRender data textures (GPU cache,
    /// prim headers, transforms, render tasks).
    #[test]
    fn draw_instanced_brush_solid_red_rect() {
        let Some(mut dev) = try_device() else { return };
        let size: u32 = 32;

        // ── Render target ───────────────────────────────────────────────
        let rt = {
            let tex = dev.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("brush_solid red RT"),
                size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            WgpuTexture { texture: tex, format: wgpu::TextureFormat::Bgra8Unorm, width: size, height: size }
        };
        let rt_view = rt.create_view();

        // ── GPU cache (Rgba32Float) ─────────────────────────────────────
        // Address 0 (u=0, v=0): solid red color [1.0, 0.0, 0.0, 1.0].
        // brush_solid fetches 1 vec4 via fetch_from_gpu_cache_1(prim_address).
        // The prim_address comes from PrimitiveHeaderI.specific_prim_address.
        let mut gpu_cache_data = vec![0u8; 1024 * 16]; // 1024 texels × 16 bytes/texel
        // Texel 0: red color
        let red: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
        gpu_cache_data[..16].copy_from_slice(unsafe {
            std::slice::from_raw_parts(red.as_ptr() as *const u8, 16)
        });
        let gpu_cache = dev.create_data_texture(
            "test gpu_cache", 1024, 1, wgpu::TextureFormat::Rgba32Float, &gpu_cache_data,
        );

        // ── Transform palette (Rgba32Float) ─────────────────────────────
        // VECS_PER_TRANSFORM = 8 texels per transform.
        // Transform 0: identity matrix + identity inverse.
        // get_fetch_uv(0, 8) = ivec2(0, 0), then reads texels (0..7, 0).
        let identity_4x4: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            0.0, 0.0, 0.0, 1.0,
        ];
        let mut transform_data = vec![0u8; 1024 * 16]; // 1024 texels
        // Forward matrix: texels 0-3
        let identity_bytes = unsafe {
            std::slice::from_raw_parts(identity_4x4.as_ptr() as *const u8, 64)
        };
        transform_data[..64].copy_from_slice(identity_bytes);
        // Inverse matrix: texels 4-7
        transform_data[64..128].copy_from_slice(identity_bytes);
        let transforms = dev.create_data_texture(
            "test transforms", 1024, 1, wgpu::TextureFormat::Rgba32Float, &transform_data,
        );

        // ── Render tasks (Rgba32Float) ──────────────────────────────────
        // VECS_PER_RENDER_TASK = 2 texels per task.
        // Task 0 at texels (0,0) and (1,0):
        //   texel 0: task_rect = (0, 0, size, size)
        //   texel 1: user_data = (device_pixel_scale=1.0, content_origin=(0,0), 0)
        let s = size as f32;
        let task_texel0: [f32; 4] = [0.0, 0.0, s, s];
        let task_texel1: [f32; 4] = [1.0, 0.0, 0.0, 0.0];
        let mut render_tasks_data = vec![0u8; 1024 * 16];
        render_tasks_data[..16].copy_from_slice(unsafe {
            std::slice::from_raw_parts(task_texel0.as_ptr() as *const u8, 16)
        });
        render_tasks_data[16..32].copy_from_slice(unsafe {
            std::slice::from_raw_parts(task_texel1.as_ptr() as *const u8, 16)
        });
        let render_tasks = dev.create_data_texture(
            "test render_tasks", 1024, 1, wgpu::TextureFormat::Rgba32Float, &render_tasks_data,
        );

        // ── Primitive headers F (Rgba32Float) ───────────────────────────
        // VECS_PER_PRIM_HEADER_F = 2 texels per header.
        // Header 0 at texels (0,0) and (1,0):
        //   texel 0: local_rect = (0, 0, size, size)
        //   texel 1: local_clip_rect = (0, 0, size, size)
        let rect_f: [f32; 4] = [0.0, 0.0, s, s];
        let mut prim_f_data = vec![0u8; 1024 * 16];
        let rect_bytes = unsafe {
            std::slice::from_raw_parts(rect_f.as_ptr() as *const u8, 16)
        };
        prim_f_data[..16].copy_from_slice(rect_bytes);
        prim_f_data[16..32].copy_from_slice(rect_bytes);
        let prim_headers_f = dev.create_data_texture(
            "test prim_headers_f", 1024, 1, wgpu::TextureFormat::Rgba32Float, &prim_f_data,
        );

        // ── Primitive headers I (Rgba32Sint) ────────────────────────────
        // VECS_PER_PRIM_HEADER_I = 2 texels per header.
        // Header 0 at texels (0,0) and (1,0):
        //   texel 0: z=0, specific_prim_address=0, transform_id=0, render_task_address=0
        //   texel 1: user_data = [65535, 0, 0, 0]
        //     user_data.x = opacity as i32 (65535 = full opacity, divided by 65535.0 in shader)
        let prim_i_texel0: [i32; 4] = [0, 0, 0, 0];
        let prim_i_texel1: [i32; 4] = [65535, 0, 0, 0];
        let mut prim_i_data = vec![0u8; 1024 * 16];
        prim_i_data[..16].copy_from_slice(unsafe {
            std::slice::from_raw_parts(prim_i_texel0.as_ptr() as *const u8, 16)
        });
        prim_i_data[16..32].copy_from_slice(unsafe {
            std::slice::from_raw_parts(prim_i_texel1.as_ptr() as *const u8, 16)
        });
        let prim_headers_i = dev.create_data_texture(
            "test prim_headers_i", 1024, 1, wgpu::TextureFormat::Rgba32Sint, &prim_i_data,
        );

        // ── Build texture bindings ──────────────────────────────────────
        let gc_view = gpu_cache.create_view();
        let tf_view = transforms.create_view();
        let rt_view2 = render_tasks.create_view();
        let phf_view = prim_headers_f.create_view();
        let phi_view = prim_headers_i.create_view();

        let textures = TextureBindings {
            gpu_cache: Some(&gc_view),
            transform_palette: Some(&tf_view),
            render_tasks: Some(&rt_view2),
            prim_headers_f: Some(&phf_view),
            prim_headers_i: Some(&phi_view),
            ..Default::default()
        };

        // ── Instance data ───────────────────────────────────────────────
        // PrimitiveInstanceData { data: [prim_header_address, clip_address, packed, resource] }
        // prim_header_address = 0 (header index 0)
        // clip_address = 0x7FFFFFFF (CLIP_TASK_EMPTY — no clipping)
        // packed = segment_index(0xFFFF=INVALID) | flags(0) = 0x0000FFFF
        // resource_address = 0 (GPU cache address for the brush) | brush_kind(0) = 0
        let instance_data: [i32; 4] = [
            0,                // prim_header_address
            0x7FFF_FFFFi32,   // clip_address = CLIP_TASK_EMPTY
            0x0000_FFFFi32,   // segment_index = INVALID_SEGMENT_INDEX (0xFFFF)
            0,                // resource_address=0, brush_kind=0
        ];
        let instance_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                instance_data.as_ptr() as *const u8,
                std::mem::size_of_val(&instance_data),
            )
        };

        // ── Draw! ───────────────────────────────────────────────────────
        dev.draw_instanced(
            WgpuShaderVariant::BrushSolid,
            WgpuBlendMode::None,
            WgpuDepthState::None,
            &rt_view,
            size,
            size,
            wgpu::TextureFormat::Bgra8Unorm,
            &textures,
            instance_bytes,
            1,
            Some(wgpu::Color::BLACK), // clear to black first
            None, // no scissor
            None, // no depth
        );
        dev.flush_encoder();

        // ── Readback and verify ─────────────────────────────────────────
        let mut pixels = vec![0u8; (size * size * 4) as usize];
        dev.read_texture_pixels(&rt, &mut pixels);

        // Check the center pixel — should be red (BGRA: 0, 0, 255, 255).
        let cx = size / 2;
        let cy = size / 2;
        let idx = ((cy * size + cx) * 4) as usize;
        let b = pixels[idx];
        let g = pixels[idx + 1];
        let r = pixels[idx + 2];
        let a = pixels[idx + 3];

        assert!(
            r > 200 && g < 30 && b < 30 && a > 200,
            "Expected red pixel at center, got BGRA=({}, {}, {}, {})",
            b, g, r, a,
        );
    }

    /// Smoke test: `draw_instanced` with `brush_solid` ALPHA_PASS pipeline.
    #[test]
    fn draw_instanced_brush_solid_alpha_smoke() {
        let Some(mut dev) = try_device() else { return };
        let size: u32 = 16;

        let rt = {
            let tex = dev.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("brush_solid alpha RT"),
                size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            WgpuTexture { texture: tex, format: wgpu::TextureFormat::Bgra8Unorm, width: size, height: size }
        };
        let rt_view = rt.create_view();

        let textures = TextureBindings::default(); // all dummy

        let instance_data: [i32; 4] = [0, 0, 0, 0];
        let instance_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                instance_data.as_ptr() as *const u8,
                std::mem::size_of_val(&instance_data),
            )
        };

        dev.draw_instanced(
            WgpuShaderVariant::BrushSolidAlpha,
            WgpuBlendMode::PremultipliedAlpha,
            WgpuDepthState::None,
            &rt_view,
            size,
            size,
            wgpu::TextureFormat::Bgra8Unorm,
            &textures,
            instance_bytes,
            1,
            Some(wgpu::Color::BLACK),
            None, // no scissor
            None, // no depth
        );
        dev.flush_encoder();

        let mut pixels = vec![0u8; (size * size * 4) as usize];
        dev.read_texture_pixels(&rt, &mut pixels);
        // Success = no GPU validation error.
    }
}
