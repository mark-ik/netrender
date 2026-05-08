# netrender — feature roadmap checklist (2026-05-04, last updated 2026-05-06)

Forward-looking work items in three layers:

- **Open refinements** (Phase R) — known wart fixes on shipped
  features. Each is a specific, bounded change with a documented
  design; gated on consumer pull. Originally lived as §11.99 of
  the rasterizer plan; folded in here so all open items are in
  one place. Move entries into a `§11.x — CLEARED` finding in
  [`2026-05-01_vello_rasterizer_plan.md`](2026-05-01_vello_rasterizer_plan.md)
  when they land.
- **New capability** (Phases A–G) — features the codebase doesn't
  yet express.
- **Out of scope (visibility-only)** — items that are real consumer
  pain but explicitly aren't netrender's job. Listed once so
  embedders know we know; not tracked, no checkbox.

Activation history of the originally-deferred items
(12c' backdrop filter, 13' compositor handoff, linear-light
blending) lives in
[`archive/2026-05-05_deferred_phases.md`](archive/2026-05-05_deferred_phases.md).
All three were activated 2026-05-05; their canonical entries are
on this roadmap (D1, D3, R9 respectively). The 13' design detail
lives in
[`2026-05-05_compositor_handoff_path_b_prime.md`](2026-05-05_compositor_handoff_path_b_prime.md).

Each entry has a brief, a trigger, and a done condition — enough to
pick up without re-deriving the design. Where rough line-count
estimates appear, they are sizing hints for sequencing, not
commitments.

**On phase ordering.** Phase A items multiply the value of everything
below them — every B/C/D feature is easier to ship, test, and debug if
A1–A4 are in place. That is the only structural ordering decision in
this file. Phase D is "architecturally significant" by content, not by
priority: D3 is already 4/5 done while Phase A is unstarted. Treat
each item by its individual trigger, not by phase position.

---

## Phase R — Open refinements (consumer-pull-gated wart fixes)

Each entry says what the wart is, when it bites, what specific signal
activates the fix, and the done condition. Move into a
`§11.x — CLEARED` finding in the rasterizer plan when landed.

- [x] **R1. Real font-metric per-glyph hit testing** —
  **CLEARED 2026-05-06**.
  `glyph_run_per_glyph_hit` now uses `skrifa::metrics::GlyphMetrics`
  for real glyph bounds; em-box fallback only for the no-font
  sentinel, font-parse failures, and glyphs with empty outline
  bounds (e.g., COLR emoji whose outline table is empty).
  Receipt at
  [`netrender/tests/pr1_per_glyph_hit_metrics.rs`](../netrender/tests/pr1_per_glyph_hit_metrics.rs)
  — clicks on a 'g' descender hit under real metrics where
  em-box would have missed. Full finding:
  [rasterizer plan §11.23](2026-05-01_vello_rasterizer_plan.md).

- [x] **R2. Per-segment point-in-polygon for `SceneOp::Shape`** —
  **CLEARED 2026-05-06**.
  `op_contains_point` for `SceneOp::Shape` now AABB-pre-passes then
  calls `kurbo::Shape::contains` on a `BezPath` built from the
  `ScenePath` after inverse-transforming the world point to local.
  Non-invertible transforms fall back to AABB-conservative.
  Sanity-checked against parley's `Selection`-style trigger framing
  per the feedback memory: the wrap was a thin pass-through over
  kurbo's existing API, no speculation. Full finding:
  [rasterizer plan §11.20](2026-05-01_vello_rasterizer_plan.md).

- [x] **R3. Layer-clip path-precise containment** — **CLEARED
  2026-05-06**.
  Same `kurbo::Shape::contains` machinery as R2, applied to
  `clip_aabb_contains_point`'s `SceneClip::Path` and rounded-rect
  branches. Sharp axis-aligned rect clips skip the path-precise
  check. Non-invertible transforms remain AABB-conservative. Full
  finding: [rasterizer plan §11.20](2026-05-01_vello_rasterizer_plan.md).

- [x] **R4. Image cache for the simple (non-tile) rasterizer** —
  **CLEARED 2026-05-06**.
  New `VelloRasterizer` struct mirrors `VelloTileRasterizer`'s
  `image_data` cache for the simple path. Stateful: cache fills on
  first call, subsequent calls only update for added/removed keys.
  Same Path B `register_texture` / `unregister_texture` interface.
  Receipt at
  [`netrender/tests/pr4_simple_rasterizer_image_cache.rs`](../netrender/tests/pr4_simple_rasterizer_image_cache.rs)
  (7/7). Full finding:
  [rasterizer plan §11.24](2026-05-01_vello_rasterizer_plan.md).

- [x] **R5. Downscale-blur-upscale for very large blurs** —
  **CLEARED 2026-05-06**.
  New `blur_kernel_plan_with_downscale(radius)` returns
  `(level, passes, step_px)` where `level ∈ {1, 2, 4, 8}` divides
  the work resolution. `build_box_shadow_mask` adds a brush_blur
  step=0 downscale task before the cascade and an upscale task
  after, both relying on the bilinear sampler for AA. Cascade runs
  at the scaled resolution with a smaller effective radius.
  Lifts the σ-clip cap from ~28px up to ~224px (8 × 28). Receipt
  at
  [`netrender/tests/pr5_downscale_blur.rs`](../netrender/tests/pr5_downscale_blur.rs)
  (2/2 GPU + 9/9 CPU planner tests in `blur_plan_tests`). Full
  finding: [rasterizer plan §11.28](2026-05-01_vello_rasterizer_plan.md).

- [x] **R6. Inline-box rendering helper in `netrender_text`** —
  **CLEARED 2026-05-06**.
  `netrender_text::push_layout_with_inline_boxes(scene, registry,
  layout, origin, on_inline_box)` walks parley's
  `PositionedLayoutItem` stream once, pushes glyph runs into the
  scene with the existing decoration / font-dedup logic, and emits
  a typed [`InlineBoxPlacement`] (scene-space coordinates,
  consumer-supplied id) per inline box for the consumer to paint.
  The simple `push_layout` / `push_layout_with_registry` entry
  points are now thin wrappers with an empty callback — no behavior
  change. Sanity-check confirmed: parley's `PositionedLayoutItem`
  surface was already in shape, so the wrap was no-speculation
  ship-now work. Full finding:
  [rasterizer plan §11.21](2026-05-01_vello_rasterizer_plan.md).

- [ ] **R9. Linear-light blending wrap** (rasterizer plan §6.3,
  Pitfall #2; `p1prime_03`).
  *Bites when:* upstream vello's GPU compute path honors
  `peniko::Gradient::interpolation_cs` — until then, gradient
  interpolation and `mix-blend-mode: linear-light` only match CSS
  reference under the `vello_hybrid` path, which we don't use.
  *Trigger:* the **R9-canary** greens (sub-bullet below). Until then,
  fully blocked.
  *Done condition:* `Scene::interpolation_color_space` field
  (default `Srgb` for back-compat) threaded through to
  `peniko::Gradient::with_interpolation_cs`; `p1prime_03` re-greens
  on the GPU path; rasterizer §3.3 caveat block dropped; §6.3
  contract advertises dual capability. Don't pre-build the enum;
  vello's API shape will dictate it.

  - **R9-canary (trigger setup) — wired 2026-05-06.** Test
    `p1prime_03_canary_linear_light_is_honored` in
    [`netrender/tests/p1prime_vello_first_light.rs`](../netrender/tests/p1prime_vello_first_light.rs)
    asserts the **fixed** behavior (LinearSrgb gradient midpoint
    differs from default by ≥ 16/255 per channel). Currently RED
    today (max_chan_diff = 0 — vello GPU path still ignores the
    field). Gated behind the `linear-light-canary` cargo feature
    so default builds stay clean; CI runs
    `cargo test --features linear-light-canary` on vello-dep bumps
    and inspects whether the canary turned green. When it does,
    R9 (the wrap above) becomes pickable; both the canary and the
    twin `p1prime_03_gradient_default_is_srgb_encoded` retire in
    favor of the wrap's own receipts.

---

## Phase A — Diagnostics first

Build the measurement infrastructure before the next round of
features. Every Phase B+ item ships cheaper if these exist: capture a
real consumer scene as a regression artifact, watch the dirty tiles
when adding a primitive, profile the impact when adding a filter.
Order within Phase A is by value-to-cost ratio, smallest first.

- [ ] **A1. Op-list inspector** — pretty-print `Vec<SceneOp>` to a
  string for debugging.
  *Done condition:* `Scene::dump_ops()` returns a multi-line string
  with per-op summary (kind, key fields, transform/clip if
  non-default). Cheapest item; useful immediately.

- [ ] **A2. Scene capture / replay** — `Scene::snapshot()` →
  serializable record, `Scene::replay(&record)` rebuilds.
  *Done condition:* capture a frame from a real consumer, ship it as
  a `*.scene.bin` artifact, replay deterministically in a unit test.
  Multiplies the value of every other test / regression diag in the
  rest of this list.

- [ ] **A3. Tile-dirty visualizer** — overlay that paints dirty tiles
  in red on a debug pass.
  *Done condition:* an `enable_tile_dirty_overlay: bool` flag on
  `NetrenderOptions`; when on, dirty tiles get a translucent red wash
  on top of the rendered output (per-tile `last_dirty_frame` field on
  `TileCache`). Bites first when nematic's Gemini/Gopher rendering
  starts behaving weirdly under tile invalidation pressure.

- [ ] **A4. Frame profiler** — per-phase timings: scene build, tile
  invalidate, vello encode, GPU submit, readback.
  *Done condition:* `Renderer::last_frame_timings() -> FrameTimings`
  with named spans (likely via `puffin` or a thin custom span type).
  Optionally exposes vello's internal `Renderer` timing hooks too.

---

## Phase B — Consumer-pull-imminent

Things nematic (Gemini, Gopher, Scroll, Markdown, feeds, Finger) and
serval (full web) will surface as parley wiring stabilizes and
graphshell-shaped consumers wire in. Nematic is the smolweb engine in
the Mere workspace (`mere/crates/nematic`); each protocol surfaces
slightly different demands on the renderer (selection in viewers,
caret in composers, scrolling in feed readers).

- [x] **B1. Selection highlight + caret emission** — **CLEARED 2026-05-06**.
  `netrender_text::selection_rects(layout, range)` and
  `netrender_text::caret_rect(layout, byte_index, affinity, width)`
  ship as thin wrappers over `parley::Selection::geometry` /
  `parley::Cursor::geometry`. Both pure CPU; bidi handled natively
  by parley. Receipts at
  [`netrender_text/tests/pb1_selection_and_caret.rs`](../netrender_text/tests/pb1_selection_and_caret.rs)
  (7/7). Trigger framing was protective rather than technical —
  parley's selection API was already in shape, so the wrap was
  ship-now-no-speculation. Full finding:
  [rasterizer plan §11.19](2026-05-01_vello_rasterizer_plan.md).

- [ ] **B2. Scrolling convenience** —
  `Scene::push_scroll_frame(clip_rect, scroll_offset)` macro that
  opens a layer with a rect clip + a translate transform, with a
  matching `pop_scroll_frame()`.
  *Done condition:* the demo gains a scrolling card list under one
  method call instead of three. No architectural commitment — just
  ergonomics over existing primitives.

- [x] **B3. Verify: color emoji / COLR fonts** — **CLEARED 2026-05-06**.
  Verification probe at
  [`netrender_text/tests/pb3_color_emoji_probe.rs`](../netrender_text/tests/pb3_color_emoji_probe.rs)
  measured a 91% chromatic ratio rendering Segoe UI Emoji through
  the vello path. Full finding:
  [rasterizer plan §11.18](2026-05-01_vello_rasterizer_plan.md).
  No netrender-side work item; re-run the probe on text-stack bumps
  as a regression canary.

---

## Phase C — Capability unlocks (`SceneOp` territory)

New op variants / extensions that genuinely expand what the API can
express. Each is an additive change to `SceneOp`; the rasterizer
gains one match arm per item.

- [x] **C1. Stroke decorations** — **CLEARED 2026-05-06**.
  `SceneStroke` gained `cap`, `join`, `dash_pattern`, `dash_offset`
  fields with `SceneStrokeCap` (Butt/Round/Square) and
  `SceneStrokeJoin` (Bevel/Miter/Round) enums. Mapped 1:1 to
  `kurbo::Stroke::with_caps` / `with_join` / `with_dashes`. New
  `Scene::push_stroke_decorated` helper. Tile-cache hash includes
  the new fields.
  Receipt at
  [`netrender/tests/pc1_stroke_decorations.rs`](../netrender/tests/pc1_stroke_decorations.rs)
  (8/8). Full finding:
  [rasterizer plan §11.25](2026-05-01_vello_rasterizer_plan.md).

- [x] **C2. `SceneOp::Pattern`** — **CLEARED 2026-05-06**.
  New `ScenePattern { tile, extent, scale, transform_id, clip_rect,
  clip_corner_radii }` op variant tiles an `ImageKey` across an
  extent rect via vello's `Extend::Repeat`. New `Scene::push_pattern`
  helper. Hit-testing reports `HitOpKind::Pattern`. Tile-cache hash
  invalidates on tile/extent/scale changes.
  Receipt at
  [`netrender/tests/pc2_pattern_op.rs`](../netrender/tests/pc2_pattern_op.rs)
  (8/8). Full finding:
  [rasterizer plan §11.26](2026-05-01_vello_rasterizer_plan.md).

- [x] **C3. Mask-image fills** — **CLEARED 2026-05-06**.
  New `SceneCompose` enum (SrcOver / DestIn) added as a per-layer
  field on `SceneLayer`. `SceneLayer::alpha_mask()` and
  `Scene::push_alpha_mask_layer` open an inner DestIn layer; with
  the standard outer-layer-then-content pattern, content survives
  only where the inner layer's draws are opaque. No new shader
  needed — vello's existing peniko BlendMode supports DestIn
  natively. Receipt at
  [`netrender/tests/pc3_alpha_mask_layer.rs`](../netrender/tests/pc3_alpha_mask_layer.rs)
  (5/5 — including GPU smoke that proves a half-and-half mask
  shows content on one side and zero on the other). Full finding:
  [rasterizer plan §11.29](2026-05-01_vello_rasterizer_plan.md).

- [x] **C4. Variable fonts axis interpolation** — **CLEARED 2026-05-06**.
  `SceneGlyphRun` gained `font_axis_values: Vec<(SceneFontAxisTag,
  f32)>` (4-byte ASCII tags + user-space values, e.g.,
  `(*b"wght", 700.0)`). `emit_glyph_run` resolves user→normalized
  via `skrifa::Axes::location` and threads through to vello's
  `DrawGlyphs::normalized_coords`. New `Scene::push_glyph_run_variable`
  helper. Tile-cache hash includes axis values.
  Receipt at
  [`netrender/tests/pc4_variable_fonts.rs`](../netrender/tests/pc4_variable_fonts.rs)
  (7/7) — including a GPU smoke that renders Bahnschrift at three
  weights and verifies bold paints visibly more ink than light. Full
  finding:
  [rasterizer plan §11.27](2026-05-01_vello_rasterizer_plan.md).

---

## Phase D — Architecturally significant

Items that need real design conversation, not just implementation.

- [x] **D1. Backdrop filter** — **CLEARED 2026-05-08**.
  New `SceneFilter::Blur(f32)` enum + `SceneLayer.backdrop_filter:
  Option<SceneFilter>` field. `Renderer::render_vello` detects
  backdrop layers, pre-renders the scene-prefix to a texture,
  blurs it via the existing render-graph (R5's downscale path
  applies for large radii), and injects a `SceneImage` covering
  the layer's bounds at the start of the layer's scope.
  Multi-pass orchestration is opaque to the consumer; no separate
  API call. Receipt at
  [`netrender/tests/pd1_backdrop_filter.rs`](../netrender/tests/pd1_backdrop_filter.rs)
  (4/4) — including a GPU smoke that verifies a busy striped
  background under a `Blur(12)` filter shows >50% reduction in
  local horizontal variance compared to the unfiltered reference.
  Full finding:
  [rasterizer plan §11.30](2026-05-01_vello_rasterizer_plan.md).

- [x] **D2. Animated values** — **CLEARED 2026-05-08**.
  New [`netrender::interpolate`](../netrender/src/interpolate.rs)
  module: CSS timing curves (`linear`, `ease`, `ease_in`,
  `ease_out`, `ease_in_out`, `step_start`, `step_end`,
  `cubic_bezier`), generic `lerp` for scalar / array / color, and
  `sample_keyframes` for keyframe-driven animation. All pure
  functions; no clock, no Scene-side state. Consumer drives time
  and rebuilds the Scene per frame with resolved values — keeps
  the determinism invariant from A2 intact. Receipt: 14 unit
  tests in `netrender::interpolate::tests`. Full finding:
  [rasterizer plan §11.31](2026-05-01_vello_rasterizer_plan.md).

- [ ] **D3. Native-compositor handoff (axiom 14) via path (b′)** —
  exporting per-surface textures to native OS compositors so the OS
  applies transform / clip / opacity at 60Hz without re-rasterizing.
  *Status:* see
  [`2026-05-05_compositor_handoff_path_b_prime.md` §5](2026-05-05_compositor_handoff_path_b_prime.md).
  Sub-phases 5.1–5.4 shipped; 5.5 (servo-wgpu adapter) lives in the
  `servo-wgpu` repo. **Do not duplicate sub-phase status here — the
  compositor plan is canonical.**
  *Trigger:* 5.5 lands in servo-wgpu.
  *Done condition:* mark complete and migrate to a `§11.x — CLEARED`
  finding in the rasterizer plan.

---

## Phase E — Performance / scaling

Don't pre-build; profile under real consumer load first. E2 is gated
on Phase A4 (frame profiler) data. E1 is **upstream-blocked, not
A4-gated** — listed for visibility only.

- [ ] **E1. GPU damage tracking (sub-tile)** — *upstream-gated.*
  Today vello re-encodes the whole scene every frame. For scrolling
  content where most of the screen is unchanged, this is wasted
  compute. The fix lives at vello's encoder level and is upstream's
  responsibility — see rasterizer §13 risk #8 (accepted). Track but
  don't start; requires upstream coordination. No netrender-side done
  condition; entry exists for visibility.

- [x] **E2. Multi-thread scene building** — **CLEARED 2026-05-08**.
  New `SceneFragment` builder type + `Scene::append_fragment`
  join API. Each fragment carries its own ops / transforms /
  fonts / image_sources; on append, fragment-local ids are
  rewritten to the parent scene's index space (identity at id 0
  / sentinel font at id 0 stay at 0). Consumer-supplied
  ImageKeys are not remapped — partition the keyspace per
  thread. Receipt at
  [`netrender/tests/pe2_scene_fragment.rs`](../netrender/tests/pe2_scene_fragment.rs)
  (9/9) — including a 4-thread parallel build of 10k ops that
  confirms the API works end-to-end. Full finding:
  [rasterizer plan §11.32](2026-05-01_vello_rasterizer_plan.md).

---

## Phase F — Platform / output

Bigger commitments that broaden what platforms / output formats the
renderer reaches.

- [ ] **F1. HDR / wide-gamut output** — Display P3 / Rec2020.
  *Trigger:* vello's color-pipeline trajectory exposes a P3 / Rec2020
  storage-target option. Watch and wait, not build.
  *Done condition:* a render target in P3 produces visibly more
  saturated reds/greens/blues vs. sRGB on a P3-capable display.

- [x] **F2. WebAssembly target — library readiness** — **CLEARED 2026-05-08**.
  Audit found `cargo check -p netrender --target wasm32-unknown-unknown`
  already passed clean — the only wasm-runtime hazard in lib code was
  `pollster::block_on` in `netrender_device::core::boot`. Fixed by
  splitting the boot into a portable async core
  [`netrender_device::boot_async`](../netrender_device/src/core.rs)
  and gating the blocking `boot()` wrapper to
  `#[cfg(not(target_arch = "wasm32"))]`. Browser consumers call
  `boot_async().await` from `wasm-bindgen-futures::spawn_local` (or any
  executor); native consumers keep the existing blocking entry point.
  `WgpuDevice::boot_async` mirrors the pattern. Both `netrender_device`
  and `netrender` now `cargo check` clean against
  `wasm32-unknown-unknown`. F2's prior framing as "real cost (wasm
  build infra)" was a protective gate — the technical work was a
  thin-wrap shape over `wgpu`'s already-async API. The wasm-bindgen
  *demo* crate (running the card grid in a browser canvas) is real
  consumer work and remains gated on a real consumer commitment, but
  that's an embedder example, not a netrender library cost.
  *Note:* the existing `wasm-portability-checklist.md` in this
  directory is for the WebRender wgpu-backend branch (a separate
  project) and never applied to netrender's smaller surface area.

---

## Phase G — WebGL-over-wgpu companion lane

The OpenGL-content path for Serval/Pelt. Web pages do not get raw
OpenGL; they get WebGL/WebGL2. The target architecture is **WebGL API
compatibility over wgpu**, sitting beside NetRender — not inside
NetRender core. NetRender's job is the final composition surface
(place the canvas texture in painter order, clip/transform it,
participate in damage and presentation), not WebGL's API state
machine, extension matrix, shader-language validation, or
resource-lifetime semantics.

- [ ] **G. WebGL-over-wgpu adapter crate** — full sub-plan in
  [`2026-05-06_webgl_over_wgpu_plan.md`](2026-05-06_webgl_over_wgpu_plan.md).
  *Trigger:* Serval/Pelt commits to a canvas-bearing demo, or a
  WebGL-using site enters the test set.
  *Done condition:* covered by the sub-plan's G0–G6 sequence; the
  netrender-side hook lands as part of G4 (texture compositing). The
  rest is sibling-crate or test-infra work.

---

## Out of scope (visibility-only)

Real consumer pain that is explicitly *not* netrender's job. Listed
once so embedders know we know; no checkbox, not tracked.

- **CSS font-cascade rules** (rasterizer plan §4.4). DirectWrite /
  CoreText-style font selection (CJK fallback chains, `font-family`
  priority lists with locale-aware preferences). Fontique enumerates
  system fonts but doesn't implement the full cascade. Embedders that
  need this implement it on top of fontique or wrap a system text
  engine.

- **Synthetic bold/italic via parley `Synthesis`**
  (`netrender_text` doc). Parley's `Synthesis` flags are ignored.
  Consumer recommendation: use real bold/italic font files instead.
  Flagged for visibility; not a netrender concern.

---

## When to revisit this list

Add an item when a real consumer surfaces a need that doesn't fit the
current API. Move an item out of the list (into a `§11.x — CLEARED`
plan finding) when it lands. Re-order liberally based on what the
consumer is actually using.
