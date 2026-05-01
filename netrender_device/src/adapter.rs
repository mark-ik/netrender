/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Wgpu-native device adapter — the primary entry point of
//! `netrender_device`.
//!
//! `WgpuDevice` composes the booted wgpu primitives from
//! [`crate::core`] plus lazy caches for things consumers build on
//! demand (pipelines, bind-group layouts, texture/buffer arenas).
//! Methods are named for the rendering verbs the renderer needs
//! (`ensure_<family>`, `encode_pass`, `upload_texture`, …).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::core::{self, REQUIRED_FEATURES, WgpuHandles};
use crate::frame;
use crate::pass::{self, DrawIntent, RenderPassTarget};
use crate::pipeline::{
    BrushBlurPipeline, BrushGradientPipeline, BrushImagePipeline, BrushRectSolidPipeline,
    BrushSolidPipeline, GradientKind, build_brush_blur, build_brush_gradient, build_brush_image,
    build_brush_rect_solid, build_brush_solid_specialized,
};
use crate::readback;
use crate::texture::{TextureDesc, WgpuTexture};

/// Wgpu-native device adapter. Holds the embedder-supplied wgpu
/// primitives plus renderer-owned caches (pipelines, bind groups,
/// samplers, vertex layouts).
///
/// Constructed via [`WgpuDevice::with_external`] in production; the
/// headless shortcut [`WgpuDevice::boot`] exists for tests / CI / tools
/// that don't have an embedder fixture.
pub struct WgpuDevice {
    pub core: WgpuHandles,
    /// Pipeline cache keyed by family + render-target format +
    /// override-specialisation flags. For `brush_solid` the only
    /// override is `ALPHA_PASS`. The
    /// `Mutex<HashMap<Key, Pipeline>>::entry().or_insert_with()`
    /// pattern is the model later families replicate for other caches
    /// (bind-group layouts, samplers, vertex layouts, etc.).
    brush_solid: Mutex<HashMap<(wgpu::TextureFormat, bool), BrushSolidPipeline>>,
    // Cache key: (color_format, depth_format, alpha_blend)
    brush_rect_solid: Mutex<HashMap<(wgpu::TextureFormat, Option<wgpu::TextureFormat>, bool), BrushRectSolidPipeline>>,
    brush_image: Mutex<HashMap<(wgpu::TextureFormat, Option<wgpu::TextureFormat>, bool), BrushImagePipeline>>,
    // Cache key: target_format
    brush_blur: Mutex<HashMap<wgpu::TextureFormat, BrushBlurPipeline>>,
    // Cache key: (color_format, depth_format, alpha_blend, kind) — Phase 8D
    brush_gradient: Mutex<HashMap<(wgpu::TextureFormat, Option<wgpu::TextureFormat>, bool, GradientKind), BrushGradientPipeline>>,
}

impl WgpuDevice {
    /// Adopt embedder-supplied wgpu primitives. The embedder has already
    /// created instance / adapter / device / queue for its own surface
    /// or compositor work; the renderer borrows the same ones so it
    /// shares a device with the embedder.
    ///
    /// Verifies [`REQUIRED_FEATURES`] are present on the adapter.
    /// Returns the missing-features set on failure so the embedder can
    /// decide whether to fall back, retry with different power
    /// preference, or surface the error. Phase 0.5 demoted
    /// `REQUIRED_FEATURES` to `Features::empty()`, so this never fails
    /// on a baseline adapter today; the return shape is preserved for
    /// later phases that re-introduce optional features.
    pub fn with_external(handles: WgpuHandles) -> Result<Self, wgpu::Features> {
        let missing = REQUIRED_FEATURES - handles.adapter.features();
        if !missing.is_empty() {
            return Err(missing);
        }
        Ok(Self {
            core: handles,
            brush_solid: Mutex::new(HashMap::new()),
            brush_rect_solid: Mutex::new(HashMap::new()),
            brush_image: Mutex::new(HashMap::new()),
            brush_blur: Mutex::new(HashMap::new()),
            brush_gradient: Mutex::new(HashMap::new()),
        })
    }

    /// Standalone headless boot. Wraps [`core::boot`] for tests / CI /
    /// tools that don't have an embedder; production goes through
    /// [`WgpuDevice::with_external`].
    pub fn boot() -> Result<Self, core::BootError> {
        Ok(Self {
            core: core::boot()?,
            brush_solid: Mutex::new(HashMap::new()),
            brush_rect_solid: Mutex::new(HashMap::new()),
            brush_image: Mutex::new(HashMap::new()),
            brush_blur: Mutex::new(HashMap::new()),
            brush_gradient: Mutex::new(HashMap::new()),
        })
    }

    /// Return the `brush_solid` pipeline for `(format, alpha_pass)`,
    /// building on first request and caching subsequent ones. wgpu
    /// 29 pipeline / bind-group-layout handles are `Clone`
    /// (Arc-wrapped internally), so returning a clone is cheap — no
    /// borrow of the cache lock escapes the call. `alpha_pass` selects
    /// the WGSL `override` specialisation: opaque vs. alpha-clipped
    /// fragment.
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

    /// Opaque `brush_rect_solid` pipeline: depth write ON, compare LESS,
    /// no blend. Sorted front-to-back in the batch for early-Z benefit.
    pub fn ensure_brush_rect_solid_opaque(
        &self,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> BrushRectSolidPipeline {
        self.ensure_brush_rect_solid_variant(color_format, Some(depth_format), false)
    }

    /// Alpha `brush_rect_solid` pipeline: depth write OFF, compare LESS,
    /// premultiplied-alpha blend. Sorted back-to-front in the batch.
    pub fn ensure_brush_rect_solid_alpha(
        &self,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> BrushRectSolidPipeline {
        self.ensure_brush_rect_solid_variant(color_format, Some(depth_format), true)
    }

    /// Opaque `brush_image` pipeline: depth write ON, compare LESS, no blend.
    pub fn ensure_brush_image_opaque(
        &self,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> BrushImagePipeline {
        self.ensure_brush_image_variant(color_format, Some(depth_format), false)
    }

    /// Alpha `brush_image` pipeline: depth write OFF, compare LESS,
    /// premultiplied-alpha blend.
    pub fn ensure_brush_image_alpha(
        &self,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
    ) -> BrushImagePipeline {
        self.ensure_brush_image_variant(color_format, Some(depth_format), true)
    }

    /// Opaque `brush_gradient` pipeline (Phase 8D) for the given
    /// gradient kind: depth write ON, compare LESS, no blend.
    pub fn ensure_brush_gradient_opaque(
        &self,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
        kind: GradientKind,
    ) -> BrushGradientPipeline {
        self.ensure_brush_gradient_variant(color_format, Some(depth_format), false, kind)
    }

    /// Alpha `brush_gradient` pipeline (Phase 8D) for the given gradient
    /// kind: depth write OFF, compare LESS, premultiplied-alpha blend.
    pub fn ensure_brush_gradient_alpha(
        &self,
        color_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
        kind: GradientKind,
    ) -> BrushGradientPipeline {
        self.ensure_brush_gradient_variant(color_format, Some(depth_format), true, kind)
    }

    fn ensure_brush_gradient_variant(
        &self,
        color_format: wgpu::TextureFormat,
        depth_format: Option<wgpu::TextureFormat>,
        alpha_blend: bool,
        kind: GradientKind,
    ) -> BrushGradientPipeline {
        let mut cache = self.brush_gradient.lock().expect("brush_gradient lock");
        cache
            .entry((color_format, depth_format, alpha_blend, kind))
            .or_insert_with(|| {
                build_brush_gradient(&self.core.device, color_format, depth_format, alpha_blend, kind)
            })
            .clone()
    }

    /// `brush_blur` pipeline for `target_format`. Built on first request,
    /// cached by format thereafter. Both H and V passes share the same
    /// pipeline; only the `BlurParams` uniform differs.
    pub fn ensure_brush_blur(&self, format: wgpu::TextureFormat) -> BrushBlurPipeline {
        let mut cache = self.brush_blur.lock().expect("brush_blur lock");
        cache
            .entry(format)
            .or_insert_with(|| build_brush_blur(&self.core.device, format))
            .clone()
    }

    fn ensure_brush_image_variant(
        &self,
        color_format: wgpu::TextureFormat,
        depth_format: Option<wgpu::TextureFormat>,
        alpha_blend: bool,
    ) -> BrushImagePipeline {
        let mut cache = self.brush_image.lock().expect("brush_image lock");
        cache
            .entry((color_format, depth_format, alpha_blend))
            .or_insert_with(|| build_brush_image(&self.core.device, color_format, depth_format, alpha_blend))
            .clone()
    }

    fn ensure_brush_rect_solid_variant(
        &self,
        color_format: wgpu::TextureFormat,
        depth_format: Option<wgpu::TextureFormat>,
        alpha_blend: bool,
    ) -> BrushRectSolidPipeline {
        let mut cache = self.brush_rect_solid.lock().expect("brush_rect_solid lock");
        cache
            .entry((color_format, depth_format, alpha_blend))
            .or_insert_with(|| build_brush_rect_solid(&self.core.device, color_format, depth_format, alpha_blend))
            .clone()
    }

    /// Create a new texture per `desc`. wgpu-native shape: returns
    /// an owned [`WgpuTexture`]; deletion is implicit at Drop.
    /// Sampler / swizzle / filter details live on a separate sampler
    /// cache; `render_target` is a `usage` bit
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
    /// `tex`. The wgpu queue is async-by-default; the upload is in
    /// flight after this returns and is observable on the next submit.
    pub fn upload_texture(&self, tex: &WgpuTexture, data: &[u8]) {
        let bytes_per_row = tex.width * crate::format::format_bytes_per_pixel_wgpu(tex.format);
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

    /// Encode a single render pass from recorded draw intents. Renderer
    /// callsites construct a [`RenderPassTarget`], collect
    /// [`DrawIntent`]s, then ask the device adapter to replay them into
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
    /// sequence. Renderer callsites should acquire encoders here
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
