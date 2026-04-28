/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! RenderPipeline cache; async compile; `wgpu::PipelineCache` disk-backed
//! reuse. See plan §4.11, §6 S1.

#[derive(Clone)]
pub struct BrushSolidPipeline {
    pub pipeline: wgpu::RenderPipeline,
    pub layout: wgpu::BindGroupLayout,
}

/// Build the brush_solid pipeline. `MAX_PALETTE_ENTRIES` is supplied as
/// a WGSL `override` constant per §4.9, so the same shader source can
/// later specialize to different palette sizes without authoring a
/// second WGSL file.
pub fn build_brush_solid(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
) -> BrushSolidPipeline {
    let layout = super::binding::brush_solid_layout(device);

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("brush_solid"),
        source: wgpu::ShaderSource::Wgsl(super::shader::BRUSH_SOLID_WGSL.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("brush_solid pipeline layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 4, // single u32 push-constant: palette_index
    });

    // Override MAX_PALETTE_ENTRIES at pipeline-compile time per §4.9.
    let constants: &[(&str, f64)] = &[("MAX_PALETTE_ENTRIES", 16.0_f64)];

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("brush_solid"),
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

    BrushSolidPipeline { pipeline, layout }
}
