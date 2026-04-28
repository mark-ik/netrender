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
