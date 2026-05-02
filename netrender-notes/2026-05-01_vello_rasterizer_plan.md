# Vello Tile Rasterizer Plan (2026-05-01)

**Status**: Active. Sibling to
[2026-04-30_netrender_design_plan.md](2026-04-30_netrender_design_plan.md)
(hereafter "the parent plan"). Does not supersede; amends Phases 8 /
9 / 10 / 11 / 12 if adopted.

**Verification spike outcome (2026-05-01)**:

- **§11.1 wgpu/vello compatibility**: cleared. vello main is bumped
  to wgpu 29.0.1 (workspace `Cargo.toml` line 137). wgpu downgrade
  not required.
- **§11.2 alpha + color**: cleared with boundary work. `peniko::Color`
  is **straight alpha** at vello's input boundary; vello premultiplies
  internally for blend math; **vello unpremultiplies before storage**
  (verified via Phase 1' p1prime_02 — `fine.wgsl:1390-1395`). Our
  scene primitives are premultiplied → unpremultiply at the vello-
  scene encoder. Storage holds straight-alpha sRGB-encoded; the
  compositor sample-shader must premultiply after the sRGB→linear
  decode. Gradient interpolation: `peniko::Gradient.interpolation_cs`
  defaults to `Srgb` and the **GPU compute path ignores the field
  entirely** (verified via Phase 1' p1prime_03 —
  `vello_encoding/src/ramp_cache.rs:86,97` hard-codes
  `to_alpha_color::<Srgb>()`). Linear-light gradient interpolation
  is not reachable on mainline vello today.
- **§11.3 scene/encoder model**: VERIFIED. `Renderer::render_to_texture`
  creates+submits its own `wgpu::CommandEncoder` per call; no
  encoder sharing; no multi-region-of-one-target API. `low_level`
  module is a dead end (`WgpuEngine::run_recording` is `pub(crate)`,
  no roadmap to expose). Forking vello: **off the table** —
  ongoing-rebase cost not justified for this project's scale.
- **§11.4 external textures**: cleared with cost.
  `Renderer::register_texture(&wgpu::Texture)` exists since 0.6;
  copies into vello's atlas every frame (not zero-copy); `Arc`-shared
  CPU blob avoids 2× memory.
- **§11.5 target format**: VERIFIED. Vello's compute target is
  hardcoded to `Rgba8Unorm`/`Bgra8Unorm`. **Rgba16Float is not
  supported** by the public API. §6's "linear-RGB intermediate"
  acceleration plan is gone; we stay on Rgba8Unorm sRGB-encoded.

**vello_hybrid (sparse_strips) investigation (2026-05-01)**: not the
answer. Workspace-internal `v0.0.7`, README says "not yet suitable
for production." Does expose caller-supplied `CommandEncoder` (one
ergonomic win) but no multi-region / multi-target / scissor / partial
updates. Different rasterizer architecture (fragment/vertex-only,
designed for compute-less GPUs). Mainline vello stays.

**Architecture decision (2026-05-01)**: **Option C — Masonry
pattern**. Per-tile `vello::Scene` cached CPU-side, composed via
`Scene::append` (bytewise extends, validated cheap), one
`render_to_texture` per frame, one submit. Loses (a) cross-frame
GPU-work skipping at the WR-tile-cache level — vello re-runs the
unioned encoding's compute every frame, can't be helped without
forking; (b) per-tile `Arc<wgpu::Texture>` for native-compositor
handoff (axiom 14) — Servo doesn't use this today; Firefox does on
macOS/Windows/Android. If axiom 14 becomes load-bearing later,
Option G (whole-frame vello + post-render tile slicing for native
compositor) is the v1.5 fallback. Option F (fork) is permanently
ruled out.

The doc has been swept (2026-05-01) to align §2 / §3.3 / §3.5 / §6 /
§11 / §12 / §13 / §14 with the spike outcomes. §10 still uses
"TileRasterizer" terminology in its two-backends-trap argument —
intentional, as historical reference to the pre-spike trait shape
that section is critiquing.

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

**Architecture revised post-§11 spike (2026-05-01).** The pre-spike
draft of this section proposed a `TileRasterizer` trait with a
shared `wgpu::CommandEncoder` parameter and per-tile dispatch. That
shape is no longer viable: vello's `Renderer::render_to_texture`
creates and submits its own encoder per call (verified — see Status
block), and the `low_level::Recording` workaround requires forking
`WgpuEngine` (ruled out). The architecture below is the Masonry
pattern (verified working in `linebender/xilem/masonry_core/src/passes/paint.rs`):
per-tile `vello::Scene` cached CPU-side, composed via `Scene::append`
into one frame Scene, one `render_to_texture` per frame, one submit.

In netrender as built, the seam is
[`Renderer::render_dirty_tiles`](../netrender/src/renderer/mod.rs)
(today a thin wrapper around the private
`render_dirty_tiles_with_transforms`). Under the vello path that
function disappears; rasterization moves into a `VelloRasterizer`
that owns a per-tile `vello::Scene` cache and a single
`vello::Renderer`.

### 2.2 The split

`TileCache` keeps its current job — frame-stamp invalidation,
dependency hashing, retain heuristic. Output: which `TileCoord`s
need their content rebuilt this frame. The *rasterizer* owns
everything from there forward, including its own per-tile cache.

```rust
pub trait Rasterizer: Send {
    /// Rebuild the cached representation for the given dirty tiles.
    /// For VelloRasterizer this means: for each TileCoord, build a
    /// fresh `vello::Scene` from `scene` filtered to that tile's
    /// world rect, store it in the rasterizer's per-tile cache.
    fn update_tiles(
        &mut self,
        scene: &Scene,
        dirty: &[TileCoord],
        tile_cache: &TileCache, // for tile_world_rect lookup
    );

    /// Compose all currently-cached tile representations into a
    /// single frame and render to `target`. For VelloRasterizer
    /// this is `Scene::append` of every cached tile-Scene into one
    /// frame Scene, then `vello::Renderer::render_to_texture` once.
    fn render_frame(
        &mut self,
        wgpu_device: &WgpuDevice,
        target: &wgpu::TextureView,
    );
}
```

`Renderer` holds a `Box<dyn Rasterizer>`. Production constructor
selects `VelloRasterizer`. The test seam (§10) is a `TestRasterizer`
that records calls without GPU work — useful for unit-testing
filter/dispatch logic without booting wgpu.

### 2.3 Why this exact shape

- **No shared `wgpu::CommandEncoder`.** Vello creates and submits
  its own per call; we can't inject one. Confirmed in
  `vello/src/wgpu_engine.rs:380-757` — `WgpuEngine::run_recording`
  builds a fresh `CommandEncoder` and calls `queue.submit()` itself.
  The trait can't pretend otherwise.
- **One submit per frame, not one per tile.** Per-tile-per-submit
  works architecturally but pays a fence/sync per tile. Composing
  N tile-Scenes via `Scene::append` (which is `extend_from_slice`
  on bytewise-encoded streams; verified cheap in
  `vello_encoding/src/encoding.rs:94-172`) into one frame Scene and
  rendering once is what Masonry does for the same reasons.
- **`TileCache` doesn't hold vello state.** No `vello::Scene` field
  on `Tile`. The rasterizer owns its cache; tile_cache stays
  rasterizer-agnostic. Means we can drop the entire `tile.texture:
  Option<Arc<wgpu::Texture>>` field along with the per-tile texture
  lifetime juggling — the rasterizer's per-tile-Scene cache replaces
  it.
- **Filter-by-AABB happens inside `update_tiles`.** The rasterizer
  walks the dirty list, asks `tile_cache` for each coord's
  `world_rect`, filters scene primitives by AABB, and builds the
  vello scene. `TileCache` exposes `tile_world_rect(coord) -> [f32;
  4]`; nothing else of `TileCache` need leak.
- **No `transforms_buf`, no `&ImageCache` in the trait.** Vello
  doesn't take a wgpu::Buffer for transforms (it reads
  `kurbo::Affine` directly from the scene). Image lookup goes
  through `peniko::Image` constructed from CPU `Blob<u8>` (Arc-shared
  with our image cache — see §3.5). The pre-spike coupling smells
  evaporate because the batched backend they were for is gone.

### 2.4 Cost shape vs. the pre-spike draft

| Property | Pre-spike (per-tile encoder) | Post-spike (Option C) |
| --- | --- | --- |
| Submits per frame | N (one per dirty tile) | 1 |
| Encoder ownership | Shared via trait param | Vello-internal |
| Per-tile texture | `Arc<wgpu::Texture>` per tile | None — composed scene |
| Native compositor handoff (axiom 14) | Trivial (per-tile texture) | Lost; v1.5 fallback in §recommendation |
| Cross-frame GPU-work skipping | Possible (re-render only dirty tile textures) | No — vello recomputes the unioned encoding's GPU dispatches every frame |
| CPU-side scene rebuild | Per dirty tile only | Per dirty tile only (clean tiles' Scenes reused) |

The two real losses (native compositor handoff, cross-frame GPU
work skipping) trade against forking `WgpuEngine`. We chose the
losses; Servo doesn't use axiom-14 today, and vello's GPU cost on
unchanged content is reportedly tractable on Linebender's UI
benchmarks.

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

**Color-space caveat (verified §11.2 + Phase 1' p1prime_03).**
`peniko::Gradient` defaults to sRGB-encoded interpolation
(`gradient.rs:21 DEFAULT_GRADIENT_COLOR_SPACE = ColorSpaceTag::Srgb`),
**and the GPU compute path ignores the override entirely.**
`vello_encoding/src/encoding.rs:289-339` reads only `gradient.kind`,
`stops`, `extend`, and `alpha` from the brush —
`gradient.interpolation_cs` is never consulted. The ramp builder at
`vello_encoding/src/ramp_cache.rs:84-111` hard-codes
`stops[i].color.to_alpha_color::<Srgb>()` before the per-channel
`lerp`. `interpolation_cs` is honored only by the `vello_hybrid`
(sparse-strips / CPU) path, which we are not using.

**Implication:** Phase 8 receipts blended in straight-RGB component
space (i.e. sRGB-encoded), so the GPU compute behavior matches Phase
8 by accident. We can drop the per-gradient
`with_interpolation_cs(LinearSrgb)` plumbing — it would be a no-op.
Linear-light gradients stay out of reach until upstream wires
`interpolation_cs` through; tracked as test
`p1prime_03_gradient_default_is_srgb_encoded`, which inverts to a
known-failure if upstream fixes this.

**Alpha boundary (verified §11.2 + Phase 1' p1prime_02).**
`peniko::Color` is straight-alpha at the input boundary; vello
premultiplies internally for blend math; **vello unpremultiplies
again before storage**
(`vello_shaders/shader/fine.wgsl:1390-1395`: `fg.rgb * a_inv` then
`textureStore`). The storage texture therefore holds straight-alpha
sRGB-encoded values — confirmed by p1prime_02 reading
`(255, 0, 0, 128)` for a half-opaque red fill, not the
`(128, 0, 0, 128)` the §3.3-as-drafted assumed.

This affects **two** boundaries:

1. Encoder input: convert our premultiplied `SceneGradient.stops`
   (and rect colors, image tints) to peniko's straight-alpha
   convention via `Color::from_rgba_f32(r/a, g/a, b/a, a)` for
   `a > 0`. Unchanged from the original plan.
2. Compositor sample: when downstream shaders sample vello's output
   through an `Rgba8UnormSrgb` view, hardware sRGB→linear decodes
   RGB but leaves alpha untouched, so the sampled value is
   straight-alpha linear. The compositor must premultiply before
   blending. **This was wrong in §6.1 as-drafted** — see corrected
   §6.1 below.

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

### 3.5 Image cache integration (verified §11.4)

Two paths, both viable, picked per-image based on lifetime shape:

- **Path A — `peniko::Image` backed by `Arc<Blob<u8>>`.** Vello
  consumes a CPU-side blob via `peniko::ImageData::new(blob, format,
  width, height)`. The blob is `linebender_resource_handle::Blob`,
  which is internally `Arc`-shared. Our `ImageCache` can hold the
  same `Blob` and hand it to peniko without 2× memory — vello
  caches by `Blob.id()` so re-handing the same blob across frames
  is one upload, not N. **This is the default path.**
- **Path B — `Renderer::register_texture(&wgpu::Texture)`.** Added
  in vello 0.6 (CHANGELOG #1161). Caller-provided wgpu texture must
  be `Rgba8Unorm`, straight alpha, with `COPY_SRC` usage. **Caveat:
  vello copies into its internal atlas every frame** — not zero-copy.
  Useful for inputs that are themselves rendered targets (render-graph
  outputs feeding into a vello scene, e.g., the Phase 6 blur result
  → vello consumer pattern), where the source is already a wgpu
  texture and CPU bytes don't exist.

The image cache stays the lifetime authority. For Path A, both
netrender and vello hold `Arc<Blob>` clones; the underlying CPU
allocation is shared. For Path B, the wgpu texture stays
`Arc`-owned by netrender and vello samples per-frame; the consumer
keeps the Arc alive until `PreparedFrame` submission completes
(axiom 16, unchanged).

**Image-tint multiplication.** `SceneImage.color` is a premultiplied
RGBA tint that the brushed pipeline multiplies element-wise with
the sampled texel. Peniko's `ImageBrushRef` doesn't have a
multiplicative tint built in. Two encoding strategies:

1. **Wrap the image draw in a `Mix::Multiply` blend layer.** Set
   `push_layer(BlendMode::new(Mix::Multiply, Compose::SrcOver), ..., tint_rect)`,
   draw the image, `pop_layer`. Heavy for the common case.
2. **Pre-multiply the tint into a per-image `peniko::Image::with_alpha(a)`
   variant.** Works for alpha-only tint (the common 7C tile-composite
   case where tint is `[1, 1, 1, alpha]`). For RGB tints, fall
   back to (1).

For Phase 7C tile composites the alpha-only path covers it. Other
RGB-tint cases (mask-as-tinted-image in 9A) need the Mix::Multiply
path; flag in §11.4-followup as worth a code spike to confirm
correctness.

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

**Major reframe post-§11 spike (2026-05-01).** The first draft of
this section assumed adopting vello would immediately give us linear-
light blending — i.e., the parent plan's Phase-7+ regime. **That's
wrong for two independent reasons:**

1. **Vello's public API doesn't render to `Rgba16Float`.** The compute
   fine-rasterizer's storage target is hardcoded to `Rgba8Unorm` /
   `Bgra8Unorm` (`vello/src/wgpu_engine.rs:825-829`,
   `render.rs:509`). No public path to a linear-light intermediate
   without forking `WgpuEngine` (ruled out).
2. **Vello blends in sRGB-encoded space, not linear.** This is the
   Cairo / 2D-canvas tradition; vello's [`vision.md:116`](https://github.com/linebender/vello)
   flags gamma-correct rendering as a *future* quality improvement,
   not current behavior. `vello_shaders/shader/shared/blend.wgsl:145`
   explicitly says "The colors are assumed to be in sRGB color
   space," and `vello_encoding/src/draw.rs:79` writes
   gamma-encoded bytes via `convert::<Srgb>().premultiply().to_rgba8()`.
   Issue #151 (closed without merging linear-light support) is the
   trail.

So the linear-light "Phase 7+" regime *isn't reachable* via mainline
vello in 2026. What IS reachable is a different and arguably-more-
useful contract: **vello blends in sRGB-encoded space, the sample
boundary recovers linear-light, downstream composition can work in
linear if it wants, framebuffer encodes back to sRGB.**

### 6.1 The view-format chain (verified §11.5-followup spike + Phase 1' p1prime_02)

Vello writes **straight-alpha** sRGB-encoded values into an `Rgba8Unorm`
storage texture. (`fine.wgsl:1390-1395` premultiplies for blend math
internally, then divides RGB by alpha and stores.) We sample that
texture downstream through an `Rgba8UnormSrgb` view-format, which
gets us hardware sRGB→linear decode of the RGB channels at sample
time — the **exact inverse** of vello's "treat sRGB-encoded bytes as
if they were linear" internal pretense. Alpha is unaffected by the
sRGB decode path; it stays straight. So:

- **Tile-Scene render target:** `Rgba8Unorm`, `view_formats:
  &[Rgba8UnormSrgb]`, usage `STORAGE_BINDING | TEXTURE_BINDING |
  COPY_SRC`. The `Rgba8UnormSrgb` view is created with explicit
  `usage: TEXTURE_BINDING` (no STORAGE_BINDING) — required by per-
  view usage rules added to WebGPU spec in late 2024 / Chrome 132.
- **Storage view (vello writes here):** native `Rgba8Unorm`. Vello's
  fine compute pass uses this. Storage holds straight-alpha
  sRGB-encoded.
- **Sample view (downstream samples here):** `Rgba8UnormSrgb`.
  Hardware decodes RGB to linear; alpha passes through untouched.
  Samples arrive as **straight-alpha linear-light** (RGB linear,
  α straight).
- **Compositor premultiply.** Because samples are straight-alpha, the
  composite shader (the netrender pipeline that consumes vello's
  output as an image source) MUST multiply RGB by alpha before
  participating in over-blend math: `rgb_premul = rgb_linear * a`.
  This is one ALU per fragment and is the same pattern the existing
  `brush_image` opaque/alpha-blend split already handles for
  CPU-uploaded straight-alpha textures.
- **Composite to framebuffer:** linear-light premultiplied pixels
  blend cleanly under standard `One, OneMinusSrcAlpha`; framebuffer
  is `Rgba8UnormSrgb` so write encodes back to sRGB on store.

Cited references: `wgpu-types-29.0.1/src/texture/format.rs:1569` for
`remove_srgb_suffix` validation; `vello/src/lib.rs:463` for
target-format requirement; precedent in `vello#689` (Iced
integration), `wgpu#3030` (closed via #3237), and bevy#15201 doing
the same trick.

**Vulkan asterisk.** `wgpu-hal` doesn't set
`VK_IMAGE_CREATE_EXTENDED_USAGE_BIT` alongside `MUTABLE_FORMAT_BIT` +
format-list ([wgpu#5379](https://github.com/gfx-rs/wgpu/issues/5379),
open). Works on most Vulkan drivers but produces validation-layer
warnings on radv / Lavapipe. Metal and DX12 are clean. If headless
CI on Lavapipe is a hard target, plan a manual-decode shader
fallback (~8 ALU ops per fragment).

### 6.2 Implications

- **Surface format stays `Rgba8UnormSrgb`.** External color contract
  (what the embedder sees) is unchanged.
- **Phase 8/9 receipts re-green with vello-encoded gradients.** Stop
  values that were previously lerped in straight-RGB component space
  match vello's default `Srgb` interpolation by accident (the GPU
  compute path ignores `interpolation_cs` per §3.3 / p1prime_03), so
  no per-gradient color-space override is needed. Tolerance ±2/255
  was already in place.
- **`Rgba16Float` linear intermediates: not on the table.** If a
  future receipt absolutely requires HDR-precision linear, the path
  is a separate non-vello compute pass that copies vello output
  through a linear conversion — high cost, only do it if forced.
- **Oracle re-capture cost is smaller than the pre-spike plan
  estimated.** The earlier draft assumed all alpha-compositing
  scenes diverge; in practice the divergence is bounded to the
  delta between "straight-RGB component lerp" (parent plan Phase 8)
  and "vello's `Srgb`-tag lerp through peniko's color crate", which
  is small to zero on primary-color / extreme-alpha cases. Mid-tone
  alpha-blend scenes will diverge — re-capture, document the diff,
  move on.

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

## 11. Verification record

All five gates have been verified through research-spike cycles
(2026-05-01). Originals stated "before writing a single line of
`VelloRasterizer`"; what follows is what we now know.

### 11.1 wgpu / vello version compatibility — **CLEARED**

Vello main is on `wgpu = "29.0.1"` (`vello/Cargo.toml:137`); this
is the wgpu-29 bump that "unblocked vello development" per the
linebender team's recent activity. Released-tag 0.8.0 still
targets wgpu 28; we'll consume vello via git ref to main until
their next tagged release.

`VELLO_BASELINE` wgpu features (the Phase-0.5 axiom-10 amendment):
the precise list is not yet enumerated — the `boot()` call site
will surface what's required when we add vello. wgpu's `Features::empty()`
baseline is unlikely to suffice; expect compute-shader + atomics +
storage-binding requirements at minimum. Lavapipe / WARP /
SwiftShader are reported to satisfy vello on community usage but
the §11.5-followup spike (Vulkan validation behavior, see §6.1)
should answer this directly when it runs.

Software-adapter validation may produce noise on Vulkan due to
[wgpu#5379](https://github.com/gfx-rs/wgpu/issues/5379) (open) —
documented in §6.1; mitigation path identified.

### 11.2 Premultiplied-alpha and color-space — **CLEARED with boundary work**

Verified: `peniko::Color` is straight alpha (not premultiplied);
vello premultiplies internally
(`vello_encoding/src/draw.rs:79`). Our scene's premultiplied colors
need unpremultiply-at-boundary in the encoder. `peniko::Gradient`
defaults to `ColorSpaceTag::Srgb` (sRGB-encoded interpolation);
explicit `with_interpolation_cs(LinearSrgb)` to override, per
§3.3 update.

### 11.3 Vello scene reuse / parallelism model — **CLEARED with architectural revision**

Verified facts (research, no code spike needed):

- One `vello::Scene` per `Renderer::render_to_texture` call. To
  render N targets, call N times.
- `render_to_texture` does NOT take a caller-supplied
  `wgpu::CommandEncoder` — it creates and submits its own per
  call (`wgpu_engine.rs:380-757`). No public path to encoder
  sharing.
- `low_level::Recording` is public but `WgpuEngine::run_recording`
  is `pub(crate)` and there's no roadmap item to expose it. Forking
  is the only path; ruled out for this project.
- No multi-region-of-one-target API. `RenderParams { width, height,
  base_color, antialiasing_method }` lacks viewport/scissor.
- `Renderer` itself amortizes pipelines + Resolver across calls.
  Reuse one `Renderer` per `(Device, surface_format)` pair.
- Resolver caches glyph encodings + ramp LUT bytes + image atlas
  slots across frames; does NOT cache scene-buffer packing,
  ramp-atlas GPU upload, dispatch buffers, or compute dispatches.

`vello_hybrid` (sparse_strips experimental crate) was investigated
as an escape hatch: it does expose caller-supplied
`CommandEncoder`, but lacks multi-region/multi-target/scissor
APIs *and* is workspace-internal at v0.0.7 ("not yet suitable for
production"). Not the answer.

**Architectural decision: Option C (Masonry pattern).** Per-tile
`vello::Scene` cached CPU-side; composed via `Scene::append`
(verified cheap — `extend_from_slice` on bytewise streams in
`vello_encoding/src/encoding.rs:94-172`); one
`render_to_texture` per frame; one submit. See §2.

### 11.4 External-texture import — **CLEARED with cost note**

`Renderer::register_texture(&wgpu::Texture)` exists in vello 0.6+
(`lib.rs:562-590`). Accepts `Rgba8Unorm`, straight alpha, with
`COPY_SRC` usage. **Caveat: copies into vello's atlas every
frame** — not zero-copy. Path A (`Arc<Blob<u8>>`) is the default
since blob ID dedup makes it effectively single-upload across
frames; Path B (`register_texture`) is the right path when the
input is itself a wgpu texture (render-graph output → vello
input). See §3.5 update.

### 11.5 Render-target format — **CLEARED with reframe**

Verified: vello's compute target is hardcoded to `Rgba8Unorm` /
`Bgra8Unorm`. **`Rgba16Float` is not supported** by the public
API. The §6 color contract is reframed accordingly: stay on
`Rgba8Unorm` storage with `Rgba8UnormSrgb` view-format trick for
sample-time sRGB→linear decode. See §6.1 for the chain and the
Vulkan validation asterisk.

The drop-shadow integration test (vello rasterizes → existing
`brush_blur.wgsl` consumes) is now a Phase 6' receipt rather
than a §11 gate; the format compatibility question is settled.

### 11.6 Items still requiring runtime spike

Two narrow questions need a real `cargo add vello` + 50-line test
to resolve, but neither is plan-blocking:

1. Vulkan validation behavior on Lavapipe / radv with
   `Rgba8Unorm` storage + `Rgba8UnormSrgb` view, given wgpu-hal
   doesn't set `EXTENDED_USAGE_BIT`. May produce warnings; may
   assert. Determines whether headless-CI on software-adapter
   Vulkan works without a manual-decode fallback shader.
2. Quantization round-trip exactness: writing `f32` to
   `Rgba8Unorm` storage and reading via `Rgba8UnormSrgb` should
   yield `srgb_decode(round(f * 255) / 255)` with no driver-
   injected linearize step on the storage write. Code-spike
   confirmation; expected to pass.

Both fall out naturally in Phase 1' first-light — schedule there,
not as separate work.

### 11.7 Phase 1' first-light findings (2026-05-02) — **CLEARED**

`netrender/tests/p1prime_vello_first_light.rs` runs three probes
against a real `boot()` device + `Renderer::render_to_texture`:

1. **`p1prime_01_vello_renders_red_rect`** — opaque red round-trips
   to `(255, 0, 0, 255)` ✓. Confirms vello compiles, links, boots on
   our device, and writes through the `Rgba8Unorm` storage with
   `Rgba8UnormSrgb` view-format slot reserved without producing
   adapter-side validation errors. Quantization round-trip clears.
2. **`p1prime_02_alpha_storage_is_straight`** — half-opaque red
   `(255, 0, 0, 128)` lands in storage as `(255, 0, 0, 128)` ✓.
   **Plan correction:** vello stores **straight-alpha**, not
   premultiplied. Internal blend math is premultiplied
   (`fine.wgsl` blend stages), but the output stage at
   `vello_shaders/shader/fine.wgsl:1390-1395` divides by alpha
   before `textureStore`. §6.1 updated: compositor must
   premultiply at sample time.
3. **`p1prime_03_gradient_default_is_srgb_encoded`** — red→blue
   linear gradient midpoint is `(128, 0, 128)` for both default and
   `with_interpolation_cs(LinearSrgb)` ✓. **Plan correction:** the
   GPU compute path ignores `interpolation_cs` entirely.
   `vello_encoding/src/encoding.rs:289-339` doesn't read it;
   `vello_encoding/src/ramp_cache.rs:84-111` hard-codes
   `to_alpha_color::<Srgb>()` for every stop. Linear-light
   gradients are unreachable until upstream wires it through.
   §3.3 updated. Test inverts to known-failure if upstream fixes
   this.

Both 11.6 items resolved as a side effect: no Vulkan validation
errors observed on the dev box (DX12-backed wgpu adapter), and
quantization round-trip is exact for primary opaque colors.

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
- **Phase 7'**: picture caching, **shape changes substantially**.
  Parent Phase 7's tile cache stored `Arc<wgpu::Texture>` per tile;
  the rasterizer rendered each dirty tile to its own texture; the
  composite drew one `brush_image_alpha` per tile. Under Option C
  (Masonry pattern), the cached unit becomes `vello::Scene` per
  tile; composition is `Scene::append` into one frame Scene; one
  `render_to_texture` per frame, one submit, no per-tile textures.
  `TileCache` keeps its invalidation algorithm (frame-stamp +
  dependency hash + retain heuristic — Phase 7A's algorithmic core
  carries forward). What's deleted: `Tile.texture: Option<Arc<wgpu::Texture>>`
  field, `render_dirty_tiles` per-tile passes, the
  `brush_image_alpha`-per-tile composite. What's added: a
  `VelloRasterizer` owning `HashMap<TileCoord, vello::Scene>` and a
  single `vello::Renderer`. Receipt: the Phase 7C pixel-equivalence
  test re-greens through the new path. Note: cross-frame GPU-work
  skipping is *not* preserved (per §11.3 / risk 8).
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
3. **Mixing vello compute and netrender render passes on one
   queue.** Verified §11.3: vello creates+submits its own encoder;
   netrender's other passes (composite, render-task graph filters)
   run as separate submissions. wgpu orders submissions in queue
   order. The only wart is per-frame submission count (vello +
   each downstream filter task = N+1 submissions), which is fine
   on modern GPUs but worth profiling under load.
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
   doesn't. Order-of-magnitude check during Phase 1' (just `cargo
   bloat --release` on the spike binary) — if it's painful, the
   project leads can decide whether to accept it or defer the
   decision.

8. **No cross-frame GPU-work skipping.** Verified §11.3: vello
   re-runs the unioned encoding's coarse + fine compute passes
   every frame, including for tiles whose contents didn't change.
   The Resolver caches CPU-side glyph encodings, gradient ramp LUT
   bytes, and image atlas slot allocations across frames; it does
   NOT cache the GPU work. WebRender's tile cache invariant —
   "clean tile = zero GPU work" — does not survive the pivot.
   Mitigation: vello's per-frame compute cost is reportedly
   tractable (Linebender benchmarks at UI scale); for browser-
   content scrolling workloads at large viewports the regression
   is real and would only be addressed by forking, which is ruled
   out. Accept the cost; profile under realistic load; revisit
   only if specific consumer profiles surface unacceptable
   regression.

9. **Forking vello is permanently off the table.** Restated for
   emphasis: any future pressure to fork (axiom-14 native-
   compositor handoff becoming load-bearing, multi-region
   rendering becoming necessary, etc.) reopens this discussion at
   the project-direction level rather than as a tactical patch.
   The maintenance cost of carrying a fork against an active
   pre-1.0 upstream is not absorbable at this project's scale.

## 14. The recommendation

**The decision now (2026-05-01) is different than the decision the
doc was first drafted for.** Phase 8 (gradients) and Phase 9 (clip
masks) shipped through the batched pipeline in the interim. Their
WGSLs go in the bin under vello. The plan-time savings argument
of §1 is unchanged for what's *ahead* (Phase 10 / 11 / 12) but
$0 for what's already shipped. Deciding now is deciding on the
remaining ~6 months of parent-plan work, not the original ~13.

**Recommendation locked (2026-05-01).** All five §11 gates have
been verified through research spikes; none surface a deal-breaker.
The pivot is a go.

Architectural shape: **Option C (Masonry pattern)** — per-tile
`vello::Scene` cached CPU-side, composed via `Scene::append`, one
`render_to_texture` per frame, one submit. See §2 for the full
trait shape and §6 for the color contract.

Two narrow runtime confirmations remain (§11.6) but neither is
plan-blocking; they fall out of Phase 1' first-light naturally.

**Stay-the-course alternative** (continue parent plan with Phase 10
or 11) is *not* recommended. The remaining unrealized Phase 10/11/12
work is ~6 months on the parent plan vs. ~3 months on the vello
path; the gap absorbs the Phase 1'–7' re-green cost (~2–3 weeks)
and net-saves time. The only serious counter-argument was "vello's
software-adapter story might be fatal" — partially confirmed
(Vulkan validation noise on Lavapipe via [wgpu#5379](https://github.com/gfx-rs/wgpu/issues/5379))
but with a known fallback (manual sRGB decode in shader), so not
fatal.

**Hybrid alternative (not recommended)**: trait-and-two-backends.
§10 covers why this is the trap to avoid. The repo's
three-direction strategy — `spirv-shader-pipeline` branch,
`idiomatic-wgpu-pipeline` branch (snapshot of pre-pivot main),
`main` (vello) — preserves the batched-WGSL work as historical
artifact without dragging it into v1's test/maintenance surface.

### Concrete next steps

1. **Plan amendment (this commit).** Sweep the doc to reflect §11
   spike outcomes. Done in this pass.
2. **`cargo add vello` on main.** Pin to a git ref against
   linebender/vello main branch (wgpu-29 bumped). Bring in
   `peniko`, `kurbo`, `skrifa` transitively.
3. **Phase 1' first-light receipt.** Smallest possible vello
   integration: render one rect through a `vello::Scene` to a
   `Rgba8Unorm` target with `view_formats: &[Rgba8UnormSrgb]`,
   composite to framebuffer, golden test. The runtime spike from
   §11.6 falls out of this — if Vulkan validation asserts on
   Lavapipe, we'll see it here.
4. **Phase 1'–7' re-green.** Oracle re-capture, brush-WGSL delete
   (preserved on idiomatic-wgpu-pipeline branch), tile cache
   rewire to Option C. Estimated 2–3 weeks.
5. **Phase 8'–11' on the collapsed-scope schedule per §12.**

## 15. Bottom line

The parent plan and this plan agree on everything *above* the tile
fill: display lists, spatial tree, picture cache, render-task graph,
compositor handoff. The only question was what runs inside
`render_dirty_tiles`. Vello answers more of the future plan than
the WGSL-family cadence does, in less time, with the color contract
that 2D-canvas-tradition consumers actually want (sRGB-encoded blend
through vello, linear at the sample boundary, sRGB on framebuffer
write). The verification gates in §11 surfaced revisions to the
draft architecture — encoder ownership, cross-frame skipping,
Rgba16Float availability, register_texture cost — none fatal,
all reflected in §2 / §3 / §6.

The pivot is committed. Phase 1' is next.
