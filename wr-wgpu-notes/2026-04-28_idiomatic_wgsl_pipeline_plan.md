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
- **No new code on `spirv-shader-pipeline` or its descendants.** That
  branch is frozen at S0. Bug fixes only if absolutely required for an
  in-flight servo-wgpu user; never feature additions.

---

## 6. Slice plan

Each slice is independently shippable and produces a real artifact.

### S0 — Branch and freeze

**Done condition**: `wgpu-native` branch exists off `upstream/upstream`;
`spirv-shader-pipeline` documented as superseded.

Checklist:

- [ ] `git switch -c wgpu-native upstream/upstream`
- [ ] Push to `origin/wgpu-native`
- [ ] Add a superseded notice + link to this doc on:
  - [2026-04-27_dual_servo_parity_plan.md](2026-04-27_dual_servo_parity_plan.md)
  - [2026-04-18_upstream_cherry_pick_plan.md](2026-04-18_upstream_cherry_pick_plan.md)
  - [2026-04-22_upstream_cherry_pick_reevaluation.md](2026-04-22_upstream_cherry_pick_reevaluation.md)
  - [2026-04-18_spirv_shader_pipeline_plan.md](2026-04-18_spirv_shader_pipeline_plan.md)
  - [2026-04-21_spirv_pipeline_reset_execution.md](2026-04-21_spirv_pipeline_reset_execution.md)
  - [2026-04-26_track3_legacy_assembly_isolation_lane.md](2026-04-26_track3_legacy_assembly_isolation_lane.md)
- [ ] Note in [PROGRESS.md](PROGRESS.md): `spirv-shader-pipeline` is
  dead state, no new work lands there.

### S1 — Empty wgpu device skeleton

**Done condition**: `cargo run` on the new branch boots wgpu, opens a
device, renders a clear color into an offscreen target, captures the
result via pixel readback, exits clean.

Checklist:

- [ ] New module `webrender/src/device/wgpu/` (decomposed from day one)
  - `wgpu/mod.rs` — public surface
  - `wgpu/init.rs` — adapter / device / queue boot
  - `wgpu/frame.rs` — render pass / encoder bookkeeping
  - `wgpu/readback.rs` — pixel capture for tests
- [ ] Headless test target: clears to a known color, reads back,
  asserts exact match.
- [ ] No coupling to anything in `webrender/res/`,
  `webrender_build/src/`, or `webrender/src/shader_source/`.

### S2 — Smallest end-to-end shader

**Done condition**: a single rectangle renders at the correct color
and position via authored WGSL.

Checklist:

- [ ] Author `shaders/brush_solid.wgsl` (or equivalent — the simplest
  family) directly. No naga, no SPIR-V intermediate.
- [ ] New module `webrender/src/device/wgpu/pipeline.rs`: bind-group
  layout + render-pipeline construction for the one shader.
- [ ] Vertex/index buffer setup: separate
  `webrender/src/device/wgpu/buffers.rs`.
- [ ] Test: render a 100×100 red rect at (50, 50) on a 200×200 frame;
  pixel-diff against an embedded reference PNG.
- [ ] No file in this slice exceeds ~600 LOC.

### S3 — Reference oracle capture

**Done condition**: a chosen seed scene set has frozen oracle PNGs,
captured from `upstream/upstream` + GL via Wrench.

Checklist:

- [ ] Side branch `oracle-capture` (or a separate worktree); clean
  `upstream/upstream` + `gl_backend` enabled there.
- [ ] Wrench harness: render N seed scenes (start: solid, linear
  gradient, radial gradient, basic image, basic text, simple clip),
  capture PNG output.
- [ ] Freeze oracle PNGs as test fixtures in `tests/oracle/<scene>.png`
  on `wgpu-native`.
- [ ] Document the capture procedure so the oracle is reproducible.
- [ ] **The oracle build is not in the main branch.** GL never
  appears on `wgpu-native`.

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
- **S1**: cleared frame captured via pixel readback; binary exits
  clean.
- **S2**: single rectangle renders correctly against embedded
  reference PNG.
- **S3**: oracle PNGs frozen for the seed scene set; capture procedure
  documented.
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

1. **Branch name.** `wgpu-native` (default), `wgpu-only`, or
   `wgpu-rebuild`? Bikeshed but matters for the next month of git
   output.
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
   once `wgpu-native` is mature? Default: keep until S9 receipts
   land, then decide.

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
