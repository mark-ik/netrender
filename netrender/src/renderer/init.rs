/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Renderer construction. The embedder owns the wgpu device and
//! hands its handles in; we install a [`WgpuDevice`] over them.

use std::sync::Mutex;

use netrender_device::{WgpuDevice, WgpuHandles};

use crate::glyph_atlas::GlyphAtlas;
use crate::image_cache::ImageCache;
use crate::renderer::{Renderer, RendererError};
use crate::tile_cache::TileCache;

/// Phase 10a.1 default atlas extent. 1024×1024 R8Unorm (1 MiB) is
/// many orders of magnitude oversized for the single-glyph receipt;
/// the size knob moves to `NetrenderOptions` at 10a.5 when the tile-
/// cache integration surfaces a real-text scene.
const DEFAULT_GLYPH_ATLAS_SIZE: u32 = 1024;

#[derive(Default)]
pub struct NetrenderOptions {
    /// Phase 7C: enable picture caching. `Some(N)` constructs the renderer
    /// with an `N`-pixel-square tile cache; `prepare()` will route through
    /// it (invalidate, re-render dirty tiles, composite via
    /// `brush_image_alpha`). `None` keeps the direct render path used
    /// in Phases 1–6.
    pub tile_cache_size: Option<u32>,
}

/// Construct a wgpu-only `Renderer`. The embedder owns the wgpu
/// device and hands the instance/adapter/device/queue handles in
/// here. The renderer fails with `WgpuFeaturesMissing(missing)` if
/// the embedder's adapter doesn't expose the features `WgpuDevice`
/// requires (Phase 0.5 demoted `REQUIRED_FEATURES` to empty, so this
/// no longer fails on a baseline adapter; the return shape is
/// preserved for later phases that re-introduce optional features).
pub fn create_netrender_instance(
    handles: WgpuHandles,
    options: NetrenderOptions,
) -> Result<Renderer, RendererError> {
    let wgpu_device =
        WgpuDevice::with_external(handles).map_err(RendererError::WgpuFeaturesMissing)?;

    let nearest_sampler =
        wgpu_device.core.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nearest clamp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

    let bilinear_sampler =
        wgpu_device.core.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bilinear clamp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

    let tile_cache = options
        .tile_cache_size
        .map(|size| Mutex::new(TileCache::new(size)));

    let glyph_atlas = Mutex::new(GlyphAtlas::new(
        &wgpu_device.core.device,
        DEFAULT_GLYPH_ATLAS_SIZE,
        DEFAULT_GLYPH_ATLAS_SIZE,
    ));

    Ok(Renderer {
        wgpu_device,
        image_cache: Mutex::new(ImageCache::new()),
        glyph_atlas,
        nearest_sampler,
        bilinear_sampler,
        tile_cache,
    })
}
