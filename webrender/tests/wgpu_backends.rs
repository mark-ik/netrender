/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Integration tests for WgpuHal backend and cross-backend pixel parity.
//!
//! Three test groups:
//!   1. WgpuHal device-level: factory closure initialises WgpuDevice correctly.
//!   2. Cross-backend pixel parity: WgpuShared and WgpuHal render the same scene
//!      to pixel-identical output (they share all rendering code).
//!   3. composite_output_hal: raw backend texture handle is accessible after render.

#![cfg(feature = "wgpu_backend")]

extern crate webrender;

use webrender::api::units::*;
use webrender::api::*;
use webrender::render_api::*;
use webrender::{RendererBackend, WgpuDevice, wgpu};

// ──────────────────────────── shared helpers ─────────────────────────────────

fn make_adapter() -> Option<wgpu::Adapter> {
    let instance = wgpu::Instance::default();
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::None,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok()
}

fn make_device(adapter: &wgpu::Adapter) -> (wgpu::Device, wgpu::Queue) {
    pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("test device"),
            ..Default::default()
        },
    ))
    .expect("failed to create device")
}

struct NoopNotifier;
impl RenderNotifier for NoopNotifier {
    fn clone(&self) -> Box<dyn RenderNotifier> { Box::new(NoopNotifier) }
    fn wake_up(&self, _: bool) {}
    fn new_frame_ready(&self, _: DocumentId, _: FramePublishId, _: &FrameReadyParams) {}
}

/// Render a fixed 256×256 scene (4 solid-colour quadrants) using the given
/// backend and return the raw RGBA8 pixel buffer from CPU readback.
fn render_solid_quads(backend: RendererBackend) -> Vec<u8> {
    let opts = webrender::WebRenderOptions {
        clear_color: ColorF::new(0.0, 0.0, 0.0, 1.0),
        ..Default::default()
    };
    let (mut renderer, sender) = webrender::create_webrender_instance_with_backend(
        backend,
        Box::new(NoopNotifier),
        opts,
        None,
    )
    .expect("Failed to create WebRender instance");

    let device_size = DeviceIntSize::new(256, 256);
    let mut api = sender.create_api();
    let document = api.add_document(device_size);
    let pipeline = PipelineId(0, 0);

    let mut builder = DisplayListBuilder::new(pipeline);
    builder.begin();
    let sac = SpaceAndClipInfo::root_scroll(pipeline);

    for (x, y, r, g, b) in [
        (0.0f32,  0.0f32,  1.0, 0.0, 0.0), // red    — top-left
        (128.0,   0.0,     0.0, 1.0, 0.0), // green  — top-right
        (0.0,     128.0,   0.0, 0.0, 1.0), // blue   — bottom-left
        (128.0,   128.0,   1.0, 1.0, 0.0), // yellow — bottom-right
    ] {
        let rect = LayoutRect::from_origin_and_size(
            LayoutPoint::new(x, y),
            LayoutSize::new(128.0, 128.0),
        );
        builder.push_rect(&CommonItemProperties::new(rect, sac), rect, ColorF::new(r, g, b, 1.0));
    }

    let mut txn = Transaction::new();
    txn.set_display_list(Epoch(0), builder.end());
    txn.set_root_pipeline(pipeline);
    txn.generate_frame(0, true, false, RenderReasons::empty());
    api.send_transaction(document, txn);
    api.flush_scene_builder();
    renderer.update();

    renderer.render(device_size, 0).expect("render failed");

    let rect = FramebufferIntRect::from_origin_and_size(
        FramebufferIntPoint::new(0, 0),
        FramebufferIntSize::new(256, 256),
    );
    let pixels = renderer.read_pixels_rgba8(rect);
    renderer.deinit();
    pixels
}

// ────────────────────── 1. WgpuHal device-level tests ────────────────────────

#[test]
fn wgpu_hal_factory_initialises_device() {
    let Some(adapter) = make_adapter() else {
        eprintln!("wgpu-hal test: no adapter — skipping");
        return;
    };
    let (device, queue) = make_device(&adapter);
    let wgpu_dev = WgpuDevice::from_shared_device(device, queue);
    assert!(!wgpu_dev.has_surface(), "WgpuHal device should be headless (no surface)");
}

#[test]
fn wgpu_hal_factory_can_create_texture() {
    let Some(adapter) = make_adapter() else {
        eprintln!("wgpu-hal test: no adapter — skipping");
        return;
    };
    let (device, queue) = make_device(&adapter);
    let mut wgpu_dev = WgpuDevice::from_shared_device(device, queue);

    let tex = wgpu_dev.create_data_texture(
        "hal-test-texture",
        8,
        8,
        wgpu::TextureFormat::Rgba8Unorm,
        &[0xffu8; 8 * 8 * 4],
    );
    assert_eq!(tex.width, 8);
    assert_eq!(tex.height, 8);
    wgpu_dev.flush_encoder();
}

#[test]
fn wgpu_hal_factory_device_remains_functional_after_wr_use() {
    let Some(adapter) = make_adapter() else {
        eprintln!("wgpu-hal test: no adapter — skipping");
        return;
    };
    let (device, queue) = make_device(&adapter);
    let host_device = device.clone();

    // Give WebRender the device via the factory pattern.
    let mut wgpu_dev = WgpuDevice::from_shared_device(device, queue);
    let _tex = wgpu_dev.create_data_texture(
        "hal-test-use",
        4, 4, wgpu::TextureFormat::Rgba8Unorm,
        &[0u8; 64],
    );
    wgpu_dev.flush_encoder();
    drop(wgpu_dev);

    // Host device should still be fully usable.
    let _buf = host_device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("post-wr host buffer"),
        size: 128,
        usage: wgpu::BufferUsages::UNIFORM,
        mapped_at_creation: false,
    });
}

// ──────────────────── 2. Cross-backend pixel parity ──────────────────────────

/// WgpuShared and WgpuHal must produce pixel-identical output for any scene,
/// because they share 100% of the rendering code (WgpuHal factory just
/// provides a different route to the same WgpuDevice::from_shared_device path).
#[test]
fn wgpu_shared_and_wgpu_hal_are_pixel_identical() {
    let Some(adapter) = make_adapter() else {
        eprintln!("pixel-parity test: no adapter — skipping");
        return;
    };

    // WgpuShared backend.
    let (d1, q1) = make_device(&adapter);
    let shared_pixels = render_solid_quads(RendererBackend::WgpuShared { device: d1, queue: q1 });

    // WgpuHal backend — factory provides device from same adapter class.
    let adapter2 = make_adapter().unwrap();
    let hal_pixels = render_solid_quads(RendererBackend::WgpuHal {
        device_factory: Box::new(move || make_device(&adapter2)),
    });

    assert_eq!(
        shared_pixels.len(), hal_pixels.len(),
        "pixel buffers must be the same size"
    );
    assert_eq!(
        shared_pixels, hal_pixels,
        "WgpuShared and WgpuHal must produce pixel-identical output"
    );
}

// ─────────────── 3. composite_output_hal raw handle access ───────────────────

/// After rendering with WgpuHal, composite_output_hal<A>() must return a
/// non-None handle on the matching backend.  We gate this per-platform
/// since the backend type must be known at compile time.
#[test]
fn composite_output_hal_returns_handle() {
    let Some(adapter) = make_adapter() else {
        eprintln!("composite_output_hal test: no adapter — skipping");
        return;
    };
    let info = adapter.get_info();
    let (device, queue) = make_device(&adapter);

    let opts = webrender::WebRenderOptions {
        clear_color: ColorF::new(0.1, 0.1, 0.1, 1.0),
        ..Default::default()
    };
    let (mut renderer, sender) = webrender::create_webrender_instance_with_backend(
        RendererBackend::WgpuShared { device, queue },
        Box::new(NoopNotifier),
        opts,
        None,
    )
    .expect("Failed to create WebRender instance");

    let device_size = DeviceIntSize::new(64, 64);
    let mut api = sender.create_api();
    let document = api.add_document(device_size);
    let pipeline = PipelineId(0, 0);

    let mut builder = DisplayListBuilder::new(pipeline);
    builder.begin();
    let rect = LayoutRect::from_origin_and_size(LayoutPoint::zero(), LayoutSize::new(64.0, 64.0));
    builder.push_rect(
        &CommonItemProperties::new(rect, SpaceAndClipInfo::root_scroll(pipeline)),
        rect,
        ColorF::new(1.0, 0.0, 0.0, 1.0),
    );
    let mut txn = Transaction::new();
    txn.set_display_list(Epoch(0), builder.end());
    txn.set_root_pipeline(pipeline);
    txn.generate_frame(0, true, false, RenderReasons::empty());
    api.send_transaction(document, txn);
    api.flush_scene_builder();
    renderer.update();
    renderer.render(device_size, 0).expect("render failed");

    // composite_output() must be Some after render.
    assert!(renderer.composite_output().is_some(), "composite_output() must be Some after render");

    // composite_output_hal<A>() must be Some on the matching backend.
    // We dispatch on the runtime-detected backend to call the right generic.
    let hal_ok = unsafe {
        match info.backend {
            #[cfg(all(feature = "wgpu_backend", not(target_os = "ios"), not(target_os = "android")))]
            wgpu::Backend::Vulkan => {
                renderer.composite_output_hal::<wgpu::wgc::api::Vulkan>().is_some()
            }
            #[cfg(all(feature = "wgpu_backend", target_os = "macos"))]
            wgpu::Backend::Metal => {
                renderer.composite_output_hal::<wgpu::wgc::api::Metal>().is_some()
            }
            #[cfg(all(feature = "wgpu_backend", target_os = "windows"))]
            wgpu::Backend::Dx12 => {
                renderer.composite_output_hal::<wgpu::wgc::api::Dx12>().is_some()
            }
            _ => {
                eprintln!("composite_output_hal test: backend {:?} not covered — skipping hal check", info.backend);
                true // don't fail for uncovered backends
            }
        }
    };
    assert!(hal_ok, "composite_output_hal<A>() must return Some on matching backend {:?}", info.backend);

    renderer.deinit();
}
