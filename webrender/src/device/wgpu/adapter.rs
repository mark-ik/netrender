/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Wgpu-native device adapter. The renderer body (eventually) holds
//! this instead of the GL-shaped `Device` re-exported from `gl.rs`.
//! Per the renderer-body adapter plan §A1: this is the design
//! fulcrum.
//!
//! The struct composes the booted wgpu primitives from `core` plus
//! lazy caches for things the renderer body builds on demand
//! (pipelines, bind-group layouts, texture/buffer arenas). Methods
//! on `WgpuDevice` are named for the rendering verbs the renderer
//! body needs (`ensure_<family>`, `encode_pass`, `upload_texture`,
//! …) — explicitly *not* the GL-shaped verbs from `gl.rs`.

use std::collections::HashMap;
use std::sync::Mutex;

use super::core;
use super::pipeline::{BrushSolidPipeline, build_brush_solid};
use super::texture::{TextureDesc, WgpuTexture};

/// Wgpu-native device adapter. Owned by the renderer body once
/// adapter-plan slices A2..A8 land; for now used only by the wgpu
/// test infrastructure.
pub struct WgpuDevice {
    pub core: core::Device,
    /// Pipeline cache keyed by family + render-target format. The
    /// `Mutex<HashMap<Key, Pipeline>>::entry().or_insert_with()`
    /// pattern is the model A2..A7 replicate for every other cache
    /// (bind-group layouts, samplers, vertex layouts, etc.).
    brush_solid: Mutex<HashMap<wgpu::TextureFormat, BrushSolidPipeline>>,
}

impl WgpuDevice {
    /// Boot the device. Wraps `core::boot` and initialises empty
    /// caches.
    pub fn boot() -> Result<Self, core::BootError> {
        Ok(Self {
            core: core::boot()?,
            brush_solid: Mutex::new(HashMap::new()),
        })
    }

    /// Return the `brush_solid` pipeline for `format`, building on
    /// first request and caching subsequent ones. wgpu 29 pipeline /
    /// bind-group-layout handles are `Clone` (Arc-wrapped internally),
    /// so returning a clone is cheap — no borrow of the cache lock
    /// escapes the call.
    pub fn ensure_brush_solid(&self, format: wgpu::TextureFormat) -> BrushSolidPipeline {
        let mut cache = self.brush_solid.lock().expect("brush_solid lock");
        cache
            .entry(format)
            .or_insert_with(|| build_brush_solid(&self.core.device, format))
            .clone()
    }

    /// Create a new texture per `desc`. wgpu-native shape: returns
    /// an owned `WgpuTexture`; deletion is implicit at Drop. Per
    /// adapter plan §A2: replaces `device::Device::create_texture`'s
    /// `(target, format, width, height, filter, render_target,
    /// layer_count) -> Texture` shape — sampler / swizzle / filter
    /// details migrate to the sampler cache (separate slice), and
    /// `render_target` becomes a `usage` bit
    /// (`TextureUsages::RENDER_ATTACHMENT`).
    pub fn create_texture(&self, desc: &TextureDesc<'_>) -> WgpuTexture {
        let texture = self.core.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(desc.label),
            size: wgpu::Extent3d {
                width: desc.width,
                height: desc.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: desc.format,
            usage: desc.usage,
            view_formats: &[],
        });
        WgpuTexture {
            texture,
            format: desc.format,
            width: desc.width,
            height: desc.height,
        }
    }

    /// Upload a tightly-packed pixel buffer to the full extent of
    /// `tex`. wgpu-native replacement for
    /// `device::Device::upload_texture_immediate`. The wgpu queue
    /// is async-by-default; the upload is in flight after this
    /// returns and is observable on the next submit.
    pub fn upload_texture(&self, tex: &WgpuTexture, data: &[u8]) {
        let bytes_per_row = tex.width
            * super::format::format_bytes_per_pixel_wgpu(tex.format);
        self.core.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(tex.height),
            },
            wgpu::Extent3d {
                width: tex.width,
                height: tex.height,
                depth_or_array_layers: 1,
            },
        );
    }
}
