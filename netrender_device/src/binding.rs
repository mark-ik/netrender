/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! BindGroupLayout + BindGroup factories. Storage buffers replace the
//! GL-era data-texture pattern (axiom 7 of the design plan).

/// brush_solid bind group layout.
///
/// - Slot 0: PrimitiveHeader storage buffer (read-only). Mirrors GL
///   `sPrimitiveHeadersF` + `sPrimitiveHeadersI`, collapsed into a
///   single std430 struct.
/// - Slot 1: Transform storage buffer (read-only). Mirrors GL
///   `sTransformPalette`; 8 × `vec4<f32>` (= `mat4` + `inv_mat4`) per
///   entry. Indexed by the low 22 bits of `header.transform_id`.
/// - Slot 2: GpuBuffer storage buffer (read-only). Holds brush-specific
///   `vec4<f32>` slots indexed by `header.specific_prim_address` (per
///   GL `fetch_from_gpu_buffer_1f`).
/// - Slot 3: RenderTaskData storage buffer (read-only). Mirrors GL
///   `sRenderTasks`. Both PictureTask and ClipArea read from this
///   table — `user_data` is task-type-specific.
/// - Slot 4: PerFrame uniform (read-only). Carries `u_transform`
///   (orthographic projection).
/// - Slot 5: Clip-mask 2D texture (R8Unorm). Mirrors GL `sClipMask`.
///   Sampled via `textureLoad` (no sampler) for the alpha-pass clip
///   multiply. Bound for both opaque and alpha pipelines (the layout
///   demands it); only the alpha-pass shader reads it.
pub(crate) fn brush_solid_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let storage_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let uniform_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("brush_solid bind group layout"),
        entries: &[
            storage_entry(0),
            storage_entry(1),
            storage_entry(2),
            storage_entry(3),
            uniform_entry(4),
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    })
}

/// brush_rect_solid bind group layout (Phase 3 ABI).
///
/// - Slot 0: instances storage buffer (read-only). Per-instance
///   `RectInstance { rect, color, clip: vec4<f32>; transform_id: u32 }`.
/// - Slot 1: transforms storage buffer (read-only). Resolved
///   `mat4x4<f32>` per spatial node, column-major, 64 bytes each.
///   Index 0 is always the identity matrix.
/// - Slot 2: PerFrame uniform (read-only). Orthographic projection
///   `u_transform: mat4x4<f32>` from device pixels to NDC.
pub(crate) fn brush_rect_solid_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let storage_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("brush_rect_solid bind group layout"),
        entries: &[
            storage_entry(0), // instances
            storage_entry(1), // transforms (Phase 3)
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX,
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

/// brush_image bind group layout (Phase 5).
///
/// - Slot 0: instances storage buffer (ImageInstance array, read-only, VERTEX)
/// - Slot 1: transforms storage buffer (mat4x4 per node, read-only, VERTEX)
/// - Slot 2: PerFrame uniform (ortho projection, VERTEX)
/// - Slot 3: image_texture — texture_2d<f32>, non-filtering (FRAGMENT)
/// - Slot 4: image_sampler — NonFiltering / nearest-clamp (FRAGMENT)
pub(crate) fn brush_image_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("brush_image bind group layout"),
        entries: &[
            // 0: instances (storage, VERTEX)
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // 1: transforms (storage, VERTEX)
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // 2: per_frame (uniform, VERTEX)
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // 3: image_texture (non-filtering, FRAGMENT)
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            // 4: image_sampler (NonFiltering, FRAGMENT)
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                count: None,
            },
        ],
    })
}

/// ps_text_run bind group layout (Phase 10a.1 / 10b.1).
///
/// Five bindings, structurally identical to `brush_image_layout`: the
/// consumer-visible delta is the shader's sample swizzle (text reads
/// `.r` for grayscale or `.rgb` for dual-source; image reads `.rgba`).
/// `TextureSampleType::Float { filterable: false }` works for both
/// the 10a.1 R8Unorm atlas and the 10b.1 Rgba8Unorm atlas; no layout
/// change was required when the atlas format flipped. NonFiltering
/// sampler at slot 4; bilinear / subpixel positioning is a 10b
/// concern.
pub(crate) fn ps_text_run_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ps_text_run bind group layout"),
        entries: &[
            // 0: instances (storage, VERTEX)
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // 1: transforms (storage, VERTEX)
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // 2: per_frame (uniform, VERTEX)
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // 3: atlas_texture (Rgba8Unorm, NonFiltering, FRAGMENT)
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            // 4: atlas_sampler (NonFiltering, FRAGMENT)
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                count: None,
            },
        ],
    })
}

/// brush_gradient bind group layout (Phase 8D unified analytic
/// gradient). Four bindings: instances (storage, VERTEX), transforms
/// (storage, VERTEX), per_frame (uniform, VERTEX), and stops (storage,
/// FRAGMENT). The N-stop ramp lives in the stops buffer; per-instance
/// `stops_offset` + `stops_count` index into it.
pub(crate) fn brush_gradient_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let vertex_storage_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("brush_gradient bind group layout"),
        entries: &[
            vertex_storage_entry(0), // instances
            vertex_storage_entry(1), // transforms
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // 3: stops storage (FRAGMENT — sampled per-pixel for the
            // N-stop ramp scan).
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

/// cs_clip_rectangle bind group layout (Phase 9A).
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

/// brush_blur bind group layout (Phase 6).
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

/// Build a brush_solid bind group from PrimitiveHeader, Transform,
/// GpuBuffer, RenderTaskData storage buffers, the PerFrame uniform,
/// and the clip-mask texture view. All bound as full-buffer ranges
/// (or full-texture views); per-draw indexing happens inside the
/// shader via the `a_data` decode chain.
#[allow(dead_code)] // exercised by tests; renderer-body consumer lands at Phase 2
pub(crate) fn brush_solid_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    prim_headers: &wgpu::Buffer,
    transforms: &wgpu::Buffer,
    gpu_buffer_f: &wgpu::Buffer,
    render_tasks: &wgpu::Buffer,
    per_frame: &wgpu::Buffer,
    clip_mask: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("brush_solid bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: prim_headers.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: transforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: gpu_buffer_f.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: render_tasks.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: per_frame.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(clip_mask),
            },
        ],
    })
}
