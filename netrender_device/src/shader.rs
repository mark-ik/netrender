/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! WGSL module loading + cache (`include_str!`-based); WGSL `override`
//! specialization is handled at pipeline-factory time.

/// Solid-colour brush shader (Phase 1 smoke test, GL-era ABI).
pub(crate) const BRUSH_SOLID_WGSL: &str = include_str!("shaders/brush_solid.wgsl");

/// Phase 2 solid-rect batch shader. Fresh layout: per-instance storage
/// buffer indexed by `@builtin(instance_index)`, color inlined per
/// instance, ortho-projection-only per-frame uniform.
pub(crate) const BRUSH_RECT_SOLID_WGSL: &str = include_str!("shaders/brush_rect_solid.wgsl");

/// Phase 5 textured-rect batch shader. Instance data in storage buffer;
/// texture + sampler bound at slots 3–4. Nearest-clamp sampler only
/// (filterable: false). sRGB handling deferred to Phase 7.
pub(crate) const BRUSH_IMAGE_WGSL: &str = include_str!("shaders/brush_image.wgsl");

/// Phase 6 separable Gaussian blur. Fullscreen-quad VS (no vertex buffer);
/// 5-tap kernel along `params.step`. Call H then V for a full 2-D blur.
pub(crate) const BRUSH_BLUR_WGSL: &str = include_str!("shaders/brush_blur.wgsl");

/// Phase 8D unified analytic gradient. One shader specializes into
/// linear / radial / conic via the `GRADIENT_KIND` override constant;
/// N-stop ramps live in a per-frame stops storage buffer.
pub(crate) const BRUSH_GRADIENT_WGSL: &str = include_str!("shaders/brush_gradient.wgsl");
