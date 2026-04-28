# Dual-Servo Parity Plan

## Goal

`webrender-wgpu` should be capable of driving both:

- upstream `servo/servo` via the **GL backend** (no wgpu required)
- `servo-wgpu` via **either GL or wgpu**

The SPIR-V pipeline's strategic value is exactly this: one canonical `.spvasm`
corpus, derive GLSL and WGSL from the same artifact, keep both backends at
parity, serve both Servo consumers from a single fork. Future target languages
(MSL, HLSL, SPIR-V direct) add derivation steps without touching the authored
corpus.

## What "as capable as the previous GL backend" means

The original WebRender 0.68 GL backend assembled GLSL strings at runtime from
the authored GLSL source tree. This fork replaced that with SPIR-V → naga →
generated GLSL. "As capable" means the naga-derived GL path produces equivalent
output to the original for every rendering family Servo exercises — not just the
33 migrated micro-scenes in `spirv-parity`.

The wgpu backend carries the same bar: equivalent output to the original GL
backend for all Servo-exercised paths.

## Principles and guard rails

These apply to all three tracks. They are constraints on *how* the work is
done, not just *what* is done.

### No hacks

Don't paper over failures. If a rendering path produces wrong output, find the
root cause and fix it. The three GL tolerance cases in `spirv-parity` are debt,
not proof of correctness — they need to be understood (regression vs.
pre-existing baseline) and addressed. New `fuzzy-if` entries in Wrench manifests
require documented root causes; adding a tolerance without an explanation is not
acceptable. Same principle applies to workarounds in Rust code: a `// TODO`
that is load-bearing is a hack in waiting.

### wgpu: leverage the modern GPU path, don't constrain it for GL parity

The wgpu backend runs over Vulkan, Metal, and DX12. Don't write it as GL with
different syntax. Specific things to avoid:

- Carrying GL Y-flip conventions into the wgpu path. wgpu surface orientation
  is explicit; declare it directly rather than replicating the GL ortho-flip
  projection.
- CPU-side channel swaps as a substitute for native texture format support. The
  RGBA/BGRA upload fix in the presenting smoke was correct; avoid reintroducing
  similar hacks.
- Synchronising where wgpu's async model allows overlap.
- Replicating GL-era fixed-function emulation (e.g., manual blend state
  assembly, format compatibility tables) in the wgpu path when the abstraction
  can be expressed cleanly in wgpu terms.

Where GL and wgpu genuinely differ in capability — explicit pipeline state
objects, compute shaders, storage textures, push constants — expose the
capability cleanly rather than hiding it for false parity. The goal is a wgpu
backend that is *better* than the old GL path where wgpu makes that possible,
not merely equivalent.

### Validate against real external targets, not just our own codebase

Our own reftest corpus and unit tests are necessary but not sufficient. The fork
should be measurable against:

- **External test corpora**: our own micro-scenes are a fast feedback loop;
  they are not a correctness claim. The following external corpora exist
  independently of this codebase and are candidates for inclusion in the
  acceptance picture. Which subset(s) are most valuable is an open question
  to evaluate, not assume.

  *Wrench-level (display-list / shader — no browser stack required):*
  - **Upstream WebRender Wrench suite** (`upstream/upstream` on
    `servo/webrender`) — same format as our existing reftests, directly tests
    WebRender rendering, no new tooling. The `spirv-parity` lane is a small
    custom slice; upstream's full suite is the larger corpus we are not yet
    running. Highest-value near-term option.

  *Browser-stack-level (layout → display list → render):*
  - **WPT (Web Platform Tests)** — the cross-browser standard, used by
    Chrome, Firefox, Safari, Edge. CSS and SVG rendering reftests. Large;
    run a subset. Servo already has WPT infrastructure. Right long-term bar,
    higher adoption cost.
  - **CSS WG Interop tests** (Interop 2024/2025) — curated high-value subset
    of WPT; the areas browser vendors agreed matter most. Lower volume.
  - **Servo's existing WPT integration** — Servo already runs a WPT slice in
    CI. Reusing that infrastructure avoids setup cost.

  *GPU / backend-level:*
  - **WebGPU CTS** (`gpuweb/cts`) — conformance tests for the WebGPU API.
    Validates that the wgpu backend uses the GPU API correctly, independent
    of rendered pixels. Different axis: API contract, not rendering
    correctness.
  - **Khronos Vulkan CTS** — lower level than directly useful; wgpu handles
    this layer. Listed for completeness.

  *Reference-render comparison:*
  - **Firefox's WebRender reftests** (`layout/reftests/` in Gecko) —
    WebRender in Firefox passes these; they were designed around WebRender's
    capabilities. Harder to run in isolation (require Gecko layout), but
    reference images could serve as a pixel oracle for specific rendering
    families.
- **Code style**: `cargo fmt` clean throughout, consistent with WebRender's
  existing conventions. New code should be idiomatic to the codebase, not
  written in an isolated style that diverges from the surrounding file.
- **Comprehensive Wrench**: Wrench should be a first-class testing tool. Add
  YAMLs that cover the full rendering surface, not just the migrated shader
  families. The `spirv-parity` suite is a regression gate, not a coverage
  statement.
- **Up-to-date dependencies**: Audit the full dep stack (wgpu, naga, euclid,
  webrender_api, the Servo integration chain). Update where sensible. The
  explicit purpose of the SPIR-V pipeline is to make wgpu version management
  tractable; don't let that benefit rot by letting the dep graph drift. Treat a
  dep audit as a recurring task, not a one-time event.
- **External validators as CI gates**: `spirv-val` for authored SPIR-V, ANGLE
  for generated GLES, `wgpu::Device::create_shader_module` for generated WGSL,
  `glslangValidator` for generated desktop GL. These are already in CI; keep
  them there and don't bypass them.

---

## Current state (2026-04-28)

**What is proven:**
- `reftests/spirv-parity` passes at 33/0 on GL hidden-window and all wgpu
  modes.
- Runtime metadata contracts cover all typed `WgpuShaderVariant` families.
- Generated WGSL validates through `wgpu::Device::create_shader_module`.
- Generated desktop GL GLSL and GLES GLSL validate through the offline ANGLE
  oracle.
- Servo presenting smoke (solid rect, linear gradient, radial gradient, clip,
  image, text) passes on `servo-wgpu`.

**Known gaps:**
- Three GL tolerance debt cases in `spirv-parity` (see investigation notes in
  `2026-04-18_spirv_shader_pipeline_plan.md`):
  - `text/large-line-decoration` — GL renders blank (root cause unconfirmed)
  - `image/segments` — 69,094 pixel diff on GL
  - `border/discontinued-dash` — 10,200 pixel diff on GL
- `build.rs` compile-time guardrail panics if `gl_backend` is enabled;
  upstream `servo/servo` (confirmed still at 0.68) would hit this.
- Parity coverage is narrow: 33 micro-scenes do not cover scroll compositing,
  SVG filters, external images, complex clip chains, or real glyph atlases.
- Servo presenting smoke does not cover scroll, SVG/filter, or
  external/video image paths.
- No concrete test drives upstream `servo/servo` (GL, no wgpu) against this
  fork.

---

## Track 1 — GL SPIR-V parity

**Goal**: every rendering path that upstream `servo/servo` exercises via the
original 0.68 GL backend also works correctly through the SPIR-V → naga → GLSL
path in this fork.

### Step 1.1: Triage the three GL tolerance cases as regression vs. baseline

Before fixing anything, confirm whether each diff was present in the original
0.68 GL backend (pre-SPIR-V) or was introduced by the naga-derived GLSL.

Method: run each failing YAML against an unmodified WebRender 0.68 GL
checkout. If the diff also appears there, it is a pre-existing GL baseline
difference — not a regression, and the current tolerance is the correct
long-term expectation. If the diff does not appear there, it is a regression
introduced by the SPIR-V path and must be fixed.

Record the outcome for each case in this plan's Progress section.

### Step 1.2: Fix confirmed regressions

**`text/large-line-decoration` blank on GL:**
- Current hypothesis: `frame.present=false` → `composite_frame` skips the
  composite pass (`device_size=None`, `renderer/mod.rs` line ~10434).
- Confirmation method: add a temporary `assert!(present)` or print at
  `composite_frame` entry for a `--gl-hidden` run of this YAML, or bisect with
  a forced `present=true`.
- If confirmed: trace back why `frame.present` is false for this scene on GL
  and fix the condition.
- If refuted: examine GL FBO state (framebuffer completeness, clear on FBO
  creation) with a minimal repro before widening the search.

**`image/segments` / `border/discontinued-dash` (if regressions):**
- Current hypothesis: clip mask and border segment render task FBO sampling
  uses a Y-convention inconsistent with how the GL compositor reads it.
- Confirmation method: compare `cs_clip_rectangle` and `cs_border_segment`
  generated GLSL against the original assembled GLSL for UV and Y-convention
  assumptions.
- Fix site: likely in the generated GLSL sampling orientation, or in the
  compositor's UV construction when consuming those render task FBOs on GL.

### Step 1.3: Remove the gl_backend compile guardrail

`build.rs` currently panics when `gl_backend` is enabled. This was a migration
guardrail to prevent accidental GL use while the artifact-backed path was
incomplete. Now that Phase 5 is closed and GL runs the parity lane at 33/0,
the guardrail should be removed.

Acceptance: `cargo check -p webrender --no-default-features --features gl_backend`
succeeds cleanly, no panic.

### Step 1.4: Expand GL parity coverage

The 33-test `spirv-parity` suite covers migrated shader families in isolation.
Servo page rendering exercises paths not in that suite. Add coverage for:

- **Scroll compositing**: a page taller than the viewport with `scrollTop` set;
  verifies picture cache invalidation and re-composite across frames.
- **SVG filters**: a basic `<svg filter>` (`blur`, `drop-shadow`) via Servo GL.
- **External image path**: `<img>` decoded as an external texture; exercises the
  external image compositor path on GL.
- **Complex clip chains and stacking contexts**: nested clips, `mix-blend-mode`.
- **Text at multiple DPI**: real glyph atlas population, not just the micro
  text-run scene.

These can be added as either new `spirv-parity` sub-slices or as Wrench YAMLs
with GL reference images generated from the original 0.68 backend.

The longer-term acceptance bar for rendering correctness should include at
least one external test corpus independent of this codebase. Which corpus fits
best (WPT slice, CSS WG reftests, or something else) should be evaluated as
part of this step, not assumed in advance.

### Step 1.5: Upstream Servo GL smoke

Prove the fork is a viable drop-in for upstream `servo/servo` over GL.

Method:
1. Add a `[patch.crates-io]` entry in a test workspace pointing webrender at
   this fork.
2. `cargo check -p servo` (GL features, no wgpu) — must succeed.
3. A basic page render via `--gl-hidden` or similar headless GL path — must
   produce correct output against a reference screenshot.

Acceptance gate: upstream Servo GL smoke green, no panics, no visual
regressions against a reference generated from unmodified 0.68 GL.

---

## Track 2 — wgpu coverage parity

**Goal**: the wgpu backend is at least as capable as the original GL backend
for all Servo-exercised rendering paths.

### Step 2.1: Extend presenting smoke to scroll

Add a page taller than the viewport with verified scrolled-position pixel
samples. This exercises picture cache tile invalidation and re-composite on
wgpu across frame boundaries.

### Step 2.2: Extend presenting smoke to SVG/filter

Add a page with at least one SVG filter primitive (`feGaussianBlur` or
`feDropShadow`). Exercises `cs_svg_filter` / `cs_svg_filter_node` on the wgpu
presenting path.

### Step 2.3: External image / video path

Add a case that exercises the external image compositor path on wgpu —
`<video>` or `<canvas>`. This exercises the shared-device external texture
route rather than the internal atlas path.

---

## Track 3 — Dual-servo compatibility gate

**Goal**: a concrete, runnable gate that proves both Servo consumers work
against this fork simultaneously.

### Gate A: upstream `servo/servo` + GL (no wgpu)

Upstream `servo/servo` declares `webrender = { version = "0.68", features =
["capture"] }`. It does not enable `wgpu_backend`.

Test:
```
# In a scratch workspace that patches crates-io webrender to this fork:
cargo check -p servo --features gl_backend --no-default-features
cargo run  -p servo --features gl_backend --no-default-features -- <smoke-page>
```

Acceptance: compiles, renders smoke page, no panics, output matches reference.

### Gate B: `servo-wgpu` + wgpu

Covered by the existing Servo presenting smoke in `servo-wgpu/`. Keep that
smoke current as Track 2 expands it.

### Gate C: compile matrix

Both feature configurations must compile cleanly from a single checkout:

| Configuration | Command |
|---|---|
| GL only (for upstream Servo) | `cargo check -p webrender --no-default-features --features gl_backend` |
| wgpu (for servo-wgpu) | `cargo check -p webrender --features wgpu_backend` |
| Both enabled | `cargo check -p webrender --features gl_backend,wgpu_backend` |

No feature-flag interaction must cause a panic or compile error.

---

## Sequencing

The tracks are mostly independent but have one hard dependency:

- **Track 1 step 1.3** (remove the guardrail) must precede **Track 3 Gate A**
  (upstream Servo compile), since Gate A fails to compile while the guardrail
  is in place.
- **Track 1 step 1.1** (triage) should precede step 1.2 (fix), to avoid fixing
  pre-existing baseline differences that belong in the tolerance record.

Suggested order:
1. Track 1.1 — triage (quick, no code change, confirms what to fix)
2. Track 1.3 — remove guardrail (unblocks Gate A and reduces friction on all GL work)
3. Track 1.2 — fix confirmed regressions (especially the blank line decoration)
4. Track 3.C — compile matrix gate (confirm dual-feature compile health)
5. Track 3.A — upstream Servo GL smoke (first full end-to-end check)
6. Dep audit — audit full dep stack (wgpu, naga, euclid, Servo integration
   chain); update where sensible; record findings before expanding test coverage
   so coverage is measured against current rather than drifted deps
7. Track 1.4 + Track 2 — expand coverage in parallel; evaluate external
   corpus options as part of Track 1.4 scoping
8. Track 3.B — update Servo presenting smoke as Track 2 milestones land

---

## Progress

**2026-04-27** — Plan written. No code changed. Current state as described
above. Three GL tolerance debt cases documented with investigation notes in
`2026-04-18_spirv_shader_pipeline_plan.md`.
