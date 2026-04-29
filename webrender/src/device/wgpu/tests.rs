/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Cross-module integration smoke tests. See plan §6 S2 / S4 receipts.

use super::*;
use std::path::Path;

/// Decode an oracle PNG into (width, height, RGBA8 bytes).
fn load_oracle_png(name: &str) -> (u32, u32, Vec<u8>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("oracle")
        .join(name);
    let file =
        std::fs::File::open(&path).unwrap_or_else(|e| panic!("opening {}: {}", path.display(), e));
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().expect("png read_info");
    let info = reader.info();
    assert_eq!(
        info.color_type,
        png::ColorType::Rgba,
        "oracle PNGs are expected to be RGBA",
    );
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    let (w, h) = (info.width, info.height);
    let mut buf = vec![0u8; reader.output_buffer_size()];
    reader.next_frame(&mut buf).expect("png decode frame");
    (w, h, buf)
}

/// Read an entire wgpu colour target back to CPU as tightly-packed
/// RGBA8 bytes (no row padding). Internally pads on the GPU side
/// and unpacks rows on the CPU side.
fn readback_target(dev: &adapter::WgpuDevice, target: &wgpu::Texture, w: u32, h: u32) -> Vec<u8> {
    dev.read_rgba8_texture(target, w, h)
}

/// Count pixels whose any RGBA channel differs by more than `tolerance`.
fn count_pixel_diffs(actual: &[u8], expected: &[u8], tolerance: u8) -> usize {
    assert_eq!(actual.len(), expected.len());
    let mut diffs = 0;
    for (a, b) in actual.chunks_exact(4).zip(expected.chunks_exact(4)) {
        for c in 0..4 {
            if a[c].abs_diff(b[c]) > tolerance {
                diffs += 1;
                break;
            }
        }
    }
    diffs
}

/// P1.1 receipt: render one solid quad through the production-shape
/// brush_solid pipeline. Inputs flow through two storage buffers that
/// mirror `prim_shared.glsl::PrimitiveHeader` and the gpu_buffer's
/// brush-specific `vec4` slot — replacing the S2 palette/push-constant
/// smoke shape. Renderer-side wiring (per-instance vertex attributes,
/// transform, picture-task, clip-area, draw-loop dispatch) lands in
/// later P1 sub-slices; this smoke pins the storage-buffer pattern.
#[test]
fn render_rect_smoke() {
    let adapter = adapter::WgpuDevice::boot().expect("wgpu boot");
    let dev = &adapter.core;
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let dim = 8_u32;

    let target = dev.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("P1.1 smoke target"),
        size: wgpu::Extent3d {
            width: dim,
            height: dim,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let pipe = pipeline::build_brush_solid(&dev.device, format);

    // PrimitiveHeader storage: one entry. `local_rect` covers full clip
    // space (-1..1); `specific_prim_address` points at gpu_buffer slot 0
    // where the colour lives. The other fields are placeholders until
    // P1.2+ wires the transform / picture-task / clip pipeline.
    //
    // Layout (std430, 64 bytes; matches the WGSL struct in
    // `shaders/brush_solid.wgsl`):
    //   local_rect            vec4<f32>
    //   local_clip_rect       vec4<f32>
    //   z, specific_prim_address, transform_id, picture_task_address
    //                         4 × i32
    //   user_data             vec4<i32>
    let mut header_bytes: Vec<u8> = Vec::with_capacity(64);
    for f in [-1.0_f32, -1.0, 1.0, 1.0] {
        header_bytes.extend_from_slice(&f.to_ne_bytes());
    }
    for f in [-1.0_f32, -1.0, 1.0, 1.0] {
        header_bytes.extend_from_slice(&f.to_ne_bytes());
    }
    // z, specific_prim_address, transform_id, picture_task_address
    for i in [0_i32, 0, 0, 0] {
        header_bytes.extend_from_slice(&i.to_ne_bytes());
    }
    for i in [0_i32; 4] {
        header_bytes.extend_from_slice(&i.to_ne_bytes());
    }
    let prim_headers =
        buffer::create_storage_buffer(&dev.device, &dev.queue, "P1 prim_headers", &header_bytes);

    // Transform storage: identity matrix for both `m` and `inv_m`.
    // 128 bytes std430 — see WGSL `Transform { m, inv_m }`.
    let identity: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
        0.0, 0.0, 0.0, 1.0,
    ];
    let transform_bytes: Vec<u8> = identity
        .iter()
        .chain(identity.iter())
        .flat_map(|f| f.to_ne_bytes())
        .collect();
    let transforms =
        buffer::create_storage_buffer(&dev.device, &dev.queue, "P1 transforms", &transform_bytes);

    // GpuBuffer storage: slot 0 holds opaque red as a vec4<f32>.
    let color: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    let gpu_buffer_bytes: Vec<u8> = color.iter().flat_map(|f| f.to_ne_bytes()).collect();
    let gpu_buffer_f =
        buffer::create_storage_buffer(&dev.device, &dev.queue, "P1 gpu_buffer_f", &gpu_buffer_bytes);

    let bind_group = binding::brush_solid_bind_group(
        &dev.device,
        &pipe.layout,
        &prim_headers,
        &transforms,
        &gpu_buffer_f,
    );

    // One DrawIntent: instance_index 0 reads PrimitiveHeader[0],
    // which addresses gpu_buffer_f[0] for the colour. No push constants
    // (production routes everything through indexed storage reads).
    let draws = vec![pass::DrawIntent {
        pipeline: pipe.pipeline.clone(),
        bind_group: bind_group.clone(),
        vertex_range: 0..4,
        instance_range: 0..1,
        dynamic_offsets: Vec::new(),
        push_constants: Vec::new(),
    }];

    let mut encoder = adapter.create_encoder("P1.1 smoke encoder");
    adapter.encode_pass(
        &mut encoder,
        pass::RenderPassTarget {
            label: "P1.1 smoke pass",
            color: pass::ColorAttachment::clear(&target_view, wgpu::Color::TRANSPARENT),
            depth: None,
        },
        &draws,
    );
    adapter.submit(encoder);

    // The full-NDC quad covers the whole target. Centre row's first
    // pixel confirms the storage-buffer fetch path delivers the colour.
    let actual_rgba = readback_target(&adapter, &target, dim, dim);
    let mid_row = (dim / 2) as usize;
    let row_start = mid_row * dim as usize * 4;
    assert_eq!(&actual_rgba[row_start..row_start + 4], &[255, 0, 0, 255]);
}

/// Adapter-plan §A1 receipt: `WgpuDevice::boot()` succeeds, and
/// the lazy `ensure_<family>` cache pattern works for both
/// repeated and distinct format keys. Compiling + non-panicking is
/// the receipt; cache hit/miss is a `HashMap` invariant we don't
/// need to retest.
#[test]
fn wgpu_device_a1_smoke() {
    let dev = adapter::WgpuDevice::boot().expect("WgpuDevice boot");
    let _ = dev.ensure_brush_solid(wgpu::TextureFormat::Rgba8Unorm);
    let _ = dev.ensure_brush_solid(wgpu::TextureFormat::Rgba8Unorm);
    let _ = dev.ensure_brush_solid(wgpu::TextureFormat::Bgra8Unorm);
}

/// Adapter-plan §A2 design seed: `WgpuDevice::create_texture` works
/// in isolation; produces a `WgpuTexture` that can hand out a
/// default view. Not yet wired into renderer/* (callsite migration
/// is per-call-site sub-slices of A2).
#[test]
fn wgpu_device_a2_create_texture_smoke() {
    let dev = adapter::WgpuDevice::boot().expect("WgpuDevice boot");
    let tex = dev.create_texture(&texture::TextureDesc {
        label: "A2 smoke",
        width: 16,
        height: 16,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
    });
    assert_eq!((tex.width, tex.height), (16, 16));
    assert_eq!(tex.format, wgpu::TextureFormat::Rgba8Unorm);
    let _view = tex.create_view();
}

/// Adapter-plan §A2.1 prep: dither-shaped texture (8×8 R8) gets
/// created and uploaded via `WgpuDevice::create_texture` +
/// `upload_texture`. Mirrors what `init.rs:484` does today via
/// `device::Device::create_texture` + `upload_texture_immediate`.
/// Receipt for the texture API surface that the dither migration
/// will use once the per-pass encoding (A2.4) is in place to handle
/// the bind sites.
#[test]
fn wgpu_device_a21_dither_create_upload_smoke() {
    let dev = adapter::WgpuDevice::boot().expect("WgpuDevice boot");
    let tex = dev.create_texture(&texture::TextureDesc {
        label: "dither_matrix (A2.1 prep)",
        width: 8,
        height: 8,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    });
    // Synthetic 8×8 dither pattern (real dither matrix is in
    // init.rs; this test just exercises upload).
    let data: Vec<u8> = (0..64).collect();
    dev.upload_texture(&tex, &data);
    // Force a flush so the upload is observable.
    dev.core
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
}

/// S4 first slice: render the `blank` oracle scene (full-frame white
/// clear at wrench's 3840×2160 hidpi default) through the new wgpu path
/// and pixel-diff against the captured oracle PNG. Tolerance: 0 (exact
/// match expected — clear-to-white is the simplest possible scene).
#[test]
fn oracle_blank_smoke() {
    let (oracle_w, oracle_h, oracle_rgba) = load_oracle_png("blank.png");
    assert_eq!((oracle_w, oracle_h), (3840, 2160));

    let adapter = adapter::WgpuDevice::boot().expect("wgpu boot");
    let dev = &adapter.core;
    let target = dev.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("oracle blank target"),
        size: wgpu::Extent3d {
            width: oracle_w,
            height: oracle_h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = adapter.create_encoder("oracle blank encoder");
    adapter.encode_pass(
        &mut encoder,
        pass::RenderPassTarget {
            label: "oracle blank pass",
            color: pass::ColorAttachment::clear(&view, wgpu::Color::WHITE),
            depth: None,
        },
        &[],
    );
    adapter.submit(encoder);

    let actual_rgba = readback_target(&adapter, &target, oracle_w, oracle_h);
    let diffs = count_pixel_diffs(&actual_rgba, &oracle_rgba, 0);
    assert_eq!(
        diffs, 0,
        "blank scene must match oracle exactly (got {} pixel mismatches)",
        diffs
    );
}

/// Adapter-plan §A2.X.2 receipt: pass targets carry depth load/store
/// policy alongside colour. This is the wgpu-native landing spot for
/// renderer callsites that currently pair `clear_target(...,
/// Some(depth), ...)` with `invalidate_depth_target()`.
#[test]
fn pass_target_depth_smoke() {
    let adapter = adapter::WgpuDevice::boot().expect("wgpu boot");
    let dev = &adapter.core;

    let color = dev.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("A2.X.2 color target"),
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let depth = dev.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("A2.X.2 depth target"),
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = adapter.create_encoder("A2.X.2 depth encoder");
    adapter.encode_pass(
        &mut encoder,
        pass::RenderPassTarget {
            label: "A2.X.2 depth pass",
            color: pass::ColorAttachment::clear(&color_view, wgpu::Color::TRANSPARENT),
            depth: Some(pass::DepthAttachment::clear(&depth_view, 1.0).discard()),
        },
        &[],
    );
    adapter.submit(encoder);

    let actual_rgba = readback_target(&adapter, &color, 4, 4);
    assert_eq!(actual_rgba, vec![0; 4 * 4 * 4]);
}
