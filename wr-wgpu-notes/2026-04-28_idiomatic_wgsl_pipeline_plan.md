# Idiomatic WGSL Pipeline Plan (2026-04-28)

**Status**: Active — supersedes the
[2026-04-27 dual-servo parity plan](2026-04-27_dual_servo_parity_plan.md),
the [2026-04-18 upstream cherry-pick plan](2026-04-18_upstream_cherry_pick_plan.md),
the [2026-04-22 cherry-pick reevaluation](2026-04-22_upstream_cherry_pick_reevaluation.md),
the [2026-04-18 SPIR-V shader pipeline plan](2026-04-18_spirv_shader_pipeline_plan.md),
the [2026-04-21 SPIR-V pipeline reset execution](2026-04-21_spirv_pipeline_reset_execution.md),
and the [2026-04-26 track-3 legacy assembly isolation lane](2026-04-26_track3_legacy_assembly_isolation_lane.md).

**Lane**: Jump ship from `spirv-shader-pipeline` to a clean wgpu-native
fork of `upstream/upstream`. Authored WGSL only. No GL backend. No
SPIR-V intermediate. No artifact pipeline. No GL parity tests.

**Related**:

- [PROGRESS.md](PROGRESS.md) — branch state and milestone receipts
- existing wgpu device, *as reference only*:
  `webrender/src/device/wgpu_device.rs` on `spirv-shader-pipeline`
- WebRender Wrench reftest format — inherited from `upstream/upstream`
- WebGPU CTS — `gpuweb/cts`

---

## 1. Intent

The `spirv-shader-pipeline` branch and its dual-servo parity story are
treated as broken. We do not freeze them at "shippable." We do not
preserve their semantics. We do not write parity tests against them.
The SPIR-V → naga → multi-target derivation pipeline existed to justify
serving two consumers (upstream `servo/servo` GL + `servo-wgpu`); we
are no longer doing that.

`webrender-wgpu` is built as a wgpu-only fork of `upstream/upstream`
(the literal branch on `servo/webrender` — the Mozilla gecko-dev
gfx/wr mirror, ~263 commits ahead of where the current fork started).
Authored WGSL only. The renderer body is inherited from
`upstream/upstream` — that is the asset. Everything below the renderer
body — the device layer, the shader-authoring pipeline, the test
harness, the cargo feature surface — gets rebuilt to wgpu idioms from
line one.

**Upstream Servo is no longer a target.** It stays on its own
WebRender 0.68 forever, or migrates to wgpu on its own schedule. The
dual-servo concern was the entire reason for GL preservation; once it
goes, GL goes with it.

---

## 2. What we are not preserving

- **`webrender/src/device/wgpu_device.rs`** (8094 LOC). Mark's
  god-object rule — *"no struct exceeds ~600 LOC or owns more than
  ~6 distinct responsibilities"* per the
  [iced jump-ship plan §5](../../graphshell/design_docs/graphshell_docs/implementation_strategy/shell/2026-04-28_iced_jump_ship_plan.md)
  — rules it out as a port target. The 2150-LOC impl block on
  `WgpuDevice` violates that by ~4×. Decisions inside the file
  (format mappings, bind-group layouts, blend/depth state assembly)
  are reference inputs, not code to drop in.
- **The SPIR-V → naga → multi-target derivation pipeline.** With one
  target the intermediate is overhead. WGSL is authored directly and
  fed to `wgpu::Device::create_shader_module` via `include_str!`.
- **`webrender/res/*.glsl`** authored shader source tree. Replaced by
  authored WGSL.
- **`webrender_build::shader_runtime_contract::*` and the artifact
  registry.** All `validate_artifact_*` / `validate_runtime_contract_*`
  machinery in the existing wgpu_device.rs exists to check pipeline
  output against runtime expectations. Without the pipeline, the
  validators are dead.
- **`gl_backend` feature, `gleam` dep, `webrender/src/device/gl.rs`,
  `webrender/src/device/query_gl.rs`.** Single backend = no flag, no
  GL device.
- **`swgl/` software renderer + `glsl-to-cxx/`.** Firefox's software
  fallback path; not on the wgpu road.
- **`reftests/spirv-parity/`.** Replaced by wgpu-native reftests
  against a frozen reference oracle. The 33-test suite stops being a
  signal once the branch is replaced; it was never the bar.
- **All cherry-pick batches** enumerated in the
  [2026-04-18 plan](2026-04-18_upstream_cherry_pick_plan.md) and
  [2026-04-22 reevaluation](2026-04-22_upstream_cherry_pick_reevaluation.md).
  Inherited as part of branching from `upstream/upstream`. The fixes
  they were trying to land are already there.
- **`super::GpuDevice` trait** (the GL-shaped device contract on the
  current branch). The new wgpu device does not implement it; the
  renderer body adapts to wgpu idioms at the device boundary.
- **The 2026-04-22 §"WGPU picture-cache opaque depth" fix and similar
  workarounds, *as patches*.** The insight (e.g.
  `WgpuDepthState::WriteAndTest` for picture-cache opaque batches) is
  carried forward as designed-correct from line one, not as a fix on
  existing code.
- **CPU-side channel swaps, manual blend tables, Y-flip ortho carry,
  fixed-function emulation.** Anti-patterns called out in
  [dual-servo plan §2.2](2026-04-27_dual_servo_parity_plan.md). In a
  wgpu-only world they just stop existing.
- **Compile-matrix testing.** No GL-only / wgpu-only / both-on
  configurations to track. One configuration; one tree.

---

## 3. What survives — the inputs and references

These are the assets that make this jump-ship cheap.

| Asset | Role | State |
|---|---|---|
| `upstream/upstream` on `servo/webrender` | Mozilla gecko-dev gfx/wr mirror; canonical WebRender shape | Stable (last sync 2026-04-08); used as the new branch base |
| WebRender renderer body (frame builder, batch builder, picture cache, render task graph) | Architectural shape inherited via the branch | Inherited as-is; not rebuilt |
| `webrender_api/` types (display list, frame, scene) | Public API consumed by Servo | Inherited as-is |
| `wgpu_device.rs` decisions on `spirv-shader-pipeline` | Reference for "which wgpu calls work, what blend/depth/format mappings WebRender needs" | Reference document — not ported |
| `WgpuShaderVariant::ALL` enumeration (~50 shader programs) | Catalog of which shader programs WebRender needs | Names/families stable; WGSL bodies authored fresh |
| Servo presenting smoke pattern from `servo-wgpu/` | End-to-end integration shape | Reusable when S7 lands |
| 2026-04-22 §"WGPU picture-cache opaque depth" insight | Locally-discovered correctness — picture-cache opaque batches need WriteAndTest, not AlwaysPass | Carried forward into new code, not as a patch |

The `upstream/upstream` base inherits the post-0.68 work the 2026-04-22
reevaluation enumerated as conditional cherry-picks — render-task-graph
fixes, dirty-rect clipping, PBO fallback, gradient fixes, snapping
correctness, and quad-path enablement are all already there. The
cherry-pick batches in the 2026-04-18 plan stop being a backlog and
become history.

---

## 4. Quality bar

Anchored to receipts, not to "as capable as the previous GL backend."

### 4.1 Pixel correctness — frozen reference oracle

A one-time tool (lives in a side branch or `tools/oracle-capture/`,
runs on demand, never gates the main build) builds `upstream/upstream`
with GL, runs Wrench against a chosen scene set, and freezes the
output PNGs as test fixtures. After that, GL is never built or run on
`wgpu-native`.

The oracle is the visual ground truth for the rest of the plan. wgpu
output is pixel-diffed against frozen oracle PNGs. New scenes get
added to the oracle on demand; the oracle scene set grows with the
test set, not ahead of it.

### 4.2 API correctness — WebGPU CTS

A subset of `gpuweb/cts` runs as a CI gate against the new wgpu
device. Subset chosen to exercise the surface webrender uses: texture
creation/upload, render passes, bind groups, blend states,
depth/stencil, vertex layouts. Compute and storage textures are
deferred unless and until webrender starts using them.

### 4.3 Code structure — no god objects

Per the iced jump-ship plan §5: no struct over ~600 LOC or owning more
than ~6 distinct responsibilities lands without refactor. The new wgpu
device is decomposed from line one — separate caches for
textures/samplers, pipelines, bind groups; separate frame encoder;
separate format/conversion utilities. `WgpuDevice` does not return as
a 2150-LOC impl block.

### 4.4 Dependency currency

`wgpu`, `naga` (insofar as it's a wgpu transitive dep), `euclid`, the
wgpu-types stack, and Servo integration deps are current at branch
time. A dep audit is recurring, not one-time — the explicit purpose of
the SPIR-V pipeline was to manage wgpu-version churn; that benefit is
preserved in spirit by keeping the dep graph audited rather than by
maintaining a translation pipeline.

### 4.5 No dual-authority

There is one device. There is one shader source language (WGSL). There
is one backend feature. There is no `gl_backend`, no compile matrix,
no parity gate, no dual-write glue, no sync layer. Every change goes
through one path.

### 4.6 Storage buffers, not data textures

WebRender today uses 2D textures (`gpu_cache_texture`,
`transforms_texture`, `prim_header_texture`) as carriers for
structured GPU-side data — a workaround for GL pre-4.3 not having
SSBOs broadly. Modern wgpu has `BufferBindingType::Storage {
read_only: true }` on every backend we target. The wgpu-native shape
uses storage buffers for shared tables: direct random access, no
`texelFetch` 2D-coord arithmetic, no RGBA32F packing of non-color
data.

Caveats:

- Respect `max_storage_buffer_binding_size` (typically 128MB portable;
  up to 2GB on some desktop wgpu/Vulkan but not portably).
- Respect `max_storage_buffers_per_shader_stage` (commonly 8–16;
  budget shader bindings accordingly).
- WebRender's existing `gpu_cache` paging logic carries forward for
  tables that grow past binding-size limits; only the access path
  changes.

### 4.7 Uniform hierarchy

Per-draw and per-frame data uses a four-tier hierarchy chosen by size
and update cadence:

| Tier | Size | Cadence | Use |
|---|---|---|---|
| Push constants | ≤128B (Vulkan), ≤256B (Metal) | per-draw | flags, indices, tile coords |
| Dynamic uniform buffer | aligned to `min_uniform_buffer_offset_alignment` (typically 256B), up to `max_uniform_buffer_binding_size` (~64KB) | per-draw | per-primitive transforms, material refs |
| Storage buffer | large, indexed | per-frame, indexed per draw | shared tables (gpu_cache, transforms, prim instance arrays) |
| Static uniform buffer | small | per-pass / per-frame | viewport, time, global constants |

Bind-group layouts are created once per pipeline. Bind groups are
created with `has_dynamic_offset: true` so a single
`set_bind_group(offset)` per draw selects the per-draw region without
rebinding the group.

### 4.8 Record draws, never execute inline

The device API surface has no `draw()`. Display-list traversal
records `DrawIntent` values — pipeline, bind groups, dynamic offsets,
vertex range — into per-pass buckets. `pass.rs` flushes a bucket as
a single `wgpu::RenderPass`, sorting intents by pipeline within the
pass to minimize state changes. One `BeginRenderPass` per target
switch.

This reflects the Vulkan/Metal/DX12 cost model: render-pass
boundaries are expensive, pipeline switches inside a pass are cheap,
draw calls inside a pipeline group are very cheap. WebRender already
has a pass structure in `RenderTaskGraph` — `pass.rs`'s job is to
consume that structure correctly into wgpu render-pass lifetimes,
not to invent batching.

### 4.9 Pipeline specialization via WGSL overrides

WGSL `override` declarations are pipeline-time constants. Many of
the ~50 variants in `WgpuShaderVariant::ALL` are GLSL `#define`
permutations (alpha vs. opaque, fast-path vs. full, dual-source vs.
single). With overrides, those collapse to **one WGSL source plus N
specialized pipelines** via `wgpu::PipelineLayoutDescriptor` constant
overrides. Keeps the shader corpus DRY, isolates true family
differences (brush vs. text vs. clip vs. composite) from per-variant
parameter differences, and reduces the WGSL authoring burden in S6.

### 4.10 Required wgpu features

The device requires a documented `wgpu::Features` set at adapter
selection. Adapters without the required set are rejected at boot.

Core required:

- `PUSH_CONSTANTS` (per §4.7)
- `DUAL_SOURCE_BLENDING` (subpixel AA in `PsTextRunDualSource` family)

Optional / capability-dependent:

- `INDIRECT_FIRST_INSTANCE` — if indirect drawing is introduced later
- `PIPELINE_STATISTICS_QUERY` — debug only

The required set is declared in `core.rs::REQUIRED_FEATURES` so
adapter selection is explicit and reviewable. Debug labels on
buffers/textures/pipelines/passes are populated by default (free
RenderDoc/Xcode/PIX value).

### 4.11 Async pipeline compilation + on-disk pipeline cache

Pipeline creation is expensive (hundreds of ms for complex shaders).
S6's ~50-shader corpus, even after override-based collapse, is enough
that synchronous compile-at-boot would be a visible startup hit.

- All pipelines are compiled async at boot from `pipeline.rs`'s cache.
- `wgpu::PipelineCache` (disk-backed serialized pipelines) is enabled
  so second-run boots reuse compiled artifacts.
- Until the cache warms, the app shows a "warming" state rather than
  blocking the main thread. Visible only on first run.

---

## 5. Anti-patterns to avoid

- **No god objects.** No struct over ~600 LOC or more than ~6
  responsibilities. `WgpuDevice` does not come back. If a struct grows
  past the bar, refactor before the slice lands.
- **No GL-shaped trait conformance.** The renderer body adapts to wgpu
  at its device boundary; we do not preserve a `GpuDevice` trait
  shaped around GL state.
- **No artifact pipeline.** WGSL files are authored.
  `wgpu::Device::create_shader_module` consumes them via
  `include_str!`. No build-time SPIR-V → naga → WGSL derivation. No
  runtime contract validators.
- **No GL parity tests.** Reference is the frozen oracle. Parity
  comparison with the spirv-shader-pipeline branch's GL output is not
  a goal; that branch is dead state.
- **No GL-emulation residue in the wgpu path.** No CPU-side channel
  swaps. No Y-flip ortho carry. No manual blend tables. No
  fixed-function emulation. wgpu pipeline state is declared
  explicitly.
- **No "wgpu-shaped GL."** When wgpu has a native idiom different from
  GL — explicit pipeline state objects, push constants, storage
  textures, compute, async submission — wgpu uses it. Goal is a wgpu
  backend that is *better* than the old GL path where wgpu makes that
  possible, not merely equivalent.
- **No data textures for structured data.** Per §4.6: shared tables
  (gpu_cache, transforms, prim instance arrays) live in storage
  buffers, not 2D textures. Carrying `texelFetch` access patterns
  forward is the GL-era assumption we are explicitly removing.
- **No per-draw bind-group creation.** Bind groups are created at
  pipeline-binding setup with `has_dynamic_offset: true`;
  `set_bind_group(offset)` selects per-draw data. Creating fresh bind
  groups per draw is the cost we're eliminating.
- **No inline `device.draw()`.** Per §4.8: display-list traversal
  records `DrawIntent`s; `pass.rs` flushes per pass. There is no API
  surface for "execute this draw now."
- **No GLSL-style `#define` permutation explosion.** Per §4.9: where
  variants differ only by parameter (alpha-vs-opaque,
  fast-path-vs-full, dual-source toggle), use WGSL `override`
  specialization. New shader source only when the family genuinely
  differs.
- **No new code on `spirv-shader-pipeline` or its descendants.** That
  branch is frozen at S0. Bug fixes only if absolutely required for
  an in-flight servo-wgpu user; never feature additions.

---

## 6. Slice plan

Each slice is independently shippable and produces a real artifact.

### S0 — Branch and freeze

**Done condition**: branch off `upstream/upstream` exists;
`spirv-shader-pipeline` documented as superseded.

Checklist:

- [x] `git switch -c idiomatic-wgpu-pipeline upstream/upstream`
- [x] Bump crate versions to satisfy servo-wgpu's `^0.68` patch:
  `webrender`, `webrender_api`, `webrender_build`, `wr_glyph_rasterizer`
  to `0.68.0`; matching inter-crate dep version refs updated. Mozilla's
  `upstream/upstream` is frozen at `0.62.0` because Mozilla doesn't bump
  versions in gecko-dev — Servo's `upstream/0.68` adds the version-bump
  and `[workspace.package]` setup as 5 packaging-prep commits we did
  not inherit. Adopting Servo's `[workspace.package]` workspace-managed
  pattern is deferred; manual per-crate bumps are sufficient for now.
- [x] Pushed to `origin/idiomatic-wgpu-pipeline`
- [x] Added superseded notice + link to this doc on:
  - [2026-04-27_dual_servo_parity_plan.md](2026-04-27_dual_servo_parity_plan.md)
  - [2026-04-18_upstream_cherry_pick_plan.md](2026-04-18_upstream_cherry_pick_plan.md)
  - [2026-04-22_upstream_cherry_pick_reevaluation.md](2026-04-22_upstream_cherry_pick_reevaluation.md)
  - [2026-04-18_spirv_shader_pipeline_plan.md](2026-04-18_spirv_shader_pipeline_plan.md)
  - [2026-04-21_spirv_pipeline_reset_execution.md](2026-04-21_spirv_pipeline_reset_execution.md)
  - [2026-04-26_track3_legacy_assembly_isolation_lane.md](2026-04-26_track3_legacy_assembly_isolation_lane.md)
- [x] [PROGRESS.md](PROGRESS.md) updated: idiomatic-wgpu-pipeline is
  the active branch; spirv-shader-pipeline is dead state.

### S1 — Empty wgpu device skeleton

**Done condition**: `cargo run` on the new branch boots wgpu, opens a
device, renders a clear color into an offscreen target, captures the
result via pixel readback, exits clean.

Checklist:

- [x] New module `webrender/src/device/wgpu/` scaffolded (decomposed
  from day one; no file > ~600 LOC):
  - [x] `wgpu/mod.rs` — public surface (declares submodules)
  - [x] `wgpu/core.rs` — Adapter / Device / Queue boot,
    `REQUIRED_FEATURES` check, debug-label population. **Boot
    landed.** `IMMEDIATES | DUAL_SOURCE_BLENDING` required;
    rejection on missing features. `max_inter_stage_shader_variables: 28`
    matches servo-wgpu's known wgpu 29 setting. Surface lifecycle
    deferred until a windowed slice surfaces it.
  - [x] `wgpu/format.rs`, `wgpu/buffer.rs`, `wgpu/texture.rs`,
    `wgpu/shader.rs`, `wgpu/binding.rs`, `wgpu/pipeline.rs`,
    `wgpu/pass.rs`, `wgpu/frame.rs`, `wgpu/readback.rs` — stub
    files with module docs; populated in S2 / S6.
- [x] Headless test target: `device::wgpu::core::tests::boot_clear_readback_smoke`
  — boots, clears a 4×4 to red, reads back, asserts pixel matches.
  Inline in `core.rs` for now; refactor into `frame.rs` /
  `readback.rs` when there's a second usage.
- [x] No coupling to `webrender/res/`, `webrender_build/src/`, or
  `webrender/src/shader_source/`.
- [x] No `super::GpuDevice` trait conformance. The renderer body
  adapts to wgpu at the device boundary.

**Sequenced fixes that landed during S1**:

- `wgpu::Features::PUSH_CONSTANTS` was removed in wgpu 29 (renamed
  to `IMMEDIATES` per WebGPU spec evolution — same underlying GPU
  primitive). `core.rs::REQUIRED_FEATURES` and the §4.7 plan prose
  use the wgpu 29 name in code; plan keeps "push constants" in prose
  since it's the better-known name across Vulkan / Metal / DX12.
- wgpu 29 added `depth_slice` to `RenderPassColorAttachment` and
  `multiview_mask` to `RenderPassDescriptor`. Smoke test includes
  both as `None`.

### S2 — Smallest end-to-end shader, sets the architectural shape

**Done condition**: a single rectangle renders at the correct color
and position via authored WGSL, **using the architectural patterns
from §4.6–4.9 from line one**: storage-buffer access for structured
data, dynamic-offset uniform for per-draw uniforms, push constants
for per-draw flags, `DrawIntent` recording into `pass.rs` (no inline
draw call), debug labels populated.

Checklist:

- [x] Authored
  [`shaders/brush_solid.wgsl`](../webrender/src/device/wgpu/shaders/brush_solid.wgsl)
  directly. No naga, no SPIR-V intermediate.
- [x] [`wgpu/pipeline.rs`](../webrender/src/device/wgpu/pipeline.rs):
  single pipeline `build_brush_solid`; bind-group layout has
  `has_dynamic_offset: true` for the per-draw uniform;
  `MAX_PALETTE_ENTRIES` declared as a WGSL `override` and supplied
  via `PipelineCompilationOptions::constants` to exercise the
  specialization path.
- [x] [`wgpu/buffer.rs`](../webrender/src/device/wgpu/buffer.rs):
  uniform arena that sub-allocates at
  `min_uniform_buffer_offset_alignment`, plus a storage-buffer-bound
  palette so the §4.6 storage-buffer access pattern runs end-to-end.
- [x] [`wgpu/pass.rs`](../webrender/src/device/wgpu/pass.rs):
  `DrawIntent` accepted and flushed into one render pass;
  `BeginRenderPass` exactly once per `flush_pass` call.
- [x] Test: `device::wgpu::tests::render_rect_smoke` records a
  single `DrawIntent`, flushes it via `pass::flush_pass`, reads back
  the 8×8 target, and asserts the centre-row pixel matches the
  palette-bound colour. Single-sample pixel check for S2's smallest-
  end-to-end purpose; full PNG oracle comparison comes online in S3.
- [x] No file exceeds ~600 LOC.

S2 is the slice that *sets* the architectural shape. S4 and S6 then
extend that shape across more shaders and scenes; the patterns do
not change.

**Sequenced fixes that landed during S2**:

- `wgpu::PushConstantRange` was removed in wgpu 29;
  `PipelineLayoutDescriptor` carries a single `immediate_size: u32`
  instead. There is no per-stage range — the shader's
  `var<immediate>` declaration locks the stage(s) that read it.
- `PipelineLayoutDescriptor::bind_group_layouts` is now
  `&[Option<&BindGroupLayout>]` — sparse layouts allowed; present
  entries must be wrapped in `Some(&layout)`.
- `PipelineCompilationOptions::constants` is `&[(&str, f64)]`, not a
  `HashMap`. The slice must outlive the options struct.
- `RenderPipelineDescriptor::multiview` was renamed to
  `multiview_mask` (matches `RenderPassDescriptor::multiview_mask`).
- `RenderPass::set_immediates(offset, data)` takes only two args; the
  stage is implicit (inferred from the pipeline's `immediate_size`
  declaration plus the shader's `var<immediate>` reads).
- WGSL `var<push_constant>` was renamed to `var<immediate>` in
  naga 29's WGSL parser. `push_constant` is kept as a reserved
  keyword so the parser surfaces a clear error when old code is
  encountered. The SPIR-V backend still maps `Immediate` →
  `PushConstant` storage class.
- WGSL override-specialized array sizes (`array<T, OVERRIDE>`)
  cannot be the top-level type of a storage binding: storage bindings
  require their type to be `CREATION_RESOLVED` at module creation,
  but override values aren't supplied until pipeline creation. Use a
  runtime-sized array (`array<T>`) for the binding and apply the
  override elsewhere (e.g., index clamps).
- `Limits::max_immediate_size` defaults to `0` even when
  `Features::IMMEDIATES` is enabled. Must be requested explicitly
  (set to 128 in `core.rs::boot` — portable Vulkan minimum).
  Features enable capability; limits unlock budget.

### S3 — Reference oracle capture

**Done condition**: a chosen seed scene set has frozen oracle PNGs,
captured from a GL build of WebRender via Wrench.

Checklist:

- [x] Side worktree at `../webrender-wgpu-oracle` off **`upstream/0.68`**
  (not `upstream/upstream` — Mozilla's gecko-dev mirror dropped
  `wrench/reftests/` from the standalone webrender tree; 0.68 is the
  closest source for both wrench and the reftest corpus). GL is
  unconditional on 0.68; the fork's `gl_backend` feature does not
  exist there yet.
- [x] Wrench rendered five initial seed scenes via
  `wrench png <YAML> <OUT.png>` — narrower than the original
  "5–10 scenes" suggestion's full breadth (image / text deferred
  pending asset-dependency handling), but covers clear / shape / AA /
  transform / gradient. Seed list in
  [`webrender/tests/oracle/README.md`](../webrender/tests/oracle/README.md).
- [x] Frozen as `webrender/tests/oracle/<scene>.{png,yaml}` on
  `idiomatic-wgpu-pipeline` (both the YAML and the rendered PNG —
  S4 will need both to render-and-compare).
- [x] Capture procedure documented in
  [`webrender/tests/oracle/README.md`](../webrender/tests/oracle/README.md);
  reproducible from a fresh checkout.
- [x] **GL never appears on `idiomatic-wgpu-pipeline`.** Verified —
  the only places GL builds are the worktree and the inherited
  upstream/0.68 source files (which we'll delete in S9).

### S4 — Reference scene rendering

**Done condition**: each S3 oracle scene renders correctly through the
new wgpu path; pixel-diff passes within tolerance.

Checklist:

- [ ] Author WGSL for each shader family the seed scenes need.
- [ ] Connect to the inherited renderer body — adapt at the device
  boundary; do not modify `frame_builder` / picture caching.
- [ ] Reftest harness: load oracle PNG, render scene through new wgpu
  device, pixel-diff.
- [ ] Tolerance policy: exact match by default; documented `fuzzy-if`
  per scene only with a root-cause comment (per dual-servo plan §"No
  hacks"). No undocumented tolerances.

### S5 — WebGPU CTS gate

**Done condition**: a chosen WebGPU CTS subset runs green in CI
against the new wgpu device.

Checklist:

- [ ] Add `gpuweb/cts` as a vendored test runner or dev-dep.
- [ ] Pick subset:
  - `api/operation/buffers/*` (texture creation/upload paths)
  - `api/operation/render_pass/*`
  - `api/operation/bind_groups/*`
  - `api/operation/blend/*`
  - `api/operation/depth_stencil/*`
  - `api/operation/vertex_state/*`
- [ ] Wire as `cargo test --test cts_subset` or equivalent.
- [ ] Document subset rationale.
- [ ] Compute, storage textures, advanced features deferred unless
  webrender starts using them.

### S6 — Full shader corpus

**Done condition**: the ~50 shader programs WebRender needs are
authored as WGSL; family-level reftests pass against the oracle.

Checklist by family:

- [ ] Brush: solid, image, image-repeat, blend, mix-blend,
  linear-gradient, opacity, yuv-image (alpha + opaque variants each)
- [ ] Text: ps_text_run, glyph-transform, dual-source variants
- [ ] Quad: textured, gradient, radial-gradient, conic-gradient, mask,
  mask-fast-path
- [ ] Prim: split-composite
- [ ] Clip: cs_clip_rectangle (+ fast path), cs_clip_box_shadow
- [ ] Cache task: cs_border_solid, cs_border_segment, cs_line_decoration,
  cs_fast_linear_gradient, cs_linear_gradient, cs_radial_gradient,
  cs_conic_gradient, cs_blur (color + alpha), cs_scale, cs_svg_filter,
  cs_svg_filter_node
- [ ] Composite: composite, fast path, yuv variants
- [ ] Debug: debug_color, debug_font
- [ ] Utility: ps_clear, ps_copy
- [ ] Each family has at least one scene in the oracle.
- [ ] Pipeline cache decomposed: separate cache per family or by
  pipeline-key shape; no single 2000-LOC pipeline-cache impl.
- [ ] **WGSL override-based specialization (per §4.9)**: where two
  variants in `WgpuShaderVariant::ALL` differ only by parameter
  (alpha vs. opaque, fast-path vs. full, dual-source toggle),
  collapse to one WGSL source + N specialized pipelines via
  `PipelineLayoutDescriptor` overrides. Document the
  family-vs-variant distinction in the corpus.
- [ ] **Glyph cache: texture-array migration.** Replace the
  single-atlas approach with a layered texture array; layer-per-
  format possible (color emoji vs. mono glyph), no fragmentation,
  layer growth on demand. Sub-task of S6, not blocking; can land
  after the rest of the corpus. Atlas is owned internally by
  webrender-wgpu (per Q14, resolved 2026-04-28).
- [ ] **`wgpu::RenderBundle` for picture-cache tile replay**
  (investigation): if WebRender's picture cache holds rendered tiles
  that replay across frames, recording each tile as a render bundle
  avoids re-encoding cost. Frame-time win; investigate after the
  corpus is authored, not before.

### S7 — Servo-wgpu integration

**Done condition**: `servo-wgpu` renders the basic presenting smoke
set (solid, linear gradient, radial gradient, clip, image, text)
through the new wgpu webrender. Equivalent to current Servo presenting
smoke on `spirv-shader-pipeline`, against the new code.

Checklist:

- [ ] Wire the new wgpu device into servo-wgpu's webrender consumer.
- [ ] Confirm presenting smoke renders correctly (visual check + diff
  against oracle).
- [ ] Decide what to do with current servo-wgpu glue that assumes the
  old `WgpuDevice` shape — almost certainly adapted, not deleted.

### S8 — External corpus coverage

**Done condition**: at least one external test corpus has a chosen
subset running green.

Checklist:

- [ ] Pick one: WPT slice, CSS WG Interop subset, or upstream Wrench
  full reftest suite. Document the choice.
- [ ] Integrate the subset's runner.
- [ ] Triage and address failures.
- [ ] Coverage areas to ensure: scroll compositing, SVG/filter,
  external image, complex clip chains, text at multiple DPI.

### S9 — Delete the dead

**Done condition**: GL crates are uncited in `Cargo.toml`, nothing
imports them, the binary works, `cargo tree | grep -i gl` returns
nothing surprising.

Checklist:

- [ ] Delete `webrender/src/device/gl.rs` and `query_gl.rs` (these
  come along via inheritance from `upstream/upstream` — we delete
  from our branch).
- [ ] Drop `gleam` dep from `webrender/Cargo.toml`.
- [ ] Delete authored GLSL source tree `webrender/res/*.glsl`.
- [ ] Delete `swgl/` and `glsl-to-cxx/`.
- [ ] Delete `webrender_build/src/glsl.rs`,
  `webrender_build/src/wgsl.rs` (if any), and any
  `shader_runtime_contract*` content. Keep `webrender_build` only
  for non-shader-pipeline content.
- [ ] Delete any SPIR-V build infrastructure that leaked in.
- [ ] `cargo build` is clean. Default features have no `gl_backend`.

---

## 7. Sequencing

Slice dependencies:

- S0 → everything (need the branch).
- S1 → S2 (need device before shaders).
- S2 → S4 (need a shader-family pattern before scene rendering).
- S3 is independent of S1–S2 — runs in parallel from start.
- S5 (CTS) is independent of S2–S4 — runs alongside.
- S6 expands S4's pattern across all shader families.
- S7 needs S6 (or a sufficient subset).
- S8 needs S6+.
- S9 is the final cleanup.

Suggested order:
S0 → (S1 ∥ S3) → S2 → S4 → (S5 ∥ S6) → S7 → S8 → S9.

---

## 8. Receipts

- **S0**: branch exists; supersession notes added on the six prior
  plans; PROGRESS.md updated.
- **S1**: ✅ landed 2026-04-28. `boot_clear_readback_smoke` test
  boots wgpu, clears a 4×4 target to red, reads back, asserts the
  pixel matches (255, 0, 0, 255). 1.29s test runtime.
- **S2**: ✅ landed 2026-04-28.
  `device::wgpu::tests::render_rect_smoke` records a single
  `DrawIntent`, flushes via `pass::flush_pass` into one render pass,
  reads back the 8×8 target, asserts the centre-row pixel matches
  the palette-bound colour. Exercises §4.6 storage buffer, §4.7
  dynamic-offset uniform + immediate (push constant), §4.8 record-
  then-flush, §4.9 WGSL override specialization. 0.78s for both
  wgpu tests combined.
- **S3**: ✅ landed 2026-04-28. Five seed scenes (`blank`,
  `rotated_line`, `fractional_radii`, `indirect_rotate`,
  `linear_aligned_border_radius`) captured via `wrench png` on a
  worktree at `upstream/0.68` (NVIDIA RTX 4060, OpenGL 3.2). PNGs +
  YAMLs frozen in `webrender/tests/oracle/`; capture procedure
  documented in the same dir's README. Gotcha logged: wrench's
  `YamlFrameReader::new_from_args` panics on clap 3 because the
  `png` subcommand on 0.68 doesn't declare the
  `keyframes`/`list-resources`/`watch` args; local oracle worktree
  carries a one-function patch to skip those decorators.
- **S4**: each seed scene passes pixel-diff against oracle.
- **S5**: chosen CTS subset green in CI.
- **S6**: all ~50 shader programs authored; family-level reftests
  pass.
- **S7**: servo-wgpu renders presenting smoke set through new code.
- **S8**: chosen external corpus subset green.
- **S9**: GL deps gone; default build is wgpu;
  `cargo tree | grep -i gl` returns nothing surprising; binary works.

---

## 9. Risks

- **WGSL authorship cost.** ~50 shaders is real work, more than
  naga-derived WGSL was. *Mitigation*: family at a time (S2 first,
  broadest in S6); oracle scenes as receipts; start narrow.
- **Reference-oracle scope creep.** Capturing every possible scene is
  endless. *Mitigation*: 5–10 seed scenes for S3; expand only when
  S6/S8 surface a concrete gap.
- **Renderer body has GL-shaped assumptions.** WebRender's
  `frame_builder`, `batch_builder`, picture cache, and render-task
  graph were authored for GL. *Mitigation*: this is shared with the
  original SPIR-V plan and was navigated successfully there. Treat the
  renderer body as inherited from `upstream/upstream`; adapt at the
  device boundary in `webrender/src/device/wgpu/`.
- **Servo-wgpu integration churn.** Existing servo-wgpu glue assumes
  the old `WgpuDevice` shape. *Mitigation*: defer to S7. S1–S6 do not
  depend on Servo.
- **Oracle drift.** If we re-base on a newer `upstream/upstream`
  later, oracle PNGs may not match. *Mitigation*: capture against the
  branched commit; freeze; re-capture only on intentional re-base.
- **Dropping `spirv-parity` coverage.** Was the only correctness
  signal we had. *Mitigation*: S3 + S4 explicitly replace it. The 33
  passing tests stop mattering when the branch is replaced; they were
  never the bar.
- **Locally-discovered correctness on `spirv-shader-pipeline` not
  carried forward.** E.g. the picture-cache opaque-depth fix.
  *Mitigation*: §3 lists insights to carry forward as
  designed-correct from line one. New ones surface as S4–S6 expand;
  codify each one as it lands.
- **WGSL feature parity with what WebRender's GLSL relied on.** GL
  branches in shaders sometimes used features that don't translate
  cleanly to WGSL (e.g. dual-source blending guards, dynamic
  indexing). *Mitigation*: the SPIR-V branch already discovered these;
  use that branch as a reference for "which GL features needed
  workarounds in WGSL," but author the WGSL fresh rather than
  porting workarounds.

---

## 10. Open questions

These belong to S0/S1 and are flagged for input rather than assumed.

1. ~~**Branch name.**~~ Resolved 2026-04-28: branched
   `idiomatic-wgpu-pipeline` from `upstream/upstream`.
2. **Crate layout.** Stay with `webrender/` and rebuild internals,
   rename to `webrender_wgpu`, or split out a new top-level
   `wgpu_renderer/` crate that depends on webrender for display-list
   types? A clean wgpu-native crate is the cleaner conceptual model
   but disrupts the cargo-tree shape Servo expects. Default: stay
   with `webrender/`.
3. **Use naga as a build-time tool at all?** Even for authored WGSL,
   wgpu uses naga internally for validation. The "no naga" question
   is really "do we use naga as a build-time tool" — almost certainly
   no; `wgpu::Device::create_shader_module` is enough. Confirm we
   aren't relying on `naga::front::wgsl` as a build-time check we'd
   otherwise want.
4. **Oracle host platform.** Capturing oracle PNGs from
   `upstream/upstream` + GL means a working GL build. Linux/EGL is
   the most reproducible. Or skip the GL oracle and use Firefox's
   WebRender output as reference (harder to isolate, more
   authoritative). Default: GL build on Linux/EGL.
5. **WGSL authorship: from scratch or naga-translated GLSL as a
   starting point?** Translating GLSL once with naga and then
   evolving is faster but contradicts "author WGSL directly."
   Authoring fresh is purer but slower. Default: fresh, with
   naga-translated versions allowed as a *comparison reference*
   during authoring.
6. **CTS subset depth.** The S5 list is a starting point. The
   specific tests within each suite that catch real wgpu-integration
   bugs should be enumerated when S5 lands; deferred until then.
7. **Where to keep the oracle build.** Side branch on this repo
   (`oracle-capture`), separate repo, or worktree? Default: side
   branch on this repo, not merged to main; PNG fixtures committed
   to main.
8. **Disposition of the frozen `spirv-shader-pipeline` branch.** Keep
   indefinitely as historical artifact, or delete after some interval
   once `idiomatic-wgpu-pipeline` is mature? Default: keep until S9
   receipts land, then decide.

9. **Variant collapse via WGSL overrides.** S6 §4.9 plans to collapse
   ~50 variants via override specialization. How aggressive: collapse
   all parameter-only variants (likely about half the corpus), or
   stay closer to one-WGSL-per-variant for readability and only
   collapse where the parameter delta is small? Default: aggressive
   collapse where the family is the same; re-split only if a
   specialization causes shader compile-time blow-up or readability
   issues.

10. **Decision recorded — compute-based clipping is deferred.**
    Floated during planning. WebRender's clip system (`cs_clip_*`
    shader family + clip-mask render targets + `RenderTaskGraph`
    clip dependency tracking) is a multi-quarter rewrite with
    active research debate (rasterized vs. SDF vs. per-tile compute
    clip). Conflating it with this branch gates S4–S6 on an
    unsolved-for-WebRender problem. The existing rasterized-clip
    structure ports to wgpu cleanly as authored WGSL. Compute-based
    clip rewrite is a follow-up plan, not part of this one.

11. **Decision recorded — texture-array glyph cache is a S6
    sub-task.** Floated during planning. Single-atlas inherited from
    `upstream/upstream` is correct; texture-array migration is an
    optimization (no fragmentation, layer-per-format possible)
    slotted as a S6 sub-task, not a core architectural pillar.

12. **`wgpu::RenderBundle` for retained content** (investigation,
    not core). Picture-cache tile replay across frames without
    re-encoding is a frame-time win. Slot for investigation in S6
    after the shader corpus is authored.

13. **Async pipeline compilation UX.** Per §4.11, first-run pipeline
    compilation is async + the app shows a "warming" state until
    pipelines resolve. What does "warming" look like for Servo's
    integration — empty page, loading splash, or render-anyway-
    with-fallback-pipeline? Default: render fallback (cleared
    background) until the pipeline for a given draw is ready;
    don't block the main thread.

14. **Decision recorded 2026-04-28 — webrender-wgpu owns the glyph
    atlas (resolution (a) below).** Confirmed: webrender-wgpu keeps
    the internal atlas; graphshell-gpu does not duplicate it on
    content paths that land in WebRender. Graphshell's
    `2026-04-20_graphshell_gpu_spec.md §5.5` (currently "leaning")
    needs a corresponding update to qualify its "single glyph atlas"
    line as scoped to the graphshell-gpu-owned paths (Direct Lane /
    vello), not the WebRender path. Original conflict context
    preserved below for posterity.

    - **WebRender does not shape text** — embedders shape, then
      submit pre-shaped glyph runs via the display-list API. Parley
      sits *above* webrender-wgpu (in graphshell's HTML Lane:
      Stylo → Taffy → Parley → webrender-wgpu) and is API-compatible
      without any change to webrender-wgpu's text path. **There is
      no parley-vs-swash conflict at the shaping layer.**
    - **Atlas ownership *is* the conflict.** Webrender-wgpu owns the
      glyph atlas internally today via [wr_glyph_rasterizer/](../wr_glyph_rasterizer/).
      Graphshell's [graphshell_gpu_spec.md §5.5](../../graphshell/design_docs/graphshell_docs/technical_architecture/2026-04-20_graphshell_gpu_spec.md)
      states a "leaning" decision for **a single glyph atlas owned
      by `graphshell-gpu`**, keyed by
      `(font_id, glyph_id, size_bucket, subpixel_pos)`. Both can't
      be the canonical atlas.

    Three resolutions:

    - (a) **Status quo / defer** — webrender-wgpu keeps its internal
      atlas; graphshell's §5.5 is revised to "single atlas, lives
      inside the WebRender consumer." Cheapest. The current S6
      glyph-cache sub-task implicitly assumes this. Reasonable
      until graphshell-gpu actually needs an atlas for non-WebRender
      renderers (vello, Direct Lane).
    - (b) **Move the atlas out** — webrender-wgpu's text path is
      reshaped so the atlas is borrowed from `graphshell-gpu`.
      `wr_glyph_rasterizer` becomes a populator of a borrowed
      atlas surface, not an owner. Bigger change; affects API
      surface and texture-cache plumbing. Only worth it if
      graphshell-gpu's vello/Direct-Lane atlas usage is concrete.
    - (c) **Two atlases with key-level dedup** — each subsystem
      keeps an atlas; they share the keying scheme but pages live
      in both places. Usually worst-of-both-worlds; listed for
      completeness.

    **The decision lives in graphshell, not here.** This plan defers
    to whichever direction graphshell-gpu lands; S6's texture-array
    migration is mostly the same work in (a) and (b) up to the
    ownership-boundary call. Default until graphshell-gpu work
    reaches text: (a).

---

## 11. Bottom line

Branch `wgpu-native` from `upstream/upstream`. Rebuild the wgpu device
fresh, decomposed from line one, against authored WGSL. Frozen oracle
PNGs are the visual ground truth. WebGPU CTS is the API gate. Delete
GL when the new branch covers the target.

The asset that makes this jump-ship cheap is the architectural shape
inherited from `upstream/upstream` — frame builder, batch builder,
picture cache, render-task graph — none of which we rebuild. We
rebuild only what the SPIR-V/parity story shaped: the device layer,
the shader pipeline, the test harness, the cargo features.

Receipts in §8 are the done condition. Open questions in §10 gate
S0/S1. Everything else is ordered work.
