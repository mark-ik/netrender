# Pipeline-First wgpu Migration Plan (2026-04-29)

**Status**: Active — supersedes the
[2026-04-28 renderer-body wgpu adapter plan](2026-04-28_renderer_body_wgpu_adapter_plan.md).

**Lane**: Replace WebRender's GL backend with an idiomatic wgpu / WGSL
backend, end-to-end. No GL-shaped types preserved as wgpu wrappers. No
second wgpu boot. No data-as-texture carryover. Renderer-body migration
sequenced one shader family at a time, with each slice producing a real
wgpu-native rendering operation through the renderer body.

**Related**:

- Parent plan:
  [2026-04-28_idiomatic_wgsl_pipeline_plan.md](2026-04-28_idiomatic_wgsl_pipeline_plan.md)
- Superseded follow-up:
  [2026-04-28_renderer_body_wgpu_adapter_plan.md](2026-04-28_renderer_body_wgpu_adapter_plan.md)
  (A1 / A2.X.0–4 / A2.3.0 work landed on that plan survives intact —
  see §2)
- Existing wgpu module: [`webrender/src/device/wgpu/`](../webrender/src/device/wgpu/)
- Existing GL device (target for deletion in phase D):
  [`webrender/src/device/gl.rs`](../webrender/src/device/gl.rs)

---

## 1. Why this supersedes the prior adapter plan

The 2026-04-28 adapter plan ordered migration A2 (textures) → A3
(vertex / buffer) → A4 (shader / pipeline) → … → A8 (re-export flip).
That ordering carried two GL-era assumptions:

1. **Textures-first treats data as the unit.** wgpu does not migrate
   textures in isolation — a texture only matters as an input to a
   pipeline's bind group. Migrating
   `dither_matrix_texture: Option<Texture>` →
   `Option<WgpuTexture>` would have preserved the GL anti-pattern of
   using a 2D texture as a 64-byte data carrier (parent plan §4.6
   forbids this for shared tables; dither is the same shape). The
   fully idiomatic dither shape is a WGSL `const` inline in the
   consuming shaders.
2. **A2.X "first per-pass callsite migration" was a fiction.** Every
   renderer pass-encoding callsite touches a `Texture`, a `Program`,
   and a render target — you can't migrate one without the others.
   Trying to find a "narrowest first callsite" forces either
   parallel wgpu-native plumbing for one path or a `Texture` /
   `DrawTarget` dual-handle bridge, both of which are shims.

The adapter plan also let A2.X.5 install a `WgpuDevice` on `Renderer`
via an internal `WgpuDevice::boot()`, separate from the embedder's
wgpu device. Two adapter selections, two devices, no shared textures,
no possible `ExternalTexture` integration — exactly the kind of
"wgpu-shaped GL" the parent plan §5 forbids. Commit `ad655dc09` is
reverted (`40661cd22`); the reverted state is preserved in branch
history as a documented misstep.

This plan reorders around shader families. Each slice migrates one
family end-to-end with all of its inputs (textures, buffers, bindings,
pipeline, pass encoding) reshaped to their idiomatic wgpu form
together. The renderer's main draw loop dispatches per-family between
GL (unmigrated) and wgpu (migrated) during transition; once every
family migrates, GL deletion follows.

---

## 2. What survives from prior work

A1 / A2.X.0–4 / A2.3.0 work from the superseded adapter plan survives
intact. The wgpu module landed there is the foundation this plan
builds on.

| Module | Prior status | Role under pipeline-first |
|---|---|---|
| `device/wgpu/core.rs` | A1 ✅ | `boot()` becomes a test-only helper; production uses `with_external(handles)` introduced in P0. `REQUIRED_FEATURES` constant unchanged. |
| `device/wgpu/pass.rs` | A2.X.0–2 ✅ | `DrawIntent`, `RenderPassTarget`, `ColorAttachment`, `DepthAttachment`, `flush_pass`. Idiomatic; unchanged. |
| `device/wgpu/frame.rs` | A2.X.4 ✅ | `create_encoder` / `submit`. Unchanged. |
| `device/wgpu/pipeline.rs` | S2 ✅ | `build_brush_solid` is the P1 pilot's pipeline. Other family `build_*` factories land per P slice. |
| `device/wgpu/buffer.rs` | S2 ✅ | Uniform arena + storage-buffer creator. Used by P1+. |
| `device/wgpu/binding.rs` | S2 ✅ | `brush_solid_bind_group` is the P1 pilot's bind-group factory. Sampler cache lands in P2. |
| `device/wgpu/texture.rs` | A2.0 ✅ | `WgpuTexture` for legitimately texture-shaped inputs (image cache, glyph atlas, render targets). Not used for data carriers. |
| `device/wgpu/format.rs` | A2.1.0 ✅ | Format mappings for legit textures. |
| `device/wgpu/readback.rs` | A2.3.0 ✅ | RGBA8 readback for the oracle harness; eventually for renderer `read_pixels`. |
| `device/wgpu/adapter.rs` | A1 ✅ | `WgpuDevice` shape; `boot()` swaps to `with_external(handles)` in P0. |
| `device/wgpu/shaders/brush_solid.wgsl` | S2 ✅ | The P1 pilot's WGSL. |

Seven wgpu device-side tests remain green and are the receipt that
A1 / A2.X foundational machinery is wgpu-correct.

---

## 3. What gets deleted (no wgpu wrapper struct)

These GL-shaped concepts have no wgpu equivalent and do not get a
`WgpuFoo` replacement. They disappear when their users migrate.

- **`VAO`** — wgpu sets vertex buffers per-pass via
  `RenderPass::set_vertex_buffer`; no VAO concept.
- **`VBOId` / `RBOId` / `FBOId`** — wgpu has no GL-handle types.
  Buffers are `wgpu::Buffer`; textures are `wgpu::Texture` /
  `TextureView`; framebuffers are an emergent property of
  `BeginRenderPass`.
- **`UploadPBOPool` / `UploadMethod`** — `wgpu::Queue::write_texture`
  is async-by-default and batched at the driver level.
- **`Program` / `ProgramBinary` / `ProgramCache` /
  `ProgramCacheObserver`** — replaced by `wgpu::RenderPipeline`
  (per-family) plus `wgpu::PipelineCache` (parent §4.11). No
  webrender-specific binary blob format.
- **`get_unoptimized_shader_source` and `webrender/res/*.glsl`** —
  WGSL is authored under `device/wgpu/shaders/`.
- **`get_gl_target`** — wgpu textures carry their dimension via
  descriptor.
- **GL `Capabilities`** — replaced by `wgpu::Features` +
  `wgpu::Limits`, declared in `core::REQUIRED_FEATURES`.
- **Y-flip ortho carry** — wgpu surface orientation is explicit;
  declare it.
- **CPU-side channel swaps, manual blend tables, fixed-function
  emulation** — anti-patterns from the dual-servo era.
- **`bind_program` / `bind_texture` / `set_uniform` mutable per-call
  binding state** — pipelines bind once per render pass; per-draw
  differences come from dynamic offsets (`set_bind_group(offset)`)
  or push constants (`set_immediates`).

---

## 4. What gets reshaped (data carriers → idiomatic wgpu)

These are GL-era data-as-texture patterns. They get *deleted in their
texture form* and *recreated* in their idiomatic wgpu shape — typically
a storage buffer per parent plan §4.6, sometimes a WGSL `const`. The
migration happens with the consumer slice, not as a standalone "data
lifecycle migration."

| Today (GL) | Tomorrow (wgpu) | Why |
|---|---|---|
| `gpu_buffer_texture_f` (RGBA32F 2D) | `wgpu::Buffer` storage, `read_only: true` | Per-frame structured table; direct random access via index. |
| `gpu_buffer_texture_i` (RGBA32I 2D) | storage buffer | Same shape as above. |
| `transforms_texture` | storage buffer | Per-primitive transforms; indexed per draw. |
| `prim_header_texture` | storage buffer | Per-primitive metadata; indexed per draw. |
| `vertex_data_textures` (`Vec<VertexDataTextures>`, triple-buffered) | storage buffers | Triple-buffer becomes wgpu submit-boundary semantics; explicit. |
| `dither_matrix_texture` (8×8 R8) | WGSL `const DITHER` inline in consuming shaders | 64 bytes — too small to be a binding. |
| `gpu_cache_texture` (paged 2D) | one or more `wgpu::Buffer` storage entries | Existing paging logic carries forward; only the access path changes. Respect `max_storage_buffer_binding_size` (typ. 128 MB portable). |

`gpu_cache` may need multiple storage bindings if the working set
exceeds a single buffer's portable size limit; that decision is
sized at P1 entry (§10 Q2).

---

## 5. What stays texture-shaped

These are genuine images / framebuffer surfaces. They migrate as
`wgpu::Texture` and live in `device/wgpu/texture.rs`'s purview.

- **Texture cache** (`texture_resolver.texture_cache_map`): image
  cache, glyph atlas, picture-cache tiles, standalone textures.
  Sampled in shaders.
- **Render-target attachments**: color and depth attachments produced
  by `RenderTaskGraph` passes; carried as `wgpu::TextureView`.
- **Zoom-debug widget**: a rendered widget bitmap.
- **External textures**: embedder-supplied `wgpu::TextureView` via
  servo-wgpu's `WgpuRenderingContext`-style handoff. No separate
  type — just a view from the embedder's wgpu device, valid because
  the embedder's device is *the same* wgpu device the renderer holds
  (P0 handoff).

---

## 6. Slice plan

Each slice produces a real artifact and is independently reviewable.
P0 is gating; P1..P8 each migrate one shader family end-to-end; D
deletes GL.

### P0 — Embedder wgpu handoff

**Done condition** (✅ landed 2026-04-29 webrender side; servo-wgpu
side outstanding):

- [x] New `WgpuHandles` struct in `device::wgpu::core` carrying
  `wgpu::Instance` / `Adapter` / `Device` / `Queue` by value. wgpu 29
  handle types are `Clone` (Arc-wrapped internally), so the bundle
  is itself `#[derive(Clone)]` — passing by value is four cheap Arc
  bumps. Renamed the prior `core::Device` shape to `WgpuHandles` in
  the same change (clearer boundary against `WgpuDevice` and resolves
  the parent §10 Q1 shadow concern at the type level).
- [x] `WgpuDevice::with_external(handles: WgpuHandles) -> Result<Self,
  wgpu::Features>` replaces internal-boot for production. Returns the
  missing-features set on adapter mismatch so the embedder can decide
  fallback / retry / surface. `core::boot()` and `WgpuDevice::boot()`
  both gated behind `#[cfg(test)]`.
- [x] `core::REQUIRED_FEATURES` check runs against the embedder's
  adapter at `with_external(...)`; mismatch surfaces as
  `RendererError::WgpuFeaturesMissing(::wgpu::Features)` (the
  absolute-path `::wgpu` is intentional — defends against the local
  `wgpu` module shadow).
- [x] `create_webrender_instance(gl, wgpu: WgpuHandles, notifier,
  options, shaders)` — new `wgpu` parameter as the second positional
  argument. GL parameter remains during transition.
- [x] `Renderer.wgpu_device: WgpuDevice` field installed correctly
  (this time via the embedder handoff, not via an internal boot —
  the misstep the A2.X.5 revert documented).
- [ ] **Servo-wgpu's call site update** is the outstanding piece.
  Pre-P0 tag `pre-p0` (at `aa1850ed7`) marks the last
  `create_webrender_instance(gl, notifier, options, shaders)`
  signature; servo-wgpu can pin against it until its compositor
  hands its `wgpu::Device` / `Queue` through to webrender. Tracked
  outside this repo.

Receipt: `cargo check -p webrender` green (7 warnings, all
unused-helper warnings on adapter machinery awaiting P1 callers);
seven device-side wgpu tests pass via `WgpuDevice::boot()` (the
`#[cfg(test)]` shortcut) in 1.93s. Tests now exercise the
`with_external` path implicitly via the renamed `WgpuHandles` /
`WgpuDevice::boot()` (which constructs a `WgpuDevice` from `boot()`'s
`WgpuHandles` — same shape as the production handoff).

**Sequenced finding during P0**:

- The `use super::core::{self, ...}` import in `adapter.rs` had to
  split into two: `use super::core::{REQUIRED_FEATURES, WgpuHandles};`
  unconditionally, plus `#[cfg(test)] use super::core;` for the
  test-only `boot()` method's references to `core::BootError` /
  `core::boot()`. Without the split, the unconditional `self` was
  unused in non-test compilation.
- `Renderer.wgpu_device` is `pub` (matches the GL `pub device:
  Device` next to it) so embedder code that already reaches into
  `renderer.device.*` can reach `renderer.wgpu_device.*` symmetrically
  during the transition. P slices may tighten visibility once
  callsites stabilise.

### P1 — `brush_solid` end-to-end pilot

**Done condition**: `brush_solid` primitives render correctly through
the actual renderer body via wgpu; pixel-diff against a captured
oracle within tolerance.

This is the largest single slice in the plan and lands as a sequence
of sub-slices, each independently committed. P1 closes when every
sub-slice has landed and the oracle-match receipt passes.

#### Sub-slices (planned)

- [x] **P1.1 — Production-shape storage buffers in brush_solid smoke
  (2026-04-29).** Replaced the S2 contrived palette/push-constant
  shape with two production-shape storage buffers: `PrimitiveHeader`
  (mirrors `prim_shared.glsl::PrimitiveHeader` collapsed into a
  single 64-byte std430 struct — parent §4.6) and `gpu_buffer_f`
  (mirrors GL `fetch_from_gpu_buffer_*`, `vec4<f32>` array indexed by
  `header.specific_prim_address`). brush_solid now fetches its
  header by `instance_index` and reads its colour via
  `gpu_buffer_f[header.specific_prim_address]`, the same shape
  production will use. The `ALPHA_PASS` WGSL `override` replaces
  `MAX_PALETTE_ENTRIES` as the demo of §4.9 specialisation.
  `DrawIntent::uniform_offset` (a single `u32`) became
  `dynamic_offsets: Vec<u32>` to support bind groups with no
  dynamic-offset entries (the new layout has none). `render_rect_smoke`
  exercises the new path; remaining 6 wgpu tests untouched. **Not
  yet wired**: `Transform`, `PictureTask`, `ClipArea`, per-instance
  vertex attributes (`aData`), draw-loop dispatch in renderer body —
  P1.2 onward.
- [x] **P1.2 — Transform storage buffer (2026-04-29).** Added
  `Transform { m: mat4x4, inv_m: mat4x4 }` storage buffer at bind
  slot 1 (mirrors GL `transform.glsl::Transform`, 128 bytes std430).
  Vertex shader fetches via `transforms[header.transform_id &
  TRANSFORM_INDEX_MASK]` (the `0x003fffff` mask matches GL; high bit
  reserved for is_axis_aligned, used in P1.5's AA path) and applies
  `transform.m` to the corner. Smoke feeds identity matrices for
  both `m` and `inv_m`, so the rendered output is unchanged — the
  matrix-multiply path is exercised end-to-end. `inv_m` is part of
  the production table shape but unused by brush_solid's vertex
  path; other families read it for fragment-side untransform.
- [x] **P1.3 — Per-instance `a_data` vertex attribute (2026-04-29).**
  brush_solid vertex shader now reads `@location(0) a_data: vec4<i32>`
  with `VertexStepMode::Instance`. Field decoding matches GL
  `decode_instance_attributes`: `a_data.x` is `prim_header_address`,
  used for the header lookup. The other fields (clip_address,
  segment_index|flags, resource_address|brush_kind) are reserved for
  P1.4 / P1.5 consumers. `DrawIntent` extended with
  `vertex_buffers: Vec<wgpu::Buffer>`; `flush_pass` loops
  `set_vertex_buffer` over them at slots `0..N`. New
  `buffer::create_vertex_buffer` helper. Pipeline gains
  `A_DATA_LAYOUT` (`Sint32x4` at location 0, stride 16, instance step).
- [x] **P1.4 — PictureTask (RenderTaskData) + per-frame uniform; full
  GL vertex math (2026-04-29).** Added `RenderTaskData` storage at
  bind slot 3 (mirrors GL `render_task.glsl::RenderTaskData` — 32
  bytes std430; PictureTask and ClipArea are both interpretations of
  this single struct, `user_data.x` = device_pixel_scale,
  `user_data.yz` = content_origin / screen_origin) and a `PerFrame`
  uniform at bind slot 4 (`u_transform: mat4x4<f32>`, mirrors GL
  `uTransform` orthographic projection — parent §4.7 tier 4). Vertex
  shader now does the full GL pipeline:
  `world_pos = transform.m * (local, 0, 1);
   device_pos = world_pos.xy * device_pixel_scale;
   final_offset = -content_origin + task.task_rect.xy;
   clip_pos = u_transform * (device_pos + final_offset * world_pos.w,
   z * world_pos.w, world_pos.w)`. Smoke feeds identity-equivalent
   inputs (zero offsets, scale=1, identity projection) so the math
   collapses to the P1.3 receipt — every multiplication and addition
   in the production transform pipeline is exercised, just with values
   that preserve the rendered shape. **Renderer-body render-target
   lifecycle** (the original P1.4 second clause) is moved to P1.6
   alongside the draw-loop dispatch where it actually belongs.
   `buffer::create_uniform_buffer` helper added for single-shot
   per-frame uniforms (paired with the per-draw `create_uniform_arena`).
- [x] **P1.5 — ClipArea + clip-mask + alpha-pass override
  (2026-04-29).** ClipArea reads the same `RenderTaskData` table as
  PictureTask (one storage, two interpretations — `user_data.yz` is
  `screen_origin` instead of `content_origin`). Vertex shader now
  decodes `clip_address = a_data.y`, fetches the clip area, and
  computes `clip_uv` / `clip_bounds` varyings per GL `write_clip`.
  Fragment shader gains `do_clip()` (mirrors GL's non-SWGL path):
  bounds-rect early-out for the dummy zero rect, bounds check, then
  `textureLoad(clip_mask, vec2<i32>(mask_uv * position.w), 0).r`.
  `position.w` in WGSL fragment matches GL `gl_FragCoord.w`
  (per-fragment 1 / clip_pos.w). New bind slot 5 carries the
  clip-mask texture (R8Unorm `texture_2d<f32>`, no sampler — uses
  `textureLoad` like GL `texelFetch`); always bound for both
  pipelines, only read in the alpha-pass shader. New
  `render_rect_alpha_smoke` test runs `ALPHA_PASS=true` with an 8×8
  R8 mask of `1.0` and a `screen_origin=(-4,-4)` /
  `device_pixel_scale=4` ClipArea so every fragment passes bounds
  and samples `1.0` — output remains red, validating the textureLoad
  path end-to-end. Existing `render_rect_smoke` (opaque) gains a
  dummy 1×1 mask binding to satisfy the layout.
- [~] **P1.6 — Per-family draw-loop dispatch** (split further during
  6b recon — see "Sequenced finding" below):
  - [x] **P1.6a — Dispatch hook landed (2026-04-29).** First
    renderer-body edit. `draw_alpha_batch_container` (mod.rs:3022)
    now calls `self.try_dispatch_wgpu(batch)` before the GL
    `bind` + `draw_instanced_batch` path; if the wgpu side returns
    `true` the GL path is skipped. The `try_dispatch_wgpu` method
    is the per-family `match` site — currently a single `_ =>
    false` arm, so behaviour is unchanged. The hook establishes
    the dispatch contract; subsequent sub-slices populate
    families. `PrimitiveBatch` added to the renderer's batch
    imports.
  - [x] **P1.6b.1 — Per-frame wgpu storage-buffer producer
    (2026-04-29).** New `renderer::wgpu_frame_data` module +
    `Renderer.wgpu_frame_data: Option<WgpuFrameData>` field.
    Populated in `draw_frame` immediately after
    `update_gpu_buffer_texture`, from the same `Frame` source GL
    consumes: `gpu_buffer_f.data` → `array<vec4<f32>>` storage,
    `transform_palette` → `array<Transform>` storage,
    `render_tasks.task_data` → `array<RenderTaskData>` storage,
    and `prim_headers.headers_float[i]` ++ `headers_int[i]`
    collapsed into 64 B std430 `array<PrimitiveHeader>` per WGSL.
    Buffers recreated per-frame (mirrors GL "re-upload every
    frame" pattern in `VertexDataTexture`); reuse / streaming
    later if profiling demands. **No consumer yet** — receipt is
    `cargo check -p webrender` clean (the 4 "field never read"
    warnings on `WgpuFrameData` are the receipt) and 8/8 wgpu
    device-side tests still pass. P1.6b.2's
    `BrushBatchKind::Solid` arm body will read.
  - [x] **P1.6b.2 — `BrushBatchKind::Solid` arm body
    (2026-04-29).** Grew `try_dispatch_wgpu` to take `draw_target:
    DrawTarget` and `projection: &Transform3D<f32>`. New arm
    `BatchKind::Brush(BrushBatchKind::Solid)` calls
    `dispatch_brush_solid_temp` and returns `false` (GL still
    draws). The dispatch reads from `wgpu_frame_data`, copies
    `batch.instances` straight into a per-batch vertex buffer (no
    transform — `PrimitiveInstanceData` is already byte-equal to
    `a_data: vec4<i32>`), builds a `PerFrame` uniform from
    `projection.to_array()` (column-major matches WGSL
    `mat4x4<f32>`), creates a throwaway `Rgba8Unorm` target sized
    to `draw_target.dimensions()`, assembles a `DrawIntent`, and
    submits via `wgpu_device.encode_pass`. Per-call resource churn
    (texture + encoder + submit) is intentional — the temp shape
    proves the bridge end-to-end without committing to render-
    target replacement. **Hardened** `WgpuFrameData::build` to pad
    empty source slices to one element of zeros (wgpu rejects
    `size: 0` buffers; lightly-loaded frames legitimately have
    empty `gpu_buffer_f`). Receipt: `cargo check -p webrender`
    clean (5 warnings, all pre-existing or unrelated); 8/8 wgpu
    device-side tests still pass in 2.11s. **Outstanding for
    P1.6b.3**: the temp target is hardcoded `Rgba8Unorm`, which
    is fine for P1.6b.2 (output discarded) but needs to query the
    actual `DrawTarget` format once we render into the real
    surface; pipeline cache already keys on format so the
    promotion is mechanical.
  - [x] **P1.6c — Render-target cache on Renderer (2026-04-29).**
    New `Renderer.wgpu_render_targets: FastHashMap<(u32, u32,
    wgpu::TextureFormat), wgpu::Texture>` field plus
    `ensure_wgpu_render_target(width, height, format)` method
    (split-borrow: borrows `&self.wgpu_device.core.device` outside
    the `entry().or_insert_with(...)` closure). The dispatch in
    `dispatch_brush_solid_temp` no longer creates a per-call
    throwaway texture — it asks the cache. Distinct GL `DrawTarget`s
    with equal `(w, h, format)` share a single wgpu mirror, which
    is incorrect for production semantics but harmless in the
    debug-discard era (every dispatch clears the cached texture
    and never reads it). No eviction yet; cache grows monotonically
    with distinct surface sizes (small in practice — main
    framebuffer + a handful of picture-cache tile sizes). LRU /
    GL-deletion-tied eviction lands later if needed. Receipt:
    `cargo check -p webrender` clean (5 warnings, all pre-existing
    or unrelated); 8/8 wgpu device-side tests still pass in 1.68s.
  - [x] **P1.6b.3a — Hook returns true for Solid (2026-04-29).**
    `BrushBatchKind::Solid` arm flipped from `false` to `true`:
    GL fallthrough is now skipped for solid batches, both opaque
    and alpha-pass. The wgpu render-target cache (P1.6c) is the
    canonical surface — the embedder presents from these
    textures, so dropping the GL solid draw doesn't lose visible
    output (the wgpu draw produced it). Unmigrated families
    still fall through to GL, and their output goes to the GL
    framebuffer that nobody presents — a known dev-time gap that
    closes family-by-family across P2..P8. Renamed
    `dispatch_brush_solid_temp` → `dispatch_brush_solid` and
    tightened: `wgpu_frame_data` access uses `expect` (the
    guard's only failure mode is being called outside
    `draw_frame`, a programming error). Cached render-target
    texture usage extended with `COPY_SRC` (for P1.9-style
    readback) and `TEXTURE_BINDING` (for the embedder's eventual
    sample-on-present). Receipt: `cargo check -p webrender`
    clean (5 warnings, all pre-existing or unrelated); 8/8 wgpu
    device-side tests still pass in 2.34s.

    **Why no blit / GL preservation**: WebRender's GL framebuffer
    is internal scratch — nothing outside webrender presents
    from it. Per P0, the embedder owns the wgpu device and
    presents from wgpu textures. A migrated family's GL draw
    therefore goes to a framebuffer no consumer reads, so
    skipping it loses nothing observable; the wgpu draw to the
    cached render target *is* the visible output (once the
    embedder side of P0 lands and starts presenting). Phase D
    deletes the GL device entirely; until then GL stays alive
    only to render unmigrated families' (invisible) output, and
    the GL/wgpu interop / cross-API blit story I had drafted as
    a possible P1.6b.3 design was unnecessary.
  - [x] **P1.6b.3b — Readback accessor on Renderer (2026-04-29).**
    New `Renderer::read_wgpu_render_target_rgba8(width, height,
    format) -> Option<Vec<u8>>` returns the tightly-packed
    pixels of a cached wgpu render-target via
    `wgpu_device.read_rgba8_texture`. Returns `None` if no
    target is cached for that triple (or the format isn't an
    RGBA8 equivalent). For oracle / pixel-comparison tests
    (P1.9 et al.); production presents directly from the wgpu
    texture without a CPU round-trip. Receipt: same as 6b.3a.
  - [x] **P1.6d — Alpha-batch loop dispatch (2026-04-29).** Same
    `try_dispatch_wgpu` hook now fires inside the
    `alpha_batch_container.alpha_batches` loop alongside the
    opaque-batch hook from P1.6a. Hook signature grew with
    `is_alpha_pass: bool`; opaque call site passes `false`, alpha
    call site passes `true`. WgpuDevice's pipeline cache rekeyed
    from `format → BrushSolidPipeline` to `(format, alpha_pass) →
    BrushSolidPipeline`; `ensure_brush_solid` now takes both
    arguments and goes straight to
    `build_brush_solid_specialized`. The convenience wrapper
    `build_brush_solid` is removed (its only caller, the
    `render_rect_smoke` test, calls `_specialized` directly). The
    `wgpu_device_a1_smoke` test gained an `alpha_pass=true`
    cache-miss to exercise the alpha pipeline build path.
    Behaviour unchanged: hook still returns `false`, GL still
    draws everything; alpha-pass solid batches now also pump
    through the wgpu bridge each frame. Receipt: `cargo check
    -p webrender` clean (5 warnings, all pre-existing or
    unrelated); 8/8 wgpu device-side tests pass in 1.99s.

**Sequenced finding during P1.6b.1 recon (2026-04-29)**: the
original P1.6b as written ("first sub-slice where GL fallthrough
actually skips") tries to land four things at once — storage-buffer
producer, hook signature growth, DrawIntent assembly, *and* a wgpu
render-target view that doesn't exist until 6c's cache. Splitting
into 6b.1 (producer, this slice) → 6b.2 (DrawIntent assembly to a
temp target, GL still draws) → 6c (render-target cache) → 6b.3 (flip
hook to `true`) keeps each commit independently reviewable and
pushes the "GL output replaced" boundary to a single small step
after the bridge is proven.

- [ ] **P1.7 — Pipeline cache (§4.11).** `wgpu::PipelineCache` with
  on-disk path; async compile.
- [ ] **P1.8 — Authored brush_solid-only oracle scene + capture.**
  Extends `webrender/tests/oracle/` via the
  `webrender-wgpu-oracle` worktree.
- [ ] **P1.9 — End-to-end test: oracle\_brush\_solid\_smoke
  matches.** Closes P1.

#### What lands across the sub-slices

- [ ] **Storage-buffer reshape** of `gpu_cache`, `transforms`,
  `prim_headers` for `brush_solid`'s consumption. The 2D-texture
  forms remain populated for unmigrated families until D; this slice
  adds parallel storage-buffer producers fed by the same backend
  data.
- [ ] **§4.7 uniform hierarchy** wired in production: dynamic-offset
  uniform buffer for per-draw transforms; push constants
  (`var<immediate>`) for per-draw flags / indices; static uniform
  for per-pass viewport / time.
- [ ] **§4.8 record-then-flush** in the renderer body: the
  `brush_solid` path's draw call becomes a `DrawIntent` recorded
  into a per-pass bucket; pass replay through
  `WgpuDevice::encode_pass`.
- [ ] **§4.9 override specialization**: `brush_solid` alpha vs.
  opaque variants collapse to one WGSL plus override-specialized
  pipelines (the override pattern from S2 already works for
  `MAX_PALETTE_ENTRIES`; widen to alpha-mode here).
- [ ] **§4.11 async pipeline compile + on-disk cache** introduced
  for the `brush_solid` pipeline; `wgpu::PipelineCache` gets its
  on-disk path. Warming UX deferred to compositor integration but
  the cache mechanic lands here.
- [ ] **Render-target attachment**: one `wgpu::Texture` /
  `wgpu::TextureView` for `brush_solid`'s output, attached via
  `RenderPassTarget`. Render-target lifecycle (size, recreation on
  resize) lands here.
- [ ] **Per-family draw-loop dispatch**: the renderer's main draw
  loop recognizes `brush_solid` batches and routes them through
  wgpu; other families fall through to GL during transition. Single
  `match` in the draw loop — not a trait, not a compatibility shim.
  Annotated with a TODO marker so all migration sites grep
  (§10 Q1).

Receipt: a `brush_solid`-only oracle scene (e.g., an authored solid-
rectangle YAML rendered through Wrench → captured PNG) pixel-matches
through the renderer body's wgpu path. Tolerance default 0;
documented per-scene tolerance only on root cause (parent §S4
"No hacks"). Test name: `renderer::wgpu_brush_solid_smoke` or
similar; runs as part of `cargo test -p webrender`.

### P2 — Brush family expansion

**Done condition**: `brush_image`, `brush_image_repeat`,
`brush_blend`, `brush_mix_blend`, `brush_linear_gradient`,
`brush_opacity`, `brush_yuv_image` migrate through the same
pipeline-first pattern P1 established.

What lands beyond P1:

- [ ] **Sampled texture machinery**: `wgpu::Texture` +
  `wgpu::Sampler` + bind-group entries for image-cache textures.
- [ ] **Sampler cache** in `WgpuDevice` (same
  `Mutex<HashMap>::entry().or_insert_with()` pattern as the brush
  pipeline cache).
- [ ] **Texture-cache integration**: `texture_resolver.
  texture_cache_map` entries become `wgpu::Texture` for image-cache,
  picture-cache tile, atlas, and standalone categories.
- [ ] **YUV / blend-mode / opacity variants** through override
  specialization where parameter-only.

Receipt: brush-family oracle scenes match. The texture cache entries
consumed by migrated families no longer back to GL textures.

### P3 — `ps_quad` family

**Done condition**: textured / gradient / radial / conic / mask /
mask-fast-path quad shaders migrate.

What lands beyond P2:

- [ ] More override specialization cases (gradient interpolation
  modes, mask vs. mask-fast-path).
- [ ] First time a single pipeline family handles multiple primitive
  types via overrides.

### P4 — Clip-mask family (`cs_clip_*`)

**Done condition**: `cs_clip_rectangle` (incl. fast path) and
`cs_clip_box_shadow` migrate.

What lands beyond P3:

- [ ] **First cache-task render pass**: clip masks render to
  dedicated render-target textures used as inputs to subsequent
  draws. Demonstrates multi-pass orchestration via
  `RenderTaskGraph` + `encode_pass` per pass.
- [ ] **Depth attachment policy** exercised under realistic load
  (clear / discard via `RenderPassTarget`).

### P5 — Cache-task family (gradient / blur / scale / svg-filter)

**Done condition**: `cs_fast_linear_gradient`, `cs_linear_gradient`,
`cs_radial_gradient`, `cs_conic_gradient`, `cs_blur` (color + alpha),
`cs_scale`, `cs_svg_filter`, `cs_svg_filter_node` migrate.

### P6 — Border / line cache tasks

**Done condition**: `cs_border_solid`, `cs_border_segment`,
`cs_line_decoration` migrate.

### P7 — Text family

**Done condition**: `ps_text_run` + dual-source variants migrate.
Glyph atlas becomes a `wgpu::Texture` (or texture array per
parent §S6 sub-task; parent §Q14 atlas-ownership stays as resolved).

What lands beyond P6:

- [ ] **Glyph atlas as `wgpu::Texture`** — the genuine-texture case;
  no reshape to buffer.
- [ ] **Dual-source blending** verified at runtime via subpixel-AA
  test scene.

### P8 — Composite / debug / utility

**Done condition**: `composite` (incl. fast path + yuv variants),
`debug_color`, `debug_font`, `ps_clear`, `ps_copy` migrate.

After P8, every shader family in `WgpuShaderVariant::ALL`
(parent §S6) renders through wgpu. The renderer's draw loop has
only the wgpu branch reached at runtime, even though the GL branch
still compiles.

#### CPU rendering after swgl deletion

`swgl` and `glsl-to-cxx` go in phase D. They are GLSL-shaped (a CPU
emulator of the GL shader corpus); they cannot survive the move to
authored WGSL. Their loss is intentional but not free.

CPU-rendering use cases (headless CI, no-GPU machines, fallback)
are served instead by wgpu's existing software-backend paths,
selected by the embedder via `RequestAdapterOptions {
force_fallback_adapter: true, … }`:

- **Vulkan via Lavapipe** (Mesa's CPU Vulkan ICD; Linux / Windows /
  macOS).
- **DX12 via WARP** (Microsoft's CPU DX12; Windows).
- **Vulkan via SwiftShader** (Google's CPU Vulkan; used by
  Chromium for the same purpose).

P0's design already supports this — the embedder chooses the
adapter, webrender uses what it's given. No webrender-side branching,
no CPU-specific path.

Trade-off: swgl was *optimised for WebRender's specific shader
corpus and draw patterns*; Lavapipe / WARP are general-purpose. CPU
rendering will likely be measurably slower post-deletion. For
Servo's headless / CI / dev-tier use cases this is acceptable; if a
deployment ever needs swgl-grade CPU performance, the right response
is to invest in Lavapipe performance for WebRender's draw patterns
(benefits the wgpu ecosystem at large), not revive swgl (which
depends on GLSL we no longer author).

### D — Delete GL backend

**Done condition**: `cargo build` is GL-free.
`cargo tree | grep -i gl` returns nothing surprising. The binary
works in servo-wgpu.

Checklist:

- [ ] Delete `webrender/src/device/gl.rs`, `query_gl.rs`.
- [ ] Drop `gleam` dep from `webrender/Cargo.toml`.
- [ ] Delete authored GLSL: `webrender/res/*.glsl`.
- [ ] Delete `swgl/`, `glsl-to-cxx/`.
- [ ] Delete `webrender_build/src/glsl.rs`,
  `webrender_build/src/wgsl.rs` if any, and any
  `shader_runtime_contract*` content.
- [ ] Delete `dither_matrix_texture`, `gpu_buffer_texture_f/i`,
  `vertex_data_textures`, `transforms_texture`, `prim_header_texture`
  from `Renderer`. (The data is still produced — into storage
  buffers — but the texture-shaped fields go.)
- [ ] Delete VAO / VBO / PBO / FBO / `Program` / `ProgramCache` /
  `Capabilities` infrastructure.
- [ ] Delete `Renderer::device: Device` (the GL one). The wgpu
  handles become the only GPU surface.
- [ ] `gl: Rc<dyn gl::Gl>` parameter to `create_webrender_instance`
  deleted.
- [ ] Per-family draw-loop dispatch collapses to a single wgpu path.
- [ ] Servo-wgpu drops its GL dep.

Receipt: parent plan §S4 oracle scenes all pass through the wgpu
path; `cargo build -p webrender` clean; binary works in servo-wgpu.

---

## 7. Sequencing

Hard dependencies:

- P0 → everything (need embedder handoff before any family-side
  work)
- P1 → P2..P8 (need the pilot's pattern before extending)
- P2 → P3..P7 (need sampled-texture machinery)
- P4 → P5, P6 (need cache-task pass shape)
- D → all P slices (need every family migrated)

Suggested order: P0 → P1 → P2 → P3 → P4 → P5 → P6 → P7 → P8 → D.

P3..P6 are largely independent of each other after P2's machinery
lands; multiple hands could parallelize. P7 (text) is gated on glyph
atlas decisions (parent §Q14, resolved as "atlas owned by webrender-
wgpu").

---

## 8. Receipts

- **P0**: `cargo check -p webrender` green; existing 7 wgpu device-
  side tests pass; servo-wgpu compiles against new signature.
- **P1**: `brush_solid`-only oracle scene pixel-matches through the
  renderer body's wgpu path (tolerance 0 default; documented
  per-scene tolerance only on root cause).
- **P2..P8**: each family's representative oracle scene matches.
- **D**: `cargo tree | grep -i gl` clean; binary works in
  servo-wgpu's compositor; parent plan §S4 oracle set fully passes
  through the wgpu path.

---

## 9. Risks

- **Each P slice is multi-day to multi-week.** Renderer-body surgery
  against a 5,316-LOC god object. Slow, careful, more compile / debug
  cycles per turn than design slices were.
- **P0 changes Servo's call site.** Pre-P0 tag on
  `idiomatic-wgpu-pipeline` for servo-wgpu to pin against until both
  sides land in lockstep.
- **Storage-buffer size limits.** `max_storage_buffer_binding_size`
  is typically 128 MB portable; gpu_cache may push this. Existing
  paging logic carries forward; verify pre-P1 (§10 Q2).
- **Per-family dispatch in the draw loop is transitional code that
  lives across P1..P8.** Each P slice slims it; D collapses it.
  Until D, both backends compile.
- **Async pipeline compile UX (parent §Q13).** First-run compile
  latency surfaces during P1+; warming UX shape decided alongside
  servo-wgpu compositor integration.
- **WGSL feature parity with GL-era usage.** Dynamic indexing into
  storage buffers, dual-source blending, override specialization
  bounds. Surface concrete cases as P slices encounter them; document
  here.
- **Reverted A2.X.5 commit (`ad655dc09`) preserved in branch
  history.** Documented as a misstep (independent boot was a hack).
  Branch history is honest about why.

---

## 10. Open questions

1. **Per-family dispatch shape.** A `match` on family kind in the
   renderer's draw loop is the simplest. Risk: two parallel branches
   become entrenched. Default: single `match` with a TODO grep
   target; revisit if it grows beyond ~50 LOC.
2. **`gpu_cache` as one storage buffer or many?** A single binding
   may exceed `max_storage_buffer_binding_size`; multiple bindings
   stress `max_storage_buffers_per_shader_stage`. Decide at P1
   entry against realistic gpu_cache size on the test machine and
   the portable wgpu adapters servo-wgpu targets.
3. **`ExternalTexture` design.** Embedder-supplied
   `wgpu::TextureView`. Materializes in P2 alongside texture-cache
   integration. Now naturally supported because embedder and
   webrender share one wgpu device (P0 handoff).
4. **Pipeline cache key shape.** Per-family vs. per-(family,
   format, override) keys. Default: per-(family, format, immediates)
   matches what `WgpuDevice::ensure_brush_solid` already does.
5. **Disposition of `core::boot()` after P0.** Stay test-only?
   Move to `core::test_boot` to make the boundary obvious in code?
   Default: gate behind `cfg(test)` initially; rename if tests
   acquire a separate fixture file.
6. **Oracle worktree retirement.** The oracle harness depends on
   `upstream/0.68` + GL Wrench at
   `../webrender-wgpu-oracle`. After D, can the worktree be retired?
   It still captures pixels for new scenes. Default: keep until
   parent §S4 oracle set is fully captured for the corpus we
   expect to test against.
7. **WGSL `const` vs. uniform buffer for dither.** A WGSL `const`
   inlines into every consuming shader (cheap at 64 bytes). A
   tiny uniform buffer would be shared. Default: `const` per
   shader unless multiple families converge on identical dither
   tables and the inline duplication becomes annoying.

---

## 11. Bottom line

Pipeline-first, family-by-family. Embedder owns the wgpu device.
Data-as-texture carriers get deleted, not migrated. The renderer's
main draw loop dispatches per-family during transition; once every
family migrates, GL deletion follows.

Start P0. The rest follows shader families in roughly increasing
complexity. P1 (`brush_solid`) is the largest single slice in the
plan because it forces every architectural decision in parent plan
§4.6–4.11 to land at once; subsequent slices reuse its machinery.

---

## 12. Appendix: GL → idiomatic wgpu translation

| GL shape | Idiomatic wgpu replacement | Pattern / notes |
|---|---|---|
| `Device` | `WgpuDevice` constructed via `with_external(handles)` | No internal boot in production. |
| `Texture` (data carrier) | `wgpu::Buffer` storage, read-only | gpu_cache, transforms, prim_headers, gpu_buffer_texture_f/i, vertex_data_textures. |
| `Texture` (image) | `wgpu::Texture` + `wgpu::TextureView` | Image cache, glyph atlas, render targets. |
| `ExternalTexture` | embedder-supplied `wgpu::TextureView` | Same wgpu device shared with embedder via P0 handoff. |
| `TextureSlot` | bind-group binding index (u32) | Compile-time slot, not runtime active-texture-unit. |
| `TextureFilter` | `wgpu::FilterMode` in a `wgpu::Sampler` | Sampler cache in `WgpuDevice` lands P2. |
| `TextureFlags` | _deleted_ | GL-specific. |
| `Program` / `ProgramBinary` / `ProgramCache` | `wgpu::RenderPipeline` + `wgpu::PipelineCache` | One pipeline per (family, format, override) key. |
| `ProgramCacheObserver` | _deleted_ | Cache observation is wgpu's. |
| `ShaderError` | wgpu validation error via `Result` | Surface naturally. |
| `VAO` | _deleted_ | Per-pass `set_vertex_buffer`. |
| `VertexAttribute` / `VertexAttributeKind` / `VertexDescriptor` | `wgpu::VertexAttribute` / `VertexFormat` / `VertexBufferLayout` | Per-pipeline. |
| `VertexUsageHint` | _ignored_ | wgpu manages buffer usage at allocation. |
| `UploadMethod` / `UploadPBOPool` | _deleted_ | `Queue::write_texture` is the replacement. |
| `DrawTarget` | `wgpu::TextureView` + `RenderPassTarget` policy | Color load / store declared at pass begin. |
| `ReadTarget` | `wgpu::Texture` with `COPY_SRC` usage | Adapter readback via `read_rgba8_texture`. |
| `FBOId` / `RBOId` / `VBOId` | _deleted_ | wgpu has no GL handles. |
| `DepthFunction` | `wgpu::CompareFunction` | Per-pipeline. |
| `FormatDesc` | `wgpu::TextureFormat` | Per-pipeline / per-texture. |
| `Texel` | `wgpu::TextureFormat` element type | For legit textures only. |
| `GpuFrameId` | unchanged (host-side counter) | Carry through. |
| `GpuProfiler` / `GpuTimer` | `wgpu::QuerySet` | Needs `Features::TIMESTAMP_QUERY`; lands when profiler integrates. |
| `GpuDebugMethod` / `GpuSampler` | _ignored or stubbed_ | |
| `Capabilities` | `wgpu::Features` + `wgpu::Limits` | Declared in `core::REQUIRED_FEATURES`. |
| `dither_matrix_texture` | WGSL `const DITHER` | 64 bytes — too small to be a binding; inline in consuming shaders. |
| `gpu_buffer_texture_f/i` | storage buffer, `read_only: true` | Per-frame structured table. |
| `transforms_texture` | storage buffer | Per-primitive transforms. |
| `prim_header_texture` | storage buffer | Per-primitive metadata. |
| `vertex_data_textures` | storage buffer (or vertex buffer if per-vertex) | Triple-buffer → wgpu submit-boundary semantics. |
| `gpu_cache_texture` | one or more storage buffers | Existing paging carries forward; access path changes. |
| `bind_program` / `bind_texture` / `set_uniform` | _deleted_ | Per-pass `set_pipeline` + `set_bind_group(offset)`; no mutable per-call binding state. |
| `get_gl_target` | _deleted_ | Texture descriptor carries the dimension. |
| `get_unoptimized_shader_source` | _deleted_ | WGSL is authored under `device/wgpu/shaders/`. |
| `clear_target(...)` mutable state | `RenderPassTarget` color / depth load policy | Declared at pass begin. |
| `invalidate_depth_target()` | `StoreOp::Discard` on depth attachment | Declared at pass begin. |
| Y-flip ortho carry | _deleted_ | Surface orientation declared explicitly. |
| CPU-side channel swaps | _deleted_ | wgpu pipeline state declared explicitly. |
| Manual blend tables | _deleted_ | wgpu blend state declared per pipeline. |
| Fixed-function emulation | _deleted_ | wgpu has no fixed-function. |
