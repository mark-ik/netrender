/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Native-compositor handoff trait — axis 14, path (b′).
//!
//! See
//! [`netrender-notes/2026-05-05_compositor_handoff_path_b_prime.md`](../../netrender-notes/2026-05-05_compositor_handoff_path_b_prime.md)
//! for the full design. Sub-phase 5.1 (this module) is scaffolding —
//! types and trait shape, no per-surface dirty logic yet. Renderer
//! always calls `present_frame` with `layers = &[]`.
//!
//! The trait + types live here in `netrender_device` so the consumer-
//! facing import surface is one crate. `netrender` is upstream of
//! `netrender_device` (depends on it) — putting consumer-facing
//! types here avoids a crate cycle.

use crate::core::WgpuHandles;

/// Stable, consumer-supplied identifier for a compositor surface.
/// Survives across frames; the consumer owns the keyspace and chooses
/// how it maps to its native textures.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SurfaceKey(pub u64);

/// One compositor surface's per-frame present payload.
///
/// `LayerPresent` is the per-frame record netrender emits for each
/// declared compositor surface. The consumer maps each `SurfaceKey`
/// to a native texture it owns (IOSurface / DXGI / Wayland subsurface)
/// and routes it to the OS compositor with the supplied
/// `world_transform` / `clip` / `opacity`.
pub struct LayerPresent {
    pub key: SurfaceKey,
    /// Where this surface's pixels live within the master texture
    /// passed via [`PresentedFrame::master`]. Always in master pixel
    /// space, clamped to master bounds.
    pub source_rect_in_master: [u32; 4],
    /// World-to-screen transform the OS should apply at present time.
    /// 2D affine, column-major: `[a, b, c, d, tx, ty]` represents
    /// `[[a, c, tx], [b, d, ty], [0, 0, 1]]`. Identity is
    /// `[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]`. Distinct from any transform
    /// internal to the master.
    ///
    /// netrender composes the surface's `bounds.origin` (top-left)
    /// into the user-supplied `CompositorSurface.transform` before
    /// emitting it here, so consumers that hold the surface at
    /// layer-local origin (e.g. macOS `CALayer.contents = IOSurface`)
    /// get one transform that's complete on its own — no separate
    /// `bounds.origin` handling is required at the consumer side.
    pub world_transform: [f32; 6],
    /// Optional axis-aligned clip applied by the OS at present time.
    pub clip: Option<[f32; 4]>,
    /// Per-surface opacity applied by the OS at present time.
    pub opacity: f32,
    /// `true` ↔ netrender repainted the contents of
    /// `source_rect_in_master` this frame OR the surface was newly
    /// declared / had its bounds changed / is returning after absence.
    /// `false` ↔ contents unchanged from the last frame; the consumer
    /// may skip the blit.
    ///
    /// The consumer ORs in its own "destination texture reallocated"
    /// concern when deciding whether to copy; netrender only signals
    /// the content-side state.
    pub dirty: bool,
}

/// The per-frame payload netrender hands to
/// [`Compositor::present_frame`].
///
/// The consumer is responsible for any GPU copies from
/// [`PresentedFrame::master`] into its own per-surface destination
/// textures. Use [`PresentedFrame::handles`] to encode + submit
/// those copies.
///
/// `wgpu::Texture` in wgpu 29 is internally Arc-shared; consumers
/// needing to keep the handle past `present_frame` (for async blit
/// work) may `master.clone()` and outlive the borrow.
pub struct PresentedFrame<'a> {
    pub master: &'a wgpu::Texture,
    pub handles: &'a WgpuHandles,
    /// One entry per declared surface, in scene declaration order.
    /// Declaration order is the surface z-order: first declared is
    /// bottom-most. Consumers hand native textures to the OS
    /// compositor in this slice's iteration order.
    pub layers: &'a [LayerPresent],
}

/// Native-compositor handoff hook. Implemented by consumers
/// (servo-wgpu, eventually others) to receive the per-frame master
/// texture + per-surface metadata, copy dirty surface regions into
/// their own native textures, and route those textures to the OS
/// compositor.
///
/// netrender does not encode any GPU copies — that responsibility
/// lives wholly inside `present_frame`. This keeps native-texture
/// lifecycle (allocation, reallocation on DPI change, OS
/// surface-role registration) on the consumer side where the
/// platform glue already lives.
pub trait Compositor {
    /// Register a new compositor surface or update an existing one's
    /// world-bounds. Idempotent on repeat calls with the same key:
    /// updates bounds in place without destroying any consumer-side
    /// resources keyed under it.
    fn declare_surface(&mut self, key: SurfaceKey, world_bounds: [f32; 4]);

    /// Drop a previously-declared surface. After this, the
    /// `SurfaceKey` is not present in subsequent `present_frame`
    /// `layers` slices unless re-declared.
    fn destroy_surface(&mut self, key: SurfaceKey);

    /// Called once per `Renderer::render_with_compositor`. The
    /// consumer encodes any blits it needs from `frame.master` into
    /// its destination textures, submits them via
    /// `frame.handles.queue`, and hands the resulting native textures
    /// to the OS compositor.
    ///
    /// Returning from this method should leave the consumer's frame
    /// in a presentable state — netrender does no further work on
    /// the frame after this call.
    fn present_frame(&mut self, frame: PresentedFrame<'_>);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time check that `Compositor` is object-safe (consumers
    /// pass `&mut dyn Compositor`).
    #[test]
    fn compositor_is_object_safe() {
        fn _accepts_dyn(_: &mut dyn Compositor) {}
    }

    /// Compile-time check that `LayerPresent` and `PresentedFrame`
    /// are layout-stable enough to construct in tests with literal
    /// fields. Catches accidental private fields or required
    /// constructors.
    #[test]
    fn layer_present_constructible_with_literals() {
        let lp = LayerPresent {
            key: SurfaceKey(42),
            source_rect_in_master: [0, 0, 256, 256],
            world_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            clip: None,
            opacity: 1.0,
            dirty: false,
        };
        assert_eq!(lp.key, SurfaceKey(42));
    }
}
