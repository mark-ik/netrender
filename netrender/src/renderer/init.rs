/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Renderer construction. The embedder owns the wgpu device and
//! hands its handles in; we install a [`WgpuDevice`] over them.

use std::sync::Mutex;

use netrender_device::{WgpuDevice, WgpuHandles};

use crate::image_cache::ImageCache;
use crate::renderer::{Renderer, RendererError};
use crate::tile_cache::TileCache;
use crate::vello_tile_rasterizer::VelloTileRasterizer;

#[derive(Default)]
pub struct NetrenderOptions {
    /// Phase 7C: enable picture caching. `Some(N)` constructs the renderer
    /// with an `N`-pixel-square tile cache; `prepare()` will route through
    /// it (invalidate, re-render dirty tiles, composite via
    /// `brush_image_alpha`). `None` keeps the direct render path used
    /// in Phases 1–6.
    pub tile_cache_size: Option<u32>,
    /// Phase 7' — when `true`, eagerly construct a `VelloTileRasterizer`
    /// and route [`Renderer::render_vello`] through it. Requires
    /// `tile_cache_size = Some(_)` (the vello rasterizer reuses the same
    /// tile cache the batched path uses). Independent of the existing
    /// `prepare()` / `render()` path — both pipelines coexist; callers
    /// pick one per frame. Default: `false`.
    pub enable_vello: bool,
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

    let vello_rasterizer = if options.enable_vello {
        if tile_cache.is_none() {
            return Err(RendererError::VelloRequiresTileCache);
        }
        let handles = wgpu_device.core.clone();
        let rast = VelloTileRasterizer::new(handles)
            .map_err(|e| RendererError::VelloInit(format!("{:?}", e)))?;
        Some(Mutex::new(rast))
    } else {
        None
    };

    Ok(Renderer {
        wgpu_device,
        image_cache: Mutex::new(ImageCache::new()),
        nearest_sampler,
        bilinear_sampler,
        tile_cache,
        vello_rasterizer,
    })
}
