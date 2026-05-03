/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! BindGroupLayout factories for the WGSL pipelines used by render-graph
//! tasks. The brush_solid / brush_rect_solid / brush_image /
//! brush_gradient layouts were retired alongside netrender's batched
//! WGSL rasterizer.

/// `cs_clip_rectangle` bind group layout (Phase 9A).
///
/// One binding: a uniform `ClipParams` carrying the rect bounds and
/// corner radii in target-pixel space. The shader reads
/// `position.xy` directly for per-pixel coords; no storage buffers
/// needed.
pub(crate) fn cs_clip_rectangle_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("cs_clip_rectangle bind group layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

/// `brush_blur` bind group layout (Phase 6).
///
/// - Slot 0: input_texture — texture_2d<f32>, filterable (FRAGMENT)
/// - Slot 1: input_sampler — Filtering / bilinear-clamp (FRAGMENT)
/// - Slot 2: params uniform — `BlurParams { step: vec2<f32>, _pad: vec2<f32> }` (FRAGMENT)
pub(crate) fn brush_blur_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("brush_blur bind group layout"),
        entries: &[
            // 0: input_texture (filterable, FRAGMENT)
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
            // 1: input_sampler (Filtering, FRAGMENT)
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            // 2: params uniform (FRAGMENT)
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}
