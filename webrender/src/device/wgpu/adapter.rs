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

use super::core::{REQUIRED_FEATURES, WgpuHandles};
#[cfg(test)]
use super::core;
use super::frame;
use super::pass::{self, DrawIntent, RenderPassTarget};
use super::pipeline::{BrushSolidPipeline, build_brush_solid_specialized};
use super::readback;
use super::texture::{TextureDesc, WgpuTexture};

/// Wgpu-native device adapter. Holds the embedder-supplied wgpu
/// primitives plus renderer-owned caches (pipelines, bind groups,
/// samplers, vertex layouts).
///
/// Constructed via `with_external(handles)` in production; the test
/// shortcut `boot()` exists for device-side tests that don't have an
/// embedder fixture.
pub struct WgpuDevice {
    pub core: WgpuHandles,
    /// Pipeline cache keyed by family + render-target format +
    /// override-specialisation flags. For `brush_solid` the only
    /// override is `ALPHA_PASS` (parent §4.9). The
    /// `Mutex<HashMap<Key, Pipeline>>::entry().or_insert_with()`
    /// pattern is the model later P slices replicate for other caches
    /// (bind-group layouts, samplers, vertex layouts, etc.).
    brush_solid: Mutex<HashMap<(wgpu::TextureFormat, bool), BrushSolidPipeline>>,
}

impl WgpuDevice {
    /// Adopt embedder-supplied wgpu primitives. The embedder has already
    /// created instance / adapter / device / queue for its own surface
    /// or compositor work; the renderer borrows the same ones so it
    /// shares a device with the embedder (P0 — pipeline-first migration
    /// plan §6).
    ///
    /// Verifies `REQUIRED_FEATURES` are present on the adapter. Returns
    /// the missing-features set on failure so the embedder can decide
    /// whether to fall back, retry with different power preference, or
    /// surface the error.
    pub fn with_external(handles: WgpuHandles) -> Result<Self, wgpu::Features> {
        let missing = REQUIRED_FEATURES - handles.adapter.features();
        if !missing.is_empty() {
            return Err(missing);
        }
        Ok(Self {
            core: handles,
            brush_solid: Mutex::new(HashMap::new()),
        })
    }

    /// Test-only standalone boot. Wraps `core::boot()` for
    /// device-side tests; production goes through `with_external`.
    #[cfg(test)]
    pub fn boot() -> Result<Self, core::BootError> {
        Ok(Self {
            core: core::boot()?,
            brush_solid: Mutex::new(HashMap::new()),
        })
    }

    /// Return the `brush_solid` pipeline for `(format, alpha_pass)`,
    /// building on first request and caching subsequent ones. wgpu
    /// 29 pipeline / bind-group-layout handles are `Clone`
    /// (Arc-wrapped internally), so returning a clone is cheap — no
    /// borrow of the cache lock escapes the call. `alpha_pass` selects
    /// the WGSL `override` specialisation (parent §4.9): opaque vs.
    /// alpha-clipped fragment.
    pub fn ensure_brush_solid(
        &self,
        format: wgpu::TextureFormat,
        alpha_pass: bool,
    ) -> BrushSolidPipeline {
        let mut cache = self.brush_solid.lock().expect("brush_solid lock");
        cache
            .entry((format, alpha_pass))
            .or_insert_with(|| build_brush_solid_specialized(&self.core.device, format, alpha_pass))
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
        let bytes_per_row = tex.width * super::format::format_bytes_per_pixel_wgpu(tex.format);
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

    /// Encode a single render pass from recorded draw intents. This
    /// is the renderer-facing adapter method for A2.X: renderer
    /// callsites construct a wgpu-native `RenderPassTarget`, collect
    /// `DrawIntent`s, then ask the device adapter to replay them into
    /// the active command encoder.
    pub fn encode_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: RenderPassTarget<'_>,
        draws: &[DrawIntent],
    ) {
        pass::flush_pass(encoder, target, draws);
    }

    /// Create the command encoder for one frame or offscreen pass
    /// sequence. Renderer-body callsites should acquire encoders here
    /// instead of reaching through to `core.device`.
    pub fn create_encoder(&self, label: &str) -> wgpu::CommandEncoder {
        frame::create_encoder(&self.core.device, label)
    }

    /// Finish and submit a command encoder. Keeps queue submission on
    /// the adapter boundary, matching the future renderer-owned frame
    /// lifecycle.
    pub fn submit(&self, encoder: wgpu::CommandEncoder) {
        frame::submit(&self.core.queue, encoder);
    }

    /// Read an RGBA8 texture into tightly-packed CPU bytes. Renderer
    /// read-pixels paths should use this adapter method instead of
    /// hand-building staging buffers at callsites.
    pub fn read_rgba8_texture(&self, target: &wgpu::Texture, width: u32, height: u32) -> Vec<u8> {
        readback::read_rgba8_texture(&self.core, target, width, height)
    }
}
