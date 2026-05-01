# Vello Tile Rasterizer Plan (2026-05-01)

**Status**: Proposal. Sibling to
[2026-04-30_netrender_design_plan.md](2026-04-30_netrender_design_plan.md)
(hereafter "the parent plan"). Does not supersede; amends Phases 8 / 9
/ 10 / 11 / 12 if adopted.

**Premise**: Replace netrender's per-primitive WGSL pipeline cadence
with vello as the tile rasterizer. Webrender's display-list ingestion,
spatial tree, picture cache, tile invalidation, render-task graph,
and compositor handoff stay. Vello takes over everything that
currently lives in the brush family WGSLs.

**Decision window has partially closed (refresh 2026-05-01).**
Phase 8D (gradient unification) and 9A/9B/9C (rounded-rect clip
mask + box-shadow + fast path) shipped between this doc's first
draft and now. Every WGSL family that already shipped through the
batched pipeline is sunk cost — under vello adoption we delete
those shaders. The plan-time savings calculus in §1 still holds
but the *unrealized* portion has shrunk: Phase 10 (text), Phase 11
(borders / box shadows / line decorations), Phase 12 (filter chains
/ nested isolation) are the remaining recoverable months. Phase 8
and Phase 9 are no longer recoverable; they're already in tree.
This affects §14's recommendation, not the architectural argument.

---

## 1. What this solves

The parent plan budgets ~13 months for full webrender-equivalent. The
bulk of that — Phases 8 (shader families), 9 (clip masks), 11 (borders
/ box shadows / line decorations), and parts of 12 (filter chains,
nested isolation) — is *primitive-rasterization work*: each family
gets its own WGSL file, pipeline factory, primitive-layout extension,
batch-builder slot, and golden scene. Vello already does all of this
natively.

Concretely, vello obviates:

- **Gradient families** (Phase 8A–8D): linear / radial / conic with
  N-stop ramps. `peniko::Gradient` covers all three with arbitrary
  stops and color spaces.
- **Clip masks** (Phase 9): vello supports arbitrary path-shaped
  clipping via `Scene::push_layer(clip_path, ...)`. Webrender's
  rectangle-AA-mask shader path is not needed.
- **Borders, box shadows, line decorations** (Phase 11): vello renders
  arbitrary paths with per-vertex AA. A box shadow is a blurred
  filled rect; a border is a stroked path. No `area-lut.tga` LUT,
  no segment decomposition, no `border.rs` math.
- **Antialiased path fills** for any future shape primitive: free.
- **Group isolation / opacity layers** (Phase 12): `push_layer` with
  alpha is the same compute pass.

What vello does *not* obviate:

- Display-list ingestion and the `Scene` builder — netrender owns
  this.
- Spatial tree, transform composition, scroll resolution — Phase 3.
- Picture-cache invalidation — Phase 7. Tile invalidation is
  upstream of rasterization; vello is the per-tile fill.
- The render-task graph as a topology — Phase 6. Vello rasterizes
  *into* graph-allocated targets; the graph still orders dependent
  passes.
- Native compositor handoff — Phase 13. Vello renders into wgpu
  textures the compositor exports; the export class (axiom 14) is
  unchanged.
- The image cache — Phase 5. Vello samples textures; netrender owns
  the texture lifetime.
- Hit testing — open question 3. Decision unchanged.

This is a rasterizer swap, not a renderer swap. The pipeline above
the tile fill stays.

## 2. The seam

### 2.1 Where it lands

In netrender as built, the seam is
[`Renderer::render_dirty_tiles`](../netrender/src/renderer/mod.rs)
(today a thin wrapper around the private
`render_dirty_tiles_with_transforms`, which is where the actual
per-tile work happens — both would call into `TileRasterizer`).
Current contract:

```rust
pub fn render_dirty_tiles(
    &self,
    scene: &Scene,
    tile_cache: &mut TileCache,
) -> Vec<TileCoord>;
```

For each dirty tile: take the tile's world rect, filter
`scene.{rects, images, gradients}` to those whose AABB intersects
the tile, allocate a fresh `Rgba8Unorm` texture, render the
intersecting primitives through the brush pipelines under a
tile-local orthographic projection, store the texture as
`tile.texture`. Phase 7C composites those textures into the
framebuffer via one `brush_image_alpha` draw per tile.

The proposed `TileRasterizer` trait extracts the "render the
intersecting primitives into the tile target" step. Picture
cache, invalidation, AABB filter, allocation policy, and
composite stay where they are.

### 2.2 Trait shape

```rust
/// One tile's worth of primitives, post-AABB-filter, in painter
/// order. References Scene-owned data so the rasterizer doesn't
/// re-walk the full scene per tile.
pub struct TileWork<'a> {
    pub world_rect: WorldRect,
    pub tile_size: u32,
    pub format: wgpu::TextureFormat,
    pub scene: &'a Scene,
    pub rect_indices: &'a [usize],
    pub image_indices: &'a [usize],
    pub gradient_indices: &'a [usize],
    pub image_cache: &'a ImageCache,
    pub transforms_buf: &'a wgpu::Buffer,
}

pub trait TileRasterizer: Send {
    /// Rasterize one tile's primitives into `target`. The encoder
    /// is shared across all tiles in a frame; the implementation
    /// records its work and returns. Output texture lifetime is
    /// owned by the caller (the tile cache).
    fn rasterize_tile(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        work: &TileWork<'_>,
        target: &wgpu::TextureView,
    );

    /// Per-frame setup hook. Called once before the first
    /// `rasterize_tile` of a frame. Lets implementations stage
    /// their per-frame buffers (for vello: encode the frame's
    /// glyph runs into the resolver, prepare scene buffer pool).
    fn begin_frame(&mut self, _device: &wgpu::Device) {}

    /// Per-frame teardown / submission hook. Called once after
    /// the last `rasterize_tile` of a frame, before the encoder
    /// is submitted. Vello flushes its compute dispatches here.
    fn end_frame(&mut self, _queue: &wgpu::Queue) {}
}
```

`Renderer` holds a `Box<dyn TileRasterizer>`. Default constructor
selects vello; tests can inject the batched implementation for
parity scenes (see §10).

### 2.3 Why this exact shape

- **`encoder` parameter, not `device.create_command_encoder()` per
  call**: vello's compute dispatches and webrender's render passes
  must coexist on one queue with explicit barriers. One encoder
  per frame, multiple tile calls into it, single submit at frame
  end.
- **`begin_frame` / `end_frame`**: vello's `Renderer::render_to_texture`
  takes a whole `vello::Scene` per call — it isn't designed for
  N independent tile renders sharing global state. The frame hooks
  are where vello's scene-resolver state lives across the per-tile
  encoding. (Whether vello's API even *allows* one resolver to
  drive N targets in one frame is the first thing §11.1 verifies.)
- **`scene: &Scene` plus filtered index slices**: the rasterizer
  does not get a copy of the per-tile primitive list. Indices into
  the scene-level Vecs preserve cache locality and let vello reuse
  glyph encodings across tiles that share text runs.
- **No `&mut self` on `Scene`**: the rasterizer cannot mutate scene
  state. It can mutate its own (vello scene buffer pool, glyph
  resolver cache).

**Two coupling smells worth flagging in this trait shape**:

- `transforms_buf: &'a wgpu::Buffer` is *only* meaningful to the
  batched backend — that's where it ends up bound at slot 1 of the
  brush bind groups. Vello reads `scene.transforms[id]` as
  `kurbo::Affine` and never touches a wgpu::Buffer for transforms.
  Including it in `TileWork` couples the trait to the implementation
  it's supposed to abstract over. Cleaner: drop it; let the batched
  backend (if retained as `TestRasterizer` per §10) build its own
  buffer per-frame, or hold a buffer cache keyed on a scene fingerprint.
- `image_cache: &'a ImageCache` exposes netrender's cache type
  through the trait surface. ImageCache is `pub(crate)` today, and
  vello will want a different lookup shape (peniko::Image, not
  Arc<wgpu::Texture>). The right shape is probably an
  `ImageResolver` trait passed in (or an enum of accepted resolver
  outputs) rather than the full `&ImageCache` reference. Decide at
  §11.4 once we know what vello accepts.

These don't kill the trait shape — both fields can become Optional
or move to a `BatchedRasterizer`-specific construction call — but
the as-drafted struct embeds the brushed-backend's internals where
they shouldn't be.

## 3. Vello-scene encoding for current primitives

This section maps each `Scene*` type onto vello / peniko / kurbo
concepts. The mapping is the substance of what `VelloRasterizer`
does inside `rasterize_tile`. Tile-local projection is applied
once as a `kurbo::Affine` translation pre-multiplied onto every
primitive's transform — vello renders to the tile's local
`(0..tile_size, 0..tile_size)` coordinate space.

### 3.1 SceneRect → filled rect with solid brush

```rust
let aff = tile_local(tile.world_rect, scene.transforms[r.transform_id]);
let shape = Rect::new(r.x0, r.y0, r.x1, r.y1);
let brush = Brush::Solid(Color::rgba(r.color[0], r.color[1], r.color[2], r.color[3]));
vscene.fill(Fill::NonZero, aff, &brush, None, &shape);
```

Premultiplied-alpha contract: `peniko::Color` ingests straight
RGBA; we pass `r.color` directly because netrender's brush WGSLs
already work in premultiplied space. **Verification step at §11.2**
confirms vello's blend math matches premultiplied input. If it
doesn't, the encoder unpremultiplies at the boundary.

### 3.2 SceneImage → filled rect with image brush

```rust
let aff = tile_local(tile.world_rect, scene.transforms[i.transform_id]);
let shape = Rect::new(i.x0, i.y0, i.x1, i.y1);
let img = image_cache.get_peniko_image(i.key)?;  // see §3.5
let uv_xform = uv_to_local(i.uv, shape);
let brush_xform = Some(uv_xform);
vscene.fill(Fill::NonZero, aff, &img, brush_xform, &shape);
```

The tint `i.color` becomes a `peniko::Image::with_alpha` plus a
multiplicative mix layer if RGB tint is non-identity. Pure-alpha
tint is the common case (used by the tile cache's composite draw
itself in Phase 7C).

### 3.3 SceneGradient → peniko::Gradient

`GradientKind::Linear`, `Radial`, and `Conic` map directly:

```rust
let g = match grad.kind {
    Linear => Gradient::new_linear(p0, p1).with_stops(&stops),
    Radial => Gradient::new_radial(center, radius).with_stops(&stops),
    Conic  => Gradient::new_sweep(center, start_angle, end_angle).with_stops(&stops),
};
vscene.fill(Fill::NonZero, aff, &g, None, &shape);
```

`stops` builds from `grad.stops: Vec<GradientStop>` directly.
N-stop is native — Phase 8D's per-instance `stops_offset` /
`stops_count` storage-buffer plumbing disappears.

`peniko::Gradient` supports color-space selection (linear vs.
sRGB interpolation). This is where the long-tail color-correctness
question gets answered for free.

### 3.4 Clip rectangles

`SceneRect.clip_rect` (and its siblings) currently land as a
device-space AABB consumed by the brush WGSL. Under vello:

```rust
let clip_shape = Rect::new(c[0], c[1], c[2], c[3]);
vscene.push_layer(BlendMode::default(), 1.0, identity_aff, &clip_shape);
// emit the prim
vscene.pop_layer();
```

The push/pop bracket is the natural shape for arbitrary-path clips
(Phase 9). For axis-aligned clips this is wasteful; an optimization
opportunity is to coalesce contiguous prims sharing the same clip
into one layer. Defer until profile shows it matters.

**`NO_CLIP` fast path.** netrender's `NO_CLIP` sentinel
(`[NEG_INFINITY, NEG_INFINITY, INFINITY, INFINITY]`) is the common
case for primitives that don't need clipping at all. The vello
encoder must skip `push_layer`/`pop_layer` entirely when it sees
the sentinel — emitting a layer per primitive for the no-clip
majority would dwarf any other rasterization cost. Detect via a
cheap `clip_rect[0].is_finite()` check at encode time.

### 3.5 Image cache integration

The current `ImageCache` stores `Arc<wgpu::Texture>` by `ImageKey`.
Vello's image brush takes a `peniko::Image` constructed from
`ImageData::new(blob, format, width, height)` where `blob` is
CPU-side bytes. Two integration paths:

- **Path A — re-decode for vello.** ImageCache keeps both a CPU-side
  `Arc<[u8]>` (blob) and a GPU `Arc<wgpu::Texture>`. Vello consumes
  the blob; webrender-batched fallback (if retained, see §10)
  consumes the texture. Memory cost: 2x for cached images. Decode
  cost: zero (blob is the post-decode bytes).
- **Path B — vello samples GPU textures directly.** Vello supports
  `peniko::Image` backed by an external `wgpu::Texture` since
  vello 0.3+ via `vello::wgpu_engine::ExternalImage` (or whatever
  the post-renaming API is — verify in §11.1). This is the right
  path; Path A is a fallback if external-texture import doesn't
  fit netrender's lifetime model.

The image cache stays the lifetime authority. Vello holds borrowed
views; the consumer holds the `Arc` until `PreparedFrame` submission
completes (axiom 16, unchanged).

## 4. Glyphs

This is the hardest delta. The parent plan (Phase 10) lifts
`wr_glyph_rasterizer` and builds an atlas. Vello's glyph path is
fundamentally different: glyphs are encoded as paths via `skrifa`,
rasterized by vello's compute pipeline per frame, no atlas, no
CPU-side rasterization, no `Proggy.ttf` LUT.

### 4.1 Decision: drop the atlas plan

If vello is the rasterizer, drop wr_glyph_rasterizer entirely.
Phase 10a (atlas + glyph quads) and Phase 10b (subpixel policy,
snapping, atlas churn, fallback fonts) collapse to:

- 10a': font ingestion through skrifa, `Glyph` runs as
  `vello::Glyph { id, x, y }`, `vscene.draw_glyphs(font_ref).
  brush(...).draw(...)`.
- 10b': skrifa already handles hinting via fontations/swash; subpixel
  policy is a vello config, not a netrender-side reinvention.

Net plan-time delta: roughly -2 months. Larger if the parent's
"browser-grade text correctness" estimate (1–2 months) was on the
optimistic side.

### 4.2 Frame cost vs. cache cost

The atlas-based path amortizes glyph rasterization across frames;
vello re-encodes paths every frame. On modern GPUs the compute
rasterization cost is generally not the bottleneck for typical text
volumes, but this is a real change in cost shape:

- atlas path: O(unique_glyphs_ever_seen) raster work; O(visible_glyphs)
  per-frame sampling.
- vello path: O(visible_glyphs) raster work per frame.

For static pages this is roughly equivalent. For long scrolling
sessions over the same fonts, the atlas wins. For dynamic content
that introduces new glyphs (CJK pages, infinite-scroll feeds), vello
wins (no atlas churn / eviction). Browser workloads span both regimes;
vello's per-frame cost has been shown adequate on Chromium-class
content in vello's own benchmarks. This is a "verify on real
servo-wgpu pages, profile, decide if a glyph cache layer is needed"
follow-up, not a Phase 10 blocker.

### 4.3 Embedder font ingestion

Skrifa consumes font bytes. Servo's font system emits decoded font
data in a form skrifa can ingest (TTF/OTF blob). The consumer
(Servo, Graphshell) supplies the blob; netrender resolves it to
`vello::peniko::Font`. Same axiom-16 contract as images: external
resources are local by the time they hit the renderer.

## 5. Filters and the render-task graph

Phase 6 is delivered. Phase 12 (filter chains, nested isolation)
is queued. Vello's relationship to the render-task graph:

- **Vello does *not* own the graph.** Webrender's `RenderGraph`
  topology, topo-sort, and per-task encode callback all stay.
- **Tile rasterization is one node** in the graph. The node's
  encode callback dispatches the vello rasterizer for the tile's
  primitives. Multiple tile nodes can run in parallel within the
  graph's sequencing (vello's scene encoder is `&mut self`, so
  per-tile `vello::Scene` instances are needed if parallelizing —
  see §11.3).
- **Filter render-tasks consume tile outputs as inputs.** A blur
  task takes a vello-rasterized tile texture, runs the existing
  `brush_blur.wgsl` (Phase 6), produces a blurred texture. The
  filter pipeline is webrender-native; only the upstream
  rasterization changed.
- **Backdrop filters** read from a backdrop texture (the composite
  below the picture). That's a graph dependency edge — the picture's
  rasterization waits on the backdrop being composited. Vello on
  the picture, webrender composite below it; both ends of the
  edge are explicit in the graph.

Vello has its own filter primitives (`Mix` blend modes, opacity
layers via `push_layer`). For Phase 12's compositing-correctness
work, the question is: do filters happen *inside* vello (as part
of one tile's scene encoding) or *between* graph tasks? Default:
inside vello when the filter is local to one picture (opacity, mix-
blend); between graph tasks when the filter consumes a finished
target (drop shadow with offset, backdrop blur). The parent plan's
"render-task graph as DAG" stays the right abstraction.

## 6. Color contract

The parent plan defines two color regimes:

- Phase 1–6: surface `Rgba8UnormSrgb`, internal blend in sRGB-encoded
  space ("wrong-but-consistent"), goldens lock in this output.
- Phase 7+: linear `Rgba16Float` intermediate, linear blend math,
  composite back to `Rgba8UnormSrgb` surface, goldens stay valid.

Vello expects linear blend space. Adopting vello forces the Phase
7+ regime *immediately* instead of in a later phase. Implications:

- Tile textures move to `Rgba16Float` (or a vello-default linear
  format; verify in §11.1). The Phase 7 default `Rgba8Unorm` for
  tile storage is replaced.
- Phase 2-7 oracles captured under sRGB-encoded blend math will
  *diverge* from vello output in scenes that exercise alpha
  compositing. Pure-opaque scenes are unaffected. Of the five
  Phase-0.5 oracles, `blank` is unaffected; the four others must
  be re-captured against the vello pipeline as part of Phase 1'
  (see §10).
- The Phase 1 "wart" disappears: gradients and blends are
  mathematically right from day one of the vello path.
- Surface format stays `Rgba8UnormSrgb`. Final composite encodes
  on store. External color contract (what the embedder sees) is
  unchanged.

This is correctness-*mandatory* under vello, not optional. The
parent plan let Phase 1–6 ship in the sRGB-encoded-blend regime
deliberately; the regression to vello's linear regime is forced,
not voluntary. The oracle re-capture is genuine cost (Phase 1'
budgets a few days for it) and the four affected goldens'
reference images change in ways that the *parent plan would have
considered "incorrect" until Phase 7+*. Anyone reviewing
diff-against-old-goldens during the migration will see what looks
like regressions; they're not, they're the corrected math
arriving early. Worth a one-pager in Phase 1' explaining the diff
to the reader before they file a bug.

## 7. Axiom amendments

The parent plan's axiom 10 says "feature tiering is real" and that
phases 1–9 work on `wgpu::Features::empty()` baseline. Vello does
not. It needs (verify exact list in §11.1; this is the expected
ballpark):

- compute pipelines (universal in wgpu — not gated)
- storage buffers with read/write access (universal)
- atomic operations on storage buffers (universal in wgpu 25+)
- subgroup operations for the fast path; vello has a fallback
  when absent
- larger-than-baseline `max_compute_workgroup_storage_size` (verify)

Practically: vello runs on the same hardware tier netrender targets
(Vulkan / Metal / DX12 / WebGPU), but the *exact wgpu features*
required exceed `Features::empty()`.

**Axiom 10 amendment under this plan**: the rasterizer baseline
becomes the union of `Features::empty()` and vello's required
features (call it `VELLO_BASELINE`). Boot fails if those are
unavailable. Software fallbacks (Lavapipe / WARP / SwiftShader)
must be verified to satisfy `VELLO_BASELINE`; if any does not,
Phase 0.5's headless-CI assumption breaks for that adapter.

§11.1 owns this verification. The doc *cannot* stand without it.

## 8. Doesn't this conflict with axiom 11?

Axiom 11: "WGSL is authored, never translated." Vello ships
pre-built WGSL shaders inside its crate. We don't author them; we
don't translate them. We *consume* them.

The axiom's intent — no GLSL→WGSL pipeline, no glsl-to-cxx, no
template-language opacity — is satisfied. Vello's WGSL is human-
authored upstream and ships as-is in our binary. The crate import
does not introduce a translation step.

Stricter reading of axiom 11 ("we author every WGSL line in our
binary") would prohibit any third-party shader. That reading
makes vello and any other GPU library un-usable. Reject the
strict reading; the intent reading is what survives.

Add to the parent doc: "axiom 11 prohibits *translation pipelines*
in our build, not third-party shader crates."

## 9. Crate structure

The parent plan introduces `netrender_device`, `netrender`, and a
deferred `netrender_compositor`. Vello adoption adds:

- `vello = "{ pinned version, see §11.1 }"` as a dependency on
  `netrender` (not `netrender_device` — vello operates above the
  device-foundation layer).
- `peniko`, `kurbo`, `skrifa`, `fontations` arrive transitively.
- `netrender_device` is unaffected. Its WGPU foundation, pipeline
  factories for non-rasterization passes (compositor blits, blur,
  filter primitives), and pass-encoding helpers all stand.

No new netrender crate split is required for this plan. A future
`netrender_text` crate could wrap font ingestion + glyph runs if
that surface grows enough to warrant separation; not a launch-time
concern.

## 10. The "two backends" trap

The temptation: keep the batched WGSL implementation and add vello
as a second backend behind `TileRasterizer`. Don't.

Two production backends means:

- Every golden scene runs in two flavors. Test matrix doubles.
- Every primitive-shape change (new clip semantics, new gradient
  interpolation policy) lands twice or one backend silently lags.
- Color contracts diverge: batched is sRGB-blend until Phase 7+;
  vello is linear from day one. Goldens for one cannot golden the
  other without a tolerance band wide enough to mask real
  regressions.
- The Phase 8/9/11 plan-time savings (§1) only materialize if vello
  is *the* path. Maintaining the batched path means still authoring
  the WGSL, the pipeline factory, the batch slot, the golden — for
  every family — to keep the fallback alive.

The defensible role for the trait is *testability and option value*:

- A `TestRasterizer` impl that records calls (no GPU work) for unit
  tests of the per-tile filter / dispatch logic. **In tree, in the
  `tests/common/` module, not in the production `netrender` crate.**
  Means the trait surface is `pub(crate)` enough to mock without
  exporting it as a stable API.
- The trait stays in tree as escape hatch for the year vello turns
  out to mishandle some browser-shaped corner case nobody anticipated.
  But there is no "official second implementation" we maintain.
  The escape hatch is documented as load-bearing-in-emergencies-only.
  *If* such an emergency materializes, that's a "fork the project,
  don't graft a second backend" situation; the codebase's coherence
  is more valuable than the optionality.

The parent plan's batched WGSLs (`brush_rect_solid`, `brush_image`,
`brush_linear_gradient`, etc.) and their goldens are *deleted* when
vello takes over the corresponding tile-fill path. They land in
git history; they don't live alongside vello in the binary.

## 11. Verification before commit

Before writing a single line of `VelloRasterizer`, the following
must be confirmed. Each item produces a yes/no decision; any "no"
that can't be designed around kills this plan.

### 11.1 wgpu / vello version compatibility

netrender currently pins `wgpu = "29"`. Vello tracks wgpu releases
on its own cadence. Find the vello version that pairs with wgpu 29
(or accept a wgpu downgrade — high cost, axiom-13 implications).
Pin both in `Cargo.toml`.

Also list vello's required wgpu features (axiom 10 amendment, §7),
and confirm Lavapipe / WARP / SwiftShader satisfy them on CI's
software adapters. If software fallback breaks, Phase 0.5's
headless-CI promise is in tension with this plan and one of the
two has to give.

**Output**: a Cargo.lock entry, a `VELLO_BASELINE` features list,
and a CI smoke test that boots vello on the same software adapter
the netrender suite uses.

### 11.2 Premultiplied-alpha and color-space

netrender's brush WGSLs work in premultiplied space. Vello's
`peniko::Color` and brush-input contract — does it ingest
premultiplied or straight? If premultiplied, identity. If straight,
encode unpremultiply at the boundary in `VelloRasterizer`.

Same question for color stops in gradients (linear-RGB
interpolation vs. sRGB). Verify peniko's default and pin it
explicitly.

**Output**: a one-tile parity test — render a half-alpha red rect
through the batched path and through vello, compare. Document the
delta (expected zero in premultiplied; small if not).

### 11.3 Vello scene reuse / parallelism model

Does vello support N independent scene encodings sharing a glyph
resolver / image cache, or is one `vello::Scene` the unit and we
build N of them per frame? §2.3 assumes the latter. If vello has
explicit support for multi-target-per-frame, §2.3's `begin_frame`
/ `end_frame` simplifies.

Related: can `vello::Renderer::render_to_texture` share an encoder
with non-vello commands? If it requires its own encoder per call,
§2.3's "one encoder per frame" needs revision.

**Bigger question lurking here**: can one `vello::Scene` render to
N viewport regions of one larger target in a single dispatch, or
to N independent targets sharing one resolver state? If yes, our
"per-tile vello scene" architecture is wasteful — we'd build one
whole-frame scene and ask vello to fill the dirty tiles.
That collapses §2.3 substantially and avoids vello's
internal-tiling overhead being applied to our 256² tiles. If no,
the per-tile-scene shape stands but is performance-gated on
vello's amortized per-scene cost. Worth verifying first because
the answer reshapes the trait.

**Output**: a working spike that renders 4 separate tile textures
in one frame, sharing whatever vello state can be shared. Measure
overhead per tile. If multi-region-from-one-scene is supported,
spike that path too and compare.

### 11.4 External-texture import

§3.5 Path B assumes vello can sample wgpu textures owned by
netrender's image cache. Verify the API exists (or what shape it
takes — plain `wgpu::Texture` reference vs. peniko-side wrapper),
and confirm lifetime semantics align with axiom 16.

**Output**: image-rect parity test, vello path samples the same
GPU texture the batched path samples. Same pixel output (within
color-contract delta).

### 11.5 Filter task interop

§5 asserts vello renders into a graph-allocated target and
downstream filter tasks consume it. Verify the format vello
prefers as render target is one filter tasks can sample.
`Rgba16Float` is the likely answer; confirm.

**Output**: drop-shadow integration test — vello rasterizes a
rect, blur task (existing `brush_blur.wgsl`) consumes it, golden
matches.

If 11.1–11.5 all pass, the plan is implementable. If any fails in
a way that requires substantial workarounds, revisit.

## 12. Phase mapping under this plan

Renumbered; "Phase X' " is the vello-path equivalent of the parent
plan's Phase X.

- **Phase 0.5'**: parent's 0.5, unchanged. Crate split lands
  before any vello work.
- **Phase 1'**: parent's 1 + color-contract acceleration. Surface
  `Rgba8UnormSrgb` pinned (unchanged); tile / intermediate textures
  pin to vello's preferred linear format (likely `Rgba16Float`).
  Re-capture `rotated_line`, `fractional_radii`, `indirect_rotate`,
  `linear_aligned_border_radius` oracles against the vello path.
  `blank` survives without re-capture. Receipt: oracle smoke green
  through `VelloRasterizer`.
- **Phase 2'**: rect ingestion. `SceneRect` → vello fill. 5 rect-only
  goldens. Same as parent Phase 2 in scope, different rasterizer
  inside. Receipt unchanged.
- **Phase 3'**: transforms + axis-aligned clips. `transform_id` →
  `kurbo::Affine`; clip rect → `push_layer` / `pop_layer`. Scope
  identical to parent Phase 3.
- **Phase 4'**: depth and ordering. *Substantially smaller than
  parent Phase 4.* Vello handles painter-order natively. The work
  here is mapping netrender's z-depth assignment (which today
  drives webrender's depth pre-pass for opaques) onto vello's
  layer model. Likely: drop the depth pre-pass entirely; vello's
  prefix-sum tile rasterizer handles overdraw correctly without
  early-Z. Receipt: 100-overlapping-rect scene matches reference.
- **Phase 5'**: image primitives. `SceneImage` → vello image fill
  (§3.2). ImageCache decision (§3.5 Path A vs. B) settles here.
- **Phase 6'**: render-task graph. *Same scope* as parent Phase 6
  — already delivered. Vello slots in as the per-tile rasterization
  task; everything else (graph topo-sort, transient pool, encode
  callbacks) stays. Drop-shadow receipt (parent's `p6_02`)
  re-greens through the vello path.
- **Phase 7'**: picture caching. *Same scope* as parent Phase 7 —
  already delivered, but `render_dirty_tiles` rewires through
  `TileRasterizer`. Three-tile parity test against the existing
  Phase 7C tile composite.
- **Phase 8'**: gradients. Collapses to one slice: `SceneGradient`
  → `peniko::Gradient` (§3.3). Linear / radial / conic / N-stop
  all in one push. Estimate: ~1 week vs. parent Phase 8's
  ~3 months.
- **Phase 9'**: clips beyond axis-aligned. Vello `push_layer` with
  arbitrary path. Estimate: ~1 week vs. parent Phase 9's
  ~1 month, because the rasterizer side is free.
- **Phase 10'**: text. Per §4: skrifa-based glyph runs through
  `vello::Scene::draw_glyphs`. Drops `wr_glyph_rasterizer` lift
  and the atlas. Estimate: ~1 month total (consumer-side font
  ingestion plumbing is the bulk of this), vs. parent's combined
  Phase 10a + 10b at ~2–3 months.
- **Phase 11'**: borders / box shadows / line decorations. Strokes,
  filled paths, blurred fills — vello primitives. Estimate: ~3 weeks
  vs. parent Phase 11's ~2 months.
- **Phase 12'**: compositing correctness. Same scope as parent
  Phase 12 (filter chains, nested isolation, group opacity,
  backdrop). Vello does the in-picture parts; render-task graph
  does between-picture parts. Estimate: similar to parent at
  ~1–2 months — this is where vello *doesn't* save much, because
  the hard work is graph topology.
- **Phase 13'**: native compositor. Unchanged from parent.

**Total revised estimate**: ~6–7 months for full webrender-equivalent
under the vello path, vs. parent's ~13. The savings come almost
entirely from Phases 8 / 10 / 11. Static-page demo lands at
month 2–3 (rects + transforms + clips + images + simple text).
Production-quality on a single platform at month 5–6.

These are targets in the parent's idiom, not estimates. Done
conditions per phase are the receipts above; calendar is whatever
calendar lands those receipts.

## 13. Risks not already covered

1. **Vello's correctness on browser-shaped scenes is less battle-
   tested than webrender's.** Servo's display lists exercise weird
   corners (overlapping transformed clips, deeply nested pictures,
   fractional-pixel snapshot scrolling, sub-pixel-translation
   re-rasterization). Webrender has years of fuzz/regress data
   here; vello has less. Mitigation: keep the test corpus
   aggressive; treat first-run servo-wgpu integration as a fuzz
   campaign; budget time for upstream vello issues.
2. **Vello's API churn.** Vello pre-1.0 has reshaped its public
   API across versions. Pinning a version costs us upstream fixes;
   floating costs us stability. Pin at adoption, treat upgrades as
   phase-equivalent work.
3. **Mixing vello compute and webrender render passes on one
   queue.** Synchronization is wgpu's job, but our barrier
   placement and pass scoping (axiom 8) get more constrained. §11.3
   spike must validate.
4. **Loss of the "WGSL we authored is the source of truth"
   property.** Today every shader in the binary is in
   `netrender_device/src/shaders/`. Post-vello, vello's shaders
   live in its crate. Debugging a wrong-pixel involves vello's
   sources, not ours. This is a real comprehension cost; budget
   it.
5. **Glyph atlas advocates may reappear.** §4.2's "glyph cache
   layer is a follow-up" is not a guarantee. If servo-wgpu's
   text-heavy content profiles unfavorably, a glyph atlas in
   front of vello becomes a Phase 14 question. Don't pre-build
   it; don't pre-rule it out.
6. **Ecosystem-direction divergence.** Vello is led by Linebender;
   their primary consumer is Xilem (UI toolkit), not a browser
   engine. Servo-shaped edge cases (transformed-clip stacks, sub-
   pixel scrolling re-raster, deeply nested isolation, complex
   font fallback) may be lower-priority upstream than they are
   for us. Mitigation: budget for upstream contributions or carry
   patches; treat the relationship as collaborative rather than
   "we're a downstream consumer." The risk is real but tractable
   if the project owners go in eyes-open.
7. **Bundle size.** vello + peniko + kurbo + skrifa + fontations
   (and ICU4X transitively, once text lands) is a non-trivial
   addition to the binary. For a Servo-fork shipping at Firefox-
   scale, this matters; for a Graphshell-style desktop app it
   doesn't. Order-of-magnitude check during §11.1 (just `cargo
   bloat --release` on the spike binary) — if it's painful, the
   project leads can decide whether to accept it or defer the
   decision.

## 14. The recommendation

**The decision now (2026-05-01) is different than the decision the
doc was first drafted for.** Phase 8 (gradients) and Phase 9 (clip
masks) shipped through the batched pipeline in the interim. Their
WGSLs go in the bin under vello. The plan-time savings argument
of §1 is unchanged for what's *ahead* (Phase 10 / 11 / 12) but
$0 for what's already shipped. Deciding now is deciding on the
remaining ~6 months of parent-plan work, not the original ~13.

That recalibration doesn't kill the recommendation, but it does
weaken the urgency. Adopt the swap if:

1. **§11.1–11.5 verifications all pass** — single biggest gate.
   Realistic spike cost: 1 week (not "a day or two each" as the
   first draft optimistically claimed; vello-API research alone
   absorbs 2–3 days before any code lands).
2. The remaining-phase savings (Phase 10/11/12) outweigh the
   pivot cost: Phase 1'–7' re-green and oracle re-capture (2–3
   weeks), plus carrying any vello upstream churn.
3. The consumer's content profile is path-heavy or text-heavy
   enough that vello's design pays off. Pure-AABB-rect content
   (the WebRender sweet spot) doesn't pull strongly toward the
   pivot.

Steps if adopting:

1. Run §11.1–11.5 verifications. Single ~1-week spike, not
   parallel mini-spikes — they share scaffolding.
2. If all pass: pause Phase 10 work (we're at "decide between
   Phase 10 and Phase 11 next" right now; this becomes
   "Phase 1' next").
3. Land §11's parity spike as the receipt for plan viability.
4. Pivot Phase 1' → 7' rewire (2–3 weeks). The Phase 8 / 9 WGSL
   delete lands as part of this — they're cleanly separable
   surface, gone in a single refactor commit.
5. Phase 8'–11' (collapsed scope) per §12.

Stay-the-course alternative: continue parent plan with Phase 10
or Phase 11 next. Defensible if §11 verifications surface a deal-
breaker (vello's software-adapter story is fatal, premultiplied-
alpha forces an unacceptable boundary cost, bundle-size cost is
unacceptable for the consumer, etc.) — *or* if the consumer pull
is solidly browser-content-shaped where webrender lowering
competes well.

**Hybrid alternative (not recommended)**: trait-and-two-backends.
§10 covers why this is the trap to avoid.

## 15. Bottom line

The parent plan and this plan agree on everything *above* the tile
fill: display lists, spatial tree, picture cache, render-task graph,
compositor handoff. The only question is what runs inside
`render_dirty_tiles`. Vello answers more of the future plan than
the WGSL-family cadence does, in less time, with stronger color
correctness from day one. The verification gates in §11 are the
honest "but only if" — pass them, then commit.
