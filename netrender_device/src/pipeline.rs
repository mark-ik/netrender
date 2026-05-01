/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! RenderPipeline factories per shader family. Async compile + on-disk
//! `wgpu::PipelineCache` integration land later; for now pipelines are
//! built synchronously at first use via the `WgpuDevice` cache.
//!
//! Note (per design plan §5 Phase 2 / axiom 12): `brush_solid`'s
//! `PrimitiveHeader` and `a_data: vec4<i32>` layout is the GL-era
//! contract preserved here as a smoke test that proves the device
//! path. Phase 2 re-decides the primitive ABI once the batch builder
//! lands; this factory's signature is expected to shift then.

/// Phase 4 solid-rect batch pipeline. Fresh ABI: no GL-era
/// PrimitiveHeader indirection. No vertex buffers — instance data in
/// storage buffer indexed by `@builtin(instance_index)`.
///
/// `depth_format`: when `Some`, the pipeline is compiled with a
/// matching `DepthStencilState`. Opaques (`alpha_blend=false`) write
/// depth; alphas (`alpha_blend=true`) test depth but do not write it.
#[derive(Clone)]
pub struct BrushRectSolidPipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub layout: wgpu::BindGroupLayout,
}

/// Build the `brush_rect_solid` pipeline.
///
/// - `depth_format`: attach a depth-stencil state matching this format.
///   `None` for depth-less passes (legacy / off-screen).
/// - `alpha_blend`: enable premultiplied-alpha blending and disable
///   depth writes. Use `false` for opaque passes (depth write ON,
///   compare LESS) and `true` for alpha passes (depth write OFF,
///   compare LESS, premultiplied blend).
pub fn build_brush_rect_solid(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
    depth_format: Option<wgpu::TextureFormat>,
    alpha_blend: bool,
) -> BrushRectSolidPipeline {
    let layout = crate::binding::brush_rect_solid_layout(device);

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("brush_rect_solid"),
        source: wgpu::ShaderSource::Wgsl(crate::shader::BRUSH_RECT_SOLID_WGSL.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("brush_rect_solid pipeline layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });

    let blend = if alpha_blend {
        Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING)
    } else {
        None
    };

    let depth_stencil = depth_format.map(|fmt| wgpu::DepthStencilState {
        format: fmt,
        depth_write_enabled: Some(!alpha_blend),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    });

    let label = match (alpha_blend, depth_format.is_some()) {
        (false, false) => "brush_rect_solid opaque nodepth",
        (false, true) => "brush_rect_solid opaque",
        (true, false) => "brush_rect_solid alpha nodepth",
        (true, true) => "brush_rect_solid alpha",
    };

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    BrushRectSolidPipeline { pipeline, layout }
}

/// Phase 5 textured-rect pipeline. Same depth/blend logic as
/// `BrushRectSolidPipeline`; different layout (5 bindings: adds
/// `image_texture` and `image_sampler`).
#[derive(Clone)]
pub struct BrushImagePipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub layout: wgpu::BindGroupLayout,
}

/// Build the `brush_image` pipeline.
///
/// - `depth_format`: attach a depth-stencil state matching this format.
/// - `alpha_blend`: enable premultiplied-alpha blend + disable depth writes.
pub fn build_brush_image(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
    depth_format: Option<wgpu::TextureFormat>,
    alpha_blend: bool,
) -> BrushImagePipeline {
    let layout = crate::binding::brush_image_layout(device);

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("brush_image"),
        source: wgpu::ShaderSource::Wgsl(crate::shader::BRUSH_IMAGE_WGSL.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("brush_image pipeline layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });

    let blend = if alpha_blend {
        Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING)
    } else {
        None
    };

    let depth_stencil = depth_format.map(|fmt| wgpu::DepthStencilState {
        format: fmt,
        depth_write_enabled: Some(!alpha_blend),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    });

    let label = match (alpha_blend, depth_format.is_some()) {
        (false, false) => "brush_image opaque nodepth",
        (false, true) => "brush_image opaque",
        (true, false) => "brush_image alpha nodepth",
        (true, true) => "brush_image alpha",
    };

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    BrushImagePipeline { pipeline, layout }
}

/// Analytic gradient kind, selected at pipeline-compile time via the
/// WGSL `override GRADIENT_KIND` constant. The numeric values match
/// the constants the shader compares against; do not renumber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GradientKind {
    Linear = 0,
    Radial = 1,
    Conic = 2,
}

impl GradientKind {
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

/// Phase 8D unified analytic-gradient pipeline. Selects per-kind
/// behavior via the `GRADIENT_KIND` override constant; cache key on
/// `WgpuDevice` includes `(color_format, depth_format, alpha_blend,
/// kind)` for 6 specialized pipelines per format combo.
#[derive(Clone)]
pub struct BrushGradientPipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub layout: wgpu::BindGroupLayout,
}

/// Build the `brush_gradient` pipeline with the given `kind`
/// specialization.
pub fn build_brush_gradient(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
    depth_format: Option<wgpu::TextureFormat>,
    alpha_blend: bool,
    kind: GradientKind,
) -> BrushGradientPipeline {
    let layout = crate::binding::brush_gradient_layout(device);

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("brush_gradient"),
        source: wgpu::ShaderSource::Wgsl(crate::shader::BRUSH_GRADIENT_WGSL.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("brush_gradient pipeline layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });

    let blend = if alpha_blend {
        Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING)
    } else {
        None
    };

    let depth_stencil = depth_format.map(|fmt| wgpu::DepthStencilState {
        format: fmt,
        depth_write_enabled: Some(!alpha_blend),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    });

    let kind_label = match kind {
        GradientKind::Linear => "linear",
        GradientKind::Radial => "radial",
        GradientKind::Conic => "conic",
    };
    let blend_label = if alpha_blend { "alpha" } else { "opaque" };
    let depth_label = if depth_format.is_some() { "" } else { " nodepth" };
    let label = format!("brush_gradient {} {}{}", kind_label, blend_label, depth_label);

    let constants: &[(&str, f64)] = &[("GRADIENT_KIND", kind.as_u32() as f64)];

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(&label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions {
                constants,
                zero_initialize_workgroup_memory: false,
            },
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants,
                zero_initialize_workgroup_memory: false,
            },
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    BrushGradientPipeline { pipeline, layout }
}



/// Phase 9A rounded-rect clip-mask pipeline. Renders a fullscreen
/// quad outputting `Rgba8Unorm` (or any single-color target) coverage
/// for a rounded rect. The `HAS_ROUNDED_CORNERS` specialization
/// (Phase 9C fast path) toggles the SDF math vs. an axis-aligned
/// step.
#[derive(Clone)]
pub struct ClipRectanglePipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub layout: wgpu::BindGroupLayout,
}

/// Build the `cs_clip_rectangle` pipeline.
///
/// - `target_format`: typically `Rgba8Unorm` for use as a coverage
///   image; any single-color format works.
/// - `has_rounded_corners`: when `false`, specializes the WGSL
///   `HAS_ROUNDED_CORNERS` override to skip the SDF (Phase 9C fast
///   path).
pub fn build_clip_rectangle(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
    has_rounded_corners: bool,
) -> ClipRectanglePipeline {
    let layout = crate::binding::cs_clip_rectangle_layout(device);

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("cs_clip_rectangle"),
        source: wgpu::ShaderSource::Wgsl(crate::shader::CS_CLIP_RECTANGLE_WGSL.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("cs_clip_rectangle pipeline layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });

    let constants: &[(&str, f64)] = &[(
        "HAS_ROUNDED_CORNERS",
        if has_rounded_corners { 1.0 } else { 0.0 },
    )];

    let label = if has_rounded_corners {
        "cs_clip_rectangle rounded"
    } else {
        "cs_clip_rectangle fast_path"
    };

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions {
                constants,
                zero_initialize_workgroup_memory: false,
            },
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants,
                zero_initialize_workgroup_memory: false,
            },
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    ClipRectanglePipeline { pipeline, layout }
}

/// Phase 6 separable-Gaussian-blur pipeline. No depth stencil — blur
/// targets are off-screen intermediates that don't participate in the
/// main scene depth buffer.
#[derive(Clone)]
pub struct BrushBlurPipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub layout: wgpu::BindGroupLayout,
}

/// Build the `brush_blur` pipeline for `target_format`.
///
/// No depth, no blend (each blur pass writes opaque intermediate values).
/// The same pipeline is used for both horizontal and vertical passes; only
/// the `BlurParams.step` uniform differs.
pub fn build_brush_blur(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
) -> BrushBlurPipeline {
    let layout = crate::binding::brush_blur_layout(device);

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("brush_blur"),
        source: wgpu::ShaderSource::Wgsl(crate::shader::BRUSH_BLUR_WGSL.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("brush_blur pipeline layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("brush_blur"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    BrushBlurPipeline { pipeline, layout }
}

#[derive(Clone)]
pub struct BrushSolidPipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub layout: wgpu::BindGroupLayout,
}

/// Build a brush_solid pipeline. `alpha_pass` selects the WGSL
/// `override` specialisation: the same shader source specialises into
/// opaque and alpha-clipped pipelines without authoring a second WGSL
/// file. `target_format` is the second cache key dimension —
/// `Rgba8Unorm` for the main framebuffer vs. `R8Unorm` for alpha masks
/// each get their own compiled pipeline.
pub fn build_brush_solid_specialized(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
    alpha_pass: bool,
) -> BrushSolidPipeline {
    let layout = crate::binding::brush_solid_layout(device);

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("brush_solid"),
        source: wgpu::ShaderSource::Wgsl(crate::shader::BRUSH_SOLID_WGSL.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("brush_solid pipeline layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });

    // Override `ALPHA_PASS` at pipeline-compile time. The `f64` type
    // is wgpu 29's required ABI for override constants; booleans
    // cross via 0.0 / 1.0.
    let constants: &[(&str, f64)] = &[
        ("ALPHA_PASS", if alpha_pass { 1.0 } else { 0.0 }),
    ];

    // Per-instance `a_data: vec4<i32>` — one ivec4 per primitive,
    // four vertices per primitive (the triangle strip's corners).
    const A_DATA_LAYOUT: wgpu::VertexBufferLayout = wgpu::VertexBufferLayout {
        array_stride: 16, // 4 × i32
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &[wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Sint32x4,
            offset: 0,
            shader_location: 0,
        }],
    };

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(if alpha_pass { "brush_solid alpha" } else { "brush_solid opaque" }),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[A_DATA_LAYOUT],
            compilation_options: wgpu::PipelineCompilationOptions {
                constants,
                zero_initialize_workgroup_memory: false,
            },
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants,
                zero_initialize_workgroup_memory: false,
            },
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    BrushSolidPipeline { pipeline, layout }
}
