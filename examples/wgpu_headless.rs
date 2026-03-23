/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

#[cfg(feature = "wgpu_backend")]
use webrender::api::{ImageBufferKind, ImageFormat};
#[cfg(feature = "wgpu_backend")]
use webrender::{TextureFilter, WgpuDevice};

#[cfg(feature = "wgpu_backend")]
fn main() {
    let mut device = match WgpuDevice::new_headless() {
        Some(device) => device,
        None => {
            eprintln!("No wgpu adapter available.");
            std::process::exit(1);
        }
    };

    println!("wgpu pipelines: {}", device.pipeline_count());

    let target = device.create_texture(
        ImageBufferKind::Texture2D,
        ImageFormat::BGRA8,
        64,
        64,
        TextureFilter::Nearest,
        Some(webrender::RenderTargetInfo { has_depth: false }),
    );
    let source = device.create_texture(
        ImageBufferKind::Texture2D,
        ImageFormat::R8,
        1,
        1,
        TextureFilter::Nearest,
        None,
    );
    device.upload_texture_immediate(&source, &[255]);
    device.render_debug_font_quad(&target, &source, [0, 255, 0, 255]);

    let mut pixels = vec![0u8; 64 * 64 * 4];
    device.read_texture_pixels(&target, &mut pixels);

    let idx = ((32 * 64 + 32) * 4) as usize;
    println!(
        "center pixel BGRA=({}, {}, {}, {})",
        pixels[idx],
        pixels[idx + 1],
        pixels[idx + 2],
        pixels[idx + 3]
    );
}

#[cfg(not(feature = "wgpu_backend"))]
fn main() {
    eprintln!("Run with `cargo run -p webrender-examples --bin wgpu_headless --features wgpu_backend`.");
    std::process::exit(1);
}
