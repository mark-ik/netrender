/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Proof-of-concept: `RendererBackend::WgpuHal` — deferred device creation
//! via factory closure.
//!
//! # What this demonstrates
//!
//! `WgpuHal` differs from `WgpuShared` in *when* the GPU device is created:
//!
//! - `WgpuShared`: the host app already has a `wgpu::Device` and hands it to
//!   WebRender pre-built.
//! - `WgpuHal`: the host app provides a *factory closure* that WebRender calls
//!   exactly once during its own init sequence.  Device creation is deferred.
//!
//! The factory closure is intentionally opaque — it can internally call
//! any wgpu device-creation path, including the raw hal path:
//!
//! ```rust,ignore
//! // Example: wrapping a raw Vulkan hal device inside the closure
//! let factory = Box::new(move || {
//!     unsafe {
//!         adapter.create_device_from_hal(
//!             hal_open_device,  // hal::OpenDevice<wgpu_hal::api::Vulkan>
//!             &wgpu::DeviceDescriptor::default(),
//!         )
//!     }
//!     .expect("hal device creation failed")
//! });
//! ```
//!
//! For this cross-platform demo we use a standard device so it runs on all
//! backends (Vulkan, DX12, Metal, etc.).
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

    // === Step 1: Negotiate an adapter (done by the host app before init) ===
    //
    // In a real hal-device scenario, the host would:
    //   1. Create a wgpu Instance
    //   2. Enumerate adapters and pick one
    //   3. Open a raw hal device (e.g. `ash::Device` for Vulkan)
    //   4. Move the raw device + adapter into the factory closure below
    //
    // Here we just record the adapter name to prove the device is created
    // inside the closure rather than before it.

    let instance = webrender::wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(
        &webrender::wgpu::RequestAdapterOptions::default(),
    ))
    .expect("No wgpu adapter available — cannot run wgpu_hal_device demo");

    let adapter_name = adapter.get_info().name.clone();
    println!("Selected adapter: {adapter_name}");

    // === Step 2: Build the factory closure ===
    //
    // The closure captures whatever it needs (here: the adapter) and creates
    // the wgpu Device + Queue when called by WebRender's init sequence.
    // This is the same type signature as what a raw-hal path would produce.
    let device_factory: Box<dyn FnOnce() -> (webrender::wgpu::Device, webrender::wgpu::Queue) + Send> =
        Box::new(move || {
            println!("  [factory] creating device on adapter: {adapter_name}");
            let (device, queue) = pollster::block_on(adapter.request_device(
                &webrender::wgpu::DeviceDescriptor {
                    label: Some("wgpu-hal-device demo"),
                    required_features: webrender::wgpu::Features::TEXTURE_FORMAT_16BIT_NORM,
                    ..Default::default()
                },
            ))
            // If 16-bit norm isn't supported, fall back to no extra features.
            .or_else(|_| {
                pollster::block_on(adapter.request_device(
                    &webrender::wgpu::DeviceDescriptor {
                        label: Some("wgpu-hal-device demo (no 16bit)"),
                        ..Default::default()
                    },
                ))
            })
            .expect("Device creation failed in WgpuHal factory");
            (device, queue)
        });

    // === Step 3: Hand the factory to WebRender ===
    struct DemoNotifier;
    impl RenderNotifier for DemoNotifier {
        fn clone(&self) -> Box<dyn RenderNotifier> { Box::new(DemoNotifier) }
        fn wake_up(&self, _composite_needed: bool) {}
        fn new_frame_ready(&self, _: DocumentId, _: FramePublishId, _: &FrameReadyParams) {}
    }

    println!("Handing factory to WebRender (device not yet created)...");
    let (mut renderer, sender) = webrender::create_webrender_instance_with_backend(
        RendererBackend::WgpuHal { device_factory },
        Box::new(DemoNotifier),
        webrender::WebRenderOptions {
            clear_color: ColorF::new(0.15, 0.15, 0.15, 1.0),
            ..Default::default()
        },
        None,
    )
    .expect("Failed to create WebRender instance via WgpuHal");
    println!("WebRender init complete (factory was called during init).");

    // === Step 4: Build and submit the same 4-quadrant scene as wgpu_shared_device ===
    let device_size = DeviceIntSize::new(256, 256);
    let mut api = sender.create_api();
    let document = api.add_document(device_size);
    let epoch = Epoch(0);
    let pipeline_id = PipelineId(0, 0);

    let mut builder = DisplayListBuilder::new(pipeline_id);
    builder.begin();
    let sac = SpaceAndClipInfo::root_scroll(pipeline_id);

    let quad = |builder: &mut DisplayListBuilder, x: f32, y: f32, color: ColorF| {
        let rect = LayoutRect::from_origin_and_size(
            LayoutPoint::new(x, y),
            LayoutSize::new(128.0, 128.0),
        );
        builder.push_rect(
            &CommonItemProperties::new(rect, sac),
            rect,
            color,
        );
    };

    quad(&mut builder, 0.0,   0.0,   ColorF::new(1.0, 0.0, 0.0, 1.0)); // red    TL
    quad(&mut builder, 128.0, 0.0,   ColorF::new(0.0, 1.0, 0.0, 1.0)); // green  TR
    quad(&mut builder, 0.0,   128.0, ColorF::new(0.0, 0.0, 1.0, 1.0)); // blue   BL
    quad(&mut builder, 128.0, 128.0, ColorF::new(1.0, 1.0, 0.0, 1.0)); // yellow BR

    let mut txn = Transaction::new();
    txn.set_display_list(epoch, builder.end());
    txn.set_root_pipeline(pipeline_id);
    txn.generate_frame(0, true, false, RenderReasons::empty());
    api.send_transaction(document, txn);
    api.flush_scene_builder();
    renderer.update();

    // === Step 5: Render and verify ===
    renderer.render(device_size, 0)
        .expect("render failed");

    if let Some(out) = renderer.composite_output() {
        println!("Composite output: {}×{} {:?}", out.width, out.height, out.format());
    }

    let rect = FramebufferIntRect::from_origin_and_size(
        FramebufferIntPoint::new(0, 0),
        FramebufferIntSize::new(256, 256),
    );
    let pixels = renderer.read_pixels_rgba8(rect);

    if !pixels.is_empty() {
        let sample = |x: usize, y: usize| -> (u8, u8, u8, u8) {
            let idx = (y * 256 + x) * 4;
            (pixels[idx], pixels[idx + 1], pixels[idx + 2], pixels[idx + 3])
        };
        // read_pixels_rgba8 is Y-flipped: row 192 = top of screen, row 64 = bottom.
        let tl = sample(64,  192); // red
        let tr = sample(192, 192); // green
        let bl = sample(64,  64);  // blue
        let br = sample(192, 64);  // yellow

        println!("Pixel readback (RGBA):");
        println!("  Top-left    (expected red):    {tl:?}");
        println!("  Top-right   (expected green):  {tr:?}");
        println!("  Bottom-left (expected blue):   {bl:?}");
        println!("  Bottom-right(expected yellow): {br:?}");

        let close = |a: u8, b: u8| (a as i16 - b as i16).unsigned_abs() < 5;
        let ok = close(tl.0, 255) && close(tl.1, 0)   && close(tl.2, 0)
              && close(tr.0, 0)   && close(tr.1, 255)  && close(tr.2, 0)
              && close(bl.0, 0)   && close(bl.1, 0)    && close(bl.2, 255)
              && close(br.0, 255) && close(br.1, 255)  && close(br.2, 0);

        if ok {
            println!("\nSUCCESS: WgpuHal factory path rendering verified!");
        } else {
            println!("\nWARNING: Pixel values don't match expected colors.");
            println!("  The frame may not have been composited yet (normal for first frame).");
        }
    } else {
        println!("No pixels read back — frame may need more pumping to composite.");
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
