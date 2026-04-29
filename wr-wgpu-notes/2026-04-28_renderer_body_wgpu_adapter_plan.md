# Renderer-Body wgpu-Native Adapter Plan (2026-04-28)

**Status**: Active follow-up to
[2026-04-28 idiomatic-wgsl pipeline plan §S4](2026-04-28_idiomatic_wgsl_pipeline_plan.md).
Spawned at S4-1/5 closure when recon surfaced the integration scope.

**Lane**: Rewrite webrender's renderer body so its boundary with the
GPU is wgpu-native instead of GL-shaped. Per the parent plan §5,
"no GL-shaped trait conformance" — the renderer body adapts to wgpu
at its device boundary.

**Related**:

- Parent plan: [2026-04-28_idiomatic_wgsl_pipeline_plan.md](2026-04-28_idiomatic_wgsl_pipeline_plan.md)
- Scope of change is in: [`webrender/src/renderer/`](../webrender/src/renderer/)
- Existing wgpu module: [`webrender/src/device/wgpu/`](../webrender/src/device/wgpu/)
- Existing GL device (target for deletion in parent plan §S9):
  [`webrender/src/device/gl.rs`](../webrender/src/device/gl.rs)

---

## 1. Intent

The renderer body (~11.6k LOC across `webrender/src/renderer/`) calls
into a GL-shaped `Device` API re-exported by `device/mod.rs`. Today
that re-export points at `gl.rs`; tomorrow it points at `device/wgpu/`,
and the renderer's call sites speak wgpu idioms instead of GL ones.
By the end of this plan, `gl.rs` is unreachable from the renderer
body and ready for deletion in parent §S9.

This is *not* "a wgpu-backed Device that mirrors gl.rs's API". That
shape was the pre-jump-ship architecture; parent plan §5 explicitly
forbids it. Here, the renderer body's call shapes change.

---

## 2. Recon (2026-04-28)

Concrete API surface, measured on `idiomatic-wgpu-pipeline` HEAD:

| Metric | Value |
|---|---|
| `self.device.*` callsites in `webrender/src/renderer/*.rs` | 169 |
| Unique device method names called | 57 |
| Types imported from `device::*` by renderer body | ~25 |
| `webrender/src/renderer/mod.rs` line count (god object) | 5,316 |
| Total lines in `webrender/src/renderer/` | ~11,600 |

Imported types (renderer side, every `use crate::device::*` in
`renderer/`):

- **Mutable Device wrapper**: `Device`
- **Shader/program**: `Program`, `ProgramBinary`, `ProgramCache`,
  `ProgramCacheObserver`, `ShaderError`,
  `get_unoptimized_shader_source`
- **Texture**: `Texture`, `ExternalTexture`, `TextureSlot`,
  `TextureFilter`, `TextureFlags`
- **Vertex / VBO / VAO**: `VAO`, `VertexAttribute`,
  `VertexAttributeKind`, `VertexDescriptor`, `VertexUsageHint`
- **Upload**: `UploadMethod`, `UploadPBOPool`
- **Render targets**: `DrawTarget`, `ReadTarget`, `FBOId`,
  `get_gl_target`
- **Pipeline state**: `DepthFunction`
- **Format / texel**: `FormatDesc`, `Texel`
- **Frame ID**: `GpuFrameId`
- **Query (separate module `device::query`)**: `GpuProfiler`,
  `GpuDebugMethod`, `GpuSampler`, `GpuTimer`

The "bark vs. bite" read: many of these types are simple wrappers
or enums whose wgpu equivalents are existing wgpu types. Specifically
GL-shaped (and so requiring real conceptual work):
`FBOId`, `VAO`, `UploadPBOPool`, `Program`, `ProgramCache`,
`Capabilities`, plus the implicit binding-state model the
`Device` struct carries.

---

## 3. What we are not preserving

- **`FBOId` / `RBOId` / `VBOId`**. wgpu uses `wgpu::TextureView` for
  attachment, `wgpu::Buffer` for vertex data, `wgpu::RenderPass` for
  the framebuffer concept. No GL handles.
- **`VAO` (vertex array object)**. wgpu sets vertex buffers per-pass
  via `RenderPass::set_vertex_buffer`; the VAO concept dissolves.
- **`PBO` (pixel buffer object) and `UploadPBOPool`**. wgpu's
  `Queue::write_texture` is async-by-default and batched at the
  driver level; staging buffers exist when needed but aren't a
  pooled abstraction the renderer manages.
- **`Program`'s GL shape**. Today `Program` wraps a GL shader program
  with uniform-location lookup. The wgpu shape is
  `wgpu::RenderPipeline` + `wgpu::BindGroupLayout` + dynamic-offset
  bindings, which is what `device/wgpu/pipeline.rs` already produces.
- **`ProgramCache` and `ProgramBinary` (binary cache)**. wgpu has
  `wgpu::PipelineCache` (parent §4.11). That replaces the cache
  layer; the on-disk format is wgpu's, not webrender's.
- **Mutable per-call binding state on `Device`**. wgpu pipelines
  bind once per render pass; per-draw differences come from dynamic
  offsets / push constants (parent §4.7). The "bind program → bind
  texture → set uniforms → draw" sequence collapses into "record
  `DrawIntent`s, flush_pass" (parent §4.8).
- **GL `Capabilities`**. wgpu uses `wgpu::Features` and `wgpu::Limits`,
  declared in `device/wgpu/core.rs::REQUIRED_FEATURES` (parent §4.10).
- **Y-flip ortho projection**. wgpu surface orientation is explicit;
  declare it directly (parent §2 ✗ list).
- **`get_gl_target` / `get_unoptimized_shader_source`**. The first
  is a GL-target enum mapper; the second is the legacy authored-GLSL
  source loader. Both gone — WGSL is authored under
  `device/wgpu/shaders/`.

---

## 4. What survives

- **Frame / `RenderTaskGraph` / `BatchBuilder` / picture caching**.
  Parent plan §S4 explicitly says "do not modify `frame_builder` /
  picture caching." Their internal logic stays; only their *output
  consumers* (the things that take their results and emit GPU calls)
  change.
- **Texture format / blend mode / depth function semantics**. The
  enums change shape (wgpu types replace GL types), but the
  rendering-correctness decisions don't.
- **The renderer's overall control flow**: traverse render-task graph,
  group draws by target into passes, render each pass. Same shape;
  per-pass code changes from "GL state machine" to "wgpu pass
  encoder."
- **Shader corpus families** (`brush_solid`, `cs_clip_rectangle`,
  `ps_text_run`, etc., enumerated in parent §S6). Same families;
  authored as WGSL.

---

## 5. Slice plan

Each slice produces a real artifact and is independently reviewable.

### A0 — Type-by-type translation table

**Done condition**: appendix to this plan listing every imported
device-side type with its wgpu-native replacement (or "deleted;
replaced by pattern X"). One row per type. Lives in §11 below.

This is recon-only — no code changes. Catches design questions
before code lands.

### A1 — wgpu-native `Device` adapter struct

**Done condition** (✅ landed 2026-04-28):

- [x] [`webrender/src/device/wgpu/adapter.rs`](../webrender/src/device/wgpu/adapter.rs)
  defines `WgpuDevice`, composing `core::Device` plus a lazy
  pipeline cache keyed by `wgpu::TextureFormat`. Cache pattern is
  `Mutex<HashMap<Key, Family>>::entry().or_insert_with()` —
  returns clones (wgpu 29 handle types are `Clone`, Arc-wrapped
  internally). This is the model A2..A7 replicate for every other
  cache (bind-group layouts, samplers, vertex layouts, etc.).
- [~] **Method surface kicked off** with `WgpuDevice::ensure_brush_solid(format)`.
  Broader rendering verbs (`encode_pass`, `create_texture`,
  `ensure_<other_family>`, `upload_texture`, …) added by A2..A7
  as each path migrates.
- [x] **Does not mimic `gl.rs::Device` API.** No `bind_program`,
  no `set_uniform`, no per-call binding-state mutations. The
  receiver is `&self`; per-pass state lives inside `pass.rs`'s
  `flush_pass`.
- [x] Smoke test `device::wgpu::tests::wgpu_device_a1_smoke`
  boots the device and exercises lazy build for two formats.

**Sequenced fix during A1**:

- `wgpu::RenderPipeline` in wgpu 29 has no `global_id()` method
  (used in older wgpu for handle-equality assertions). Adapter
  smoke test relies on `cargo test` non-panicking + no compile
  errors for cache verification rather than handle equality;
  `HashMap::entry().or_insert_with()` is a `std` invariant we
  don't need to retest.

### A2 — Texture path migration

**Done condition**: every renderer callsite that creates / binds /
samples a texture goes through `WgpuDevice` instead of `device::Texture`.
`device::Texture`, `TextureSlot`, `TextureFilter`, `TextureFlags`,
`ExternalTexture`, `Texel`, `FormatDesc` callsites all updated.
`cargo check -p webrender` green; no `gl.rs::Texture` reachable from
renderer/.

**Status (2026-04-28)**:

- [x] **A2.0 Design seed.**
  [`device/wgpu/texture.rs`](../webrender/src/device/wgpu/texture.rs)
  defines `WgpuTexture` (wraps `wgpu::Texture` + format + dimensions)
  and `TextureDesc`. `WgpuDevice::create_texture(&TextureDesc) ->
  WgpuTexture` in [`adapter.rs`](../webrender/src/device/wgpu/adapter.rs).
  Smoke test `wgpu_device_a2_create_texture_smoke` boots the device,
  creates a 16×16 RGBA8 texture, produces a default view. **No
  `renderer/*` callsites touched yet** — that's the per-sub-slice
  work below.
- [x] **A2.1.0 dither texture API prep.** Reading the actual
  dither sites surfaced an architectural dependency: the field
  type `Option<Texture>` (mod.rs:824) is bound at four sites via
  `self.device.bind_texture(slot, &Texture, swizzle)`. wgpu has no
  implicit-bind state machine; bindings live on `BindGroup` at
  pass-encoding time, so migrating the field type to `WgpuTexture`
  cannot work in isolation — it requires the bind sites to be
  pass-encoding-shaped (i.e. A2.X foundational pass encoding) to
  exist first. **A2.1 full lifecycle migration is gated on
  A2.X closure.** What landed instead this sub-slice:
  [`format.rs`](../webrender/src/device/wgpu/format.rs)
  defines `image_format_to_wgpu` / `image_format_bytes_per_pixel`
  / `format_bytes_per_pixel_wgpu` for the `ImageFormat` variants
  the renderer body actually uses (R8, R16, RG8, RGBA8, BGRA8,
  RGBAF32). `WgpuDevice::upload_texture(&WgpuTexture, &[u8])`
  added in `adapter.rs` (wraps `Queue::write_texture`). Smoke
  test `wgpu_device_a21_dither_create_upload_smoke` exercises an
  8×8 R8 dither-shaped texture create + upload + flush, mirroring
  what `init.rs:484` does today via the GL device.
- [~] **A2.X — foundational pass encoding (was A2.4).**
  Foundational for every other texture-lifecycle migration.
  - [x] **A2.X.0 design seed (2026-04-28).**
    [`pass.rs`](../webrender/src/device/wgpu/pass.rs) refactored:
    `DrawIntent` now carries `pipeline` and `bind_group` references
    by value (wgpu 29 handle types are `Clone`, Arc-wrapped
    internally — per-draw cloning is cheap, multi-pipeline passes
    work via per-draw `pipeline` switching). `flush_pass` drops
    its top-level pipeline / bind-group args; colour load policy
    moved into the pass-target descriptor in A2.X.1 so
    composite-onto-existing passes (`LoadOp::Load`) are first-class
    rather than needing a sentinel. `render_rect_smoke` updated to
    the new shape; all 6 wgpu tests green.
  - [x] **A2.X.1 pass-target groundwork (2026-04-28).**
    [`pass.rs`](../webrender/src/device/wgpu/pass.rs) now owns a
    wgpu-native `RenderPassTarget` / `ColorAttachment` descriptor:
    colour load policy is declared at pass begin (`LoadOp::Clear` /
    `LoadOp::Load`) rather than modeled as mutable GL-style device
    state. `oracle_blank_smoke` moved off its hand-written
    `begin_render_pass` block and now uses `pass::flush_pass` with
    an empty draw list, so the blank oracle receipt exercises the
    same pass abstraction future renderer paths will use. Focused
    receipt: `cargo test --manifest-path webrender/Cargo.toml
    device::wgpu` — 6 passed.
  - [x] **A2.X.2 depth-target groundwork (2026-04-28).**
    `RenderPassTarget` now carries optional `DepthAttachment`
    policy alongside colour. This gives the renderer's
    `clear_target(..., Some(depth), ...)` and
    `invalidate_depth_target()` call shapes a wgpu-native landing
    spot: depth load/store behavior is declared on
    `RenderPassDescriptor` (`LoadOp::{Clear, Load}` plus
    `StoreOp::{Store, Discard}`), not modeled as mutable GL
    framebuffer state. Receipt: `pass_target_depth_smoke` clears a
    colour/depth target through `WgpuDevice::encode_pass` with
    `DepthAttachment::clear(...).discard()`. Focused receipt:
    `cargo test --manifest-path webrender/Cargo.toml device::wgpu`
    — 7 passed.
  - [x] **A2.X.3 adapter pass-encoding bridge (2026-04-28).**
    `WgpuDevice::encode_pass(&mut CommandEncoder,
    RenderPassTarget, &[DrawIntent])` is now the renderer-facing
    pass replay surface. The smoke/oracle pass tests route through
    the adapter instead of calling `pass::flush_pass` directly, so
    future renderer callsites target `WgpuDevice` while `pass.rs`
    remains the focused implementation module.
  - [x] **A2.X.4 command encoder lifecycle bridge (2026-04-29).**
    [`frame.rs`](../webrender/src/device/wgpu/frame.rs) now owns
    `create_encoder` / `submit`, and `WgpuDevice` exposes them as
    `create_encoder(label)` / `submit(encoder)`. The pass smoke and
    oracle receipts acquire and submit encoders through the adapter,
    so upcoming renderer callsites no longer need to reach through
    `core.device` / `core.queue` for the frame command lifecycle.
  - [ ] **A2.X.5+ per-callsite migration**: renderer's per-pass
    code paths shift from "GL state machine" (`bind_draw_target`,
    `clear_target`, `invalidate_depth_target`, plus per-draw
    `bind_texture`) to "open `wgpu::RenderPass`, replay
    `DrawIntent`s, close pass." Sites: `mod.rs:1507, 1983, 2332,
    2844, 2909, 3182, 3222, 3234, 3338, 3674, …`. Each sub-slice
    migrates one per-pass code path; the renderer's traversal
    accumulates `DrawIntent`s into a per-pass bucket and calls
    `WgpuDevice::encode_pass` to flip them into wgpu calls.
    Multi-turn.
- [ ] **A2.1 — dither texture lifecycle** (full): now gated on
  A2.X. Sites: `init.rs:484` (create + upload),
  `mod.rs:824` (field type), `mod.rs:2178/3501/3528/3555` (bind,
  rewritten to BindGroup setup once pass encoding is wgpu-native),
  `mod.rs:4640` (delete → drop).
- [ ] **A2.2 — zoom-debug texture lifecycle**: gated on A2.X for
  the same reason as A2.1.
- [~] **A2.3 — read-pixels path**:
  - [x] **A2.3.0 readback adapter prep (2026-04-29).**
    [`readback.rs`](../webrender/src/device/wgpu/readback.rs) now
    owns the RGBA8 texture readback staging path, and
    `WgpuDevice::read_rgba8_texture(&Texture, width, height)`
    exposes it at the adapter boundary. The pass/oracle receipts use
    the adapter helper instead of carrying a private test-only
    `copy_texture_to_buffer` implementation.
  - [ ] **A2.3.1 renderer read-pixels callsites**:
  `mod.rs:1262/4614/4619`. The `tests::readback_target` helper is
  now promoted; the remaining work is replacing the GL-shaped
  `bind_read_target_impl` / `read_pixels*` sequence with a texture
  handle + adapter readback call. `copy_texture_to_buffer` itself is
  unblocked, but the renderer callsites still sit behind read-target
  binding state, so migrate them as a separate sub-slice.
- [ ] **A2.5 — blit_render_target**: wgpu has no direct blit;
  same-format / same-size cases use
  `CommandEncoder::copy_texture_to_texture`, others need a
  render-pass helper. Sites: `mod.rs:2321/2635/2814/2946/
  4362/4374`. Gated on A2.X.
- [ ] **A2.6 — misc**: `max_texture`, `attach_read_texture`,
  `use_batched_texture`, `delete_external_texture`,
  `delete_fbo` (no-op in wgpu), `begin_frame` / `end_frame`.
- [ ] **A2 close**: confirm no `device::Texture` /
  `device::ExternalTexture` / `TextureSlot` / `TextureFilter` /
  `TextureFlags` / `Texel` / `FormatDesc` imports remain in
  `renderer/`. Rolls into A8.

**Sequenced finding during A2.1 prep**: the original ordering
(smallest-contained-first → biggest-last) inverted the actual
dependency graph. The "smallest" texture-lifecycle migrations all
depend on pass-encoding being wgpu-native because of how
`bind_texture` and per-pass `bind_draw_target` interact. Revised
order puts A2.X (pass encoding) first as foundational; per-texture
lifecycles drop in afterward. The texture-creation + upload + format
API surface (A2.0 + A2.1.0) can ship ahead because it's
self-contained — the renderer body just doesn't call it yet.

#### A2 recon (2026-04-28, at A2.0 closure)

Renderer-side texture coupling:

| Surface | Methods | Notes |
|---|---|---|
| Texture lifecycle | `create_texture`, `delete_texture`, `bind_texture`, `use_batched_texture` | `bind_texture` has no wgpu equivalent — bindings live on `BindGroup` at pass-encoding time |
| Render-target / FBO | `bind_draw_target`, `bind_read_target_impl`, `attach_read_texture`, `clear_target`, `blit_render_target`, `invalidate_depth_target`, `delete_fbo` | wgpu has no FBO concept; views are passed to `BeginRenderPass`. `invalidate_depth_target` → `StoreOp::Discard` |
| Frame lifecycle | `begin_frame`, `end_frame` | Maps to `wgpu::CommandEncoder` lifecycle in `device/wgpu/frame.rs` |
| Pixel readback | `read_pixels`, `read_pixels_into` | Already prototyped in `tests::readback_target` |
| Query | `max_texture` | `wgpu::Limits::max_texture_dimension_2d` |
| External | `delete_external_texture` | Embedder hands us a `wgpu::TextureView` (per servo-wgpu pattern); no separate delete needed |

No file in `renderer/` is both small AND self-contained:
`external_image.rs` (4 KB, smallest) goes through the cross-repo
`ExternalImageHandler` API in `webrender_api`. `debug.rs` (14 KB,
next smallest) is a full mini-renderer touching `Program`, `VAO`,
`Texture`, `TextureSlot` together. Migration therefore proceeds
per-method-per-callsite within sub-slices, not per-file.

### A3 — Vertex / buffer path migration

**Done condition**: renderer callsites that create / bind VAOs /
VBOs / buffers go through `WgpuDevice` instead of `device::VAO` /
`VBO` / `Stream`. `VertexAttribute`, `VertexDescriptor`,
`VertexUsageHint` callsites updated. `cargo check` green.

### A4 — Shader / pipeline path migration

**Done condition**: renderer callsites for `Program` /
`ProgramCache` / `bind_program` / uniform setting all go through
`WgpuDevice::ensure_pipeline` plus the dynamic-offset / push-
constant uniform tiers (parent §4.7). `Program`, `ProgramBinary`,
`ProgramCache`, `ProgramCacheObserver` no longer imported by
renderer/. `cargo check` green.

### A5 — Render-target / FBO migration

**Done condition**: `DrawTarget`, `ReadTarget`, `FBOId` callsites
go through `WgpuDevice` and produce `wgpu::TextureView`s for
attachment. The renderer's per-pass loop opens one
`wgpu::RenderPass` per target switch (parent §4.8). `cargo check`
green.

### A6 — Upload path migration

**Done condition**: `UploadMethod` / `UploadPBOPool` callsites go
through `WgpuDevice::upload_texture` (one function, encapsulating
`wgpu::Queue::write_texture`'s async behaviour). PBO pooling
deleted. `cargo check` green.

### A7 — Query / profiler migration

**Done condition**: `device::query::{GpuProfiler, GpuTimer,
GpuSampler, GpuDebugMethod}` either route through
`wgpu::QuerySet` (timestamp queries — needs
`Features::TIMESTAMP_QUERY` in parent §4.10) or get stubbed if not
needed for our test-driven workflow. `cargo check` green.

### A8 — Re-export flip + final cleanup

**Done condition**: `webrender/src/device/mod.rs` switches from
`pub use self::gl::*;` to `pub use self::wgpu::*;` (or equivalent —
maybe rename our wgpu module first to disambiguate). Compiler
errors point at remaining residual usages of GL-shaped types;
clean those up. `cargo check -p webrender` and
`cargo test -p webrender device::wgpu` both green. Remaining
oracle scenes (parent §S4) start passing as they exercise the
adapter; that's the receipt for parent §S4 closure too.

---

## 6. Sequencing

Slices have these hard dependencies:

- A0 → A1 (need the translation table before designing the adapter)
- A1 → A2..A7 (need the adapter struct before migrating each path)
- A2..A7 are mostly independent; suggested order matches code
  density (texture is the broadest)
- A8 needs A2..A7 done

Suggested order: A0 → A1 → A2 → A3 → A4 → A5 → A6 → A7 → A8.

Slices may produce a runnable binary at A4-A5 if the renderer body
gets far enough to issue draws. The parent plan's S4 oracle scenes
(`rotated_line` etc.) start matching as the corresponding paths land.

---

## 7. Receipts

- **A0**: translation table in §11.
- **A1**: `WgpuDevice` builds via `core::boot`; covered by a smoke
  test in the existing `device::wgpu::tests` module.
- **A2–A7**: per slice, `cargo check -p webrender` green and the
  imports they migrate are no longer in renderer/'s `use`
  statements.
- **A8**: `cargo test -p webrender device::wgpu` green;
  `cargo check -p webrender` green; the remaining four oracle
  scenes from parent §S4 pass within tolerance.

---

## 8. Risks

- **Renderer body has implicit ordering / state assumptions** that
  the GL Device API quietly satisfies. *Mitigation*: A0 surfaces
  these in the translation table; A1 designs the adapter to
  preserve necessary ordering invariants explicitly.
- **`renderer/mod.rs` is a 5,316-LOC god object**. Modifying it
  surface-by-surface is fine; rewriting it isn't this plan's job
  (decomposition is parent §S6 / future). *Mitigation*: keep edits
  surgical — change only the lines that touch device/.
- **Some types may have no clean wgpu equivalent** (e.g. `ExternalTexture`
  for compositor handoff). *Mitigation*: when one surfaces, document
  it in the translation table with the chosen pattern; if no good
  pattern exists, raise as an open question.
- **wgpu's lack of mutable per-call binding state** changes the
  rendering loop's shape. *Mitigation*: parent §4.8's
  record-then-flush pattern is the answer; A4 / A5 have to make
  every per-draw mutation a `DrawIntent` field instead of a
  device-state mutation.
- **Build can break for long stretches** while migrating. *Mitigation*:
  each slice's done condition is `cargo check` green. If a slice
  is too big to finish in one pass, sub-slice further rather than
  letting the build sit broken.

---

## 9. Open questions

1. **Naming**. Today the wgpu device module is at
   `webrender/src/device/wgpu/`. The local module name `wgpu` shadows
   the extern crate `wgpu` in path-resolution edge cases. When A1
   introduces `WgpuDevice`, do we rename the module to `wgpu_dev` /
   `gpu` / something else, or live with the (so-far-painless) shadowing?
2. **External image / compositor handoff** (`ExternalTexture`). Today
   webrender accepts external GL textures from embedders. The wgpu
   equivalent is "embedder hands us a `wgpu::TextureView`" — but
   that requires the embedder to share a wgpu device. Already a
   known concern via servo-wgpu's `WgpuRenderingContext`; resolve
   in A2 with reference to that pattern.
3. **`ProgramCache` disk format**. The current cache writes a
   webrender-specific binary blob. wgpu's `PipelineCache` is the
   replacement (parent §4.11). Decide in A4 whether to shim the old
   cache surface or remove the cache plumbing entirely from the
   renderer's public API.
4. **`Capabilities`**. The renderer reads adapter capabilities to
   gate optional rendering paths. wgpu's `Features` / `Limits` carry
   the same info but with different shapes. A1 decides the
   translation pattern.
5. **Test strategy during migration**. Per-slice `cargo check` is
   the build gate, but full rendering correctness is parent §S4's
   oracle harness. We'll be in a state where the tree builds but
   renders nothing for some slices. *Document* this honestly in
   each slice's commit message; don't claim "renders" when only
   "compiles."
6. **Servo integration during migration**. servo-wgpu currently
   patches `webrender = { path = "../webrender-wgpu/webrender" }`.
   While the renderer body is mid-migration, servo-wgpu may break.
   Coordinate with the servo-wgpu side; consider tagging a
   pre-migration commit on `idiomatic-wgpu-pipeline` for them to
   pin until the migration lands.

---

## 10. Bottom line

169 callsites, 57 methods, ~11.6k LOC. The bark is loud, but each
slice is bounded — most are mechanical translations once A0's
translation table is in hand. A1's adapter struct is the design
fulcrum; A2–A7 are surface-area migrations that benefit from
parallel work if multiple hands are on it. A8 flips the re-export
and turns parent §S4 green.

Start with A0. The rest follows the table.

---

## 11. Appendix: A0 translation table

_(Populated as A0 lands. Each row: imported type → wgpu-native
replacement → pattern note.)_

| GL-shaped type | wgpu-native replacement | Pattern |
|---|---|---|
| `Device` | `WgpuDevice` (new) | Record-and-flush; no mutable per-call binding state |
| `Texture` | wraps `wgpu::Texture` + `wgpu::TextureView` | Owned by `device/wgpu/texture.rs` cache |
| `ExternalTexture` | embedder-supplied `wgpu::TextureView` | Per servo-wgpu's `WgpuRenderingContext` pattern; revisit in A2 |
| `TextureSlot` | `u32` (binding index) | A bind-group slot, not a runtime "active texture unit" |
| `TextureFilter` | `wgpu::FilterMode` + `wgpu::AddressMode` | Stored in `wgpu::Sampler` |
| `TextureFlags` | TBD | Most flags are GL-specific; A2 decides |
| `Program` | `(wgpu::RenderPipeline, BindGroupLayouts)` | Per `device/wgpu/pipeline.rs` |
| `ProgramBinary` | `wgpu::PipelineCache` blob | A4 |
| `ProgramCache` | `device/wgpu/pipeline.rs` cache + `wgpu::PipelineCache` | A4 |
| `ProgramCacheObserver` | TBD | A4 — likely deleted; cache observation is wgpu's |
| `ShaderError` | wgpu validation error | A4 — propagate via `Result` |
| `VAO` | _deleted_ | wgpu sets vertex buffers per pass via `RenderPass::set_vertex_buffer` |
| `VertexAttribute` | `wgpu::VertexAttribute` | A3 |
| `VertexAttributeKind` | `wgpu::VertexFormat` | A3 |
| `VertexDescriptor` | `wgpu::VertexBufferLayout` | A3 |
| `VertexUsageHint` | _ignored_ | wgpu manages buffer usage at allocation; no per-frame hint |
| `UploadMethod` | _deleted_ | wgpu's `Queue::write_texture` is async-by-default and batched |
| `UploadPBOPool` | _deleted_ | A6 |
| `DrawTarget` | `wgpu::TextureView` + clear/load policy | A5 |
| `ReadTarget` | `wgpu::Texture` + COPY_SRC usage | A5 |
| `FBOId` | _deleted_ | wgpu has no framebuffer object handles; views are passed to `BeginRenderPass` |
| `DepthFunction` | `wgpu::CompareFunction` | A4 / A5 |
| `FormatDesc` | `wgpu::TextureFormat` | A2 / A4 |
| `Texel` | `wgpu::TextureFormat` element type | A2 |
| `GpuFrameId` | unchanged (host-side counter) | Carry through |
| `GpuProfiler` | wraps `wgpu::QuerySet` | A7; needs `Features::TIMESTAMP_QUERY` |
| `GpuDebugMethod` | _ignored or stubbed_ | A7 |
| `GpuSampler` | _stubbed_ | A7 |
| `GpuTimer` | wraps `wgpu::QuerySet` | A7 |
| `Capabilities` | `wgpu::Features` + `wgpu::Limits` | A1 |
| `get_gl_target` | _deleted_ | A2 — wgpu textures carry their target via descriptor |
| `get_unoptimized_shader_source` | _deleted_ | A4 — WGSL authoring replaces this |
