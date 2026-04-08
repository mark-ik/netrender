/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Smoke test for the shared-device API (`WgpuDevice::from_shared_device`).
//!
//! Verifies that an externally-created wgpu::Device+Queue can be used to
//! initialise WebRender's GPU resources and perform basic operations.

#![cfg(feature = "wgpu_backend")]

extern crate webrender;

use webrender::wgpu;
use webrender::WgpuDevice;

/// Helper: create a wgpu Device+Queue the way a host app (e.g. egui) would.
fn create_external_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::None,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok()?;

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("host-app device"),
            ..Default::default()
        },
    ))
    .ok()?;

    Some((device, queue))
}

#[test]
fn shared_device_creates_successfully() {
    let (device, queue) = create_external_device()
        .expect("failed to create wgpu device (no GPU adapter available?)");

    // Clone handles — this is what a host app would do before handing them to WebRender.
    let wr_device = device.clone();
    let wr_queue = queue.clone();

    let wgpu_dev = WgpuDevice::from_shared_device(wr_device, wr_queue);

    // The device should report no surface (headless mode).
    assert!(!wgpu_dev.has_surface());

    // The underlying wgpu::Device should be the same object (Arc-shared).
    // Verify by checking that both references can query features without panicking.
    let _host_features = device.features();
    let _wr_features = wgpu_dev.wgpu_device().features();
}

#[test]
fn shared_device_can_create_and_upload_texture() {
    let (device, queue) = create_external_device()
        .expect("failed to create wgpu device (no GPU adapter available?)");

    let mut wgpu_dev = WgpuDevice::from_shared_device(device.clone(), queue.clone());

    // Create a small RGBA8 texture through WebRender's device abstraction.
    let tex = wgpu_dev.create_data_texture(
        "smoke-test",
        4,  // width
        4,  // height
        wgpu::TextureFormat::Rgba8Unorm,
        &[0u8; 4 * 4 * 4],  // 4x4 pixels, 4 bytes each
    );

    // Verify we got a texture with the right dimensions.
    assert_eq!(tex.width, 4);
    assert_eq!(tex.height, 4);

    // Flush any pending work (the host would do this at frame end).
    wgpu_dev.flush_encoder();

    // The host device should still be functional after WebRender used it.
    let _buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("host-side buffer after WR"),
        size: 64,
        usage: wgpu::BufferUsages::UNIFORM,
        mapped_at_creation: false,
    });
}
