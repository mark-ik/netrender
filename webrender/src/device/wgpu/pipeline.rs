/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! RenderPipeline factories per shader family. Async compile + on-disk
//! `wgpu::PipelineCache` integration land later in P1; for now pipelines
//! are built synchronously at first use via the `WgpuDevice` cache.
//! See parent plan §4.9 (override specialisation), §4.11 (pipeline
//! cache).

#[derive(Clone)]
pub struct BrushSolidPipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub layout: wgpu::BindGroupLayout,
}

/// Build a brush_solid pipeline. `alpha_pass` selects the WGSL
/// `override` specialisation per parent §4.9: the same shader source
/// specialises into opaque and alpha-clipped pipelines without
/// authoring a second WGSL file. `target_format` is the second cache
/// key dimension — `Rgba8Unorm` for the main framebuffer vs.
/// `R8Unorm` for alpha masks each get their own compiled pipeline.
pub fn build_brush_solid_specialized(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
    alpha_pass: bool,
) -> BrushSolidPipeline {
    let layout = super::binding::brush_solid_layout(device);

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("brush_solid"),
        source: wgpu::ShaderSource::Wgsl(super::shader::BRUSH_SOLID_WGSL.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("brush_solid pipeline layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });

    // Override `ALPHA_PASS` at pipeline-compile time per §4.9. The
    // `f64` type is wgpu 29's required ABI for override constants;
    // booleans cross via 0.0 / 1.0.
    let constants: &[(&str, f64)] = &[
        ("ALPHA_PASS", if alpha_pass { 1.0 } else { 0.0 }),
    ];

    // Per-instance `a_data: vec4<i32>` matches GL
    // `PER_INSTANCE in ivec4 aData`. Step rate Instance — one
    // `aData` per primitive, four vertices per primitive (the
    // triangle strip's corners).
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
