# WebRender wgpu Renderer — Implementation Plan

**Date**: 2026-03-01
**Status**: Active convergence / upstream-first (reframed 2026-03-14, current-state corrected 2026-04-02, refreshed 2026-04-09) — Graphshell still ships on `egui_glow`, but WebRender `wgpu` work is now treated as upstream renderer development first, with thin Servo integration and Graphshell-local validation. The current WebRender branch contains a functional `wgpu` renderer with remaining parity-validation and architectural cleanup work, so active planning emphasizes convergence from the existing hybrid branch shape rather than an immediate clean-slate backend split. See `PLANNING_REGISTER.md` §0.12/§0.13.
**Author**: Arc
**Source research**: `research/2026-03-01_webrender_wgpu_renderer_research.md`
**Active audit log**: `2026-03-03_servo_wgpu_upgrade_audit_report.md`
**Feeds into**:
- `2026-03-01_webrender_readiness_gate_feature_guardrails.md` (readiness gates G1–G5)
- `2026-03-01_backend_bridge_contract_c_plus_f_receipt.md` (C+F closure policy)
- `aspect_render/2026-02-27_egui_wgpu_custom_canvas_migration_strategy.md` (renderer backend migration)
- `research/2026-02-27_egui_wgpu_custom_canvas_migration_requirements.md` (GPU ownership model)

**Tracker**: `#183` (backend migration), `#180` (runtime-viewer GL→wgpu bridge)
**Related lanes**: `#88` (stabilization), `#90` (embedder-debt), `#92` (viewer-platform), `#99` (spec-code-parity)

---

## 0. Executive Summary

This plan converts the research document's specification into a phased, technically
validatable execution sequence for introducing a wgpu rendering backend into WebRender
as consumed by Graphshell through Servo. Each phase has explicit entry conditions, exit
criteria, validation methods, and rollback posture. The plan is structured so that every
phase produces independently useful evidence and no phase requires speculative assumptions
from a later phase.

**End-state**: Servo's WebRender compositor emits a `wgpu::Texture` that Graphshell's
`CompositorAdapter` binds zero-copy into the `egui_wgpu` frame pass, eliminating GL state
save/restore chaos mode and closing `#180`.

**Current-state constraint**: Glow remains the active runtime composition policy for the
current milestone. This plan runs as Track B (WebRender readiness) in parallel with
Track A (milestone delivery on Glow), per the readiness gate contract.

**Current audit note (2026-03-14)**: the Servo-side `wgpu 26 -> 27` compatibility pass still
matters, but it is now treated as enabling integration work rather than as the main development
home. The primary implementation target is upstream WebRender `device` / `renderer`; Servo should
only carry the minimum dependency-redirection or integration delta required to exercise a local
editable WebRender checkout. See `2026-03-03_servo_wgpu_upgrade_audit_report.md`.

**Execution posture update (2026-03-14)**:

- Do renderer development and most validation in upstream WebRender first.
- Use Servo as the thinnest possible integration consumer.
- Use Graphshell as the final downstream validation environment.
- Avoid a long-lived behavioral Servo fork unless Cargo/source topology makes it unavoidable.

**Current-state correction (2026-04-02)**:

- The local WebRender `wgpu-backend-0.68-minimal` branch is no longer a hypothetical starting
     point; it already executes real wgpu rendering work.
- The current implementation is a hybrid backend shape centered on `Renderer` owning both
     `device: Option<Device>` and `wgpu_device: Option<WgpuDevice>`, with wgpu execution routed
     through dedicated renderer methods rather than a settled backend-neutral executor seam.
- Several renderer subsystems still expose placeholder `Wgpu(...)` variants or compatibility
     carriers, so the branch should be treated as a proof backend, not as the final architecture.
- GL remains mandatory for parity testing, fallback behavior, compositor maturity, and richer
     diagnostics while the wgpu path converges.

**Progress update (2026-04-03)**:

- Branch `wgpu-device-renderer` (commit `da8eb84` + uncommitted P7 work) now passes **225/366
     reftests (61%)** via the built-in `wrench --wgpu reftest` harness.
- All `BatchKind` variants are now routed to correct shaders (Quad, Brush, TextRun, SplitComposite).
- Offscreen rendering (`alpha_batch_containers`) is now dispatched for texture cache targets,
     unblocking filters, gradients, and blend modes that render through intermediate surfaces.
- SVG filter shaders (`cs_svg_filter`, `cs_svg_filter_node`) are compiled and dispatched,
     with CPU-side repacking of u16 instance fields to i32 for wgpu vertex attribute compatibility.
- Texture-to-texture blits implemented via `copy_texture_to_texture`.
- Shader compilation moved to a 16 MB stack thread to work around naga recursive-descent
     stack overflow on large transpiled WGSL (pre-existing bug).
- Primary remaining gaps: `resolve_ops`, `clip_masks` (ClipMaskInstanceList), subpixel text
     (dual-source blending), and encoder-invalid validation errors on some targets.
- Detailed historical breakdown in `archive/progress/2026-04-03_p6_progress_report.md`.

**Current-state refresh (2026-04-09)**:

- Current branch tip: `wgpu-backend-0.68-experimental` @ `e403cc14b`.
- The specific gaps called out in the 2026-04-03 snapshot are now closed in-tree:
     `resolve_ops`, `clip_masks`, subpixel text via `DUAL_SOURCE_BLENDING`, YUV external-surface
     compositing, real `GraphicsApi::Wgpu` metadata, and graceful `CompositorConfig::Native`
     fallback in `wgpu`-only mode.
- Focused validation on the current checkout is green:
     `cargo check -p webrender --features wgpu_backend`,
     `cargo test -p webrender --features wgpu_backend --test wgpu_shared_device`,
     and `cargo run -p webrender-examples --bin wgpu_headless --features wgpu_backend`.
- WGSL translation now reports `68/68` variants succeeding in the local build.
- The branch should no longer be described as a mere proof backend. It is better characterized as
     a functional renderer backend with remaining work concentrated in full-suite parity refresh,
     embedder integration validation, and cleanup of hybrid GL/wgpu compatibility carriers.
- Recent branch history reports substantially higher reftest completion than the 2026-04-03 note
     (for example commit `366ed169e` reports `434/441 pass`), but this document should treat that
     figure as provisional until a fresh full `wrench --wgpu reftest` run is captured against the
     current branch tip.

### 0.1 Active Convergence Tracks

The original phase map below remains valuable as a dependency and validation inventory, but it
is no longer the only reasonable ordering for active work. Current execution should be organized
around four convergence tracks.

#### Track C1 — Typed Backend Metadata

Replace stringly pipeline, shader, and layout metadata with typed descriptors where practical.
Immediate targets include:

- pipeline identity and blend/depth variants,
- shader family / batch-kind mapping,
- resource binding declarations,
- compatibility metadata currently inferred from shader-name prefixes.

#### Track C2 — Subsystem Execution Seams

Move toward backend-specific execution seams subsystem-by-subsystem instead of attempting a
single renderer-wide abstraction jump. Immediate seam candidates are:

- texture cache updates,
- GPU cache uploads,
- frame-data texture or buffer upload,
- pass-local draw submission.

The goal is to make backend ownership honest without freezing the branch behind a large
trait-extraction rewrite.

#### Track C3 — GL Parity and Diagnostics Preservation

Keep GL available as:

- the correctness oracle for pixel and geometry comparisons,
- the fallback backend when wgpu parity is incomplete,
- the richer diagnostics path for capture/profiler/compositor investigation.

All active wgpu work should improve, not weaken, the ability to compare behavior against GL.

#### Track C4 — Thin Servo / Stable Graphshell Integration

Continue to keep:

- Servo integration thin and primarily dependency-topology oriented,
- Graphshell bridge contracts stable,
- Graphshell compositor assumptions anchored to the existing three-pass model,
- `GlowCallback` as the current production-safe bridge policy until readiness evidence closes.

### 0.2 Working Rule For This Document

Use the convergence tracks above as the default execution guide for current work.

Use the `P0`-`P12` plan below as:

- a dependency checklist,
- a validation ledger,
- a risk inventory,
- a record of the fuller March target architecture.

Do **not** read `P3` as meaning that a renderer-wide backend trait extraction must happen before
useful wgpu improvements can continue.

---

## 1. Phase Map

| Phase | Name | Gate | Produces | Risk |
|-------|------|------|----------|------|
| **P0** | Dependency Audit + Version Alignment | G1 | Version compatibility matrix, patch path proof | Low |
| **P1** | Upstream Reconnaissance | G1 | Coordination posture, duplicate-work avoidance | Low |
| **P2** | Local Patch / Thin Integration Scaffold | G1 | Reproducible local WebRender checkout consumed by Servo/Graphshell with minimal integration delta | Medium |
| **P3** | WebRender Backend Trait Extraction | G2 | Backend-agnostic `Device`/`Renderer` trait boundary inside WebRender | High |
| **P4** | Shader Translation Pipeline | G2 | GLSL → WGSL build-time pipeline via naga; translated shader set | Medium |
| **P5** | wgpu Device Implementation | G2, G3 | `WgpuDevice` struct passing WebRender resource management tests | High |
| **P6** | wgpu Renderer Implementation | G2, G3 | `WgpuRenderer` struct passing WebRender frame rendering tests | High |
| **P7** | Compositor Output Contract | G2, G3 | `wgpu::Texture` handoff from WebRender to embedder | High |
| **P8** | Graphshell Integration Spike | G3, G4 | One composited Servo viewer in a wgpu-backed Graphshell frame | High |
| **P9** | Pixel Parity Validation | G5 | Reference-set comparison between GL and wgpu paths | Medium |
| **P10** | Performance Validation | G5 | Frame budget measurements meeting §5.3 targets | Medium |
| **P11** | Platform Matrix Validation | G4 | Per-platform pass/fail evidence | Medium |
| **P12** | Production Cutover Preparation | G1–G5 | Switch authorization evidence package | Low |

**Interpretation note (2026-04-02)**: keep this phase map as the long-range inventory, but route
near-term work through Tracks C1-C4 above. In particular, `P3` is now an optional convergence
destination, not the mandatory first major implementation step.

---

## 2. Phase Specifications

### P0 — Dependency Audit + Version Alignment

**Objective**: Determine whether Servo's wgpu version and Graphshell's target `egui_wgpu`
version are compatible, and document the exact dependency graph.

**Entry conditions**:
- Access to Servo main branch `Cargo.lock`
- Knowledge of target `egui_wgpu` version for Graphshell

**Tasks**:

| # | Task | Validation method |
|---|------|-------------------|
| P0.1 | Run `cargo tree -i wgpu --depth 4` against Servo main to extract wgpu version | Version string captured |
| P0.2 | Run `cargo tree -i wgpu --depth 4` against target `egui_wgpu` release | Version string captured |
| P0.3 | Compare wgpu versions; document compatibility or skew | Written compatibility matrix |
| P0.4 | Identify `naga` version bundled with each wgpu release | Version strings captured |
| P0.5 | If versions diverge: identify the minimum `egui_wgpu` release compatible with Servo's wgpu, or vice versa | Candidate version pair documented |
| P0.6 | Document wgpu feature flags required by WebRender vs egui_wgpu | Feature flag comparison table |
| P0.7 | Produce dependency version alignment report | Report posted to `#183` |

**Exit criteria**:
- Version compatibility matrix is complete and posted to tracker
- Either: versions align naturally, OR a specific version-unification patch is identified
- G1 evidence: dependency control validated for wgpu/naga/webrender_api

**Rollback**: No code changes; pure analysis. No rollback needed.

**Answers research Q1, Q4 partially.**

---

### P1 — Upstream Reconnaissance

**Objective**: Determine the state of upstream wgpu renderer work in Servo/WebRender
to avoid duplicating effort and identify coordination opportunities.

**Entry conditions**:
- None (can run in parallel with P0)

**Tasks**:

| # | Task | Validation method |
|---|------|-------------------|
| P1.1 | Search Servo GitHub issues/PRs for "wgpu renderer", "wgpu backend", "webrender wgpu" | Search results documented |
| P1.2 | Search WebRender crate issues for wgpu tracking | Search results documented |
| P1.3 | Check Wu Yu-Wei's public repositories and contributions for implementation progress | Activity documented |
| P1.4 | Check Servo Zulip/Matrix channels for wgpu renderer discussion threads | Summary documented |
| P1.5 | Determine upstream contributor(s) and their current focus | Contact points identified |
| P1.6 | Assess whether upstream work exists that Graphshell can consume directly | Consume/fork/build decision documented |
| P1.7 | Produce upstream coordination posture document | Posted to `#183` |

**Exit criteria**:
- Upstream state is known: active work exists (coordinate), or no active work (proceed independently)
- Coordination posture decision is documented

**Rollback**: No code changes.

**Answers research Q2.**

---

### P2 — Fork + Patch Scaffold

**Objective**: Establish a reproducible build path that lets Graphshell exercise a local editable
WebRender checkout through Servo while keeping Servo changes as thin as possible.

**Entry conditions**:
- P0 complete (version alignment known)
- P1 complete (upstream posture known — determines whether local patching is sufficient or a thin
  integration branch is required)

**Tasks**:

| # | Task | Validation method |
|---|------|-------------------|
| P2.1 | Create or adopt a local editable WebRender checkout (`../webrender` or equivalent) | Checkout exists and builds standalone |
| P2.2 | Point Servo at the local WebRender checkout through `[patch.crates-io]` or equivalent local override | Cargo resolves `webrender` from the local checkout |
| P2.3 | If local patching is insufficient, create a thin Servo integration branch whose only job is dependency redirection and compatibility shims | Branch exists with narrowly scoped diff |
| P2.4 | Run `cargo check -q` with zero behavioral WebRender renderer changes | Clean build |
| P2.5 | Run targeted Servo/Graphshell validation to prove the local checkout is in the dependency graph | Validation passes |
| P2.6 | Add a no-op `wgpu_device` module stub inside the local WebRender checkout to prove the patch slot works | Module compiles, no behavior change |
| P2.7 | Document the patch path in a `WEBRENDER_PATCH.md` file | Documentation exists |
| P2.8 | Document the rollback path: reverting the local override restores stock Servo consumption | Rollback tested and documented |

**Exit criteria**:
- Graphshell or targeted integration checks pass against the local WebRender checkout
- Rollback to stock Servo/WebRender consumption is proven
- G1 closed: dependency control and reproducibility demonstrated

**Rollback**: Revert the local override path (remove `[patch.crates-io]` or restore original
dependency source).

---

### P3 — WebRender Backend Trait Extraction

**Objective**: Inside the editable WebRender checkout, introduce a backend abstraction trait that
the existing GL implementation satisfies, without changing any GL behavior. This creates
the seam where the wgpu implementation plugs in.

**Entry conditions**:
- P2 complete (patch scaffold works)

**Tasks**:

| # | Task | Validation method |
|---|------|-------------------|
| P3.1 | Audit WebRender `Device` struct — enumerate all public methods and GL calls | Method inventory spreadsheet/doc (count, categorized by concern) |
| P3.2 | Audit WebRender `Renderer` struct — enumerate pass scheduling, render target, and draw call patterns | Method inventory |
| P3.3 | Define `trait WrDevice` abstracting the `Device` interface: texture ops, buffer ops, framebuffer ops, shader program ops, draw calls | Trait definition compiles |
| P3.4 | Define `trait WrRenderer` abstracting the `Renderer` interface: pass scheduling, target management, command dispatch | Trait definition compiles |
| P3.5 | Implement `WrDevice for GlDevice` wrapping the existing `Device` struct | All existing WebRender tests pass |
| P3.6 | Implement `WrRenderer for GlRenderer` wrapping the existing `Renderer` struct | All existing WebRender tests pass |
| P3.7 | Make WebRender generic over `<D: WrDevice, R: WrRenderer>` at the top-level integration points | Compiles; all existing behavior preserved |
| P3.8 | Run Graphshell full test suite against the trait-extracted WebRender | All tests green, no rendering differences |
| P3.9 | Measure build time delta from trait extraction | Documented; acceptable if ≤15% increase |

**Exit criteria**:
- WebRender compiles and passes all tests with trait-abstracted backend
- Existing GL path behavior is byte-identical (no rendering changes)
- The trait surface is documented with method-level doc comments
- G2 evidence: backend contract boundary exists

**Rollback**: Revert the WebRender checkout or upstream branch to the pre-extraction commit.

**Risk mitigation**: This is the highest-risk refactoring phase. If the `Device`/`Renderer`
interface proves too entangled for clean trait extraction:
- Fallback A: Extract only `Device`, leave `Renderer` monomorphic with `match` dispatch
- Fallback B: Use a `BackendKind` enum dispatch instead of trait generics (avoids monomorphization cost)
- Fallback C: Skip trait extraction entirely; implement `WgpuDevice` as a parallel struct with a top-level `match` at the `Renderer` entry point (less elegant, equally functional)

**Estimated scope**: The research document notes ~200k lines in WebRender, but GL surface
is bounded to `Device` + `Renderer`. Trait extraction touches only those two structs plus
their callsites within WebRender. Expected diff: 2,000–5,000 lines.

---

### P4 — Shader Translation Pipeline

**Objective**: Translate WebRender's GLSL shaders to WGSL via naga at build time, producing
a validated shader set for the wgpu backend.

**Entry conditions**:
- P3 complete (trait boundary exists, so shader requirements are fully enumerated)

**Tasks**:

| # | Task | Validation method |
|---|------|-------------------|
| P4.1 | Inventory all GLSL shaders in WebRender: count, vertex/fragment/compute classification, feature usage | Shader inventory document |
| P4.2 | Identify GLSL features used: extensions, precision qualifiers, built-in variables, texture sampling modes, UBO layouts, custom blend modes | Feature usage matrix |
| P4.3 | Set up a `build.rs` or standalone tool using `naga` to compile GLSL → WGSL | Tool runs, produces `.wgsl` files |
| P4.4 | Run naga translation on each shader; document any translation failures or warnings | Per-shader pass/fail log |
| P4.5 | For any shader that fails naga translation: identify the specific GLSL feature gap and produce a manual WGSL port | Manual port exists; compiles in naga |
| P4.6 | Validate translated shaders by loading them through `wgpu::Device::create_shader_module()` on a test device | All shaders load without error |
| P4.7 | Cross-reference translated uniform/binding layouts against the `WrDevice` trait's expected bind group structure | Layout compatibility confirmed |
| P4.8 | Store translated shaders in the fork under `webrender/res/wgpu/` with a build-time inclusion mechanism | Shaders available at compile time |

**Exit criteria**:
- All WebRender shaders have validated WGSL equivalents
- No runtime shader compilation is required
- Bind group layouts match the `WrDevice` trait expectations
- G2 evidence: shader parity demonstrated

**Rollback**: Shader files are additive; removing the `wgpu/` shader directory reverts.

**Answers research Q5.**

---

### P5 — wgpu Device Implementation

**Objective**: Implement `WgpuDevice` satisfying the `WrDevice` trait, providing wgpu
equivalents for all WebRender GPU resource management operations.

**Entry conditions**:
- P3 complete (trait boundary defined)
- P4 complete (shaders translated)

**Tasks**:

| # | Task | Validation method |
|---|------|-------------------|
| P5.1 | Implement `WgpuDevice` struct holding `Arc<wgpu::Device>`, `Arc<wgpu::Queue>` | Struct compiles |
| P5.2 | Implement texture management: `create_texture`, `update_texture`, `delete_texture` using `wgpu::Texture` + `write_texture` | Unit test: create → write → read-back matches input data |
| P5.3 | Implement atlas texture support for glyph and image caches | Unit test: atlas sub-region writes produce correct pixel data |
| P5.4 | Implement buffer management: vertex buffers (`COPY_DST \| VERTEX`), instance buffers, index buffers | Unit test: buffer write → draw → output matches expected geometry |
| P5.5 | Implement bind group management: uniform bind groups replacing GL UBOs | Unit test: uniform data accessible in shader |
| P5.6 | Implement render target management: `wgpu::Texture` with `RENDER_ATTACHMENT \| TEXTURE_BINDING` for intermediate targets | Unit test: render to intermediate → sample from intermediate → correct pixels |
| P5.7 | Implement shader program management: load translated WGSL shaders, create `wgpu::RenderPipeline` instances | All pipelines create without error |
| P5.8 | Implement draw call dispatch: `wgpu::RenderPass::draw()` / `draw_indexed()` with correct vertex/instance counts | Unit test: triangle renders correctly |
| P5.9 | Implement external image import stub (platform-specific; initially returns error) | Stub compiles; returns `Err` with documented reason |
| P5.10 | Run `WrDevice` trait compliance test suite (shared between GL and wgpu implementations) | All trait tests pass for `WgpuDevice` |

**Exit criteria**:
- `WgpuDevice` passes all `WrDevice` trait compliance tests
- Resource creation/destruction lifecycle is clean (no GPU resource leaks on drop)
- Device accepts an externally-provided `wgpu::Device` handle (shared ownership model)
- G2, G3 evidence: backend contract parity for device layer

**Rollback**: Remove `WgpuDevice` module from fork; `GlDevice` remains unaffected.

---

### P6 — wgpu Renderer Implementation

**Objective**: Implement `WgpuRenderer` satisfying the `WrRenderer` trait, orchestrating
wgpu render passes for WebRender's multi-pass architecture.

**Entry conditions**:
- P5 complete (`WgpuDevice` passes trait tests)

**Tasks**:

| # | Task | Validation method |
|---|------|-------------------|
| P6.1 | Implement shadow pass as `wgpu::RenderPass` with depth-only attachment | Visual: shadow renders match GL reference |
| P6.2 | Implement opaque pass as `wgpu::RenderPass` with color + depth attachments | Visual: opaque geometry renders correctly |
| P6.3 | Implement alpha pass as `wgpu::RenderPass` with blend state configuration | Visual: transparency and blend modes match GL reference |
| P6.4 | Implement composite pass as final `wgpu::RenderPass` writing to the compositor output texture | Visual: final composited frame renders |
| P6.5 | Implement per-pass load/store operations using `wgpu::RenderPassDescriptor` | Correct clearing and preservation behavior |
| P6.6 | Implement `wgpu::CommandEncoder` per-frame lifecycle: begin → encode passes → submit | Frame renders end-to-end |
| P6.7 | Implement render target switching between passes (intermediate textures ↔ output) | Multi-pass rendering produces correct final output |
| P6.8 | Implement batch draw call encoding (WebRender batching → wgpu draw calls) | Draw call counts match expectations |
| P6.9 | Implement texture sampling setup (glyph atlas, image cache, intermediate targets) | Text and images render correctly |
| P6.10 | Run WebRender integration test suite with `WgpuRenderer` + `WgpuDevice` | All integration tests pass |

**Exit criteria**:
- `WgpuRenderer` passes all `WrRenderer` trait compliance tests
- WebRender integration tests pass with wgpu backend selected
- Visual output for test scenes matches GL reference within §5.1 criteria (≤0.5% pixel diff)
- G2, G3 evidence: full backend contract parity

**Rollback**: Remove `WgpuRenderer` module; `GlRenderer` remains unaffected.

**Status (2026-04-03)**: Active. The original task breakdown assumed a trait-based
`WrRenderer` which does not exist in the current branch shape. Instead, wgpu rendering
is implemented as parallel methods on `Renderer` (e.g. `draw_passes_wgpu()`,
`draw_cache_target_tasks_wgpu()`). Effective status of each sub-task:

| # | Status | Notes |
|---|--------|-------|
| P6.1 | N/A | WebRender doesn't have a separate "shadow pass" — shadows are rendered via blur tasks in texture cache targets |
| P6.2 | Done | Opaque batches drawn front-to-back with depth write in both picture cache tiles and texture cache targets |
| P6.3 | Done | Alpha batches drawn back-to-front with per-batch blend mode via `blend_mode_to_wgpu()` (7 blend modes + fallback) |
| P6.4 | Done | Composite pass draws color tiles + textured tiles via `CompositeFastPath` shader to readback texture |
| P6.5 | Done | Per-target load/store ops: picture cache tiles clear to background color, texture cache targets use LoadOp::Load |
| P6.6 | Done | Encoder lifecycle: `take_encoder()` → record passes → `return_encoder()` → `flush_encoder()` per pass group |
| P6.7 | Done | Render targets resolved via `wgpu_texture_cache` HashMap keyed by `CacheTextureId` |
| P6.8 | Done | All `BatchKind` variants (Brush, Quad, TextRun, SplitComposite) routed to correct `WgpuShaderVariant` pipelines |
| P6.9 | Partial | Glyph atlas and image cache work via `TextureBindings`; some image variants (ANTIALIASING+REPETITION) not mapped |
| P6.10 | Active | The stale 225/366 snapshot is superseded: current branch work has closed resolve_ops, clip_masks, subpixel-text, YUV-composite, and graphics-metadata gaps. Focused local validation is green; next evidence needed is a fresh full `wrench --wgpu reftest` pass count on current HEAD |

---

### P7 — Compositor Output Contract

**Objective**: Define and implement the embedder-facing interface for receiving a
`wgpu::Texture` from WebRender's compositor output, replacing the current GL FBO readback.

**Entry conditions**:
- P6 complete (wgpu renderer produces correct frames)

**Tasks**:

| # | Task | Validation method |
|---|------|-------------------|
| P7.1 | Define the `CompositorOutputTexture` type: wraps `wgpu::TextureView` + metadata (dimensions, format, generation counter) | Type compiles |
| P7.2 | Implement `WgpuRenderer::compositor_output() -> CompositorOutputTexture` | Returns valid texture after frame render |
| P7.3 | Define the shared device handoff API (embedder provides `Arc<wgpu::Device>` + `Arc<wgpu::Queue>` to Servo at init) | API signature defined |
| P7.4 | Implement Servo-side device acceptance: `initialize_with_wgpu_device()` (or equivalent patch) | Servo accepts provided device; WebRender uses it |
| P7.5 | Prove zero-copy path: the `wgpu::Texture` returned by WebRender is directly bindable by the embedder without copy | Test: bind the texture in an egui_wgpu callback; pixels are visible |
| P7.6 | Implement texture pool for compositor output (pre-allocate N textures; rotate per frame to avoid GPU stalls) | Pool allocates on init; rotates without frame drops |
| P7.7 | Implement fallback path: if shared device handoff fails, fall back to copy-based texture transfer | Fallback produces correct output (with measured latency) |
| P7.8 | Emit diagnostics for texture handoff: `CHANNEL_COMPOSITOR_WGPU_HANDOFF_US_SAMPLE`, `CHANNEL_COMPOSITOR_WGPU_TEXTURE_POOL_HIT`, `CHANNEL_COMPOSITOR_WGPU_TEXTURE_POOL_MISS` | Channels emit during rendering |

**Exit criteria**:
- Embedder receives a `wgpu::TextureView` from WebRender after each frame
- Zero-copy path works when device is shared
- Copy-based fallback works when device is not shared
- Diagnostics channels emitting
- G2, G3 evidence: compositor output contract complete

**Rollback**: GL compositor output path remains available (feature-flagged).

**Answers research Q6.**

---

### P8 — Graphshell Integration Spike

**Objective**: Produce one running Graphshell instance where a Servo viewer tile renders
web content through the wgpu WebRender path, composited into an egui_wgpu frame.

**Entry conditions**:
- P7 complete (compositor output contract works)
- Graphshell `render_backend` module has `BackendContentBridgeMode::WgpuPreferredFallbackGlowCallback` wired

**Tasks**:

| # | Task | Validation method |
|---|------|-------------------|
| P8.1 | Wire `BackendContentBridgePolicy` to allow `ExperimentalEnvRequestedMode` when env var is set | Policy responds to env var |
| P8.2 | Implement wgpu bridge path in `CompositorAdapter`: receive `CompositorOutputTexture`, bind as `wgpu::TextureView` in egui_wgpu paint callback | Frame renders with web content visible |
| P8.3 | Remove GL state save/restore from wgpu bridge path (inherently isolated by command encoder scoping) | No GL calls on wgpu path |
| P8.4 | Verify chaos mode diagnostics: wgpu path should report no state violations (structural isolation) | Chaos mode passes with zero violations |
| P8.5 | Verify overlay affordance pass (Pass 3) renders over web content (Pass 2) on wgpu path | Focus ring visible over page content |
| P8.6 | Test with multiple composited tiles (at least 3 concurrent Servo viewers) | All tiles render correct content |
| P8.7 | Test tile resize behavior: resize tiles every frame, verify no frame drops or texture corruption | No visual artifacts during resize |
| P8.8 | Capture screenshot evidence for `#180` | Screenshots posted to tracker |

**Exit criteria**:
- Web content renders correctly in Graphshell through wgpu WebRender path
- Overlay affordances render correctly (Pass 3 over Pass 2)
- No GL state isolation violations
- `#180` evidence posted with screenshots and measurements
- G3, G4 evidence: pass-contract safety and platform confidence on primary platform

**Rollback**: Set env var back to default; Glow path activates; no behavior change for non-spike users.

---

### P9 — Pixel Parity Validation

**Objective**: Systematically compare wgpu path output against GL path output for a
defined reference set, proving visual correctness.

**Entry conditions**:
- P8 complete (integration spike works)

**Tasks**:

| # | Task | Validation method |
|---|------|-------------------|
| P9.1 | Define reference URL set: 10 pages covering static HTML, CSS compositing, text-heavy, images, WebGPU canvas | URL list documented |
| P9.2 | Capture GL-path screenshots for each reference URL at 1920×1080 | Reference images saved |
| P9.3 | Capture wgpu-path screenshots for each reference URL at same viewport | Comparison images saved |
| P9.4 | Run pixel diff analysis (per-pixel comparison with configurable tolerance) | Diff report generated |
| P9.5 | For each page: report pixel difference percentage | All pages ≤0.5% difference |
| P9.6 | Investigate and document any differences >0.5%: identify root cause (float precision, shader translation, blend mode, etc.) | Root causes documented |
| P9.7 | Fix or accept documented differences | Acceptance decisions recorded |
| P9.8 | Run focus ring pass-order validation: verify Pass 3 affordances occlude Pass 2 content at expected positions | Pixel inspection confirms correct z-order |

**Exit criteria**:
- All reference pages within ≤0.5% pixel difference or with documented acceptable deviations
- Focus ring z-order correct on wgpu path
- Pixel parity report posted to `#183`
- G5 evidence: regression envelope validated

**Rollback**: N/A (measurement phase).

---

### P10 — Performance Validation

**Objective**: Measure wgpu path frame timings against the targets defined in research §5.3,
proving the wgpu path meets frame budget requirements.

**Entry conditions**:
- P8 complete (integration spike works)
- Can run in parallel with P9

**Tasks**:

| # | Task | Validation method |
|---|------|-------------------|
| P10.1 | Instrument wgpu `CommandEncoder` with timestamp queries for per-pass timing | Timing data collected |
| P10.2 | Measure Servo wgpu render time per active tile at 1080p | Target: ≤4 ms per tile |
| P10.3 | Measure texture handoff latency (WebRender `queue.submit()` to Graphshell texture bind) | Target: ≤0.5 ms |
| P10.4 | Measure resize handling: trigger tile rect changes every frame, measure texture recreation cost | Target: no frame drops |
| P10.5 | Measure device contention: graph canvas wgpu pass + Servo wgpu pass simultaneously | Target: no deadlock, no throughput regression |
| P10.6 | Compare total frame time (GL path vs wgpu path) for reference URL set | wgpu path ≤110% of GL path total frame time |
| P10.7 | Profile GPU memory usage: texture pool overhead, buffer allocations | Documented; no memory leaks over 1000 frames |
| P10.8 | Produce performance validation report | Report posted to `#183` and `#180` |

**Exit criteria**:
- All frame budget targets met or exceeded
- No deadlocks under concurrent tile rendering
- No GPU memory leaks
- Performance report posted with raw data
- G5 evidence: performance within regression envelope

**Rollback**: N/A (measurement phase).

---

### P11 — Platform Matrix Validation

**Objective**: Validate the wgpu WebRender path on each target platform, documenting
pass/fail and platform-specific limitations.

**Entry conditions**:
- P8 complete (integration spike works on primary platform)

**Tasks**:

| # | Task | Validation method |
|---|------|-------------------|
| P11.1 | Windows (DX12): full spike + pixel parity + performance | Pass/fail with evidence |
| P11.2 | Windows (Vulkan fallback): full spike | Pass/fail with evidence |
| P11.3 | macOS (Metal): full spike + pixel parity | Pass/fail with evidence |
| P11.4 | Linux (Vulkan): full spike + pixel parity | Pass/fail with evidence |
| P11.5 | Linux (GL fallback): document expected behavior (GL→wgpu copy path or GL-only) | Documented limitation |
| P11.6 | For each platform: test external image import (DXGI shared handle / MTLTexture IOSurface / VkImage DMABuf) | Per-platform capability documented |
| P11.7 | For each platform: test GL fallback path when wgpu is unavailable | Fallback activates correctly |
| P11.8 | Produce platform matrix report | Report posted to `#183` |

**Exit criteria**:
- Primary platform (Windows DX12) passes all validation
- Each platform has explicit pass/fail documentation
- Platform-specific limitations are tagged as switch blockers or non-blockers
- GL fallback works on all platforms
- G4 closed: platform confidence established

**Rollback**: N/A (validation phase).

**Answers research Q3 partially.**

---

### P12 — Production Cutover Preparation

**Objective**: Assemble all evidence required for switch authorization and prepare the
runtime policy change from `GlowBaseline` to wgpu-primary.

**Entry conditions**:
- P9, P10, P11 complete (all validation phases done)
- G1–G5 all closed with linked evidence

**Tasks**:

| # | Task | Validation method |
|---|------|-------------------|
| P12.1 | Verify G1–G5 closure: each gate has linked tracker evidence | Gate checklist complete |
| P12.2 | Change `active_backend_content_bridge_policy()` to return `WgpuPrimaryFallbackGlow` (new policy variant) | Policy change compiles |
| P12.3 | Add `BackendContentBridgePolicy::WgpuPrimaryFallbackGlow` variant: selects wgpu when capable, falls back to Glow when not | Policy logic tested |
| P12.4 | Update `BackendContentBridgeCapabilities` probe to detect actual wgpu WebRender availability | Probe returns correct capability state |
| P12.5 | Run full test suite with wgpu-primary policy | All tests green |
| P12.6 | Run headed smoke tests with wgpu-primary policy | Visual correctness confirmed |
| P12.7 | Document rollback procedure: revert policy function to return `GlowBaseline` | Rollback tested |
| P12.8 | Produce switch authorization receipt for `#183` | Receipt posted with all evidence links |
| P12.9 | Post final `#180` closure comment with measured evidence | `#180` closure criteria met |

**Exit criteria**:
- Switch authorization receipt posted with G1–G5 evidence
- `#180` closed with measured evidence
- Glow retirement timeline documented (not immediate — retained as fallback until one full release cycle confirms stability)

**Rollback**: Change `active_backend_content_bridge_policy()` back to `GlowBaseline`. One line change.

---

## 3. Dependency Graph

```
P0 (Dependency Audit)
P1 (Upstream Recon)
    │         │
    └────┬────┘
         │
    P2 (Fork + Patch)
         │
    P3 (Trait Extraction)
         │
    P4 (Shader Translation)
         │
    P5 (wgpu Device)
         │
    P6 (wgpu Renderer)
         │
    P7 (Compositor Output)
         │
    P8 (Integration Spike)
       / │  \
      /  │   \
  P9   P10   P11
 (Parity)(Perf)(Platform)
      \  │   /
       \ │  /
    P12 (Cutover)
```

P0 and P1 run in parallel. P2 depends on both. P3–P7 are strictly sequential.
P9, P10, P11 can run in parallel after P8. P12 depends on all three.

---

## 4. Shared Device Ownership Model

This section specifies the GPU device ownership model referenced throughout the phases.

### 4.1 Ownership Hierarchy

```
Graphshell (main)
  ├── creates wgpu::Instance
  ├── selects wgpu::Adapter
  ├── creates wgpu::Device + wgpu::Queue
  │
  ├── passes Arc<wgpu::Device> + Arc<wgpu::Queue> to:
  │   ├── egui_wgpu::Renderer (Graphshell's UI rendering)
  │   ├── Servo/WebRender (compositor output rendering)
  │   └── Future compute workloads (Burn, AI inference)
  │
  └── owns device lifetime (drop on app exit)
```

### 4.2 Key Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Who creates the device? | Graphshell | Embedder must control device lifetime and feature requirements |
| How does Servo receive the device? | `Arc<wgpu::Device>` passed at Servo initialization | Zero-copy texture sharing requires same device |
| How does egui_wgpu get the device? | Use `egui_wgpu::Renderer` directly (not `egui_wgpu::winit::Painter`) | `Painter` creates its own device; `Renderer` accepts an external one |
| What happens if shared device init fails? | Fall back to separate devices + copy-based texture transfer | Correctness preserved; performance degraded |

### 4.3 Implementation Notes

The `egui_wgpu::Renderer` constructor accepts:
```rust
pub fn new(
    device: &wgpu::Device,
    output_color_format: wgpu::TextureFormat,
    output_depth_format: Option<wgpu::TextureFormat>,
    msaa_samples: u32,
    dithering: bool,
) -> Self
```

This means Graphshell can create the `wgpu::Device` first, then pass a reference to both
`egui_wgpu::Renderer` and to Servo's WebRender initialization. The Servo side requires
a new API or patch to accept the device (see P7.3–P7.4).

---

## 5. WebRender GL Surface Audit Template

Phases P3 and P5 require a detailed audit of WebRender's GL usage. This template defines
what the audit must capture.

### 5.1 Device-Layer GL Calls to Map

| GL Operation Category | Representative Calls | wgpu Equivalent | Notes |
|-----------------------|---------------------|-----------------|-------|
| **Texture creation** | `glGenTextures`, `glTexImage2D`, `glTexSubImage2D` | `device.create_texture()`, `queue.write_texture()` | Atlas textures need sub-region update |
| **Texture binding** | `glBindTexture`, `glActiveTexture` | Bind groups; `set_bind_group()` in render pass | No direct equivalent to `glActiveTexture`; bind groups are explicit |
| **Buffer creation** | `glGenBuffers`, `glBufferData`, `glBufferSubData` | `device.create_buffer()`, `queue.write_buffer()` | Usage flags: `VERTEX`, `INDEX`, `UNIFORM`, `COPY_DST` |
| **VAO setup** | `glVertexAttribPointer`, `glEnableVertexAttribArray` | `VertexBufferLayout` in `RenderPipelineDescriptor` | Static layout; defined at pipeline creation |
| **Framebuffer ops** | `glGenFramebuffers`, `glBindFramebuffer`, `glFramebufferTexture2D` | `RenderPassDescriptor` targeting `TextureView` | Per-pass, not persistent FBO objects |
| **Shader compilation** | `glCreateShader`, `glShaderSource`, `glCompileShader`, `glLinkProgram` | `device.create_shader_module()`, `device.create_render_pipeline()` | Build-time via naga (P4) |
| **State setting** | `glEnable/glDisable`, `glBlendFunc`, `glDepthFunc`, `glScissor`, `glViewport` | Pipeline state in `RenderPipelineDescriptor`; scissor rect in `set_scissor_rect()` | Static per-pipeline or dynamic per-render-pass |
| **Draw calls** | `glDrawArrays`, `glDrawElements`, `glDrawArraysInstanced` | `render_pass.draw()`, `render_pass.draw_indexed()` | Direct mapping |
| **Readback** | `glReadPixels` | `buffer.map_async()` + staging buffer copy | Async; needs fence or callback |
| **Clear** | `glClear`, `glClearColor` | `RenderPassDescriptor::color_attachments[].ops.load = LoadOp::Clear` | Per-pass clear via load op |

### 5.2 Renderer-Layer Pass Structure to Map

| WebRender Pass | GL Operations | wgpu Pass Structure |
|----------------|--------------|---------------------|
| **Shadow pass** | Depth-only FBO, depth writes | `RenderPass` with depth attachment only; `LoadOp::Clear` depth |
| **Opaque pass** | Color + depth FBO, depth test, no blending | `RenderPass` with color + depth; blend state disabled |
| **Alpha pass** | Color FBO, depth test, blending enabled, blend func per-batch | `RenderPass` with color; blend state per-pipeline |
| **Composite pass** | Full-screen quad, texture sample from intermediate targets | `RenderPass` targeting output texture; bind intermediate as input |

---

## 6. Risk Register

| Risk | Likelihood | Impact | Mitigation | Phase |
|------|-----------|--------|------------|-------|
| wgpu version skew between Servo and egui_wgpu | Medium | High (blocks integration) | P0 audits this first; version unification patch if needed | P0 |
| Upstream Servo starts competing wgpu renderer work | Low | Medium (coordination overhead) | P1 monitors upstream; align rather than compete | P1 |
| WebRender `Device`/`Renderer` too entangled for clean trait extraction | Medium | High (P3 redesign) | P3 defines three fallback approaches (enum dispatch, partial extraction, parallel struct) | P3 |
| Shader translation via naga fails for custom WebRender blend modes | Medium | Medium (manual porting) | P4 allows manual WGSL ports for failing shaders | P4 |
| Windows DX12 external image import limitation | Medium | Low (copy fallback exists) | P7 implements copy-based fallback; P11 documents limitation | P7, P11 |
| `egui_wgpu::Renderer` cannot accept externally-created device | Low | High (ownership model breaks) | P8 validates this early; fallback: use `egui_wgpu` at lower level | P8 |
| Frame budget regression on wgpu path | Medium | Medium (delays cutover) | P10 measures early; optimize before P12 | P10 |
| Upstream Servo API change breaks fork patch | Medium | Medium (rebase work) | Pin fork to specific Servo commit; rebase on schedule | P2 ongoing |

---

## 7. Validation Matrix Summary

This matrix maps each research document QA criterion (§5) to the phase that validates it.

| QA Criterion | Research §  | Validation Phase | Method |
|-------------|-------------|-----------------|--------|
| Static page pixel parity | §5.1 | P9 | Screenshot diff ≤0.5% |
| Glyph rendering parity | §5.1 | P9 | Visual comparison |
| CSS compositing parity | §5.1 | P9 | Blend mode pixel diff |
| WebGPU canvas correctness | §5.1 | P9 | Canvas content pixel verification |
| Focus ring pass-order | §5.1 | P8, P9 | Pixel inspection: ring over content |
| GL state isolation (wgpu: structural) | §5.2 | P8 | Chaos mode: zero violations on wgpu |
| Servo wgpu render time | §5.3 | P10 | Timestamp queries ≤4 ms/tile |
| Texture handoff latency | §5.3 | P10 | Measured ≤0.5 ms |
| Resize handling | §5.3 | P10 | No frame drops |
| Device contention | §5.3 | P10 | No deadlock, no regression |
| GL fallback on capability miss | §5.4 | P11 | Probe returns false; Glow activates |
| Mixed-mode stability | §5.4 | P11 | No corruption with mixed paths |
| Capability probe accuracy | §5.4 | P12 | Probe matches actual availability |
| Replay diagnostics parity | §5.5 | P7, P8 | wgpu channels emit; inspector shows data |

---

## 8. Upstreaming Execution Sequence

Per research §6, upstreaming is staged after local evidence.

| Stage | Timing | Action | Evidence required |
|-------|--------|--------|-------------------|
| **U0** Monitor | P1 onward | Watch Servo upstream for wgpu renderer activity | None |
| **U1** Spike evidence | After P8 | Share integration spike results with Servo contributors | Screenshots, measurements, architecture description |
| **U2** Trait extraction PR | After P3 proven stable | Propose backend trait extraction to Servo/WebRender | P3 exit criteria met; WebRender tests passing |
| **U3** wgpu Device/Renderer PR | After P6 proven stable | Propose wgpu backend addition (feature-gated) | P6 exit criteria met; pixel parity initial evidence |
| **U4** Embedding API PR | After P7 proven | Propose `initialize_with_wgpu_device()` embedding API | P7 exit criteria met; shared device model validated |
| **U5** Full upstream merge | After P12 | Retire local fork; consume upstream directly | All gates closed; upstream has equivalent capability |

**Fallback**: If upstream declines trait extraction (U2) or wgpu backend (U3), Graphshell
maintains the fork indefinitely. The `[patch.crates-io]` mechanism (P2) supports this
without architectural compromise. The C+F contract ensures the GL path remains valid
regardless of upstream decisions.

---

## 9. Effort Estimates

These are rough order-of-magnitude estimates. Actual effort depends on WebRender internals
discovered during P3.

| Phase | Estimated effort | Confidence | Notes |
|-------|-----------------|------------|-------|
| P0 | 1–2 days | High | Dependency analysis and documentation |
| P1 | 1–2 days | High | Research and coordination |
| P2 | 2–3 days | High | Fork setup and build verification |
| P3 | 2–4 weeks | Medium | Largest refactoring risk; depends on WebRender coupling |
| P4 | 1–2 weeks | Medium | Shader count and naga coverage determine effort |
| P5 | 2–4 weeks | Low | Depends on P3 trait surface size |
| P6 | 2–4 weeks | Low | Depends on pass complexity discovered in P3 |
| P7 | 1–2 weeks | Medium | Servo embedding API change is the hard part |
| P8 | 1–2 weeks | Medium | Integration plumbing |
| P9 | 3–5 days | High | Mostly automated comparison |
| P10 | 3–5 days | High | Instrumentation and measurement |
| P11 | 1–2 weeks | Medium | Platform access and CI setup |
| P12 | 2–3 days | High | Evidence assembly and policy change |

**Total estimated range**: 3–5 months of focused work (one contributor).

**Critical path**: P3 (trait extraction) is the gating phase. If P3 proves tractable (≤2 weeks),
the total estimate compresses. If P3 reveals deep entanglement requiring fallback approaches,
the total estimate extends.

---

## 10. Relationship to Active Planning

### 10.1 Readiness Gate Mapping

| Gate | Closing phase(s) | Evidence type |
|------|-------------------|---------------|
| G1 (Dependency control) | P0, P2 | Version matrix, build proof, rollback proof |
| G2 (Backend contract parity) | P3, P4, P5, P6, P7 | Trait tests, shader parity, device/renderer compliance |
| G3 (Pass-contract safety) | P5, P6, P7, P8 | Pass-order tests, chaos mode, overlay affordance z-order |
| G4 (Platform confidence) | P8, P11 | Per-platform pass/fail evidence |
| G5 (Regression envelope) | P9, P10 | Pixel parity report, performance report |

### 10.2 C+F Policy Alignment

This plan is a detailed execution of the C+F contract's **F (Fallback-safe)** leg:

- Every phase preserves the GL path as a working fallback
- The wgpu path is feature-gated and policy-controlled throughout
- `active_backend_content_bridge_policy()` remains `GlowBaseline` until P12 switch authorization

### 10.3 Feature Guardrail Compliance

All feature work during plan execution must continue to follow the guardrails in
`2026-03-01_webrender_readiness_gate_feature_guardrails.md`:

- No new renderer-specific coupling in UI/workflow code
- Bridge metadata preservation for any render-path work
- Fallback-safe behavior for any new capability
- Receipt-linked evidence for migration-adjacent slices

### 10.4 Tracker Linkage

- Primary tracker: `#183` (backend migration)
- Spike evidence target: `#180` (runtime-viewer GL→wgpu bridge)
- Related lanes: `#88` (stabilization), `#90` (embedder-debt), `#92` (viewer-platform), `#99` (spec-code-parity)
- Foundation issues: `#166` (replay traces), `#171` (chaos mode), `#167` (differential composition), `#168` (GPU budget), `#169` (hot-swap), `#170` (telemetry schema)

---

## 11. Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-01 | Adopt parallel `Device` + `Renderer` wgpu implementation inside WebRender (research §2.3) | Cleanest `#180` closure; eliminates GL state chaos; aligns with upstream intent |
| 2026-03-01 | Graphshell owns `wgpu::Device`; Servo receives shared handle (research §3.5) | Enables zero-copy texture handoff; aligns with `egui_wgpu::Renderer` constructor model |
| 2026-03-01 | Use naga for build-time GLSL→WGSL shader translation (research §3.4) | Zero runtime cost; naga is wgpu's native compiler; catches errors at build time |
| 2026-03-01 | Fork-and-patch for spike; upstream-first for stable changes (research §6.1) | Evidence-first approach; avoids premature upstream proposals |
| 2026-03-01 | GL path retained behind feature flag indefinitely during bring-up (C+F contract) | Fallback-safe; no forced migration until all gates close |

---

## 12. Open Questions Inherited from Research

These remain open. Each is mapped to the phase that will resolve it.

| # | Question | Resolving phase |
|---|---------|-----------------|
| Q1 | Servo's wgpu version vs egui_wgpu's wgpu version compatibility | P0 |
| Q2 | Active upstream Servo/WebRender wgpu renderer work? | P1 |
| Q3 | Windows DX12 external image import support in wgpu? | P11 |
| Q4 | Can `egui_wgpu::Renderer` accept Graphshell-owned device? | P0 (API check), P8 (integration proof) |
| Q5 | WebRender shader inventory: count, GLSL feature usage, naga translation coverage? | P4 |
| Q6 | Is Servo `OffscreenRenderingContext` extensible to return `wgpu::Texture`? | P7 |
