/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! WGSL module sources, loaded via `include_str!`. WGSL `override`
//! specialization is handled at pipeline-factory time.

/// Phase 6 separable Gaussian blur. Fullscreen-quad VS (no vertex buffer);
/// 5-tap kernel along `params.step`. Call H then V for a full 2-D blur.
pub(crate) const BRUSH_BLUR_WGSL: &str = include_str!("shaders/brush_blur.wgsl");

/// Phase 9A rounded-rect clip-mask shader. Outputs an Rgba8Unorm
/// coverage texture (all channels = coverage). `HAS_ROUNDED_CORNERS`
/// override toggles the SDF (Phase 9A) vs. the axis-aligned fast
/// path (Phase 9C).
pub(crate) const CS_CLIP_RECTANGLE_WGSL: &str = include_str!("shaders/cs_clip_rectangle.wgsl");
