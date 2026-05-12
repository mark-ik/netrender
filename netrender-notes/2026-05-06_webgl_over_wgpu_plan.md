# netrender — WebGL-over-wgpu lane (2026-05-06)

Sub-plan extracted from
[`2026-05-04_feature_roadmap.md`](2026-05-04_feature_roadmap.md) Phase G.
Lives as its own design doc because the work is multi-month and reaches
beyond netrender's API surface; the roadmap holds a single pointer entry.

## 1. Why this is a separate lane

Web pages do not get raw OpenGL; they get WebGL/WebGL2. The target
architecture is **WebGL API compatibility over wgpu**, not GL retention.
This lane should live beside NetRender, not inside NetRender core:

```text
WebGL DOM binding / canvas context
    -> WebGL-over-wgpu state machine + validator + translator
    -> wgpu Texture output for the canvas
    -> NetRender / Pelt composites that texture into the page scene
```

NetRender's job is the final composition surface: place the canvas
texture in painter order, clip/transform it, and participate in damage
and presentation. It should not own WebGL's API state machine,
extension matrix, shader-language validation, draw-call validation, or
resource-lifetime semantics.

The alternative wgpu-graft / external-producer bridge remains a
stopgap: useful for importing an existing producer's texture when a
consumer forces it, but not the strategic implementation path. Do not
let it become the WebGL plan unless there is immediate external
pressure.

## 2. Sub-phases

Each phase ships independently; later phases depend on earlier ones.
G4 is the only sub-phase that lands inside the netrender repo; G0–G3
and G5–G6 are sibling-crate or test-infra work.

### G0. Ownership and crate boundary

*Shape:* create a sibling WebGL-over-wgpu adapter crate under the
Serval/Pelt side, with NetRender only seeing a produced
`wgpu::Texture` / `TextureView` / surface handle plus size, format,
alpha mode, generation, and damage metadata.

*Done condition:* a `webgl_canvas_to_netrender_texture` smoke compiles
without `glow`, GLES, EGL, WGL, ANGLE, or ServoShell dependencies.

### G1. WebGL 1 baseline state machine

*Shape:* implement the minimum WebGL 1 context object over wgpu:
buffers, vertex attributes, textures, framebuffers, renderbuffers,
viewport/scissor, clear, drawArrays, drawElements, readPixels, context
loss, and WebGL error generation. Start with the canonical validation
behavior, not a "whatever wgpu accepts" wrapper.

*Done condition:* a tiny conformance subset renders triangle / indexed
geometry / texture / framebuffer scenes into an RGBA8 wgpu texture,
then readback matches expected pixels.

### G2. GLSL ES validation and WGSL translation

*Shape:* treat GLSL ES as the web-facing language contract and WGSL as
the device language. The adapter must validate WebGL shader rules
before pipeline creation, translate to WGSL or an intermediate form
that produces WGSL, and preserve WebGL's attribute/uniform/linking
semantics. Do not make authored GL runtime shader text part of
NetRender.

**Strategy: extend the existing webrender-wgpu translator, don't start
fresh.** Full analysis in [§3 below](#3-g2-translation-strategy). The
short version: a complete GLSL→naga→WGSL translator already exists in
the `webrender_build/src/wgsl.rs` archive (412/412 wgpu reftests
passing on 2026-04-08; 413/413 after the wgpu 29 bump on 2026-04-10).
Port that as the baseline, drop the WebRender-only passes, replace
precision-stripping with ESSL precision-propagation, and build the
WebGL validator as a separate layer above the translator.

*Done condition:* compile/link tests cover vertex+fragment pairs,
attribute binding, uniforms/samplers, precision qualifiers, common
built-ins, failed compile/link diagnostics, and a small fragment
shader rendering oracle.

*Sub-phases* (sequenced inside G2 for clarity; details in §3):

- **G2.a** — port the existing translator into a runtime crate;
  re-baseline against the WebRender shader corpus as a regression gate.
- **G2.b** — extend ESSL 1.00 / 3.00 coverage (precision propagation,
  ES-only built-ins, sampler-type variants).
- **G2.c** — build the WebGL validator above the translator (linking,
  errors, ESSL grammar restrictions, undefined-behavior gating).
- **G2.d** — wire the WebGL CTS as the conformance gate; ANGLE-as-
  translator stays as a fallback, only triggered if a CTS class can't
  be cleared inside this stack.

### G3. Resource and synchronization contract

*Shape:* define when a canvas texture is ready for NetRender to
sample, how resize reallocates, how framebuffer writes become visible
to the compositor, how readPixels maps/stages data, and how context
loss tears down GPU resources. This should be a plain Pelt/Serval
contract, not an implicit side effect of a GL context.

*Done condition:* tests cover canvas resize, multiple frames,
readPixels after draw, context loss/recreation, and texture generation
changes visible to NetRender composition.

### G4. Texture compositing into NetRender (netrender-side hook)

*Shape:* expose a NetRender-side primitive or adapter helper for
externally produced wgpu textures that are on the same device. The
primitive must carry size, format, alpha, color-space, transform,
clip, and damage metadata. If cross-device import becomes necessary,
route it through a separate interop adapter; do not pollute the
same-device WebGL-over-wgpu path.

*Done condition:* a WebGL canvas texture appears in a NetRender scene
with correct z-order over text/rects, correct clipping, correct alpha,
and no readback round trip.

### G5. WebGL 2 and extension ladder

*Shape:* after WebGL 1 smoke is stable, add WebGL 2 features in
measured batches: VAOs, instancing, multiple render targets, 3D
textures, transform feedback strategy, integer textures, and the
extensions that appear in real WPT/compat pressure. Each extension
gets an explicit mapping to wgpu capability/limits.

*Done condition:* a documented feature matrix lists implemented,
intentionally unsupported, and blocked-by-wgpu-limit features; WPT
expectations point at those buckets.

### G6. Conformance and WPT gates

*Shape:* use WebGL CTS / WPT as the normative target. In-tree demos
are smoke tests only; they cannot define compatibility. Build a small
first gate, then grow it by feature bucket.

*Done condition:* CI has a named WebGL-over-wgpu smoke suite plus an
opt-in conformance job. Failures are bucketed by API validation,
shader translation, resource behavior, or rendering mismatch.

## 3. G2 translation strategy

This section captures the prior-art findings that make G2 an
extend-and-shed effort rather than a green-field build. Read before
estimating G2 scope.

### 3.1 Prior art: the webrender-wgpu GLSL→WGSL translator

A complete GLSL→naga→WGSL translation pipeline shipped on the
`wgpu-backend-0.68-minimal` branch and produced reftest-passing
results:

- **412/412 wgpu reftests passing** ([2026-04-08 live confirmation](archive/2026-04-08_live_full_reftest_confirmation.md)).
- **413/413 after the wgpu 29 bump** ([P15 progress report](archive/progress/2026-04-10_p15_progress_report.md)).
- **61/61 WGSL variants translating** for WebRender's full shader
  corpus, with every naga limitation and workaround documented in the
  [shader translation journal](archive/legacy/shader_translation_journal.md).

The translator lives at `webrender_build/src/wgsl.rs` on the
`origin/wgpu-backend-0.68-minimal` branch (~2319 lines); the driver
is `webrender/build.rs`. To inspect locally, recreate the worktree:
`git worktree add ../webrender-wgpu-upstream wgpu-backend-0.68-minimal`
(the prior worktree at that path is registered as prunable — the
branch on origin is the canonical reference).

The pipeline was retired not because it failed, but because the
April-18 SPIR-V plan attempted to remove the preprocessing tower
in favor of authored SPIR-V, and the April-28 idiomatic-WGSL plan
then dropped runtime translation entirely once the dual-consumer
(Servo GL + Servo wgpu) requirement collapsed. Translation
viability was never the failure mode.

### 3.2 Dependencies (thin)

The whole 2319-line translator depends on, for the wgsl path:

```toml
naga = { version = "26.0", features = ["glsl-in", "wgsl-out"] }
bitflags = "2"
lazy_static = "1"
```

No `tree-sitter`, LALRPOP, `shaderc`, glslang, or external
preprocessor. Naga is the only translation engine; everything else
is bespoke text passes around it (paren-balanced scanners,
word-boundary `replace_word`, line-based `#ifdef` resolution).

### 3.3 Architecture: pre-pass + naga + post-pass

```text
GLSL source
  → ~1700 lines of text passes (preprocess_for_naga)
  → ~60 lines of naga driving (translate_to_wgsl)
  → ~300 lines of text fixups on naga's WGSL output (fix_generated_wgsl)
  → wgpu pipeline
```

Three production-grade hardenings worth keeping verbatim:

- **8 MB stack thread.** Naga's validator does recursive flow
  analysis that overflows Windows' default stack on big shaders. The
  translator spawns naga on
  `std::thread::Builder::new().stack_size(8 * 1024 * 1024)`.
- **Panic catch.** `std::panic::catch_unwind` around the
  parse/validate/emit, because naga's validator can panic on
  malformed intermediate IR. For our prior corpus this was hardening;
  for adversarial WebGL input it is a load-bearing safety boundary.
- **Post-naga WGSL fixups.** Naga's *output* is patched for
  valid-but-rejected-by-wgpu WGSL constructs
  (`fix_generated_wgsl`, `strip_dead_adata_input`,
  `rewrite_set_sat_helpers`). Some may have aged out since 2026-04;
  re-baseline against current naga + wgpu before porting.

The 16 transforms factor as:

| Pass | Lines | Purpose |
| --- | --- | --- |
| `resolve_stage_ifdefs` | ~100 | strip inactive `WR_VERTEX_SHADER` / `WR_FRAGMENT_SHADER` blocks |
| `move_definitions_before_prototypes` | ~265 | naga forward-dependency reorder |
| `fix_switch_fallthrough` | ~750 | 6 sub-passes for WGSL-incompatible switch shapes |
| `decompose_matrix_varyings` | ~180 | mat3/mat4 varyings → column vectors |
| `rewrite_texel_fetch_offset` | ~75 | naga `texelFetchOffset` shape |
| `decompose_array_struct_stores` | ~70 | one specific WR shader idiom |
| `rewrite_sampler_params` | ~55 | function params taking `sampler2D` |
| `strip_precision` | ~25 | drop `highp` / `mediump` / `lowp` |
| `preprocess_for_naga` driver | ~260 | sampler-split + location assignment + orchestration |

### 3.4 WebGL context: drops, reshapes, wins, reframings

WebRender's input was *our* shader corpus under a build-time tower.
WebGL's input is *page-author* GLSL ES at runtime. The asymmetry
cuts in our favor more often than against:

**Drops — WebRender-only baggage that simply doesn't apply:**

- `resolve_stage_ifdefs` (~100 lines). WebGL ships VS and FS as
  separate strings. No combined source, no `WR_VERTEX_SHADER` ifdefs.
- `PER_INSTANCE` qualifier handling. WR convention; WebGL has
  standard `attribute` / `in` and (in WebGL 2) `gl_VertexID` /
  `gl_InstanceID` built-ins.
- `decompose_array_struct_stores` (~70 lines). Existed for one WR
  shader (`ps_split_composite`).
- `webrender_build::shader::*` shader-feature flag plumbing and the
  `#include`-expansion infrastructure. WebGL has no `#include`.

Roughly 250–400 lines drop out wholesale.

**Reshapes — ESSL needs different work, not no work:**

- **Precision quals: strip → preserve and propagate.** The single
  biggest reshape. WebRender stripped `highp` / `mediump` / `lowp`
  because GLSL 4.50 rejects them; for WebGL they are load-bearing
  canonical syntax with explicit semantics (defaults vary by stage
  and type, statement-scope precision blocks are valid, ESSL 1.00
  vs 3.00 differ). The right move is a map from ESSL precision →
  WGSL storage choice, not a delete. More work, but cleaner work.
- **Sampler split: still needed, broader coverage.** WebRender hit
  `sampler2D`. WebGL has `sampler3D` (WebGL 2), `samplerCube`,
  `sampler2DArray`, `sampler2DShadow`, etc. Same shape of fix.
- **WebGL 2 = ESSL 3.00.** Naga's `glsl-in` accepts both ESSL
  versions. Two frontends instead of one, but it's a parameter to
  naga, not a separate tower.

**Wins — WebGL constraints make some passes redundant:**

- WebGL 1 forbids recursion, `goto`, dynamic-bound `for` loops in
  many profiles. The matrix-varying decomposition (~180 lines) and
  switch fall-through tower (~750 lines) handled valid-but-uncommon
  WR-corpus patterns. Switch fall-through in particular may not need
  to ship if no real WebGL CTS shader exercises it. Activate on
  demand based on conformance failures.
- WebGL has a normative test suite (WebGL CTS + WPT). The
  translator's evolution becomes test-driven instead of
  bug-by-bug. We don't guess what to support; we run CTS and see
  what fails.

**Reframings — same problem, different shape:**

- **Validation lives above translation, not inside.** WebRender
  entangled "is this shader valid" with "did it translate." For
  WebGL the spec mandates a separate validation layer above
  translation: error generation matching `getError()`, attribute /
  uniform linking checks, ESSL grammar restrictions naga doesn't
  model, undefined-behavior gating. New work, but cleaner factoring:
  validate → translate → build, three layers, three error buckets.
- **Runtime caching is meaningful.** WebRender translated 61 variants
  once at build time. WebGL pages compile shaders at runtime. A
  translator cache keyed on
  `(source_hash, essl_version, context_options)` is a real
  performance lever WebRender never needed.
- **Adversarial input.** Naga panic + stack-size wrapper changes
  posture from "production hardening" to "safety boundary." Same
  code, much higher importance.

### 3.5 Worth keeping verbatim

- Naga panic-catch + 8 MB stack-size wrapper.
- Paren-balanced function scanner.
- Word-boundary `replace_word`.
- The pre-pass / naga / post-pass three-stage architecture.
- Cross-stage binding agreement (the fixed-binding-table approach).
  WebGL's `glLinkProgram` enforces VS/FS interface matching at the
  spec level; same problem, same shape of fix.

### 3.6 Rough budget

| Category | Lines |
| --- | --- |
| WR-only passes dropped | -300 to -400 |
| ESSL precision propagation (new) | +200 to +300 |
| Sampler-type variants (extended) | +80 to +120 |
| Reused unchanged | ~1200 |
| **Translator total** | **~1400–1600** (vs. 2319 today) |
| WebGL validator layer above (new, separate module) | +800 to +1500 |
| Runtime cache | +150 to +250 |

The translator lands modestly smaller; the validator above it is
the major new build. Not a rewrite — an extend-and-shed.

### 3.7 ANGLE as escape hatch, not primary path

ANGLE-as-translator (Chromium's WebGL → SPIR-V/HLSL/MSL path) stays
in scope as a fallback only:

- If a WebGL CTS class can't be cleared inside the
  naga-derived stack despite a reasonable extension effort.
- If license, build complexity, or wasm compatibility forces it.

Until then, the journal-derived path is strictly better-leveraged:
working code, documented receipts, no external dependency.

---

## 4. First work lane: same-device canvas texture slice

Added 2026-05-11 after reviewing the roadmap, vello rasterizer plan,
path-(b′) compositor surface API, Serval's `RenderingContextCore` /
`WgpuCapability` seam, and the existing netrender `insert_image_vello`
Path B hook.

This is the first executable lane through G0→G4. It deliberately does
not try to clear full WebGL correctness. The point is to prove the
ownership boundary and texture handoff first, then grow the WebGL state
machine and shader translator behind that boundary.

Status as of 2026-05-11: W0 is repaired locally. W1's Serval-side
adapter crate shell and W2's netrender-side zero-copy composition
receipt are implemented. W3, W4, and the broader W5 gate remain
pending.

### W0. Repair the map

**CLEARED 2026-05-11.** The canonical path-(b′) design note exists at
`2026-05-05_compositor_handoff_path_b_prime.md`; the roadmap, progress
index, vello rasterizer plan, and `netrender_device::compositor` links
all resolve locally.

*Shape:* restore or relocate the referenced
`2026-05-05_compositor_handoff_path_b_prime.md` plan, or update the
roadmap / progress / vello-plan links to the canonical location. The
code already contains the path-(b′) API and receipts; this step keeps
the design map aligned with the shipped API.

*Done condition:* `PROGRESS.md`, the roadmap D3 entry, the vello
rasterizer plan, and `netrender_device::compositor` all link to a file
that exists locally.

### W1. Adapter crate shell and texture contract

**CLEARED 2026-05-11.** Serval now has `servo-webgl-wgpu` under
`components/webgl-wgpu`. It depends on `paint_api`'s `wgpu_backend`
capability and `wgpu`, exposes `WebGlCanvasTexture`, allocates on the
shared device, supports resize and clear, and has a focused
`webgl_canvas_to_netrender_texture_allocates_resizes_and_clears` smoke.

*Shape:* create the sibling WebGL-over-wgpu crate on the Serval/Pelt
side, not in netrender. Preferred first home is `serval/components/`
unless the Pelt integration work needs it under `ports/` temporarily.
The crate exposes a narrow canvas output type, roughly:

```rust
pub struct WebGlCanvasTexture {
  pub texture: wgpu::Texture,
  pub size: (u32, u32),
  pub format: wgpu::TextureFormat,
  pub alpha_mode: CanvasAlphaMode,
  pub generation: u64,
  pub damage: Option<[u32; 4]>,
}
```

The initial texture usage should be at least
`RENDER_ATTACHMENT | TEXTURE_BINDING | COPY_SRC | COPY_DST`, with
`Rgba8Unorm` as the first storage format unless a CTS case proves that
another format must be surfaced earlier.

*Done condition:* a `webgl_canvas_to_netrender_texture` smoke compiles
without `glow`, GLES, EGL, WGL, ANGLE, ServoShell, or surfman
dependencies. The crate can allocate, resize, clear, and expose the
same-device `wgpu::Texture` plus metadata.

### W2. NetRender zero-copy composition smoke

**CLEARED 2026-05-11 on the netrender side.**
`Renderer::compose_external_texture` directly samples a same-device
producer `TextureView` into the already-rendered target. Receipt:
`netrender/tests/pg4_webgl_canvas_texture.rs` creates a producer
texture without `COPY_SRC`, verifies color mapping, and verifies
opacity blending over a vello-rendered scene.

*Shape:* prefer the direct same-device external texture pass over
vello's `register_texture` path. `Renderer::insert_image_vello` remains
the compatibility fallback for arbitrary `SceneImage` placement, but
current vello copies registered textures into its image atlas at frame
start and therefore is not the WebGL canvas fast path.

The first zero-copy hook is `Renderer::compose_external_texture`: render
the ordinary vello scene, then sample a producer `TextureView` directly
into the target. The source texture only needs `TEXTURE_BINDING` (plus
whatever the producer needs, e.g. `RENDER_ATTACHMENT` / `COPY_DST`); it
does **not** need `COPY_SRC`. This is ideal for topmost canvas/video
overlays and is the right first G4 receipt. Fully interleaved painter
order is the next step: split the scene around external-texture ops or
move this pass into the scene compositor so ordinary text/rect content
can appear above and below the canvas without falling back to the atlas
copy.

*Done condition:* a netrender test creates a producer texture without
`COPY_SRC`, renders ordinary vello content, composites the producer via
the direct external-texture pass, and readback confirms color mapping,
opacity blending, and no CPU readback or texture-copy dependency for
the producer texture.

### W3. Minimal WebGL-shaped render path

**FIRST SERVAL ADAPTER SLICE CLEARED 2026-05-11.**
`components/webgl-wgpu` now has a `WebGlContext` wrapper over the W1
canvas texture with context attributes by construction, default
framebuffer clear/draw/readback, buffer creation/binding/data upload,
vertex attrib reflection/setup including interleaved stride and offset,
reflected `uniform vec4` fragment color setup, one cross-stage `varying
vec4` color path, `drawArrays`, `getError`, `readPixels`, resize, and
context loss / recreation receipts. The first draw path remains
triangle-only on `Rgba8Unorm`.

The archived `webrender_build/src/wgsl.rs` translator source is not in
the current working tree, but it is present at
`origin/wgpu-backend-0.68-minimal:webrender_build/src/wgsl.rs`. The
W3 slice ports the core safety shape from that translator for the
first canonical pair: `components/webgl-wgpu/shader.rs` accepts the
canonical ESSL 1.00 vertex / fragment pair, validates that narrow
receipt independent of comments and whitespace, records `lowp` /
`mediump` / `highp` fragment-float precision for the canonical body,
accepts literal `vec4(r,g,b,a)` fragment colors in the `0.0..1.0`
range, a reflected `uniform vec4` color, or a single linked `varying
vec4` color sourced from a reflected vertex `attribute vec4`. It lowers
those receipts to naga-acceptable GLSL 4.50, runs naga on an 8 MB stack
behind a panic boundary, and returns naga-generated WGSL for
`create_program_from_essl` to pipeline along with tiny reflection
metadata for the canonical `vec2` position attribute at location 0,
optional `vec4` color attribute at location 1, optional fragment-color
uniform, and fragment-float precision. `WebGlContext` caches the
resulting canonical translation by validated shader shape so repeated
program creation does not re-run the naga path and exposes
`get_attrib_location` and `get_uniform_location` from that reflection.
Render pipelines are cached per effective vertex stride and attribute
offset so tightly packed and interleaved vertex buffers use the same
translated program with distinct wgpu vertex layouts. This first receipt
proves the API, layer boundaries, precision/reflection/cache hook, and
safety boundary; it is not general ESSL parsing.
It does **not** port the full ~2K-line WebRender pre-pass / post-pass
translator yet; do not widen shader acceptance beyond that canonical
receipt until the rest of the G2 wrapper is restored/ported behind the
same seam.

*Shape:* implement the smallest WebGL API-shaped object behind the
adapter crate: context attributes, default framebuffer backed by the
canvas texture, `clear`, buffer creation/binding, vertex attrib setup,
`getAttribLocation`, minimal `getUniformLocation` / `uniform4f`,
`drawArrays`, `getError`, `readPixels`, resize, and context loss /
recreation. Keep the first draw path intentionally tiny: triangle only,
RGBA8 target, no extensions, no WebGL 2.

Shader support for this step should enter through the G2 translator
wrapper rather than through a handwritten WGSL-only shortcut. For the
first receipt, accept a tiny set of canonical ESSL 1.00 vertex/fragment
pairs and route them through the panic-caught, 8 MB stack naga wrapper
from the prior WebRender translator work. Full precision propagation
and sampler breadth can follow once the vertical slice renders.

*Done condition:* the adapter renders clear + triangle into its canvas
texture, `readPixels` matches expected RGBA bytes, `getError` reports
the WebGL-mandated errors for at least one invalid bind/draw case, and
the resulting texture still composes through W2's netrender smoke.

### W4. Pelt/Serval integration smoke

*Shape:* wire the adapter into the existing wgpu context path:
`RenderingContextCore::wgpu()` supplies the shared device/queue,
the adapter produces a canvas texture, the paint/composition layer
registers that texture with netrender, and Pelt presents the frame.
If the canvas is also a declared compositor surface, reuse
`CompositorSurface` / `SurfaceKey` metadata rather than inventing a
parallel surface lifecycle.

*Done condition:* a headed or non-presenting Pelt smoke loads a page
or synthetic display list containing one WebGL canvas, presents it via
the netrender path, and captures a screenshot/readback with the WebGL
triangle visibly composited with surrounding 2D content.

### W5. Gates before widening

Do not start WebGL 2, broad extensions, or the full CTS matrix until
the W1-W4 lane is green. Once it is green, the next lane is the G1/G2
correctness ladder: more API validation, translator precision work,
uniform / attribute linking, samplers, framebuffer completeness, then
small CTS buckets.

The first lane's regression commands should be narrow and named:

```text
cargo test -p servo-webgl-wgpu webgl_canvas_to_netrender_texture
cargo test -p netrender pg4_webgl_canvas_texture
cargo run -p pelt -- --webgl-wgpu-smoke
```

Treat command names as placeholders until the crates/tests land; the
important part is that the lane has one adapter test, one netrender
composition test, and one Serval/Pelt integration smoke.

## 5. Cross-references

- Roadmap pointer:
  [`2026-05-04_feature_roadmap.md` — Phase G](2026-05-04_feature_roadmap.md).
- Rasterizer plan: no direct dependency. The netrender-side G4 hook
  follows the same external-texture pattern as Phase 5c
  (`register_texture`); see
  [`2026-05-01_vello_rasterizer_plan.md`](2026-05-01_vello_rasterizer_plan.md).
- Prior-art receipts (G2 §3):
  - [Shader translation journal](archive/legacy/shader_translation_journal.md)
    — every naga workaround catalogued.
  - [2026-04-08 live full reftest confirmation](archive/2026-04-08_live_full_reftest_confirmation.md)
    — 412/412 receipt.
  - [P15 progress report (2026-04-10)](archive/progress/2026-04-10_p15_progress_report.md)
    — 413/413 after the wgpu 29 bump.
  - `webrender_build/src/wgsl.rs` on
    `origin/wgpu-backend-0.68-minimal` — the translator itself.
    Recreate the worktree to inspect:
    `git worktree add ../webrender-wgpu-upstream wgpu-backend-0.68-minimal`.
