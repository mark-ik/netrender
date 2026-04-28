/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Texture / View / Sampler caches; async upload paths. See plan §6 S1.
//!
//! Per the renderer-body adapter plan §A2: this module owns the
//! wgpu-native replacement for `device::Texture` and friends. The
//! renderer body's GL-shaped texture API surface (~18 device
//! methods: create / delete / bind / blit / read_pixels / etc.)
//! migrates here over multiple sub-slices.

/// wgpu-native texture handle. Wraps a `wgpu::Texture` plus the
/// format and dimensions the renderer body wants to query without
/// reaching into wgpu metadata each time.
///
/// Replaces `device::Texture` (which is GL-shaped: tracks GL handle,
/// FBO IDs for render-target attachments, GL filter / swizzle
/// state). The wgpu version owns just the texture; views and
/// samplers are produced on demand or cached separately.
pub struct WgpuTexture {
    pub texture: wgpu::Texture,
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
}

impl WgpuTexture {
    /// Create the default `wgpu::TextureView` for this texture
    /// (full mip range, full layer range).
    pub fn create_view(&self) -> wgpu::TextureView {
        self.texture
            .create_view(&wgpu::TextureViewDescriptor::default())
    }
}

/// Description for `WgpuDevice::create_texture`. wgpu-native shape:
/// width/height/format/usage are required; filter/swizzle/etc.
/// belong to samplers (different cache) and are not part of the
/// texture itself.
pub struct TextureDesc<'a> {
    pub label: &'a str,
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
    pub usage: wgpu::TextureUsages,
}
