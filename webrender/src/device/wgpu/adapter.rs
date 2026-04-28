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
}
