/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Same-device external texture composition.
//!
//! This is the zero-copy lane for producer textures that already live
//! on the renderer's `wgpu::Device` (WebGL canvases, video frames,
//! or embedder-owned render targets). Unlike vello's
//! `register_texture` path, this pass samples the producer texture
//! directly; the source texture does not need `COPY_SRC` usage.

/// One external texture draw into a target view.
#[derive(Debug, Clone, Copy)]
pub struct ExternalTexturePlacement {
    /// Destination rectangle in target pixel coordinates.
    pub dest_rect: [f32; 4],
    /// Source UV rectangle in normalized texture coordinates.
    pub uv: [f32; 4],
    /// Additional opacity applied while blending over the target.
    pub opacity: f32,
}

impl ExternalTexturePlacement {
    pub fn new(dest_rect: [f32; 4]) -> Self {
        Self {
            dest_rect,
            uv: [0.0, 0.0, 1.0, 1.0],
            opacity: 1.0,
        }
    }

    pub fn with_uv(mut self, uv: [f32; 4]) -> Self {
        self.uv = uv;
        self
    }

    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }
}

const EXTERNAL_TEXTURE_WGSL: &str = r#"
struct Params {
    dest: vec4<f32>,
    uv: vec4<f32>,
    viewport_opacity: vec4<f32>,
};

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var<uniform> params: Params;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let local = corners[vertex_index];
    let pixel = mix(params.dest.xy, params.dest.zw, local);
    let viewport = params.viewport_opacity.xy;

    var out: VsOut;
    out.position = vec4<f32>(
        (pixel.x / viewport.x) * 2.0 - 1.0,
        1.0 - (pixel.y / viewport.y) * 2.0,
        0.0,
        1.0,
    );
    out.uv = mix(params.uv.xy, params.uv.zw, local);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let color = textureSample(source_texture, source_sampler, in.uv);
    let opacity = clamp(params.viewport_opacity.z, 0.0, 1.0);
    return vec4<f32>(color.rgb, color.a * opacity);
}
"#;

#[derive(Clone)]
pub(crate) struct ExternalTexturePipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

pub(crate) fn build_external_texture_pipeline(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
) -> ExternalTexturePipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("netrender external texture composite"),
        source: wgpu::ShaderSource::Wgsl(EXTERNAL_TEXTURE_WGSL.into()),
    });

    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("netrender external texture layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("netrender external texture pipeline layout"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("netrender external texture pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("netrender external texture sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });

    ExternalTexturePipeline {
        pipeline,
        layout,
        sampler,
    }
}

fn params_bytes(
    viewport_width: u32,
    viewport_height: u32,
    placement: ExternalTexturePlacement,
) -> [u8; 48] {
    let values = [
        placement.dest_rect[0],
        placement.dest_rect[1],
        placement.dest_rect[2],
        placement.dest_rect[3],
        placement.uv[0],
        placement.uv[1],
        placement.uv[2],
        placement.uv[3],
        viewport_width as f32,
        viewport_height as f32,
        placement.opacity,
        0.0,
    ];
    let mut bytes = [0u8; 48];
    for (index, value) in values.iter().enumerate() {
        bytes[index * 4..(index + 1) * 4].copy_from_slice(&value.to_ne_bytes());
    }
    bytes
}

pub(crate) fn compose_external_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipe: &ExternalTexturePipeline,
    source_view: &wgpu::TextureView,
    target_view: &wgpu::TextureView,
    viewport_width: u32,
    viewport_height: u32,
    placement: ExternalTexturePlacement,
) {
    if viewport_width == 0
        || viewport_height == 0
        || placement.opacity <= 0.0
        || placement.dest_rect[0] == placement.dest_rect[2]
        || placement.dest_rect[1] == placement.dest_rect[3]
    {
        return;
    }

    let bytes = params_bytes(viewport_width, viewport_height, placement);
    let params = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("netrender external texture params"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: true,
    });
    {
        let mut view = params.slice(..).get_mapped_range_mut();
        view.copy_from_slice(&bytes);
    }
    params.unmap();

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("netrender external texture bind group"),
        layout: &pipe.layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(source_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&pipe.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params.as_entire_binding(),
            },
        ],
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("netrender external texture encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("netrender external texture pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipe.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
    queue.submit([encoder.finish()]);
}
