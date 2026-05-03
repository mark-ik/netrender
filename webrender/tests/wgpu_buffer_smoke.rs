/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! P4e smoke: WgpuDevice's VBO + PBO methods produce real wgpu::Buffer
//! resources and round-trip data via fill_vbo + readback.

#![cfg(feature = "wgpu_backend")]

use std::sync::Arc;
use webrender::{
    GpuResources, VertexAttribute, VertexDescriptor, VertexUsageHint, WgpuDevice,
};

fn try_create_device() -> Option<WgpuDevice> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("wgpu_buffer_smoke device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
        experimental_features: wgpu::ExperimentalFeatures::default(),
    }))
    .ok()?;
    Some(WgpuDevice::from_parts(
        Arc::new(instance),
        Arc::new(adapter),
        Arc::new(device),
        Arc::new(queue),
        None,
        None,
    ))
}

#[test]
fn vbo_allocate_fill_round_trip() {
    let Some(mut wgpu_device) = try_create_device() else {
        eprintln!("skip: no wgpu adapter available");
        return;
    };

    let mut vbo: webrender::WgpuVbo<u32> = wgpu_device.create_vbo();
    wgpu_device.allocate_vbo(&mut vbo, 8, VertexUsageHint::Static);
    assert_eq!(vbo.count, 8);
    assert!(vbo.buffer.is_some());

    let data: [u32; 8] = [10, 20, 30, 40, 50, 60, 70, 80];
    wgpu_device.fill_vbo(&vbo, &data, 0);

    // Readback path: copy VBO -> mappable buffer -> map -> compare.
    let device = wgpu_device.device().clone();
    let queue = wgpu_device.queue().clone();
    let bytes = (data.len() * std::mem::size_of::<u32>()) as u64;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("vbo readback"),
        size: bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("vbo readback encoder"),
    });
    encoder.copy_buffer_to_buffer(vbo.buffer.as_ref().unwrap(), 0, &readback, 0, bytes);
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).expect("poll");
    let mapped = slice.get_mapped_range();
    let read: &[u32] = bytemuck_cast(&mapped);
    assert_eq!(read, &data);

    drop(mapped);
    readback.unmap();
    wgpu_device.delete_vbo(vbo);
}

// Tiny inline cast (avoiding a bytemuck dep just for a test).
fn bytemuck_cast(bytes: &[u8]) -> &[u32] {
    assert_eq!(bytes.len() % 4, 0);
    unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const u32, bytes.len() / 4) }
}

#[test]
fn vao_lazy_buffers_then_update_round_trip() {
    let Some(mut wgpu_device) = try_create_device() else {
        eprintln!("skip: no wgpu adapter available");
        return;
    };

    static VERT_ATTRS: &[VertexAttribute] = &[VertexAttribute::quad_instance_vertex()];
    static INST_ATTRS: &[VertexAttribute] = &[VertexAttribute::f32x4("aRect")];
    static DESC: VertexDescriptor = VertexDescriptor {
        vertex_attributes: VERT_ATTRS,
        instance_attributes: INST_ATTRS,
    };

    let vao = wgpu_device.create_vao(&DESC, 1);
    // Lazy: no buffers allocated yet.
    assert!(vao.vertex_buffer.borrow().is_none());
    assert!(vao.instance_buffer.borrow().is_none());
    assert!(vao.index_buffer.borrow().is_none());

    let verts: [u16; 4] = [0xAA, 0xBB, 0xCC, 0xDD];
    wgpu_device.update_vao_main_vertices(&vao, &verts, VertexUsageHint::Static);
    assert!(vao.vertex_buffer.borrow().is_some());
    assert_eq!(vao.vertex_count.get(), 4);

    let insts: [f32; 4] = [1.0, 2.0, 3.0, 4.0];
    wgpu_device.update_vao_instances(&vao, &insts, VertexUsageHint::Dynamic, None);
    assert!(vao.instance_buffer.borrow().is_some());
    assert_eq!(vao.instance_count.get(), 4);

    let idx: [u16; 6] = [0, 1, 2, 0, 2, 3];
    wgpu_device.update_vao_indices(&vao, &idx, VertexUsageHint::Static);
    assert!(vao.index_buffer.borrow().is_some());
    assert_eq!(vao.index_count.get(), 6);

    wgpu_device.delete_vao(vao);
}

#[test]
fn pbo_with_size_creates_buffer() {
    let Some(mut wgpu_device) = try_create_device() else {
        eprintln!("skip: no wgpu adapter available");
        return;
    };

    let pbo_empty = wgpu_device.create_pbo();
    assert_eq!(pbo_empty.size, 0);
    assert!(pbo_empty.buffer.is_none());

    let pbo_sized = wgpu_device.create_pbo_with_size(1024);
    assert_eq!(pbo_sized.size, 1024);
    assert!(pbo_sized.buffer.is_some());
    let buf = pbo_sized.buffer.as_ref().unwrap();
    assert_eq!(buf.size(), 1024);
    assert!(buf.usage().contains(wgpu::BufferUsages::MAP_READ));

    wgpu_device.delete_pbo(pbo_empty);
    wgpu_device.delete_pbo(pbo_sized);
}
