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

/// A wgpu-backed texture handle.
pub struct WgpuTexture {
    texture: wgpu::Texture,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
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
    /// Window surface for presentation. None in headless mode.
    surface: Option<wgpu::Surface<'static>>,
    surface_config: Option<wgpu::SurfaceConfiguration>,
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
            surface: None,
            surface_config: None,
        })
    }

    /// Create a device with a window surface for presentation.
    ///
    /// The caller creates the `wgpu::Surface<'static>` from its window handle
    /// and passes it here along with the initial framebuffer size.
    pub fn new_with_surface(
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        let instance = wgpu::Instance::default();
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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
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
            surface: Some(surface),
            surface_config: Some(surface_config),
        })
    }

    /// Returns true if this device has a presentation surface.
    pub fn has_surface(&self) -> bool {
        self.surface.is_some()
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
        &self,
        target: &WgpuTexture,
        source_texture: Option<&WgpuTexture>,
        instance_bytes: &[u8],
        instance_count: u32,
        config: &str,
        clear: bool,
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
            config,
            clear,
        );
    }

    /// Render composite tile instances to an arbitrary texture view.
    ///
    /// This is the core rendering method — `render_composite_instances` delegates
    /// here. Used directly when rendering to a surface texture view.
    pub fn render_composite_instances_to_view(
        &self,
        target_view: &wgpu::TextureView,
        target_width: u32,
        target_height: u32,
        source_texture: Option<&WgpuTexture>,
        instance_bytes: &[u8],
        instance_count: u32,
        config: &str,
        clear: bool,
    ) {
        let pipeline_key = ("composite", config);
        let program = self
            .pipelines
            .get(&pipeline_key)
            .unwrap_or_else(|| panic!("composite pipeline not found for config {:?}", config));

        // Transform: orthographic projection matching the target dimensions
        let projection = ortho(target_width as f32, target_height as f32);
        let mut transform_data = Vec::with_capacity(64);
        for f in &projection {
            transform_data.extend_from_slice(&f.to_le_bytes());
        }
        let transform_buf = self.create_uniform_buffer("composite transform", &transform_data);

        let mut tex_size_data = Vec::with_capacity(8);
        tex_size_data.extend_from_slice(&(target_width as f32).to_le_bytes());
        tex_size_data.extend_from_slice(&(target_height as f32).to_le_bytes());
        let tex_size_buf = self.create_uniform_buffer("composite texture size", &tex_size_data);
        let mali_buf =
            self.create_uniform_buffer("composite mali workaround", &0u32.to_le_bytes());

        let source_view = source_texture.map(|t| {
            t.texture
                .create_view(&wgpu::TextureViewDescriptor::default())
        });
        let (bg0, bg1) = self.create_bind_groups_with_color0(
            source_view.as_ref(),
            &transform_buf,
            &tex_size_buf,
            &mali_buf,
        );

        // Unit quad vertex buffer: 4 corners as Unorm8x2, padded to 4-byte
        // stride (VERTEX_STRIDE_ALIGNMENT). Matches GL's QUAD_VERTICES.
        let quad_verts: [[u8; 4]; 4] = [
            [0, 0, 0, 0],
            [0xFF, 0, 0, 0],
            [0, 0xFF, 0, 0],
            [0xFF, 0xFF, 0, 0],
        ];
        let quad_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                quad_verts.as_ptr() as *const u8,
                std::mem::size_of_val(&quad_verts),
            )
        };
        let vb = self.create_vertex_buffer("composite quad verts", quad_bytes);

        // Index buffer: two triangles
        let indices: [u16; 6] = [0, 1, 2, 2, 1, 3];
        let idx_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                indices.as_ptr() as *const u8,
                std::mem::size_of_val(&indices),
            )
        };
        let ib = self.create_index_buffer("composite indices", idx_bytes);

        // Instance buffer
        let instance_buf = self.create_vertex_buffer("composite instances", instance_bytes);

        let load = if clear {
            wgpu::LoadOp::Clear(wgpu::Color::BLACK)
        } else {
            wgpu::LoadOp::Load
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("composite render"),
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
            pass.set_vertex_buffer(0, vb.slice(..));
            pass.set_vertex_buffer(1, instance_buf.slice(..));
            pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..6, 0, 0..instance_count);
        }
        self.queue.submit([encoder.finish()]);
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
        other => unreachable!("WGSL vertex input type not in WebRender's set: {}", other),
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
#[allow(dead_code)]
const PRIMITIVE_INSTANCE_LAYOUT: &[(&str, wgpu::VertexFormat)] = &[
    ("aData", wgpu::VertexFormat::Sint32x4),
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

        // Determine buffer layout(s) based on shader kind.
        // - debug_color / debug_font: single per-vertex buffer (no instancing)
        // - composite: vertex + instance buffer with CompositeInstance layout
        // - all others: single per-vertex buffer for now (pipeline creates
        //   successfully; correct instanced layouts will be added as needed)
        let inputs = parse_wgsl_vertex_inputs(source.vert_source);
        let instance_layout = match name {
            "composite" => Some(COMPOSITE_INSTANCE_LAYOUT),
            _ => None,
        };

        // Build the buffer layouts — either one or two buffers.
        let vertex_buf_attrs;
        let vertex_buf_stride;
        let instance_buf_attrs;
        let instance_buf_stride;
        let vertex_layouts_1;
        let vertex_layouts_2;
        let vertex_layouts: &[wgpu::VertexBufferLayout] = match (name, instance_layout) {
            ("debug_color", _) | ("debug_font", _) => {
                let (attrs, stride) = match name {
                    "debug_color" => build_debug_color_attrs(),
                    "debug_font" => build_debug_font_attrs(),
                    _ => unreachable!(),
                };
                vertex_buf_attrs = attrs;
                vertex_buf_stride = stride;
                vertex_layouts_1 = [wgpu::VertexBufferLayout {
                    array_stride: vertex_buf_stride,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &vertex_buf_attrs,
                }];
                &vertex_layouts_1
            }
            (_, Some(inst_layout)) => {
                let (va, vs, ia, is) = build_instanced_layouts(&inputs, inst_layout);
                vertex_buf_attrs = va;
                vertex_buf_stride = vs;
                instance_buf_attrs = ia;
                instance_buf_stride = is;
                vertex_layouts_2 = [
                    wgpu::VertexBufferLayout {
                        array_stride: vertex_buf_stride,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &vertex_buf_attrs,
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: instance_buf_stride,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &instance_buf_attrs,
                    },
                ];
                &vertex_layouts_2
            }
            _ => {
                // Fallback: all inputs as a single per-vertex buffer.
                let (attrs, stride) = build_all_as_vertex_attrs(&inputs);
                vertex_buf_attrs = attrs;
                vertex_buf_stride = stride;
                vertex_layouts_1 = [wgpu::VertexBufferLayout {
                    array_stride: vertex_buf_stride,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &vertex_buf_attrs,
                }];
                &vertex_layouts_1
            }
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

    #[test]
    fn render_composite_instance() {
        let Some(mut dev) = try_device() else { return };
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
            "FAST_PATH,TEXTURE_2D",
            true,
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
}
