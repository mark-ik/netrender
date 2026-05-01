/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! wgpu device backend.
//!
//! P1 stage: skeleton struct + module compiles under `--features wgpu_backend`.
//! Trait impls (`GpuFrame`, `GpuShaders`, `GpuResources`, `GpuPass`) land in
//! subsequent P1 commits as the backend-neutral types are lifted out of
//! `gl.rs` (per assignment doc R2). Until then this is a placeholder so the
//! feature flag plumbing is verifiable end-to-end.

#![allow(dead_code)] // Skeleton fields wired but not yet read by trait impls.

use std::sync::Arc;

/// Concrete wgpu-backed device. Sibling to `GlDevice` in `gl.rs`; both
/// implement the four `device::traits` surfaces.
pub struct WgpuDevice {
    instance: Arc<wgpu::Instance>,
    adapter: Arc<wgpu::Adapter>,
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    /// Optional surface for windowed targets. Headless renderers (offscreen
    /// reftests, capture replay) construct without one.
    surface: Option<wgpu::Surface<'static>>,
    /// Format chosen at construction time; pipelines that target the surface
    /// are baked against this.
    surface_format: Option<wgpu::TextureFormat>,
}

impl WgpuDevice {
    /// Construct from a pre-existing wgpu instance/adapter/device/queue
    /// (mirrors the parity branch's host-shared-device pattern).
    pub fn from_parts(
        instance: Arc<wgpu::Instance>,
        adapter: Arc<wgpu::Adapter>,
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        surface: Option<wgpu::Surface<'static>>,
        surface_format: Option<wgpu::TextureFormat>,
    ) -> Self {
        WgpuDevice {
            instance,
            adapter,
            device,
            queue,
            surface,
            surface_format,
        }
    }
}
