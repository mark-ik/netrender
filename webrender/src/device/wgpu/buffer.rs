/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Vertex / index / uniform / storage buffer arenas; dynamic-offset
//! suballocator. See plan §4.6, §4.7, §6 S1.

/// Allocate a uniform-buffer arena big enough for `slots` per-draw
/// entries of `entry_size` bytes each, padded to the device's
/// `min_uniform_buffer_offset_alignment`. Returns the buffer plus the
/// stride (the sub-allocation step) for use with
/// `set_bind_group(offset)` per §4.7.
pub fn create_uniform_arena(
    device: &wgpu::Device,
    entry_size: u64,
    slots: u64,
) -> (wgpu::Buffer, u64) {
    let alignment = device.limits().min_uniform_buffer_offset_alignment as u64;
    let stride = entry_size.next_multiple_of(alignment);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("uniform arena"),
        size: stride * slots,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    (buffer, stride)
}

/// Create a storage buffer initialized with `contents`. Storage-buffer
/// access replaces the GL-era data-texture pattern per §4.6.
pub fn create_storage_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    contents: &[u8],
) -> wgpu::Buffer {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: contents.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, contents);
    buffer
}

/// Create a vertex buffer initialized with `contents`. The renderer's
/// per-instance attribute streams (e.g. `aData ivec4`) live here.
pub fn create_vertex_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    contents: &[u8],
) -> wgpu::Buffer {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: contents.len() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, contents);
    buffer
}
