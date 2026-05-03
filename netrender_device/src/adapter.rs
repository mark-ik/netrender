/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Wgpu-native device adapter — the primary entry point of
//! `netrender_device`.
//!
//! `WgpuDevice` composes the booted wgpu primitives from
//! [`crate::core`] plus lazy caches for the WGSL pipeline factories
//! used by render-graph tasks. Methods are named for the verbs the
//! consumer needs (`ensure_brush_blur`, `ensure_clip_rectangle`,
//! `read_rgba8_texture`).
//!
//! The brush_solid / brush_rect_solid / brush_image / brush_gradient
//! factories were retired alongside netrender's batched WGSL
//! rasterizer; vello is the sole rasterizer on main now.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::core::{self, REQUIRED_FEATURES, WgpuHandles};
use crate::pipeline::{
    BrushBlurPipeline, ClipRectanglePipeline, build_brush_blur, build_clip_rectangle,
};
use crate::readback;

/// Wgpu-native device adapter. Holds the embedder-supplied wgpu
/// primitives plus lazy caches for the WGSL pipelines used by
/// render-graph tasks.
///
/// Constructed via [`WgpuDevice::with_external`] in production; the
/// headless shortcut [`WgpuDevice::boot`] exists for tests / CI /
/// tools that don't have an embedder fixture.
pub struct WgpuDevice {
    pub core: WgpuHandles,
    // Cache key: target_format. Phase 6 separable Gaussian blur.
    brush_blur: Mutex<HashMap<wgpu::TextureFormat, BrushBlurPipeline>>,
    // Cache key: (target_format, has_rounded_corners). Phase 9A/9C
    // rounded-rect clip mask coverage.
    clip_rectangle: Mutex<HashMap<(wgpu::TextureFormat, bool), ClipRectanglePipeline>>,
}

impl WgpuDevice {
    /// Adopt embedder-supplied wgpu primitives. Verifies
    /// [`REQUIRED_FEATURES`] are present on the adapter. Returns the
    /// missing-features set on failure so the embedder can decide
    /// whether to fall back, retry with different power preference,
    /// or surface the error.
    pub fn with_external(handles: WgpuHandles) -> Result<Self, wgpu::Features> {
        let missing = REQUIRED_FEATURES - handles.adapter.features();
        if !missing.is_empty() {
            return Err(missing);
        }
        Ok(Self {
            core: handles,
            brush_blur: Mutex::new(HashMap::new()),
            clip_rectangle: Mutex::new(HashMap::new()),
        })
    }

    /// Standalone headless boot. Wraps [`core::boot`] for tests / CI /
    /// tools that don't have an embedder; production goes through
    /// [`WgpuDevice::with_external`].
    pub fn boot() -> Result<Self, core::BootError> {
        Ok(Self {
            core: core::boot()?,
            brush_blur: Mutex::new(HashMap::new()),
            clip_rectangle: Mutex::new(HashMap::new()),
        })
    }

    /// Return the rounded-rect clip-mask pipeline for `(format,
    /// has_rounded_corners)`, building on first request and caching
    /// subsequent ones. The `has_rounded_corners` override toggles
    /// between the SDF (Phase 9A) and the axis-aligned fast path
    /// (Phase 9C).
    pub fn ensure_clip_rectangle(
        &self,
        format: wgpu::TextureFormat,
        has_rounded_corners: bool,
    ) -> ClipRectanglePipeline {
        let mut cache = self.clip_rectangle.lock().expect("clip_rectangle lock");
        cache
            .entry((format, has_rounded_corners))
            .or_insert_with(|| build_clip_rectangle(&self.core.device, format, has_rounded_corners))
            .clone()
    }

    /// Return the separable Gaussian-blur pipeline for `format`,
    /// building on first request and caching subsequent ones.
    pub fn ensure_brush_blur(&self, format: wgpu::TextureFormat) -> BrushBlurPipeline {
        let mut cache = self.brush_blur.lock().expect("brush_blur lock");
        cache
            .entry(format)
            .or_insert_with(|| build_brush_blur(&self.core.device, format))
            .clone()
    }

    /// Read back a 2-D `Rgba8Unorm` / `Rgba8UnormSrgb` texture into
    /// CPU bytes via a staging buffer + map_async. Blocks until the
    /// readback completes; intended for tests and tooling, not
    /// production frames.
    pub fn read_rgba8_texture(
        &self,
        target: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        readback::read_rgba8_texture(&self.core, target, width, height)
    }
}
