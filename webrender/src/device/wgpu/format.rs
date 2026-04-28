/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! wgpu ↔ WebRender format mapping (pure functions). See plan §6 S1.
//!
//! Per the renderer-body adapter plan §11 appendix:
//! `ImageFormat` ↔ `wgpu::TextureFormat`. Covers the variants the
//! renderer actually uses today; expand as new families surface.

use api::ImageFormat;

/// Map a webrender `ImageFormat` to its `wgpu::TextureFormat`. The
/// renderer body's existing `ImageFormat` carries the same per-channel
/// semantics, but wgpu's enum is finer-grained (Unorm vs. Snorm vs.
/// Uint vs. Sint, sRGB vs. linear). For texture-cache content the
/// linear-Unorm variants are the right default; sRGB textures get
/// requested explicitly.
pub fn image_format_to_wgpu(format: ImageFormat) -> wgpu::TextureFormat {
    match format {
        ImageFormat::R8 => wgpu::TextureFormat::R8Unorm,
        ImageFormat::R16 => wgpu::TextureFormat::R16Unorm,
        ImageFormat::RG8 => wgpu::TextureFormat::Rg8Unorm,
        ImageFormat::RGBA8 => wgpu::TextureFormat::Rgba8Unorm,
        ImageFormat::BGRA8 => wgpu::TextureFormat::Bgra8Unorm,
        ImageFormat::RGBAF32 => wgpu::TextureFormat::Rgba32Float,
        // RG16 / RGBAI32 / NV12 are exotic / not in the renderer body
        // texture path today; add when a callsite needs them.
        other => panic!(
            "image_format_to_wgpu: format {:?} not yet mapped — add when first callsite needs it",
            other
        ),
    }
}

/// Bytes per pixel for the renderer-side image format. Used by upload
/// helpers to size staging buffers without round-tripping through wgpu.
pub fn image_format_bytes_per_pixel(format: ImageFormat) -> u32 {
    match format {
        ImageFormat::R8 => 1,
        ImageFormat::R16 | ImageFormat::RG8 => 2,
        ImageFormat::RGBA8 | ImageFormat::BGRA8 => 4,
        ImageFormat::RGBAF32 => 16,
        other => panic!(
            "image_format_bytes_per_pixel: format {:?} not yet mapped",
            other
        ),
    }
}

/// Bytes per pixel for a `wgpu::TextureFormat`. Used by upload helpers
/// when the source-side format is wgpu-native rather than `ImageFormat`.
pub fn format_bytes_per_pixel_wgpu(format: wgpu::TextureFormat) -> u32 {
    match format {
        wgpu::TextureFormat::R8Unorm | wgpu::TextureFormat::R8Snorm => 1,
        wgpu::TextureFormat::R16Unorm
        | wgpu::TextureFormat::R16Snorm
        | wgpu::TextureFormat::Rg8Unorm
        | wgpu::TextureFormat::Rg8Snorm => 2,
        wgpu::TextureFormat::Rgba8Unorm
        | wgpu::TextureFormat::Rgba8UnormSrgb
        | wgpu::TextureFormat::Bgra8Unorm
        | wgpu::TextureFormat::Bgra8UnormSrgb
        | wgpu::TextureFormat::R32Float
        | wgpu::TextureFormat::Rg16Unorm => 4,
        wgpu::TextureFormat::Rg32Float | wgpu::TextureFormat::Rgba16Float => 8,
        wgpu::TextureFormat::Rgba32Float => 16,
        other => panic!(
            "format_bytes_per_pixel_wgpu: format {:?} not yet mapped",
            other
        ),
    }
}
