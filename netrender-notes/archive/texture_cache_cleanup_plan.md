# Plan: Texture Cache Cleanup for wgpu Backend

## Context

The wgpu backend has a working texture subsystem — cache textures, GPU cache,
per-frame data textures, and composite source textures all function correctly.
However, the texture code grew organically during bring-up and has several
cleanup opportunities: redundant per-frame allocations, missing auxiliary
textures (dither), `unsafe` byte-slice patterns repeated in ~10 call sites,
and texture view creation scattered across the renderer instead of consolidated
in the device layer.

This cleanup pass tightens the texture boundary between renderer and device,
reduces per-frame GPU allocations, and fills in missing functionality — without
changing any shader code or build.rs.

## Problem Areas

### P1: Per-Frame Data Texture Re-allocation

`upload_frame_data_textures()` (renderer/mod.rs:1784) creates **new**
`WgpuTexture` objects every frame for prim_headers_f/i, transform_palette,
render_tasks, and gpu_buffer_f/i. Each call goes through
`wgpu_dev.create_data_texture()` which allocates a new `wgpu::Texture` +
`queue.write_texture()`. The old textures are dropped at end of frame.

The GL path recycles these via persistent textures that get re-uploaded. The
wgpu path should do the same — keep the textures alive in
`WgpuFrameDataTextures` across frames and only reallocate when dimensions
change.

### P2: Missing Dither Texture

`TextureBindings.dither` is always `None` in practice. The GL path creates a
dither matrix texture in `GlRendererAuxTextures` and binds it via
`TextureSampler::Dither`. The wgpu path has `WgpuRendererAuxTextures` as an
empty struct (`#[allow(dead_code)]`). Gradient shaders compiled with
`DITHERING` config sample `sDither` — they fall back to the dummy 1×1 white
texture, which means dithering has no effect. This doesn't cause visual bugs
(gradients still render) but means banding is worse than it should be.

### P3: Repeated Unsafe Byte-Slice Boilerplate

At least 10 call sites in renderer/mod.rs use this pattern:
```rust
let bytes: &[u8] = unsafe {
    std::slice::from_raw_parts(
        data.as_ptr() as *const u8,
        data.len() * std::mem::size_of::<T>(),
    )
};
```
This is used for PrimitiveInstanceData, ClipMaskInstanceRect,
ClipMaskInstanceBoxShadow, CompositeInstance, PrimitiveHeaderF/I,
TransformData, RenderTaskData, and GpuBlockData. A single
`as_byte_slice<T>(slice: &[T]) -> &[u8]` utility would eliminate the
repetition and centralize the safety argument.

### P4: Texture View Creation Scattered in Renderer

Every draw call site in the renderer creates `wgpu::TextureView` objects
inline: `wgpu_texture_cache.get(&id).map(|t| t.create_view())`. These views
are created, used once, and dropped. While wgpu view creation is cheap, the
pattern means the renderer is reaching into `WgpuTexture` internals instead of
going through a device-layer abstraction. More importantly, it means the
renderer must hold both `&wgpu_texture_cache` and `&mut wgpu_device`
simultaneously, which creates borrow-checker friction in several places.

### P5: Box Shadow Clip Mask Texture Not Bound

The box shadow clip draw path (renderer/mod.rs:2375) has a TODO: "resolve
mask_texture_source to a wgpu texture view for sColor0". The
`_mask_texture_source` is iterated but never used — the texture binding is
missing. This means box shadow clips render without their mask texture, which
may produce incorrect clipping for complex box shadows.

## Design

### D1: Persistent Frame Data Textures

Move `WgpuFrameDataTextures` from a per-frame local variable to a persistent
field on `Renderer`:

```rust
#[cfg(feature = "wgpu_backend")]
wgpu_frame_data: Option<WgpuFrameDataTextures>,
```

Change `upload_frame_data_textures()` to take `&mut self` and reuse existing
textures when dimensions match:

```rust
fn upload_or_update_frame_data_textures(&mut self, frame: &Frame) {
    // For each texture: if existing dimensions match, update_data_texture()
    // Otherwise, create_data_texture() and replace.
}
```

`WgpuDevice::update_data_texture()` already exists (wgpu_device.rs:1022) and
handles the "same size = write, different size = recreate" logic. We just need
to wire the renderer to use it.

### D2: Dither Matrix Texture

Create a `WgpuTexture` for the 8×8 Bayer dither matrix during renderer init.
The matrix data is the same as the GL path — it's a constant defined in
WebRender's renderer init code.

Store in `WgpuRendererAuxTextures`:
```rust
struct WgpuRendererAuxTextures {
    dither_texture: Option<WgpuTexture>,
}
```

Create a persistent `wgpu::TextureView` and pass it through to
`TextureBindings.dither` in all gradient draw paths.

### D3: `as_byte_slice` Utility

Add to `wgpu_device.rs` or a shared location:
```rust
/// Reinterpret a typed slice as raw bytes.
///
/// # Safety
/// Safe for `repr(C)` / `Pod`-like types with no padding — all WebRender
/// GPU types qualify (f32/i32 arrays, repr(C) structs of f32/i32).
pub(crate) fn as_byte_slice<T>(data: &[T]) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(
            data.as_ptr() as *const u8,
            std::mem::size_of_val(data),
        )
    }
}
```

Replace all 10+ inline `unsafe` blocks with `as_byte_slice(&instances)`.

### D4: Texture View Helper on Device

Add a method to `WgpuDevice` that takes a `CacheTextureId` and the cache map
reference, returning `Option<wgpu::TextureView>`:

```rust
pub fn cache_texture_view(
    cache: &FastHashMap<CacheTextureId, WgpuTexture>,
    id: CacheTextureId,
) -> Option<wgpu::TextureView> {
    cache.get(&id).map(|t| t.create_view())
}
```

This is a small convenience, but it consolidates the pattern and makes it
searchable. The bigger win is that the draw call sites become shorter and more
readable.

### D5: Fix Box Shadow Mask Binding

Wire `mask_texture_source` into the `TextureBindings.color0` field for box
shadow clips, following the same pattern as quad batches:
```rust
for (mask_texture_source, items) in list.box_shadows.iter() {
    let mask_view = match *mask_texture_source {
        TextureSource::TextureCache(id, _) =>
            wgpu_texture_cache.get(&id).map(|t| t.create_view()),
        _ => None,
    };
    let textures = TextureBindings {
        color0: mask_view.as_ref(),
        // ...
    };
}
```

## File Changes

### 1. `webrender/src/device/wgpu_device.rs`

- **Add `as_byte_slice<T>()` utility function** (~line 20, before WgpuTexture)
  - Public within crate: `pub(crate) fn as_byte_slice<T>(data: &[T]) -> &[u8]`

- **Add `cache_texture_view()` free function** (near TextureBindings)
  - Convenience for `cache.get(&id).map(|t| t.create_view())`

- **No changes to WgpuTexture, TextureBindings, or WgpuDevice** — the device
  layer is already clean. All cleanup is in the renderer.

### 2. `webrender/src/device/mod.rs`

- Re-export `as_byte_slice` under `#[cfg(feature = "wgpu_backend")]`.

### 3. `webrender/src/renderer/mod.rs`

- **Promote `WgpuFrameDataTextures` to Renderer field** (line 1455 area)
  - Add `wgpu_frame_data: Option<WgpuFrameDataTextures>` field
  - Initialize to `None` in constructor

- **Rewrite `upload_frame_data_textures()`** (line 1784)
  - Change to `&mut self` method that reuses or replaces textures
  - Use `update_data_texture()` when dimensions match
  - Use `as_byte_slice()` instead of inline unsafe blocks

- **Populate `WgpuRendererAuxTextures`** (line 1060)
  - Add `dither_texture: Option<WgpuTexture>` and
    `dither_view: Option<wgpu::TextureView>` fields
  - Create during `Renderer::new()` using the 8×8 Bayer matrix
  - Remove `#[allow(dead_code)]`

- **Wire dither view into gradient draw paths**
  - In `draw_batch!` macro: pass dither view to TextureBindings
  - In `draw_quad_batches_wgpu()`: pass dither view to TextureBindings
  - In `draw_cs!` / `draw_cs_blend!` for gradient tasks: pass dither view

- **Fix box shadow mask binding** (line 2375)
  - Resolve `mask_texture_source` to view, bind as `color0`
  - Remove the TODO comment

- **Replace all inline unsafe byte-slice casts** (~10 sites)
  - `draw_batch!` macro body (line 2006)
  - `draw_clip_batches_wgpu()` — rectangle clips, fast-path clips, box shadows
  - `draw_quad_batches_wgpu()` — non-scissored and scissored paths
  - `draw_cs!` / `draw_cs_blend!` macro bodies
  - `render_wgpu()` composite instance serialization
  - `upload_frame_data_textures()` — prim headers, transforms, render tasks

### 4. No changes to `build.rs`, shaders, or `webrender_build/`

## Implementation Order (build-green at each step)

1. **Add `as_byte_slice` utility** in wgpu_device.rs, re-export in mod.rs.
   Build succeeds — nothing uses it yet.

2. **Replace inline unsafe blocks** with `as_byte_slice()` across
   renderer/mod.rs. Pure refactor — same behavior, less code. Build + verify.

3. **Fix box shadow mask binding** (line 2375). Small, isolated change.
   Build + verify.

4. **Add dither texture** to `WgpuRendererAuxTextures`, create during init,
   wire into gradient TextureBindings. Build + verify.

5. **Promote frame data textures to persistent storage**. This is the biggest
   change — rework `upload_frame_data_textures()` to reuse allocations.
   Build + verify.

Each step is independently committable and build-green.

## Verification

- `cargo build --bin graphshell` from graphshell repo (full compile)
- `SERVO_WGPU_BACKEND=1 cargo run --bin graphshell` — visual check:
  - Gradients should look smoother (dithering now active)
  - Box shadow clips should be correct
  - No regressions in text, images, solid backgrounds
- `grep -rn "unsafe.*from_raw_parts" webrender/src/renderer/mod.rs` under
  `wgpu_backend` cfg — should show only `as_byte_slice` call site(s), not
  scattered inline blocks
- Performance: frame data textures should show fewer GPU allocations per frame
  (observable via wgpu validation layer / debug labels)

## Risk Assessment

- **Low risk**: Steps 1-3 are pure refactors or small isolated fixes
- **Medium risk**: Step 4 (dither) — incorrect matrix data would cause
  visually obvious banding patterns, easy to verify
- **Medium risk**: Step 5 (persistent frame data) — dimension mismatch bugs
  could cause GPU validation errors or garbled rendering, but
  `update_data_texture()` already handles this correctly in other code paths
