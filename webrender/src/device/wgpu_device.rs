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

use super::{GpuFrameId, TextureFilter};
use crate::internal_types::RenderTargetInfo;
use crate::shader_source::WGSL_SHADERS;

/// A wgpu-backed texture handle.
pub struct WgpuTexture {
    texture: wgpu::Texture,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
}

/// A wgpu-backed shader pipeline.
pub struct WgpuProgram {
    pipeline: wgpu::RenderPipeline,
}

pub struct WgpuDevice {
    device: wgpu::Device,
    queue: wgpu::Queue,
    #[allow(dead_code)]
    features: wgpu::Features,
    frame_id: GpuFrameId,
    pipelines: HashMap<(&'static str, &'static str), WgpuProgram>,
    #[allow(dead_code)]
    pipeline_layout: wgpu::PipelineLayout,
    bind_group_layout_0: wgpu::BindGroupLayout,
    bind_group_layout_1: wgpu::BindGroupLayout,
    global_sampler: wgpu::Sampler,
    dummy_texture_f32: wgpu::TextureView,
    dummy_texture_i32: wgpu::TextureView,
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

        let dummy_texture_f32 =
            create_dummy_texture(&device, &queue, wgpu::TextureFormat::Rgba8Unorm);
        let dummy_texture_i32 =
            create_dummy_texture(&device, &queue, wgpu::TextureFormat::Rgba32Sint);
        let pipelines = create_all_pipelines(&device, &pipeline_layout);

        Some(WgpuDevice {
            device,
            queue,
            features: required_features,
            frame_id: GpuFrameId::new(0),
            pipelines,
            pipeline_layout,
            bind_group_layout_0,
            bind_group_layout_1,
            global_sampler,
            dummy_texture_f32,
            dummy_texture_i32,
        })
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

    fn create_uniform_buffer(&self, label: &str, data: &[u8]) -> wgpu::Buffer {
        use wgpu::util::DeviceExt;
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: data,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            })
    }

    fn create_vertex_buffer(&self, label: &str, data: &[u8]) -> wgpu::Buffer {
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

    fn create_bind_groups_with_color0(
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

    pub fn read_texture_pixels(&self, texture: &WgpuTexture, output: &mut [u8]) {
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

    pub fn pipeline_count(&self) -> usize {
        self.pipelines.len()
    }

    pub fn render_debug_color_quad(&self, target: &WgpuTexture, color: [u8; 4]) {
        let target_view = target
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let projection = ortho(target.width as f32, target.height as f32);
        let mut transform_data = Vec::with_capacity(64);
        for f in &projection {
            transform_data.extend_from_slice(&f.to_le_bytes());
        }
        let transform_buf = self.create_uniform_buffer("debug_color transform", &transform_data);

        let mut tex_size_data = Vec::with_capacity(8);
        tex_size_data.extend_from_slice(&(target.width as f32).to_le_bytes());
        tex_size_data.extend_from_slice(&(target.height as f32).to_le_bytes());
        let tex_size_buf = self.create_uniform_buffer("debug_color texture size", &tex_size_data);
        let mali_buf =
            self.create_uniform_buffer("debug_color mali workaround", &0u32.to_le_bytes());
        let (bg0, bg1) = self.create_bind_groups(&transform_buf, &tex_size_buf, &mali_buf);

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

        let pipeline = &self.pipelines[&("debug_color", "")].pipeline;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("debug_color render"),
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
        self.queue.submit([encoder.finish()]);
    }

    pub fn render_debug_font_quad(
        &self,
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

        let projection = ortho(target.width as f32, target.height as f32);
        let mut transform_data = Vec::with_capacity(64);
        for f in &projection {
            transform_data.extend_from_slice(&f.to_le_bytes());
        }
        let transform_buf = self.create_uniform_buffer("debug_font transform", &transform_data);

        let mut tex_size_data = Vec::with_capacity(8);
        tex_size_data.extend_from_slice(&(target.width as f32).to_le_bytes());
        tex_size_data.extend_from_slice(&(target.height as f32).to_le_bytes());
        let tex_size_buf = self.create_uniform_buffer("debug_font texture size", &tex_size_data);
        let mali_buf =
            self.create_uniform_buffer("debug_font mali workaround", &0u32.to_le_bytes());
        let (bg0, bg1) =
            self.create_bind_groups_with_color0(Some(&source_view), &transform_buf, &tex_size_buf, &mali_buf);

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

        let pipeline = &self.pipelines[&("debug_font", "")].pipeline;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("debug_font render"),
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
        self.queue.submit([encoder.finish()]);
    }
}

fn ortho(w: f32, h: f32) -> [f32; 16] {
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
        -1.0,
        0.0,
        -1.0,
        1.0,
        0.0,
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
            float_tex(0),
            float_tex(1),
            float_tex(2),
            float_tex(3),
            float_tex(4),
            float_tex(5),
            float_tex(6),
            float_tex(7),
            sint_tex(8),
            float_tex(9),
            float_tex(10),
            sint_tex(11),
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
    let bpp = match format {
        wgpu::TextureFormat::Rgba8Unorm => 4,
        wgpu::TextureFormat::Rgba32Sint => 16,
        _ => 4,
    };
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &vec![0u8; bpp],
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
        other => panic!("Unsupported WGSL vertex input type: {}", other),
    }
}

fn format_size(fmt: wgpu::VertexFormat) -> u64 {
    match fmt {
        wgpu::VertexFormat::Float32 | wgpu::VertexFormat::Sint32 => 4,
        wgpu::VertexFormat::Float32x2 | wgpu::VertexFormat::Sint32x2 => 8,
        wgpu::VertexFormat::Float32x3 | wgpu::VertexFormat::Sint32x3 => 12,
        wgpu::VertexFormat::Float32x4 | wgpu::VertexFormat::Sint32x4 => 16,
        wgpu::VertexFormat::Unorm8x2 => 2,
        wgpu::VertexFormat::Unorm8x4 => 4,
        wgpu::VertexFormat::Unorm16x2 | wgpu::VertexFormat::Uint16x2 => 4,
        wgpu::VertexFormat::Unorm16x4 | wgpu::VertexFormat::Uint16x4 => 8,
        _ => panic!("Unsupported format_size: {:?}", fmt),
    }
}

fn build_vertex_layouts(vertex_wgsl: &str) -> Vec<wgpu::VertexBufferLayout<'static>> {
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

    let mut attrs: Vec<wgpu::VertexAttribute> = Vec::new();
    let mut stride: u64 = 0;

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
        let ty = param
            .rsplit_once(": ")
            .map(|(_, ty)| ty.trim())
            .expect("WGSL vertex input type not found");
        let format = vertex_format_from_wgsl_type(ty);
        attrs.push(wgpu::VertexAttribute {
            format,
            offset: stride,
            shader_location,
        });
        stride += format_size(format);
    }

    let stride = align_vertex_stride(stride);
    let attrs = attrs.leak();
    vec![wgpu::VertexBufferLayout {
        array_stride: stride,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: attrs,
    }]
}

fn build_debug_color_layouts() -> Vec<wgpu::VertexBufferLayout<'static>> {
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
    ]
    .leak();
    vec![wgpu::VertexBufferLayout {
        array_stride: 12,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: attrs,
    }]
}

fn build_debug_font_layouts() -> Vec<wgpu::VertexBufferLayout<'static>> {
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
    ]
    .leak();
    vec![wgpu::VertexBufferLayout {
        array_stride: 20,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: attrs,
    }]
}

fn align_vertex_stride(stride: u64) -> u64 {
    let align = wgpu::VERTEX_STRIDE_ALIGNMENT;
    stride.div_ceil(align) * align
}

fn create_all_pipelines(
    device: &wgpu::Device,
    pipeline_layout: &wgpu::PipelineLayout,
) -> HashMap<(&'static str, &'static str), WgpuProgram> {
    let mut pipelines = HashMap::new();

    for (&(name, config), source) in WGSL_SHADERS.iter() {
        let vs_label = format!("{}#{} (VS)", name, config);
        let fs_label = format!("{}#{} (FS)", name, config);

        let vs_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&vs_label),
            source: wgpu::ShaderSource::Wgsl(source.vert_source.into()),
        });
        let fs_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&fs_label),
            source: wgpu::ShaderSource::Wgsl(source.frag_source.into()),
        });

        let vertex_layouts = match name {
            "debug_color" => build_debug_color_layouts(),
            "debug_font" => build_debug_font_layouts(),
            _ => build_vertex_layouts(source.vert_source),
        };
        let pipeline_label = format!("{}#{}", name, config);
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&pipeline_label),
            layout: Some(pipeline_layout),
            vertex: wgpu::VertexState {
                module: &vs_module,
                entry_point: Some("main"),
                buffers: &vertex_layouts,
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &fs_module,
                entry_point: Some("main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
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
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        pipelines.insert((name, config), WgpuProgram { pipeline });
    }

    pipelines
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
        other => panic!("wgpu_format_bytes_per_pixel: unhandled format {:?}", other),
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
        assert_eq!(
            dev.pipeline_count(),
            WGSL_SHADERS.len(),
            "Expected {} shader pipelines, got {}",
            WGSL_SHADERS.len(),
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
}
