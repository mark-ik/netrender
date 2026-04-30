/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Post-Phase-D wgpu renderer skeleton.
//!
//! Owns a [`WgpuDevice`](crate::device::wgpu::adapter::WgpuDevice)
//! handed in by the embedder, plus a small per-`(width, height,
//! format)` cache of `wgpu::Texture` render targets. There is no
//! frame-builder yet — the API surface for ingesting display lists
//! and producing draws will be re-authored on top of this.

pub(crate) mod init;

use std::collections::HashMap;

pub struct Renderer {
    pub wgpu_device: crate::device::wgpu::adapter::WgpuDevice,
    /// Cache of wgpu render-target textures keyed by `(width, height,
    /// format)`. Reused across frames; sized to match the surface
    /// extents the embedder presents from. Entries accumulate
    /// monotonically — small in practice (main framebuffer plus a
    /// handful of off-screen extents).
    wgpu_render_targets: HashMap<(u32, u32, wgpu::TextureFormat), wgpu::Texture>,
}

impl Renderer {
    /// Read back a cached wgpu render-target's pixels as
    /// tightly-packed RGBA8. For oracle / pixel-comparison tests;
    /// production presents directly from the wgpu texture without a
    /// CPU round-trip. Returns `None` if no target has been cached
    /// for the given `(w, h, format)` triple, or if `format` isn't an
    /// RGBA8 equivalent.
    pub fn read_wgpu_render_target_rgba8(
        &self,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Option<Vec<u8>> {
        let texture = self.wgpu_render_targets.get(&(width, height, format))?;
        Some(self.wgpu_device.read_rgba8_texture(texture, width, height))
    }

    /// Return a cached wgpu render-target texture for the given
    /// `(width, height, format)` triple, creating one on first
    /// request. Usage bits: RENDER_ATTACHMENT (for the dispatch),
    /// COPY_SRC (oracle readback), TEXTURE_BINDING (embedder
    /// sample-on-present).
    pub fn ensure_wgpu_render_target(
        &mut self,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> &wgpu::Texture {
        let key = (width, height, format);
        let device = &self.wgpu_device.core.device;
        self.wgpu_render_targets
            .entry(key)
            .or_insert_with(|| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("wgpu render target (cached)"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
            })
    }
}

#[derive(Debug)]
pub enum RendererError {
    WgpuFeaturesMissing(wgpu::Features),
}
