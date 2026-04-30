# wgpu Device Plan — `spirv-shader-pipeline` branch

Date: 2026-04-30
Branch: `spirv-shader-pipeline` (in `mark-ik/webrender-wgpu`, formerly worktree `upstream-wgpu-device`)
Status: planning

## Goal

Add a wgpu-backed `Device` to WebRender that consumes the committed SPIR-V
corpus in `webrender/res/spirv/`, sitting alongside the existing GL device behind
a trait. Reach reftest parity with the GL device, mirroring what
`origin/wgpu-device-renderer-gl-parity` achieved (413/413). Stopgap, not the
long-term renderer — that's netrender on `main` — but built reliably enough to
serve as an oracle for netrender's bind-group / vertex-layout shape.

## Non-goals

- No GLSL→SPIRV compilation at build time. The corpus is committed; regenerate
  manually with `cargo run -p webrender_build --features shader-gen --bin gen_spirv`
  when `webrender/res/*.glsl` changes.
- No SVG filter support in the wgpu device until upstream `cs_svg_filter_node.frag`
  is reworked for Vulkan-GLSL compatibility (one known compile failure deferred).
- No replacement of the GL device on this branch. It stays compilable and
  runnable; switching is a feature flag.
- No upstream PR. Lives in fork.

## Architectural decisions (answered)

### A1. Bind group layouts: wgpu auto-derive + reflection oracle in tests

At runtime, `create_render_pipeline` is called with `layout: None` so wgpu's
internal naga reflects each `ShaderModule(SpirV)` and derives a
`PipelineLayout` automatically. No hand-authored `BindGroupLayoutEntry` tables
required for runtime correctness.

For verifiability, a build-or-test-time tool (`webrender_build` bin
`reflect_spirv`) walks every `.spv` artifact, runs `naga::front::spv` on it,
and emits a golden `bindings.json` (or Rust table) describing each shader's
expected bindings — set, binding index, type, name. Tests assert that the
golden has not drifted vs. fresh reflection. This catches:

- Glslang reassigning binding indices after a shader edit
- Driver/wgpu implementations rejecting auto-derived layouts in non-obvious ways
- Future shader corpus changes silently changing the binding contract

This is option (b) "wgpu auto-derive at runtime" plus (c) "reflected golden as
verification oracle" — not option (c) hand-authored as the prior branches did.

### A2. Coexistence: trait-ified `GpuDevice`, sibling impls, feature-gated

Mirrors `origin/wgpu-device-renderer-gl-parity` exactly. New shape:

```text
webrender/src/device/
  mod.rs        — defines `pub trait GpuDevice` + cfg-gated re-exports
  gl.rs         — existing impl, gated by `feature = "gl_backend"`
  wgpu.rs       — new impl (or wgpu/ subdir if it grows), gated by `feature = "wgpu_backend"`
  query_gl.rs   — unchanged
```

`webrender/src/renderer/init.rs` dispatches on which backend feature is active.
Both backends are compile-checked together in CI; pick one at link time.

Trait surface follows `Device`'s existing public API in `gl.rs` so the renderer
above doesn't fork. Where wgpu semantics genuinely diverge (e.g., command
encoder lifetimes, dynamic offsets), the trait method gets a wgpu-friendly
default and the GL impl adapts.

### A3. Vertex layouts: one mechanical adapter from existing typed schema

WebRender already declares vertex schemas in
`webrender/src/renderer/vertex.rs` and `webrender/src/device/gl.rs` as typed
`VertexDescriptor { vertex_attributes, instance_attributes }` with
`VertexAttribute { name, count, kind, ... }`. We add one adapter:

```rust
// illustrative signature only
fn descriptor_to_wgpu_layouts(
    desc: &VertexDescriptor,
) -> [wgpu::VertexBufferLayout<'static>; 2]; // [vertex, instance]
```

The shaderc generator was invoked with `set_auto_map_locations(true)`, which
assigns `layout(location = N)` to vertex inputs in declaration order matching
the GLSL source. The schemas in WebRender are in the same order. So the adapter
walks the schema, accumulates byte offsets, and emits `wgpu::VertexAttribute {
shader_location: i, offset, format }` per entry. No string parsing, no regex,
no WGSL inspection.

The reflection oracle from A1 also captures vertex input locations, so the
adapter's output is asserted against reflection in tests.

## Phase breakdown

Each phase has explicit done conditions. Phases are sequential except where
noted.

### P0 — Trait extraction

Done when:
- `pub trait GpuDevice` exists in `webrender/src/device/mod.rs`
- Existing GL impl moves behind `#[cfg(feature = "gl_backend")]`, builds and
  passes existing tests with that feature
- `cargo build -p webrender --features gl_backend` clean
- No wgpu code yet; this phase is risk-free refactor

Reference: study `origin/wgpu-device-renderer-gl-parity:webrender/src/device/mod.rs`
for the trait shape that proved sufficient for reftest parity.

### P1 — Skeleton wgpu device

Done when:
- `webrender/src/device/wgpu.rs` exists with `WgpuDevice` struct implementing
  `GpuDevice` for every method, even if most are `unimplemented!()`
- `wgpu` dep added to `webrender/Cargo.toml` under `[features] wgpu_backend`
- `cargo build -p webrender --features wgpu_backend` clean
- Construction wires an `Adapter`, `Device`, `Queue`, surface format

### P2 — SPIRV loading + reflection oracle

Done when:
- `webrender_build` gains a `reflect_spirv` binary that emits
  `webrender/res/spirv/bindings.json` (or Rust table) from the committed `.spv` files
- The output is committed; CI step asserts it's regenerable byte-identical
- `WgpuDevice` loads each `.spv` via `include_bytes!` (or runtime read) and
  creates `wgpu::ShaderModule` via `wgpu::ShaderSource::SpirV`
- Smallest shader (`ps_clear`) creates a `RenderPipeline` with `layout: None`,
  no draws yet
- A test asserts: pipeline's auto-derived layout matches the golden for that
  shader

### P3 — Vertex schema adapter

Done when:
- `descriptor_to_wgpu_layouts(...)` exists in `webrender/src/device/wgpu.rs`
- Unit tests cover every `VertexDescriptor` in the codebase: round-trips the
  schema to wgpu layouts, compares attribute count and total stride against
  the GL VAO setup
- Test asserts adapter output `shader_location` indices match the reflection
  oracle from P2

### P4 — Resource model: textures, buffers, samplers

Done when:
- `WgpuDevice` implements texture create/upload/bind paths through the trait
- A textured pipeline (`ps_quad_textured` is the smallest) constructs without
  panic
- Buffer upload paths (vertex, instance, index, uniform/UBO) implemented
- Sampler creation honours WebRender's existing sampler request enum

Reference: `origin/wgpu-device-sharing` shows the resource model that landed
parity. Borrow texture descriptor mapping; do not borrow the WGSL string parser.

### P5 — Frame submission

Done when:
- `WgpuDevice` builds and submits a `CommandEncoder` per frame
- Render passes correctly bind pipelines, vertex/instance buffers, bind groups,
  and issue `draw_indexed` mirroring GL device call sites
- `begin_frame` / `end_frame` symmetry preserved across the trait

### P6 — First shader end-to-end

Done when:
- `wrench` (or a minimal smoke test) renders a frame whose only draw is
  `ps_clear` through the wgpu device, output pixels match GL device output
  within 1 ULP
- Re-run with `ps_quad_textured` against a single sampled texture, same parity

This phase is the integration moment; expect to discover gaps in P0-P5 and
loop back. Build hard parity tests here so later shaders inherit them.

### P7 — Shader-by-shader expansion

Done when:
- All committed SPIRV variants (excluding `cs_svg_filter_node`) instantiate
  pipelines without errors and run their corresponding render paths
- Per-shader smoke test confirms each issues correct draws

Order suggestion (cheapest first): `ps_clear`, `ps_copy`, `ps_quad_*`,
`brush_solid`, `brush_blend`, `brush_image` family, `cs_blur`, `cs_scale`,
`cs_border_*`, `cs_line_decoration`, `composite`, `ps_text_run`,
`brush_yuv_image`, the rest.

### P8 — Reftest parity push

Done when:
- Full WebRender reftest suite runs under wgpu backend
- Failures triaged into: (a) parity bugs to fix, (b) GLSL/SPIRV
  precision/rounding differences within reasonable tolerance, (c) genuinely
  blocked (e.g. SVG filters)
- Target: match `origin/wgpu-device-renderer-gl-parity`'s 413/413 minus the
  SVG-filter cohort

## Known issues carried into this plan

1. **`cs_svg_filter_node.frag` does not compile to SPIRV.** Combined-sampler
   syntax incompatible with Vulkan GLSL at line 1544 of the assembled shader.
   Vertex stage compiles fine. Plan: defer; SVG filter effects through wgpu
   device fall back to GL device or are unsupported on this branch. Re-address
   if reftest parity push surfaces it as gating.

2. **Binding indices may shift if shaders are regenerated.** glslang's
   `set_auto_bind_uniforms` assigns indices based on declaration order. A
   shader edit that reorders uniforms changes the contract. The reflection
   oracle (A1) catches this; treat any oracle diff in PR review as a renderer
   binding contract change, not a shader-only change.

3. **wgpu's auto-derived layouts may be too permissive or strict.** If we
   discover wgpu auto-derives layouts that the renderer can't bind against
   (e.g. expects a sampler we never bind, or splits a UBO across sets in an
   inconvenient way), we fall back to hand-authored layouts seeded by the
   reflection oracle output. A1's oracle is the bridge: the same JSON could
   build a `Vec<BindGroupLayout>` programmatically.

## Verification posture

- Reflection oracle JSON committed; CI re-derives and diffs.
- Vertex-layout adapter unit-tested per descriptor.
- Per-shader pipeline-creation smoke test.
- End-to-end frame parity test for at least 2 shaders before P6 closes.
- Reftest run is the parity gate (P8).

## Oracle-for-netrender notes

If this branch reaches P8 with the reflection oracle and adapter intact, it
produces three things netrender can borrow without binding to the GL corpus:

1. `bindings.json` — set/binding/type per shader, ground truth for what
   WebRender's renderer code actually expects to bind.
2. `descriptor_to_wgpu_layouts` output — the canonical mapping from
   WebRender's typed vertex schema to wgpu vertex layouts.
3. Reftest behaviours per shader — netrender's WGSL implementations can be
   diffed against this branch's output as a known-good reference.

This is opportunistic, not load-bearing. Netrender does not depend on this
branch reaching parity.

## Out of scope, deliberately

- Rewriting any GL-device internals beyond what trait extraction requires.
- Performance work (matching wgpu throughput to GL is a P9 question, not on
  this plan).
- Multi-threaded command encoding.
- WGSL conversion paths (we have SPIRV; we don't need WGSL).
- Touching netrender on `main`.
