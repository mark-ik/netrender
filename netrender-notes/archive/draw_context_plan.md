# Plan: Draw Context & Render Pass Batching (C3)

## Context

C1 replaced stringly-typed pipeline keys with `WgpuShaderVariant`. C2 cleaned
up the texture subsystem (byte-slice utility, dither, persistent frame data,
box shadow mask fix). The remaining structural issues are:

1. **Parameter explosion**: `draw_quad_batches_wgpu()` takes 18 parameters.
   Frame-level state (7+ texture views, device refs, texture cache ref) is
   threaded through every draw function.

2. **Encoder-per-call overhead**: `draw_instanced()` creates a new
   `CommandEncoder` → `RenderPass` → `queue.submit()` for every single draw
   call. A typical frame has 50-200 draw calls — that's 50-200 encoder
   allocations, render pass creations, and queue submits per frame.

3. **Per-call buffer allocation**: `draw_instanced()` recreates the unit quad
   vertex buffer, index buffer, projection uniform, and texture-size uniform
   on every call. These are either constants or frame-level values.

These three problems compound: the parameter threading exists *because* each
draw function is standalone, and each draw function is standalone *because*
there's no shared render pass state.

## Design

### D1: `WgpuDrawContext` — Bundled Frame-Level State

A struct that holds all the frame-level texture views and references needed by
draw functions. Created once per frame in `draw_passes_wgpu()`, passed by
reference to all draw helpers.

```rust
/// Frame-level state shared by all draw calls in a render frame.
pub(crate) struct WgpuDrawContext<'a> {
    pub wgpu_dev: &'a mut WgpuDevice,
    pub texture_cache: &'a FastHashMap<CacheTextureId, WgpuTexture>,

    // Frame data texture views (from persistent WgpuFrameDataTextures)
    pub transform_palette: &'a wgpu::TextureView,
    pub render_tasks: &'a wgpu::TextureView,
    pub prim_headers_f: &'a wgpu::TextureView,
    pub prim_headers_i: &'a wgpu::TextureView,
    pub gpu_cache: Option<&'a wgpu::TextureView>,
    pub gpu_buffer_f: Option<&'a wgpu::TextureView>,
    pub gpu_buffer_i: Option<&'a wgpu::TextureView>,
    pub dither: Option<&'a wgpu::TextureView>,
}
```

This replaces the 7-10 individual texture view parameters that currently thread
through every draw function.

**Signature change examples:**

```rust
// Before: 18 parameters
fn draw_quad_batches_wgpu(
    wgpu_dev, wgpu_texture_cache, prim_instances, prim_instances_with_scissor,
    target_view, target_w, target_h, target_format,
    transform_palette_view, render_tasks_view, prim_headers_f_view,
    prim_headers_i_view, gpu_cache_view, gpu_buffer_f_view, gpu_buffer_i_view,
    dither_view, batches_drawn)

// After: 7 parameters
fn draw_quad_batches_wgpu(
    ctx, prim_instances, prim_instances_with_scissor,
    target_view, target_w, target_h, target_format, batches_drawn)
```

The `WgpuDrawContext` also makes it trivial to add new frame-level state
(e.g., profiling handles, debug flags) without touching every function
signature.

### D2: Cached Constant Buffers

Move constant GPU resources out of `draw_instanced()` into persistent fields
on `WgpuDevice`:

```rust
// In WgpuDevice:
unit_quad_vb: wgpu::Buffer,      // [0,0], [1,0], [0,1], [1,1] — never changes
unit_quad_ib: wgpu::Buffer,      // [0,1,2, 2,1,3] — never changes
```

The unit quad vertex/index buffers are identical every call. Allocate once
during device init.

### D3: Batched Command Encoding

Instead of encoder-per-call:
```
draw_instanced() → encoder → pass → submit
draw_instanced() → encoder → pass → submit
draw_instanced() → encoder → pass → submit
```

Move to encoder-per-target:
```
begin_encoder()
  begin_pass(target_view)
    draw_instanced() → set_pipeline, bind, draw
    draw_instanced() → set_pipeline, bind, draw
    draw_instanced() → set_pipeline, bind, draw
  end_pass
submit_encoder()
```

This is the biggest performance win. When multiple draw calls share the same
render target (which they always do within a picture cache tile or texture
cache target), they should share a render pass.

**Key constraint**: wgpu render passes borrow the encoder mutably, so the
render pass lifetime must be scoped carefully. The approach:

1. `WgpuDevice` gets a `begin_target()` / `end_target()` pair that manages
   encoder and pass lifetime.
2. `draw_instanced()` changes from "create encoder + pass + submit" to
   "record draw commands into the active pass".
3. The renderer calls `begin_target()` once per render target, then makes
   N `draw_instanced()` calls, then calls `end_target()`.

**Alternatively** (simpler, less invasive): keep the draw-per-submit pattern
but at least share the encoder across draws to the same target. The render
pass still opens and closes per draw (because load/store ops differ), but
the encoder + submit cost is amortized.

**Recommended approach**: The simpler encoder-batching first. Full render
pass sharing requires deeper refactoring of how clear colors and depth
attachments are managed (the first draw to a target clears, subsequent draws
load). That can be a follow-up.

### D3a: Encoder Batching (Pragmatic First Step)

```rust
// WgpuDevice:
pending_encoder: Option<wgpu::CommandEncoder>,

fn ensure_encoder(&mut self) -> &mut wgpu::CommandEncoder {
    self.pending_encoder.get_or_insert_with(|| {
        self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame encoder"),
        })
    })
}

fn flush_encoder(&mut self) {
    if let Some(encoder) = self.pending_encoder.take() {
        self.queue.submit([encoder.finish()]);
    }
}
```

`draw_instanced()` uses `self.ensure_encoder()` instead of creating its own.
The renderer calls `flush_encoder()` at key points:
- After all draws to a render target (before moving to the next target)
- After composite draws (before surface present)
- At the end of `render_wgpu()`

This eliminates N encoder allocations per target (N = batch count) while
keeping each render pass self-contained with its own load/store ops.

### D4: Per-Target Projection Caching

The projection matrix (`ortho(w, h, max_depth)`) and texture-size uniform are
recomputed per draw call. Since all draws to the same target share the same
dimensions, cache the projection buffer per target:

```rust
// In draw_passes_wgpu, before the batch loop:
let projection_buf = wgpu_dev.create_uniform_buffer(
    "target projection", &ortho_data);
let tex_size_buf = wgpu_dev.create_uniform_buffer(
    "target tex size", &size_data);
```

Pass these buffers to `draw_instanced()` instead of having it recreate them.
This requires adding two buffer parameters to `draw_instanced()` (or putting
them in a `TargetState` struct), but removes N-1 redundant buffer allocations
per target.

## File Changes

### 1. `webrender/src/device/wgpu_device.rs`

- **Add persistent buffers** to WgpuDevice struct:
  - `unit_quad_vb: wgpu::Buffer`
  - `unit_quad_ib: wgpu::Buffer`
  - `mali_workaround_buf: wgpu::Buffer`
  - Initialize in `new_headless()` and `new_with_surface()`

- **Add encoder batching**:
  - `pending_encoder: Option<wgpu::CommandEncoder>` field
  - `ensure_encoder(&mut self)` method
  - `flush_encoder(&mut self)` method

- **Simplify `draw_instanced()`**:
  - Use `self.unit_quad_vb` / `self.unit_quad_ib` instead of recreating
  - Use `self.ensure_encoder()` instead of `self.device.create_command_encoder()`
  - Remove per-call quad/index buffer allocation
  - Remove per-call `queue.submit()`

- **Simplify `render_composite_instances_to_view()`**:
  - Same encoder batching and buffer reuse

### 2. `webrender/src/renderer/mod.rs`

- **Define `WgpuDrawContext`** struct (near WgpuFrameDataTextures)

- **Refactor `draw_passes_wgpu()`**:
  - Create `WgpuDrawContext` at top, holding all texture views
  - Pass `&ctx` to draw helpers
  - Call `flush_encoder()` between render targets

- **Simplify `draw_quad_batches_wgpu()` signature**:
  - Replace 10 texture view parameters with `&WgpuDrawContext`
  - Build `TextureBindings` from context fields

- **Simplify `draw_clip_batch_list_wgpu()` signature**:
  - Replace texture view parameters with `&WgpuDrawContext`

- **Simplify `draw_cache_target_tasks_wgpu()` signature**:
  - Replace texture view parameters with `&WgpuDrawContext`

- **Add `flush_encoder()` calls** in `render_wgpu()`:
  - After `draw_passes_wgpu()` returns
  - After composite tile rendering
  - Before surface present

### 3. No changes to `build.rs`, shaders, or `webrender_build/`

## Implementation Order (build-green at each step)

1. **Cached constant buffers**: Move unit quad VB/IB and mali workaround
   buffer to WgpuDevice fields. Use them in `draw_instanced()`. Pure
   optimization, no API change. Build + verify.

2. **Encoder batching**: Add `pending_encoder`, `ensure_encoder()`,
   `flush_encoder()` to WgpuDevice. Change `draw_instanced()` and
   `render_composite_instances_to_view()` to use them. Add
   `flush_encoder()` calls in renderer. Build + verify.

3. **Define `WgpuDrawContext`**: Create the struct in renderer/mod.rs.
   Nothing uses it yet. Build succeeds.

4. **Migrate draw helpers to use `WgpuDrawContext`**: Change
   `draw_quad_batches_wgpu`, `draw_clip_batch_list_wgpu`, and
   `draw_cache_target_tasks_wgpu` to accept `&WgpuDrawContext` instead of
   individual texture views. Update `draw_passes_wgpu()` to create the
   context and pass it. Build + verify.

Steps 1-2 are device-layer changes (wgpu_device.rs + flush calls in renderer).
Steps 3-4 are renderer-layer refactoring. Each step is independently
committable.

## Verification

- `cargo build --bin graphshell` (full compile)
- `SERVO_WGPU_BACKEND=1 cargo run --bin graphshell` — visual check that all
  rendering is correct (no regressions from encoder batching)
- Performance: fewer wgpu debug validation messages about encoder creation
  (observable with `WGPU_BACKEND_DEBUG=1`)
- Grep check: no draw helper function should have more than 8 parameters

## Risk Assessment

- **Low risk**: Step 1 (cached buffers) — same data, just pre-allocated
- **Medium risk**: Step 2 (encoder batching) — timing of flush matters; if
  encoder isn't flushed before a texture is read back or used as a source,
  rendering will be incorrect. Careful placement of `flush_encoder()` calls.
- **Low risk**: Steps 3-4 (DrawContext) — pure refactor, same behavior
- **Not in scope**: Full render-pass sharing (multiple draws in one pass).
  That requires tracking whether a target has been cleared and switching
  load ops from Clear to Load. Deferred to a future track.

## What This Does NOT Cover

- **Render pass sharing** (multiple draws in one `begin_render_pass`). This
  is the next level of optimization but requires managing clear state, depth
  attachment lifetimes, and load/store ops across draws. C4 candidate.
- **GPU profiling integration** (timestamps, pipeline statistics). Separate
  concern.
- **Composite path refactoring** (merging the separate composite encoder
  into the main pass). Separate concern, lower priority.
