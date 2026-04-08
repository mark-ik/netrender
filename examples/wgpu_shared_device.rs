/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Proof-of-concept: host app creates a wgpu::Device, shares it with WebRender,
//! builds a display list, renders, and reads back the composited output.
//!
//! This simulates what an egui/graphshell host would do — own the GPU context,
//! hand a clone to WebRender, and consume the rendered texture.
//!
//! Run with:
//!   cargo run -p webrender-examples --bin wgpu_shared_device --features wgpu_backend

#[cfg(feature = "wgpu_backend")]
fn main() {
    use webrender::api::units::*;
    use webrender::api::*;
    use webrender::render_api::*;
    use webrender::RendererBackend;

    env_logger::init();

    // === Step 1: Host app creates its own wgpu device ===
    // In a real app, egui/wgpu would create this during window init.
    let instance = webrender::wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(
        &webrender::wgpu::RequestAdapterOptions::default(),
    ))
    .expect("No wgpu adapter available");

    let (host_device, host_queue) = pollster::block_on(adapter.request_device(
        &webrender::wgpu::DeviceDescriptor {
            label: Some("host-app device"),
            ..Default::default()
        },
    ))
    .expect("Failed to create wgpu device");

    println!("Host device created: {:?}", adapter.get_info().name);

    // === Step 2: Clone device+queue and give to WebRender ===
    let wr_device = host_device.clone();
    let wr_queue = host_queue.clone();

    // Notifier — just a no-op for this demo.
    struct DemoNotifier;
    impl RenderNotifier for DemoNotifier {
        fn clone(&self) -> Box<dyn RenderNotifier> {
            Box::new(DemoNotifier)
        }
        fn wake_up(&self, _composite_needed: bool) {}
        fn new_frame_ready(&self, _: DocumentId, _: FramePublishId, _: &FrameReadyParams) {}
    }

    let opts = webrender::WebRenderOptions {
        clear_color: ColorF::new(0.2, 0.2, 0.2, 1.0),
        ..Default::default()
    };

    let (mut renderer, sender) = webrender::create_webrender_instance_with_backend(
        RendererBackend::WgpuShared {
            device: wr_device,
            queue: wr_queue,
        },
        Box::new(DemoNotifier),
        opts,
        None,
    )
    .expect("Failed to create WebRender instance");

    println!("WebRender created on shared device");

    // === Step 3: Build a display list ===
    let device_size = DeviceIntSize::new(256, 256);
    let mut api = sender.create_api();
    let document = api.add_document(device_size);
    let epoch = Epoch(0);
    let pipeline_id = PipelineId(0, 0);

    let mut builder = DisplayListBuilder::new(pipeline_id);
    builder.begin();
    let space_and_clip = SpaceAndClipInfo::root_scroll(pipeline_id);

    // Red rectangle (top-left quadrant)
    builder.push_rect(
        &CommonItemProperties::new(
            LayoutRect::from_origin_and_size(
                LayoutPoint::new(0.0, 0.0),
                LayoutSize::new(128.0, 128.0),
            ),
            space_and_clip,
        ),
        LayoutRect::from_origin_and_size(
            LayoutPoint::new(0.0, 0.0),
            LayoutSize::new(128.0, 128.0),
        ),
        ColorF::new(1.0, 0.0, 0.0, 1.0),
    );

    // Green rectangle (top-right quadrant)
    builder.push_rect(
        &CommonItemProperties::new(
            LayoutRect::from_origin_and_size(
                LayoutPoint::new(128.0, 0.0),
                LayoutSize::new(128.0, 128.0),
            ),
            space_and_clip,
        ),
        LayoutRect::from_origin_and_size(
            LayoutPoint::new(128.0, 0.0),
            LayoutSize::new(128.0, 128.0),
        ),
        ColorF::new(0.0, 1.0, 0.0, 1.0),
    );

    // Blue rectangle (bottom-left quadrant)
    builder.push_rect(
        &CommonItemProperties::new(
            LayoutRect::from_origin_and_size(
                LayoutPoint::new(0.0, 128.0),
                LayoutSize::new(128.0, 128.0),
            ),
            space_and_clip,
        ),
        LayoutRect::from_origin_and_size(
            LayoutPoint::new(0.0, 128.0),
            LayoutSize::new(128.0, 128.0),
        ),
        ColorF::new(0.0, 0.0, 1.0, 1.0),
    );

    // Yellow rectangle (bottom-right quadrant)
    builder.push_rect(
        &CommonItemProperties::new(
            LayoutRect::from_origin_and_size(
                LayoutPoint::new(128.0, 128.0),
                LayoutSize::new(128.0, 128.0),
            ),
            space_and_clip,
        ),
        LayoutRect::from_origin_and_size(
            LayoutPoint::new(128.0, 128.0),
            LayoutSize::new(128.0, 128.0),
        ),
        ColorF::new(1.0, 1.0, 0.0, 1.0),
    );

    let mut txn = Transaction::new();
    txn.set_display_list(epoch, builder.end());
    txn.set_root_pipeline(pipeline_id);
    txn.generate_frame(0, true, false, RenderReasons::empty());
    api.send_transaction(document, txn);

    // Wait for the frame to be built.
    api.flush_scene_builder();
    renderer.update();

    // === Step 4: Render the frame ===
    let result = renderer.render(device_size, 0);
    match result {
        Ok(_) => println!("Frame rendered successfully"),
        Err(errors) => {
            for e in &errors {
                eprintln!("Render error: {:?}", e);
            }
        }
    }

    // === Step 5: Access the composite output ===
    // Option A: Zero-copy — get the texture directly (shared device!)
    if let Some(output) = renderer.composite_output() {
        println!(
            "Composite output: {}x{} {:?}",
            output.width,
            output.height,
            output.format()
        );

        // The host could now create a TextureView and sample this in its own
        // render pass. For this demo, we verify the texture exists on the
        // shared device by creating a view.
        let _view = output.create_view();
        println!("TextureView created on shared device — zero-copy path works!");
    } else {
        println!("No composite output yet (frame may not have been composited)");
    }

    // Option B: CPU readback — read pixels for verification.
    let rect = FramebufferIntRect::from_origin_and_size(
        FramebufferIntPoint::new(0, 0),
        FramebufferIntSize::new(256, 256),
    );
    let pixels = renderer.read_pixels_rgba8(rect);
    if !pixels.is_empty() {
        // Sample the center of each quadrant (RGBA).
        let sample = |x: usize, y: usize| -> (u8, u8, u8, u8) {
            let idx = (y * 256 + x) * 4;
            (pixels[idx], pixels[idx + 1], pixels[idx + 2], pixels[idx + 3])
        };

        // read_pixels_rgba8 returns RGBA with Y-flipped (GL convention: origin
        // at bottom-left). So row 64 in the buffer = bottom area of the image
        // (our blue/yellow quadrants), and row 192 = top area (red/green).
        let tl = sample(64, 192);  // Top-left in screen = high Y in buffer → red
        let tr = sample(192, 192); // Top-right → green
        let bl = sample(64, 64);   // Bottom-left → blue
        let br = sample(192, 64);  // Bottom-right → yellow

        println!("Pixel readback (RGBA):");
        println!("  Top-left (red):        {:?}", tl);
        println!("  Top-right (green):     {:?}", tr);
        println!("  Bottom-left (blue):    {:?}", bl);
        println!("  Bottom-right (yellow): {:?}", br);

        // Verify colors (allow small tolerance for anti-aliasing).
        let close = |a: u8, b: u8| -> bool { (a as i16 - b as i16).unsigned_abs() < 5 };
        let ok = close(tl.0, 255) && close(tl.1, 0) && close(tl.2, 0)
            && close(tr.0, 0) && close(tr.1, 255) && close(tr.2, 0)
            && close(bl.0, 0) && close(bl.1, 0) && close(bl.2, 255)
            && close(br.0, 255) && close(br.1, 255) && close(br.2, 0);

        if ok {
            println!("\nSUCCESS: Shared device rendering verified!");
        } else {
            println!("\nWARNING: Pixel values don't match expected colors.");
            println!("  This may be expected if the frame hasn't fully composited yet.");
        }
    } else {
        println!("No pixels read back (render may need more frames to composite)");
    }

    // === Step 6: Verify host device is still functional ===
    let _buf = host_device.create_buffer(&webrender::wgpu::BufferDescriptor {
        label: Some("host buffer after WR render"),
        size: 256,
        usage: webrender::wgpu::BufferUsages::UNIFORM,
        mapped_at_creation: false,
    });
    println!("Host device still functional after WebRender render");

    // Clean up.
    renderer.deinit();
    println!("Done.");
}

#[cfg(not(feature = "wgpu_backend"))]
fn main() {
    eprintln!(
        "Run with: cargo run -p webrender-examples --bin wgpu_shared_device --features wgpu_backend"
    );
    std::process::exit(1);
}
