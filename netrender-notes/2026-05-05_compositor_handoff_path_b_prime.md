# Compositor Handoff Path B Prime (2026-05-05)

Status: active design note. NetRender-side sub-phases 5.1-5.4 are shipped; the Serval / servo-wgpu adapter smoke remains pending.

## 1. Purpose

Path (b') recovers axiom-14 native-compositor handoff without forking vello or returning to per-tile render targets. Vello still renders one master texture per frame. NetRender additionally exposes declared compositor surfaces as metadata so a consumer can copy dirty surface regions from the master texture into its own native textures and hand those textures to the OS compositor.

This is not a second rasterizer and not a replacement for the `Scene` paint path. It is a present-time handoff seam for consumers that own platform-specific native surface lifetimes.

## 2. Ownership Boundary

NetRender owns:

- rendering the full scene into an internal master texture;
- tracking declared compositor surfaces across frames;
- reporting per-surface source rectangles, z-order, transform, clip, opacity, and dirty state;
- handing the consumer `WgpuHandles` during `present_frame` so the consumer can encode any required copies.

The consumer owns:

- native texture allocation and reallocation;
- platform surface roles and OS-compositor submission;
- GPU copies from NetRender's master texture into consumer-owned native textures;
- any additional dirty decision caused by destination texture reallocation.

This keeps platform glue out of netrender while still giving embedders enough metadata to avoid unnecessary per-surface blits.

## 3. Public Surface

The consumer-facing trait and payload types live in `netrender_device::compositor` and are re-exported by `netrender`:

```rust
pub struct SurfaceKey(pub u64);

pub struct LayerPresent {
    pub key: SurfaceKey,
    pub source_rect_in_master: [u32; 4],
    pub world_transform: [f32; 6],
    pub clip: Option<[f32; 4]>,
    pub opacity: f32,
    pub dirty: bool,
}

pub struct PresentedFrame<'a> {
    pub master: &'a wgpu::Texture,
    pub handles: &'a WgpuHandles,
    pub layers: &'a [LayerPresent],
}

pub trait Compositor {
    fn declare_surface(&mut self, key: SurfaceKey, world_bounds: [f32; 4]);
    fn destroy_surface(&mut self, key: SurfaceKey);
    fn present_frame(&mut self, frame: PresentedFrame<'_>);
}
```

The scene declares surfaces with `CompositorSurface` entries:

```rust
pub struct CompositorSurface {
    pub key: SurfaceKey,
    pub bounds: [f32; 4],
    pub transform: [f32; 6],
    pub clip: Option<[f32; 4]>,
    pub opacity: f32,
}
```

`Scene::declare_compositor_surface` appends new keys in z-order and updates existing keys in place without reordering. `Scene::undeclare_compositor_surface` removes a key. `set_surface_transform`, `set_surface_clip`, and `set_surface_opacity` update OS-side metadata without forcing content dirty.

## 4. Render Flow

`Renderer::render_with_compositor(scene, master_format, compositor, base_color)` performs the handoff:

1. Render the scene into the rasterizer's internal master texture, pooled by `(width, height, format)`.
2. Diff `scene.compositor_surfaces` against the previous frame.
3. Forward `destroy_surface` for keys that disappeared.
4. Forward `declare_surface` for new keys or changed bounds.
5. Build `LayerPresent` entries in declaration order.
6. Call `Compositor::present_frame` with the master texture, wgpu handles, and layer slice.
7. Commit the current surface state for next-frame diffing.

`master_format` must match the consumer destination texture format because `copy_texture_to_texture` requires matching formats. The current default receipt path uses `Rgba8Unorm`; native BGRA destinations may require `BGRA8_UNORM_STORAGE` or a consumer-side conversion path before broadening.

## 5. Dirty Contract

`LayerPresent.dirty` is the OR of:

- tile-intersection: any tile dirtied by the current render intersects the surface bounds;
- newly declared / absent last frame: the key was not seen in the previous frame;
- bounds changed: current bounds differ from the previous frame's bounds.

Transform, clip, and opacity changes are present-time OS metadata and do not set `dirty` by themselves. Consumers should still OR in their own destination-resource concern, for example when a native texture was reallocated.

`source_rect_in_master` is clamped to the master texture bounds and sorted so `x0 <= x1` and `y0 <= y1` even for defensive out-of-order surface bounds.

## 6. Shipped Receipts

The NetRender-side receipt is `netrender/tests/p13prime_path_b_present_plumbing.rs`:

- 5.1 plumbing: `render_with_compositor` reaches `present_frame`; the master-texture pool reuses an allocation across same-size frames and reallocates on resize.
- 5.2 dirty tracking: unchanged surfaces go clean on the second frame, bounds changes go dirty, and undeclared keys emit `destroy_surface`.
- 5.3 master handoff: layer order follows declaration order and a test compositor can encode `copy_texture_to_texture` only for dirty layers.
- 5.4 metadata setters: transform, clip, and opacity flow to `LayerPresent` without setting `dirty`.

The trait object-safety and literal construction checks live in `netrender_device/src/compositor.rs` unit tests.

## 7. Remaining Work

5.5 is the consumer-side adapter smoke. Serval / servo-wgpu should implement `Compositor`, allocate native destination textures keyed by `SurfaceKey`, copy dirty regions from `PresentedFrame::master`, and route those textures to the platform compositor. That smoke should verify both visible composition and skipped blits for clean surfaces.

This path is also the surface-lifecycle sibling for later WebGL-over-wgpu work: if a WebGL canvas is promoted to a compositor surface, reuse `CompositorSurface` / `SurfaceKey` rather than creating a parallel lifecycle.
