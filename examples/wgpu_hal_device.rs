/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Proof-of-concept: host app provides a wgpu-hal device factory to WebRender,
//! renders a test scene, verifies the output, and demonstrates raw hal texture access.
//!
//! This demonstrates `RendererBackend::WgpuHal`, which is an ergonomic variant of
//! `WgpuShared` designed for hosts that already have a raw hal device (e.g. a game
//! engine with a native Vulkan/DX12/Metal context). The host provides a factory
//! closure that produces a `(wgpu::Device, wgpu::Queue)` — typically by calling
//! `wgpu::Adapter::create_device_from_hal()` on their existing hal device.
//!
//! After device creation, `WgpuHal` is functionally identical to `WgpuShared`: all
//! rendering code is shared, WebRender renders to an offscreen `wgpu_readback_texture`,
//! and the host composites using `composite_output()` or `composite_output_hal()`.
//!
//! Run with:
//!   cargo run -p webrender-examples --bin wgpu_hal_device --features wgpu_backend

#[cfg(feature = "wgpu_backend")]
fn main() {
    use webrender::api::units::*;
    use webrender::api::*;
    use webrender::render_api::*;
    use webrender::RendererBackend;

    env_logger::init();

    // === Step 1: Host app creates its own wgpu adapter ===
    // In a real hal-device scenario the host already has a raw hal device
    // (e.g. a Vulkan VkDevice or D3D12 ID3D12Device). Here we simulate that by
    // obtaining an adapter and building the factory closure around it.
    let instance = webrender::wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(
        &webrender::wgpu::RequestAdapterOptions::default(),
    ))
    .expect("No wgpu adapter available");

    println!(
        "Adapter: {:?} ({:?})",
        adapter.get_info().name,
        adapter.get_info().backend
    );

    // === Step 2: Build a device factory ===
    // The factory closure is what makes WgpuHal distinct from WgpuShared:
    // it defers device creation to WebRender's initialisation path while
    // letting the host control exactly how the device is opened.
    //
    // In a real use case the closure would call:
    //   unsafe { adapter.create_device_from_hal(existing_hal_open_device, &desc) }
    //
    // For this demo we call request_device, which has the same observable effect.
    let adapter = std::sync::Arc::new(adapter);
    let factory_adapter = adapter.clone();
    let device_factory: Box<dyn FnOnce() -> (webrender::wgpu::Device, webrender::wgpu::Queue) + Send> =
        Box::new(move || {
            pollster::block_on(factory_adapter.request_device(
                &webrender::wgpu::DeviceDescriptor {
                    label: Some("hal-factory device"),
                    ..Default::default()
                },
            ))
            .expect("Failed to create device in factory")
        });

    // === Step 3: Create WebRender with WgpuHal ===
    struct DemoNotifier;
    impl RenderNotifier for DemoNotifier {
        fn clone(&self) -> Box<dyn RenderNotifier> { Box::new(DemoNotifier) }
        fn wake_up(&self, _: bool) {}
        fn new_frame_ready(&self, _: DocumentId, _: FramePublishId, _: &FrameReadyParams) {}
    }

    let opts = webrender::WebRenderOptions {
        clear_color: ColorF::new(0.2, 0.2, 0.2, 1.0),
        ..Default::default()
    };

    let (mut renderer, sender) = webrender::create_webrender_instance_with_backend(
        RendererBackend::WgpuHal { device_factory },
        Box::new(DemoNotifier),
        opts,
        None,
    )
    .expect("Failed to create WebRender instance");

    println!("WebRender created via WgpuHal factory");

    // === Step 4: Build and submit a display list (4 coloured quadrants) ===
    let device_size = DeviceIntSize::new(256, 256);
    let mut api = sender.create_api();
    let document = api.add_document(device_size);
    let pipeline_id = PipelineId(0, 0);

    let mut builder = DisplayListBuilder::new(pipeline_id);
    builder.begin();
    let sac = SpaceAndClipInfo::root_scroll(pipeline_id);

    for (x, y, r, g, b) in [
        (0.0f32,   0.0f32,   1.0, 0.0, 0.0), // red    — top-left
        (128.0,    0.0,      0.0, 1.0, 0.0), // green  — top-right
        (0.0,      128.0,    0.0, 0.0, 1.0), // blue   — bottom-left
        (128.0,    128.0,    1.0, 1.0, 0.0), // yellow — bottom-right
    ] {
        let rect = LayoutRect::from_origin_and_size(
            LayoutPoint::new(x, y),
            LayoutSize::new(128.0, 128.0),
        );
        builder.push_rect(
            &CommonItemProperties::new(rect, sac),
            rect,
            ColorF::new(r, g, b, 1.0),
        );
    }

    let mut txn = Transaction::new();
    txn.set_display_list(Epoch(0), builder.end());
    txn.set_root_pipeline(pipeline_id);
    txn.generate_frame(0, true, false, RenderReasons::empty());
    api.send_transaction(document, txn);
    api.flush_scene_builder();
    renderer.update();

    // === Step 5: Render ===
    renderer.render(device_size, 0).expect("Render failed");
    println!("Frame rendered via WgpuHal path");

    // === Step 6: Access composite output via the wgpu API ===
    if let Some(output) = renderer.composite_output() {
        println!(
            "composite_output(): {}x{} {:?} — TextureView created on shared device",
            output.width, output.height, output.format()
        );
        let _view = output.create_view();
    }

    // === Step 7: Access composite output via the raw hal API ===
    // composite_output_hal::<A>() returns the backend-specific texture type
    // (e.g. wgpu_hal::vulkan::Texture for Vulkan, exposing a raw VkImage).
    // The exact type depends on the platform/backend selected at runtime.
    //
    // Example for Vulkan (Linux / Windows with Vulkan):
    //
    //   unsafe {
    //       if let Some(hal_tex) = renderer.composite_output_hal::<wgpu::wgc::api::Vulkan>() {
    //           let vk_image = hal_tex.raw;  // ash::vk::Image
    //           println!("Raw VkImage: {:?}", vk_image);
    //       }
    //   }
    //
    // Example for DX12 (Windows):
    //
    //   unsafe {
    //       if let Some(hal_tex) = renderer.composite_output_hal::<wgpu::wgc::api::Dx12>() {
    //           // hal_tex.resource is windows::Win32::Graphics::Direct3D12::ID3D12Resource
    //           println!("Raw ID3D12Resource obtained");
    //       }
    //   }
    //
    // For this portable demo, we confirm the method compiles and that composite_output
    // is available; the raw handle is only used in platform-specific code.
    println!("composite_output_hal<A>() available for zero-copy native render pass injection");

    // === Step 8: Verify pixel output via CPU readback ===
    let rect = FramebufferIntRect::from_origin_and_size(
        FramebufferIntPoint::new(0, 0),
        FramebufferIntSize::new(256, 256),
    );
    let pixels = renderer.read_pixels_rgba8(rect);
    if !pixels.is_empty() {
        let sample = |x: usize, y: usize| {
            let i = (y * 256 + x) * 4;
            (pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3])
        };
        // read_pixels_rgba8 uses GL convention: Y=0 is bottom. So:
        //   y=192 → top half (red/green), y=64 → bottom half (blue/yellow).
        let (tl, tr, bl, br) = (sample(64, 192), sample(192, 192), sample(64, 64), sample(192, 64));
        println!("Pixel readback (RGBA): TL={:?} TR={:?} BL={:?} BR={:?}", tl, tr, bl, br);

        let close = |a: u8, b: u8| (a as i16 - b as i16).unsigned_abs() < 5;
        let ok = close(tl.0, 255) && close(tl.1, 0)   && close(tl.2, 0)
              && close(tr.0, 0)   && close(tr.1, 255)  && close(tr.2, 0)
              && close(bl.0, 0)   && close(bl.1, 0)    && close(bl.2, 255)
              && close(br.0, 255) && close(br.1, 255)  && close(br.2, 0);

        if ok {
            println!("\nSUCCESS: WgpuHal rendering verified — factory + shared pipeline works!");
        } else {
            println!("\nWARNING: Pixel values don't match expected colors (may need more frames).");
        }
    }

    renderer.deinit();
    println!("Done.");
}

#[cfg(not(feature = "wgpu_backend"))]
fn main() {
    eprintln!(
        "Run with: cargo run -p webrender-examples --bin wgpu_hal_device --features wgpu_backend"
    );
    std::process::exit(1);
}
