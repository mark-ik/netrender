/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Renderer construction. The embedder owns the wgpu device and
//! hands its handles in; we install a [`WgpuDevice`] over them.

use std::collections::HashMap;
use std::sync::Mutex;

use netrender_device::{WgpuDevice, WgpuHandles};

use crate::renderer::{Renderer, RendererError};
use crate::tile_cache::TileCache;
use crate::vello_tile_rasterizer::VelloTileRasterizer;

#[derive(Default)]
pub struct NetrenderOptions {
    /// Construct the renderer with an `N`-pixel-square tile cache.
    /// Required when `enable_vello = true`. `None` skips tile cache
    /// construction and produces a renderer that can still be used
    /// for direct render-graph access (e.g., running blur or clip
    /// mask tasks via `WgpuDevice` pipeline factories) but cannot
    /// drive `render_vello`.
    pub tile_cache_size: Option<u32>,
    /// Phase 7' — when `true`, eagerly construct a
    /// [`VelloTileRasterizer`] and route [`Renderer::render_vello`]
    /// through it. Requires `tile_cache_size = Some(_)`.
    pub enable_vello: bool,
    /// Roadmap A3 — when `true`, the renderer paints a translucent
    /// red wash on top of any tile that was reported dirty within
    /// the last `tile_dirty_overlay_window_frames` frames. Useful
    /// for visually debugging tile invalidation on dynamic scenes.
    /// No-op when `enable_vello = false` (overlay is rendered as
    /// part of the master vello scene composition).
    pub enable_tile_dirty_overlay: bool,
    /// Roadmap A3 — fade window for the tile-dirty overlay. Tiles
    /// dirtied within the last `N` frames stay visible; opacity
    /// decays linearly with age. `0` (default) is treated as a
    /// reasonable preset (~30 frames ≈ 0.5s at 60 Hz) when
    /// `enable_tile_dirty_overlay` is on; set to override.
    pub tile_dirty_overlay_window_frames: u32,
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

    let tile_cache = options
        .tile_cache_size
        .map(|size| Mutex::new(TileCache::new(size)));

    let vello_rasterizer = if options.enable_vello {
        if tile_cache.is_none() {
            return Err(RendererError::VelloRequiresTileCache);
        }
        let handles = wgpu_device.core.clone();
        let mut rast = VelloTileRasterizer::new(handles)
            .map_err(|e| RendererError::VelloInit(format!("{:?}", e)))?;
        if options.enable_tile_dirty_overlay {
            let window = if options.tile_dirty_overlay_window_frames == 0 {
                30 // default ~0.5s at 60 Hz
            } else {
                options.tile_dirty_overlay_window_frames
            };
            rast.set_dirty_overlay(true, window);
        }
        Some(Mutex::new(rast))
    } else {
        None
    };

    Ok(Renderer {
        wgpu_device,
        tile_cache,
        vello_rasterizer,
        external_texture_pipelines: Mutex::new(HashMap::new()),
    })
}
