/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Renderer construction. The embedder owns the wgpu device and
//! hands its handles in; we install a [`WgpuDevice`] over them.

use std::collections::HashMap;

use crate::device::wgpu::adapter::WgpuDevice;
use crate::device::wgpu::core::WgpuHandles;
use crate::renderer::{Renderer, RendererError};

#[derive(Default)]
pub struct WebRenderOptions {}

/// Construct a wgpu-only `Renderer`. The embedder owns the wgpu
/// device (per pipeline-first migration plan §6 P0) and hands the
/// instance/adapter/device/queue handles in here. The renderer will
/// fail with `WgpuFeaturesMissing(missing)` if the embedder's
/// adapter doesn't expose the features `WgpuDevice` requires.
pub fn create_webrender_instance(
    handles: WgpuHandles,
    _options: WebRenderOptions,
) -> Result<Renderer, RendererError> {
    let wgpu_device = WgpuDevice::with_external(handles)
        .map_err(RendererError::WgpuFeaturesMissing)?;
    Ok(Renderer {
        wgpu_device,
        wgpu_render_targets: HashMap::new(),
    })
}
