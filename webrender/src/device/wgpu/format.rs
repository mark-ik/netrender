/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! wgpu format-side helpers (pure functions).

/// Bytes per pixel for a `wgpu::TextureFormat`. Used by upload helpers
/// to size staging buffers. Expand as new formats become reachable.
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
