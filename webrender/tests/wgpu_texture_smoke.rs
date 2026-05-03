/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! P4 smoke test: WgpuDevice::create_texture (and delete_texture)
//! produce real wgpu::Texture resources for each ImageFormat WebRender
//! actually uses, with usage flags appropriate for sampling and (when
//! render_target=Some) render-target attachment.

#![cfg(feature = "wgpu_backend")]

use api::ImageFormat;
use api::units::DeviceIntSize;
use std::sync::Arc;
use webrender::{GpuResources, RenderTargetInfo, TextureFilter, WgpuDevice};

fn try_create_device() -> Option<WgpuDevice> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("wgpu_texture_smoke device"),
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
fn creates_textures_for_each_image_format() {
    let Some(mut wgpu_device) = try_create_device() else {
        eprintln!("skip: no wgpu adapter available");
        return;
    };

    // R16 and Rg16 may not be supported on every adapter; the rest are
    // sufficient to validate the format mapping table.
    let formats = [
        ImageFormat::R8,
        ImageFormat::BGRA8,
        ImageFormat::RGBAF32,
        ImageFormat::RG8,
        ImageFormat::RGBAI32,
        ImageFormat::RGBA8,
    ];

    for fmt in formats {
        let tex = wgpu_device.create_texture(
            api::ImageBufferKind::Texture2D,
            fmt,
            64,
            32,
            TextureFilter::Linear,
            None,
        );
        assert_eq!(tex.size, DeviceIntSize::new(64, 32), "format={:?}", fmt);
        assert_eq!(tex.format, fmt);
        assert!(!tex.is_render_target);
        // Texture and view must be live (texture handle non-null implicit
        // via wgpu's Drop-managed handle; if creation succeeded we have one).
        wgpu_device.delete_texture(tex);
    }
}

#[test]
fn render_target_flag_sets_attachment_usage() {
    let Some(mut wgpu_device) = try_create_device() else {
        eprintln!("skip: no wgpu adapter available");
        return;
    };

    let tex = wgpu_device.create_texture(
        api::ImageBufferKind::Texture2D,
        ImageFormat::BGRA8,
        128,
        128,
        TextureFilter::Linear,
        Some(RenderTargetInfo { has_depth: false }),
    );
    assert!(tex.is_render_target);
    assert!(tex.texture.usage().contains(wgpu::TextureUsages::RENDER_ATTACHMENT));
    wgpu_device.delete_texture(tex);
}

#[test]
fn copy_texture_methods_do_not_panic() {
    let Some(mut wgpu_device) = try_create_device() else {
        eprintln!("skip: no wgpu adapter available");
        return;
    };

    let mut a = wgpu_device.create_texture(
        api::ImageBufferKind::Texture2D,
        ImageFormat::BGRA8,
        64, 64,
        TextureFilter::Linear,
        None,
    );
    let b = wgpu_device.create_texture(
        api::ImageBufferKind::Texture2D,
        ImageFormat::BGRA8,
        64, 64,
        TextureFilter::Linear,
        None,
    );

    wgpu_device.copy_entire_texture(&mut a, &b);
    wgpu_device.copy_texture_sub_region(&b, 8, 8, &a, 0, 0, 16, 16);

    // No assertion of pixel content — verifying that the encoder + submit
    // path runs without panic is the smoke target. Real readback comes
    // when upload_texture_immediate (P4d) lets us put known data in.
    wgpu_device.delete_texture(a);
    wgpu_device.delete_texture(b);
}

#[test]
fn dimensions_clamp_to_max() {
    let Some(mut wgpu_device) = try_create_device() else {
        eprintln!("skip: no wgpu adapter available");
        return;
    };

    let max = wgpu_device.device().limits().max_texture_dimension_2d as i32;
    let tex = wgpu_device.create_texture(
        api::ImageBufferKind::Texture2D,
        ImageFormat::R8,
        max + 1000, // way over
        16,
        TextureFilter::Nearest,
        None,
    );
    assert_eq!(tex.size.width, max);
    assert_eq!(tex.size.height, 16);
    wgpu_device.delete_texture(tex);
}
