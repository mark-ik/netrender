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
    let file = std::fs::File::open(&path)
        .unwrap_or_else(|e| panic!("opening {}: {}", path.display(), e));
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
fn readback_target(dev: &core::Device, target: &wgpu::Texture, w: u32, h: u32) -> Vec<u8> {
    let row_bytes = w * 4;
    let padded = row_bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = dev.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("oracle readback"),
        size: padded as u64 * h as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = dev
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("oracle readback encoder"),
        });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target,
            mip_level: 0,
            origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    dev.queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    dev.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    rx.recv().expect("map sender").expect("map");
    let mapped = slice.get_mapped_range();

    let mut out = Vec::with_capacity((row_bytes * h) as usize);
    for row in 0..h as usize {
        let src = row * padded as usize;
        out.extend_from_slice(&mapped[src..src + row_bytes as usize]);
    }
    out
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

/// S2 receipt: record a single rectangle DrawIntent, flush via
/// `pass.rs`, read back the target, assert the pixels match the palette
/// colour. End-to-end exercise of the §4.6–4.9 architectural patterns:
/// storage-buffer palette read, dynamic-offset per-draw uniform,
/// push-constant palette index, WGSL override-specialized constant,
/// `DrawIntent` recording into `pass::flush_pass` (no inline draw).
#[test]
fn render_rect_smoke() {
    let dev = core::boot().expect("wgpu boot");
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let dim = 8_u32;

    let target = dev.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("S2 smoke target"),
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

    // Per-draw uniform: full-clip-space rect at slot 0.
    let entry_size: u64 = 16; // vec4<f32>
    let (uniform_buffer, _stride) =
        buffer::create_uniform_arena(&dev.device, entry_size, 1);
    let rect: [f32; 4] = [-1.0, -1.0, 2.0, 2.0];
    let rect_bytes: Vec<u8> = rect.iter().flat_map(|f| f.to_ne_bytes()).collect();
    dev.queue.write_buffer(&uniform_buffer, 0, &rect_bytes);

    // Storage palette: index 0 is opaque red.
    let mut palette = vec![[0.0_f32; 4]; 16];
    palette[0] = [1.0, 0.0, 0.0, 1.0];
    let palette_bytes: Vec<u8> = palette
        .iter()
        .flat_map(|c| c.iter().flat_map(|f| f.to_ne_bytes()))
        .collect();
    let palette_buffer =
        buffer::create_storage_buffer(&dev.device, &dev.queue, "S2 palette", &palette_bytes);

    let bind_group = binding::brush_solid_bind_group(
        &dev.device,
        &pipe.layout,
        &uniform_buffer,
        entry_size,
        &palette_buffer,
    );

    // Record one DrawIntent — palette_index = 0 → red.
    let palette_index: u32 = 0;
    let draws = vec![pass::DrawIntent {
        vertex_range: 0..4,
        instance_range: 0..1,
        uniform_offset: 0,
        push_constants: palette_index.to_ne_bytes().to_vec(),
    }];

    let mut encoder = dev
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("S2 smoke encoder"),
        });
    pass::flush_pass(
        &mut encoder,
        &target_view,
        &pipe.pipeline,
        &bind_group,
        wgpu::Color::TRANSPARENT,
        "S2 smoke pass",
        &draws,
    );

    // Readback.
    let padded_bytes_per_row = (dim * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let readback = dev.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("S2 smoke readback"),
        size: padded_bytes_per_row as u64 * dim as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(dim),
            },
        },
        wgpu::Extent3d {
            width: dim,
            height: dim,
            depth_or_array_layers: 1,
        },
    );
    dev.queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    dev.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    rx.recv().expect("map sender").expect("map");

    let mapped = slice.get_mapped_range();
    // The full-NDC quad covers the whole target. Sample the centre row's
    // first pixel to confirm the palette colour reached the framebuffer.
    let mid_row = (dim / 2) as usize;
    let row_start = mid_row * padded_bytes_per_row as usize;
    assert_eq!(&mapped[row_start..row_start + 4], &[255, 0, 0, 255]);
}

/// S4 first slice: render the `blank` oracle scene (full-frame white
/// clear at wrench's 3840×2160 hidpi default) through the new wgpu path
/// and pixel-diff against the captured oracle PNG. Tolerance: 0 (exact
/// match expected — clear-to-white is the simplest possible scene).
#[test]
fn oracle_blank_smoke() {
    let (oracle_w, oracle_h, oracle_rgba) = load_oracle_png("blank.png");
    assert_eq!((oracle_w, oracle_h), (3840, 2160));

    let dev = core::boot().expect("wgpu boot");
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

    let mut encoder = dev
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("oracle blank encoder"),
        });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("oracle blank pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    dev.queue.submit([encoder.finish()]);

    let actual_rgba = readback_target(&dev, &target, oracle_w, oracle_h);
    let diffs = count_pixel_diffs(&actual_rgba, &oracle_rgba, 0);
    assert_eq!(
        diffs, 0,
        "blank scene must match oracle exactly (got {} pixel mismatches)",
        diffs
    );
}
