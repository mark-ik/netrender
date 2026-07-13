# netrender

A wgpu-native 2D renderer workspace. `netrender` ingests display lists and
produces pixels on top of [wgpu](https://wgpu.rs/) and
[vello](https://github.com/linebender/vello), with no GL and no GL-era
assumptions.

This repository began as a fork of Mozilla/Servo's WebRender (the
`webrender-wgpu` lane). The original WebRender codebase
(`webrender_api/`, `wrench/`, `wr_glyph_rasterizer/`, the GL renderer) has
been removed from `main`; vello is now the sole rasterizer. The historical
implementation survives on the `webrender-wgpu-upstream/` side worktree for
reference only. The repository retains WebRender's MPL-2.0 license, some
upstream CI scaffolding (`.taskcluster.yml`, `servo-tidy.toml`), and the
crate-level MPL headers.

**Made with AI**

## What it is

A small Cargo workspace (5 member crates) that takes engine display lists,
lowers them to a renderer-internal `Scene`, and rasterizes them with vello.
It is the leaf of a larger stack: it depends only on crates.io and on its
own internal path dependencies. Consumers (the `genet` web-engine
multiplexer and other siblings) depend on netrender; netrender depends on
none of them.

The architecture is described in detail in
[`netrender-notes/2026-04-30_netrender_design_plan.md`](netrender-notes/2026-04-30_netrender_design_plan.md)
(the parent design plan) and
[`netrender-notes/2026-05-01_vello_rasterizer_plan.md`](netrender-notes/2026-05-01_vello_rasterizer_plan.md)
(the vello pivot). Start from
[`netrender-notes/PROGRESS.md`](netrender-notes/PROGRESS.md), the index to
the surviving plans.

## Workspace layout

Workspace members (root `Cargo.toml`, `resolver = "2"`):

| Crate | Role |
| --- | --- |
| `netrender_device` | wgpu-native device foundation. Boots or adopts wgpu primitives (`WgpuDevice`, `WgpuHandles`, `boot`/`boot_with`), and provides the render-graph pipeline factories: `BrushBlurPipeline` (separable Gaussian blur) and `ClipRectanglePipeline` (rounded-rect clip mask), plus the GPU readback helper used by tests. |
| `netrender` | The renderer shell. Owns the `Scene` vocabulary, the vello rasterizer, the tile cache (Phase 7 picture caching), the render-graph (Phase 6, filters / blur / clip-mask tasks), hit testing, the external-texture compositor seam, and the consumer-facing `Renderer`. `Renderer::render_vello` is the single rendering entry point. |
| `netrender_text` | One-way `parley::Layout` to `netrender::Scene` glyph-run adapter. parley owns shaping, line-breaking, BiDi, fallback, alignment; netrender owns GPU rasterization. The contract is a data type (`netrender::SceneGlyphRun`); there is no `Shaper` trait. |
| `paint_list_api` | The `PaintList` producer trait plus the closed-set `PaintCmd` vocabulary and engine-facing primitives (color, euclid geometry, border/line/blend enums). Engine-facing only; it does not depend on `netrender`. `#![deny(unsafe_code)]`. |
| `paint_list_render` | Translator from `paint_list_api`'s `PaintCmd` stream into `netrender::Scene`. The impedance bridge where the two vocabularies meet. Depends only on `netrender` + `paint_list_api`. |

Internal dependency direction:

```
paint_list_api        netrender_device
      |                     |
      v                     v
paint_list_render ----> netrender <---- netrender_text
```

## Build, run, test

No toolchain is pinned (no `rust-toolchain.toml`); use a recent stable Rust
with edition-2021 support.

Build the whole workspace:

```
cargo build
```

Run the test suite (integration tests live under `netrender/tests/`; the
`paint_list_api` and `paint_list_render` crates carry their tests inline as
`#[cfg(test)]` modules. Many tests run on the GPU via wgpu, with software
adapters as fallback):

```
cargo test
```

Run the demo / consumer reference example (renders a 3x2 card grid to
`netrender/examples/output/demo_card_grid.png`):

```
cargo run -p netrender --example demo_card_grid
```

The `output/` directory is gitignored.

Backend selection: the `WGPU_BACKEND` environment variable is honored, and
the backend choice is also exposed on `NetrenderOptions`.

### Optional cargo features

On the `netrender` crate:

- `serde` — Scene capture / replay (roadmap A2). Adds serde derives to
  `Scene` types and exposes `Scene::snapshot_postcard` / `replay_postcard`
  (compact binary) and `Scene::snapshot_json` / `replay_json`
  (human-readable). Both formats round-trip identically. Pulls in
  `netrender_device/serde`, `peniko`, `serde_json`, `postcard`.
- `linear-light-canary` — opt-in canary test asserting that vello's GPU
  compute path honors `peniko::Gradient::interpolation_cs`. Today the test
  is expected to fail (the GPU path ignores the field); when it turns
  green, roadmap item R9 becomes pickable. Gated so default builds stay
  clean.

On `netrender_device`: `serde` adds derives to the device types embedded in
`Scene` (`SurfaceKey`, `GradientKind`); normally pulled in transitively by
`netrender`'s `serde` feature.

## Dependencies

Load-bearing versions are centralized in `[workspace.dependencies]` so the
member crates declare `<dep>.workspace = true` rather than pinning
independently (this avoids the wgpu version-skew risk flagged in the
2026-05-16 audit and matches the sibling Mere workspace convention):

| Dependency | Version | Notes |
| --- | --- | --- |
| `vello` | 0.9 | `default-features = false`; the rasterizer |
| `wgpu` | 29 | `default-features = false`; GPU API (backends: dx12, metal, vulkan, wgsl) |
| `peniko` | 0.6 | shared paint/color types; re-exported as `netrender::peniko` |
| `parley` | 0.10 | text layout (used by `netrender_text`) |
| `skrifa` | 0.42 | per-glyph metrics for hit testing; matches vello's transitive pull |
| `euclid` | 0.22 | `serde` feature on; geometry types shared with genet + inker |
| `log` | 0.4 | |
| `serde` | 1 | `derive` feature |

Other notable deps: `pollster` 0.4 (`netrender_device`), `png` 0.17 and
`postcard` 1 (test/feature use).

Build profiles: both `release` and `dev` use `panic = "abort"`; `release`
keeps `debug = true`.

## Status

Active development. Per
[`netrender-notes/PROGRESS.md`](netrender-notes/PROGRESS.md) and the
rasterizer plan, the vello pivot is runtime-verified through phase 7'
(the Masonry-pattern tile cache, the architectural heart). Capabilities
landed include nested layers and arbitrary-path clips
(`SceneOp::PushLayer`/`PopLayer`), gradients (including repeating),
strokes and paths, hard and blurred (outset and inset) box-shadows,
nine-patch borders, patterns with per-axis scale, variable fonts,
backdrop and element CSS filters, scene capture/replay, an op-list
inspector, a frame profiler, hit testing (stack-returning, layer-clip
aware, per-glyph via real font metrics), and the external-texture
compositor seam (path-(b') native-compositor handoff).

Open work is tracked in
[`netrender-notes/2026-05-04_feature_roadmap.md`](netrender-notes/2026-05-04_feature_roadmap.md):
Phase R (consumer-pull-gated wart fixes) and Phases A through G (new
capability, diagnostics first). Two companion lanes are gated on consumer
pull: native-compositor handoff
([`2026-05-05_compositor_handoff_path_b_prime.md`](netrender-notes/2026-05-05_compositor_handoff_path_b_prime.md),
netrender-side sub-phases 5.1-5.4 shipped) and WebGL-over-wgpu
([`2026-05-06_webgl_over_wgpu_plan.md`](netrender-notes/2026-05-06_webgl_over_wgpu_plan.md)).

TODO: per-platform support is described by intent in the design plan
(Vulkan / Metal / DX12 / WebGPU, with Lavapipe / WARP / SwiftShader
software fallbacks); no per-platform CI matrix is wired in this repo yet.

## Relationship to sibling repos

netrender is the rendering leaf consumed by the `genet` web-engine
multiplexer (formerly referenced as `servo-wgpu` in this repo's history;
renamed to `genet` in commit `ac34ba908`). Engine-specific glue that
consumes the translator output (building blurred box-shadow masks on the
GPU, compositing external textures, the painter message loop) lives in
genet's `components/paint` crate, not here. The `paint_list_api` /
`paint_list_render` split is the extraction boundary that lets any engine
(genet, nematic, inker) emit a `PaintList` and reuse the translator.

## License

Mozilla Public License 2.0 (MPL-2.0). See [`LICENSE`](LICENSE). All member
crates declare `license = "MPL-2.0"`.
