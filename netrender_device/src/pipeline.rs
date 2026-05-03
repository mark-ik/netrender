/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! RenderPipeline factories for the WGSL pipelines used by render-graph
//! tasks (rounded-rect clip mask + separable Gaussian blur). Built
//! synchronously at first use via the `WgpuDevice` cache.
//!
//! The brush_solid / brush_rect_solid / brush_image / brush_gradient
//! factories were retired alongside netrender's batched WGSL
//! rasterizer; vello is the sole rasterizer on main now.

/// Analytic gradient kind, used by `netrender::SceneGradient` and the
/// vello rasterizer's gradient translator. The numeric values are
/// preserved for any future ABI that might re-introduce a WGSL
/// gradient pipeline; do not renumber.
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
