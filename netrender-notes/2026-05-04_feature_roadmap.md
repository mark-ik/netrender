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

- [ ] **R1. Real font-metric per-glyph hit testing**
  (rasterizer plan §11.15).
  *Bites when:* a consumer needs precise click-on-character behaviour
  (text editors, selection caret placement). Today's em-box
  approximation is enough for "click on this label" UI but imprecise
  around descenders and kerning gaps.
  *Trigger:* a consumer reports caret/selection mis-targets at
  descenders or kerning gaps, or nematic's Markdown/Scroll text
  composer needs caret precision.
  *Done condition:* `hit_test` on a glyph run with descenders
  (e.g. `g`, `y`) returns the glyph index that the click pixel
  actually overlaps, verified against `skrifa::metrics::GlyphMetrics`
  (parley already pulls skrifa transitively). Receipt: extend
  `per_glyph_hit_returns_glyph_index`.

- [ ] **R2. Per-segment point-in-polygon for `SceneOp::Shape`**
  (rasterizer plan §11.12).
  *Bites when:* a consumer hit-tests an arbitrary path (hexagonal
  node, custom widget shape) and the AABB hit area is too sloppy.
  *Trigger:* graphshell hex-node hit testing surfaces phantom hits at
  AABB corners, or an equivalent consumer report.
  *Done condition:* `hit_test` on a non-rectangular `ScenePath`
  returns true only inside the path interior, via
  `kurbo::Shape::contains` on a `BezPath` built from the
  `ScenePath`. Receipt: a probe with a triangle path that asserts
  no hit at the AABB corners.

- [ ] **R3. Layer-clip path-precise containment**
  (rasterizer plan §11.16).
  *Bites when:* a layer has an arbitrary `SceneClip::Path`, and a
  consumer needs hit-testing to honor the path's true shape (not its
  AABB). Today rounded corners and arbitrary path interiors are
  AABB-conservative — points near corners or outside the
  path-but-inside-the-AABB still register hits.
  *Trigger:* same R2 trigger applied to the layer-clip stack, or a
  rounded-corner-card report.
  *Done condition:* `hit_test` inside a `SceneClip::Path`-clipped
  layer respects the path interior, not the AABB. Same
  `kurbo::Shape::contains` machinery as R2, applied to the
  layer-clip stack pre-pass. Receipt: rounded-rect clip probe
  asserts no hit at the corners outside the rounded radius.

- [ ] **R4. Image cache for the simple (non-tile) rasterizer**
  (rasterizer plan §11.9-equivalent open item; the unification finding
  is §11.9).
  *Bites when:* a consumer uses the simple `scene_to_vello` path
  (non-tile) on image-heavy scenes. The simple path's
  `build_image_cache` rebuilds peniko `ImageData` blobs every call
  because it has no state to hold them in.
  *Trigger:* a simple-path consumer reports a per-frame allocation
  hot spot in image rebuild, or A4 frame profiler surfaces it.
  *Done condition:* an image-heavy scene rendered twice through the
  simple path reuses the cached `peniko::ImageData` blobs on the
  second call, via a stateful wrapper struct (mirror of
  `VelloTileRasterizer::image_data`) or by lifting the cache to the
  caller. Receipt: probe asserts allocation count is flat across N
  calls.

- [ ] **R5. Downscale-blur-upscale for very large blurs**
  (rasterizer plan §11.10).
  *Bites when:* a consumer needs `blur_radius_px > ~28`. Today's
  multi-pass cascade caps at `MAX_PASSES = 50`, which σ-clips beyond
  that radius. Skia and Firefox use downscale-then-blur for large
  radii — same approach would lift the cap.
  *Trigger:* a consumer requests `blur_radius > 28` (CSS
  `backdrop-filter: blur(40px)` is plausible) and the σ-clip is
  visible; or D1 (backdrop filter) reaches a radius where the cap
  bites.
  *Done condition:* `blur_radius_px = 64` produces a visually correct
  gaussian (matches a downscale-then-blur reference within tolerance)
  with no σ-clip artifact, via render to a half- or quarter-resolution
  intermediate texture, blur there, upscale to target. Two more
  render-graph tasks per "downscale level." Receipt: oracle PNG
  comparison.

- [ ] **R6. Inline-box rendering helper in `netrender_text`**
  (rasterizer plan §4.4).
  *Bites when:* a consumer's text contains inline images / nested
  layouts (`<img>` tags, embedded widgets). The adapter currently
  skips `PositionedLayoutItem::InlineBox`; consumer renders boxes
  themselves but has to re-derive line geometry to do it.
  *Trigger:* a consumer wires inline images and the per-line walk is
  awkward enough to push a helper upstream.
  *Done condition:* `netrender_text` exposes a per-line walker that
  yields glyph runs and inline-box placements in visual order; the
  consumer paints boxes inline without re-deriving line geometry.
  (Inline boxes themselves stay consumer-rendered — their content is
  consumer-typed.) Receipt: helper consumed by a synthetic
  mixed-content layout test.

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

  - **R9-canary (trigger setup).** CI canary that re-runs
    `p1prime_03` against the current vello dep on every bump,
    surfacing the moment vello's GPU compute path honors
    `peniko::Gradient::interpolation_cs`. A `linear-light-canary`
    cargo feature gates the test; CI runs
    `cargo test --features linear-light-canary` on dep bumps. Greens
    → fires the R9 trigger. Replaces the passive "every vello version
    bump, re-test by hand" cadence the previous deferred-phases doc
    documented.

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

- [ ] **B1. Selection highlight + caret emission** — the next thing
  nematic will pull on once it has shaped text. Concrete first uses:
  Gemini / Gopher / Scroll viewer selection-to-quote, Markdown
  editor caret, feed-reader excerpt selection.
  *Trigger:* nematic ships shaped text via parley (Gemini/Gopher
  viewer first) and asks for selection rects.
  *Done condition:* `netrender_text` exposes
  `selection_rects(layout, range) -> Vec<[f32; 4]>` (one rect per
  visual line in the selection), and a thin caret helper that emits a
  blink-friendly thin rect at a `parley::Cursor`. Caret blink is
  consumer-side; netrender paints the rect.

- [ ] **B2. Scrolling convenience** —
  `Scene::push_scroll_frame(clip_rect, scroll_offset)` macro that
  opens a layer with a rect clip + a translate transform, with a
  matching `pop_scroll_frame()`.
  *Done condition:* the demo gains a scrolling card list under one
  method call instead of three. No architectural commitment — just
  ergonomics over existing primitives.

- [ ] **B3. Verify: color emoji / COLR fonts** — vello + skrifa
  already handle COLR layer rendering on the glyph path; we likely
  get this for free. Verification probe, not a build item.
  *Trigger:* none — run on the next text-stack work pass.
  *Outcome A (verified):* file as a CLEARED §11.x finding; no further
  work.
  *Outcome B (regression found):* demote to a real work item with
  scope set against the actual gap (which upstream piece is missing).
  *Done condition:* load an emoji-bearing font (Segoe UI Emoji on
  Win, Apple Color Emoji on Mac, Noto Color Emoji on Linux), shape a
  string with emoji, render — emoji appear in color, not as black
  silhouettes.

---

## Phase C — Capability unlocks (`SceneOp` territory)

New op variants / extensions that genuinely expand what the API can
express. Each is an additive change to `SceneOp`; the rasterizer
gains one match arm per item.

- [ ] **C1. Stroke decorations** — line caps (`butt` / `round` /
  `square`), joins (`miter` / `round` / `bevel`), dash patterns.
  *Trigger:* a consumer needs CSS `border-style: dashed`, or
  graphshell-shaped consumers want stylized edges.
  *Done condition:* `SceneStroke` gains optional `cap`, `join`,
  `dash_pattern` fields; the rasterizer plumbs them through to
  `kurbo::Stroke`. CSS `border-style: dashed` becomes expressible.

- [ ] **C2. `SceneOp::Pattern`** — repeated-tile fill (CSS
  `background-image` with `repeat`).
  *Trigger:* a consumer renders repeating backgrounds and pushes 16
  copies by hand once.
  *Done condition:* `SceneOp::Pattern { tile: ImageKey, extent:
  [f32; 4], scale, transform_id, clip_rect, clip_corner_radii }`
  variant lands; tiling a 64×64 image across a 256×256 area takes one
  push call instead of 16.

- [ ] **C3. Mask-image fills** — using one image as an alpha mask for
  another fill (any `SceneOp` body).
  *Trigger:* a consumer needs CSS `mask-image` or shaped vignettes
  without pre-baking.
  *Done condition:* `Scene::push_layer_mask(image_key, ...)` opens a
  layer whose visibility is gated by the mask image's alpha (one new
  WGSL helper or a vello layer trick). Decouples mask from fill so
  the consumer doesn't have to pre-bake.

- [ ] **C4. Variable fonts axis interpolation** — parley + skrifa
  support this; the scene needs to thread axis values through to
  vello's `draw_glyphs`.
  *Trigger:* a consumer ships a variable font and wants animated
  weight/width.
  *Done condition:* a single font rendered at three different `wght`
  axis values produces three visibly distinct weights in one frame.

---

## Phase D — Architecturally significant

Items that need real design conversation, not just implementation.

- [ ] **D1. Backdrop filter** — frosted-glass blur of *what's behind*
  a translucent rect. Distinct from drop shadow. Architecturally hard
  because vello's "always overwrite the whole target" model is in
  tension with reading the backdrop.
  *Trigger:* a consumer commits to CSS `backdrop-filter` (Serval will
  hit this on real-world content; graphshell-shaped consumers may
  want it for chrome).
  *Pre-design discussion needed:* snapshot-then-blur-then-composite-
  over (multi-pass), or a vello layer trick if one exists in newer
  vello. Resolve before implementation lands.
  *Done condition:* CSS `backdrop-filter: blur(12px)` produces a
  frosted-glass nav bar over a busy background.

- [ ] **D2. Animated values** — interpolate alpha / transform / color
  over time.
  *Trigger:* a consumer needs CSS animations or transitions painted
  from netrender side.
  *Design choice (lock before any code lands):* does netrender own
  the timing (and read a clock), or does the consumer drive per-frame
  and rebuild ops with resolved values? Current read: keep netrender
  clockless; provide an `interpolate.rs` module of timing curves and
  let the consumer drive. Convert this read into a written decision
  on this entry before D2 is picked up.
  *Done condition:* `SceneOp::PushLayer` accepts `Animated<f32>` for
  alpha; the renderer either samples the curve at a given time or the
  consumer rebuilds the scene per frame with resolved values
  (whichever the design choice settles on).

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

- [ ] **E2. Multi-thread scene building** — building `Vec<SceneOp>`
  in parallel chunks and joining. The op-list shape is conducive (no
  cross-references between siblings).
  *Trigger:* A4 data shows scene-build CPU pressure under real
  consumer load.
  *Done condition:* a 4-thread scene build of 10k ops takes <2× the
  wall time of a 1-thread build of 2.5k ops, via a `SceneFragment`
  builder type and a join API.

---

## Phase F — Platform / output

Bigger commitments that broaden what platforms / output formats the
renderer reaches.

- [ ] **F1. HDR / wide-gamut output** — Display P3 / Rec2020.
  *Trigger:* vello's color-pipeline trajectory exposes a P3 / Rec2020
  storage-target option. Watch and wait, not build.
  *Done condition:* a render target in P3 produces visibly more
  saturated reds/greens/blues vs. sRGB on a P3-capable display.

- [ ] **F2. WebAssembly target** — browser-hosted netrender.
  *Trigger:* a real consumer commits to a browser-hosted demo. Until
  then, untracked.
  *Done condition:* `wasm-pack build` produces a `.wasm` that runs
  the demo card grid in a browser canvas. The wgpu side handles the
  webgpu adapter natively; netrender side needs `boot()` adapter
  selection + a wasm-bindgen shim crate.
  *Note:* a netrender-specific portability checklist needs to be
  authored at trigger time. The existing
  `wasm-portability-checklist.md` in this directory is for the
  WebRender wgpu-backend branch (a separate project) and does **not**
  apply as netrender's gating list.

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
