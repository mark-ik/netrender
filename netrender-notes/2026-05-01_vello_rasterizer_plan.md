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
macOS/Windows/Android. Option F (fork) is permanently ruled out.

*Update 2026-05-06.* Loss (b) has since been recovered via path (b′)
without forking — see
[`2026-05-05_compositor_handoff_path_b_prime.md`](2026-05-05_compositor_handoff_path_b_prime.md)
(sub-phases 5.1–5.4 shipped, commit `9447a852b`). The
"Option G v1.5 fallback" framing in the original draft is
superseded; path (b′) is strictly better (per-surface damage
instead of flat slicing). Loss (a) remains; partially recovered at
surface granularity (clean compositor surfaces skip their blit).

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

**Implemented shape** (post-Phase 7' delivery):
[`VelloTileRasterizer`](../netrender/src/vello_tile_rasterizer.rs)
is a concrete struct, not a trait impl. `Renderer` holds a
`Option<Mutex<VelloTileRasterizer>>` directly and routes through it
in [`Renderer::render_vello`](../netrender/src/renderer/mod.rs).

```rust
// Actual today:
impl VelloTileRasterizer {
    pub fn render(
        &mut self,
        scene: &Scene,
        tile_cache: &mut TileCache,
        target_view: &wgpu::TextureView,
        base_color: peniko::Color,
    ) -> Result<(), vello::Error>;
}
```

**Why no trait** — earlier drafts of this section proposed
`pub trait Rasterizer { fn update_tiles(…); fn render_frame(…); }`
with `Box<dyn Rasterizer>` on `Renderer`. The trait was justified by
the existence of *two* rasterizers (batched WGSL + vello) and the
need for a `TestRasterizer` seam. After the batched-WGSL path was
retired (§10's "two backends trap" decision applied) there's
exactly one rasterizer, so the trait would be an abstraction
without users. Test code talks to `VelloTileRasterizer` directly.
Re-introduce the trait if a second rasterizer ever ships (e.g. a
`vello_hybrid` variant for CPU/sparse-strips); not before.

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

> **Superseded 2026-05-06** by
> [`2026-05-05_compositor_handoff_path_b_prime.md`](2026-05-05_compositor_handoff_path_b_prime.md).
> The "axiom 14 lost" cell and the "v1.5 fallback" framing are no
> longer accurate: path (b′) recovers native-compositor handoff at
> per-declared-surface granularity (sub-phases 5.1–5.4 shipped on
> netrender's side; commit `9447a852b`). The cross-frame GPU-work
> skipping loss is also partially recovered — clean surfaces skip
> their blit. The table below is preserved as the historical
> contrast against the pre-spike draft; for the live cost shape,
> see the path (b′) plan §1.

| Property | Pre-spike (per-tile encoder) | Post-spike (Option C) |
| --- | --- | --- |
| Submits per frame | N (one per dirty tile) | 1 |
| Encoder ownership | Shared via trait param | Vello-internal |
| Per-tile texture | `Arc<wgpu::Texture>` per tile | None — composed scene |
| Native compositor handoff (axiom 14) | Trivial (per-tile texture) | ~~Lost; v1.5 fallback in §recommendation~~ **Recovered via path (b′)** |
| Cross-frame GPU-work skipping | Possible (re-render only dirty tile textures) | No — vello recomputes the unioned encoding's GPU dispatches every frame |
| CPU-side scene rebuild | Per dirty tile only | Per dirty tile only (clean tiles' Scenes reused) |

Original framing (preserved for context): the two real losses
(native compositor handoff, cross-frame GPU work skipping) trade
against forking `WgpuEngine`. We chose the losses; Servo doesn't
use axiom-14 today, and vello's GPU cost on unchanged content is
reportedly tractable on Linebender's UI benchmarks. Update: the
axiom-14 loss has since been recovered without forking (see banner
above).

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

### 4.4 Layout layer: parley is for the embedder, not netrender

Netrender's text input is a stream of **positioned** glyph runs
(`vello::Glyph { id, x, y }`) plus a font handle. It does not shape,
line-break, do BiDi, perform font matching, or run fallback. Those
are layout concerns and live one layer up in the stack.

This boundary is intentional:

- **Servo path.** Servo already has shaping (harfrust), font
  matching (`gfx`), and inline / line layout. The netrender glyph-run
  interface is the natural lowering target for what `gfx` already
  produces today — no architectural change Servo-side beyond
  swapping the eventual rasterization backend. Servo would not pull
  parley.
- **Embedders without an existing layout layer.** For self-contained
  UIs (Graphshell-style overlays, isolated text widgets, demo apps),
  [`parley`](https://github.com/linebender/parley) is the
  Linebender-blessed companion to vello and the recommended layout
  layer:
  - Pure-Rust shaping via swash / harfrust.
  - BiDi via ICU4X.
  - Line breaking + paragraph layout.
  - Font fallback through [`fontique`](https://github.com/linebender/parley/tree/main/fontique)
    (system font enumeration on macOS / Windows / Linux, plus
    embedded fallback chains).
  - Output type `parley::Layout<Brush>` exposes positioned glyph
    runs that feed `vello::Scene::draw_glyphs` near-directly — and
    therefore feed netrender's glyph-run interface near-directly
    too.

**Maturity caveats (verify before locking in for shipping content):**

- Pre-1.0 at time of writing; API breaks expected.
- Fontique enumerates system fonts but does not match CSS-cascade
  font selection rules end-to-end the way DirectWrite / CoreText do
  through Servo's `gfx`. Locale-aware shaping (CJK / RTL / complex
  scripts) inherits whatever harfrust + the bundled fallback fonts
  supply; verify against the actual content the embedder ships.
- No subpixel positioning quirks shared with Chromium / Firefox text
  engines; matches vello's own subpixel policy, which is fine for
  prototyping but won't pixel-match Blink / Gecko reference output.

**Decision:** Do not bake parley into netrender. Document it as the
recommended embedder companion when an embedder needs an off-the-
shelf layout layer that pairs cleanly with vello.

The `netrender_text` companion crate mentioned in §11.0 (the
"netrender-text wrapper" sketch around font ingestion) is the
natural home for a thin `parley::Layout<Brush>` →
`netrender::GlyphRun` adapter for embedders that adopt parley.
Servo, with its existing layout, ignores that adapter and lowers
through its own path. Both paths converge on the same downstream
glyph-run interface — the layering stays clean.

**Status — landed (2026-05-04):** `netrender_text` exists as a
sibling crate in the workspace. Public API is one function:

```rust
pub fn push_layout(scene: &mut Scene, layout: &parley::Layout<[f32; 4]>, origin: [f32; 2])
```

`Brush` is fixed to `[f32; 4]` (premultiplied RGBA, matching
netrender's color contract) so consumers vary text color via
`StyleProperty::Brush(...)` on parley spans. Fonts referenced by a
single layout are deduped within one `push_layout` call by
`peniko::Blob::id()` + font index. Across calls, fonts re-register;
a persistent cross-call font map is a future addition for streaming
consumers. Decoration painting (underline / strikethrough), inline
boxes, and synthesis are explicitly out of scope — the consumer
handles inline boxes, decorations land when there's pull, and
synthesis is upstream-blocked.

The boundary is the **data type** (`netrender::SceneGlyphRun`), not
a `Shaper` trait — same "abstraction without users" reasoning that
killed the `Rasterizer` trait in §2.2 applies. A consumer wanting
cosmic-text writes `netrender_cosmic_text` that emits SceneGlyphRuns
the same way; nothing in `netrender` or `netrender_text` changes.

Receipts at `netrender_text/tests/shape_and_paint.rs`:

- `netrender_text_01_shaped_paragraph_paints` — load a system font,
  shape "Hello, world!" through parley, push to a Scene, render via
  the vello path, count painted pixels. Skips on hosts without a
  recognized font path.
- `netrender_text_02_font_deduped_within_layout` — multi-line
  layout with a single font registers exactly one entry in
  `scene.fonts` (slot 1; slot 0 is the no-font sentinel) and every
  glyph run references that slot.

Demo (`netrender/examples/demo_card_grid.rs`) consumes the adapter
for its card labels. Comparison vs. the pre-shaping hand-rolled
fixed-pitch label code: shaped output handles mixed-case strings,
real kerning, and arbitrary characters (e.g. "Z-order probe",
"Radial + shadow") that the previous uppercase-only ASCII hack
couldn't render.

**Cross-call dedup — landed via `netrender::FontRegistry`.** Pulled
forward as part of the C-architecture readiness work (§11.17) so
the consumer side ships ready for both single-consumer and
multi-consumer patterns. Used via `push_layout_with_registry`;
the existing `push_layout` is a thin wrapper that builds a fresh
registry per call.

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

### 6.3 Scene API color contract — sRGB-encoded blend space

Decision (post-cleanup, 2026-05-04): **Scene primitive colors are
interpreted as premultiplied sRGB-encoded values, matching how vello
operates internally.** This is the contract embedders code against.
We considered and rejected an "encode-on-input" wrapper that would
sRGB-encode user-supplied "linear" values before handing them to
peniko; see "Why not encode-on-input" below.

**The contract:**

- A `SceneRect.color = [r, g, b, a]` is premultiplied RGBA in
  **sRGB-encoded space**. To match conventional usage where colors
  are specified in sRGB (CSS, designer tools, image asset bytes),
  hand the values through unchanged: 50% gray is `[0.5, 0.5, 0.5,
  1.0]` and lands at byte 128 in storage.
- Alpha-compositing (`source-over`, the universal default) happens
  in sRGB-encoded space inside vello. This matches the de facto
  behaviour of shipping web engines (Blink / Gecko / WebKit
  historically blend `source-over` in sRGB-encoded space for
  performance / legacy reasons; the "linear-light is canonical"
  reading of CSS Compositing Level 1 is more honored in spec than
  in implementations).
- p9b_02 was re-greened with assertion bands matching vello's
  sRGB-encoded blend output (interior shadow ≈ 77, not 149). The
  original linear-blend pipeline produced 149, but **149 was the
  outlier vs typical web-engine output, not 77.** The cleanup
  brought us closer to engine-conformant rendering, not further.

**Why not encode-on-input:**

The half-fix would be: at scene_to_vello time, sRGB-encode each
user-supplied channel before constructing the `peniko::Color`.

```rust
// Rejected:
let encoded = [
    linear_to_srgb(c[0]), linear_to_srgb(c[1]),
    linear_to_srgb(c[2]), c[3],
];
peniko::Color::from_rgba_f32(encoded[0], encoded[1], encoded[2], encoded[3])
```

This makes opaque-color round-trips through `Rgba8Unorm` storage +
`Rgba8UnormSrgb` view-format produce the linear value the user
supplied — endpoint preservation. **But the blend math in between
still happens in vello's sRGB-encoded space.** Vello operates
entirely in sRGB-encoded space (Cairo / 2D-canvas tradition); we
can't change that without forking `WgpuEngine` (off-limits per §11.3)
or switching to `vello_hybrid` (CPU/sparse-strips, different perf
profile, not yet production-ready per its own README).

So encode-on-input fixes endpoints while leaving the math wrong-vs-
linear for partial-cover and partial-alpha cases. That's worse than
picking a side cleanly: it obscures the actual semantic question
("is the renderer's blend space linear or sRGB-encoded?") behind a
half-truth.

Cost estimate, for the record: 3 `powf` calls per RGB color, or
~30ns. Trivial for any realistic scene. Cost is not the reason to
reject it; correctness is.

**CSS conformance — what's reachable today, what isn't:**

CSS conformance breaks into three regimes:

| Regime | Vello today | Status |
| --- | --- | --- |
| `source-over` plain alpha compositing | sRGB-encoded blend | **Matches engine reality**; document and move on |
| Gradient linear-light interpolation (CSS Color 4 `color-interpolation`) | Not honored on GPU compute path | **Upstream-blocked**; tracked by `p1prime_03` (inverts to known-failure when fixed) |
| SVG/CSS filter linear-light operations (gaussian blur, color-matrix) | Filters today run through netrender's render-graph in custom WGSL passes | Linear-light filter math is doable in those passes independently of vello blending — Phase 11'+ scope |

The path to CSS Color 4 gradient conformance is **upstream a fix to
`vello_encoding/src/ramp_cache.rs:84-111`** to honor
`gradient.interpolation_cs` instead of hard-coding
`to_alpha_color::<Srgb>()`. That's a bounded change in vello, not a
fork. `p1prime_03` will catch it the moment it lands.

### 6.4 Implications for Phase 10' / 11' / 12'

Lock the contract before adding text and stroked paths. Concretely:

- **Phase 10' text:** glyph color values from a font / glyph-run
  source are typically in sRGB. They go through unchanged — no
  per-glyph encode step needed.
- **Phase 11' borders:** CSS border colors are sRGB-specified. Pass
  through unchanged.
- **Phase 12' compositing:** group opacity, isolated blend modes, and
  backdrop filters interact with the blend-space contract. The
  composited result of a `mix-blend-mode: multiply` over a
  `source-over` background is sRGB-encoded throughout — matches
  engine behavior, but document the limitation that `linear-light`
  mode (where specified) is upstream-blocked.

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
concern. Per §4.4 it would also be the natural home for a
`parley::Layout<Brush>` → `netrender::GlyphRun` adapter for
embedders that adopt parley as their layout layer; Servo, with
its existing `gfx` + harfrust + inline-layout stack, would skip
that adapter entirely.

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

### 11.8 Phase 7' completion findings (2026-05-04) — **CLEARED**

The Masonry-pattern tile cache shipped as
[`netrender/src/vello_tile_rasterizer.rs`](../netrender/src/vello_tile_rasterizer.rs)
(305 lines). All four `p7prime_vello_tile_cache` probes pass + four
`p7prime_renderer_integration` end-to-end probes pass against the
existing batched-pipeline oracle PNGs.

**What we verified:**

1. **`Scene::append` is bytewise-cheap as expected.** No measurable
   per-tile composition overhead in the test harness; the per-frame
   work is dominated by vello's compute dispatches, not the CPU-side
   tile-Scene merge. Aligns with `vello_encoding/src/encoding.rs`
   verification from §11.3.
2. **Per-tile clip layers correctly handle spanning primitives.** A
   half-alpha rect spanning all four tiles of a 2×2 grid renders to
   uniform `(255, 0, 0, 128)` everywhere it covers — no double-blend
   at tile borders. Each tile-Scene is wrapped in
   `push_layer(tile_world_rect)` / `pop_layer` at compose time, which
   constrains each tile's draws to its own region. Verified by
   `p7prime_04_spanning_primitive_no_double_render`.
3. **TileCache invalidation drives the rasterizer correctly.** A
   no-op re-render reports zero dirty tiles
   (`p7prime_02_unchanged_scene_no_dirty`); a single-rect color
   change marks only its tile dirty
   (`p7prime_03_localized_change`). The `cached_tile_count` /
   `last_dirty_count` getters expose this for hit-rate assertions.
4. **Renderer-level integration via `enable_vello: true`.** The two
   pipelines (batched, vello) coexisted briefly via parallel
   entry points (`prepare/render` vs `render_vello`) sharing the
   same `TileCache`; this proved the integration shape, then the
   batched path was retired entirely (§10's "two backends trap"
   decision applied).

**What we deferred or simplified:**

- **No `Rasterizer` trait.** §2.2 originally proposed
  `Box<dyn Rasterizer>` on `Renderer`. With one rasterizer, the
  trait is an abstraction without users. `VelloTileRasterizer` is
  concrete on `Renderer`. Re-introduce only when a second rasterizer
  ships.
- **Per-frame image-cache rebuild — resolved (2026-05-04).**
  `VelloTileRasterizer::refresh_image_data` previously cleared and
  rebuilt the Path A `peniko::ImageData` map every frame, defeating
  vello's `Blob.id()` dedup. Now the map is persistent: new
  `ImageKey`s are added on first sight via `entry().or_insert_with`,
  keys that disappear from `scene.image_sources` are evicted via
  `retain`. Each `Arc<Vec<u8>>` lives across frames so vello's atlas
  uploads once per key. Verified by `p7prime_05` (Blob id stable
  across re-render and Scene-instance swap) and `p7prime_06`
  (eviction when key drops from scene). The same per-frame rebuild
  still exists in `vello_rasterizer::build_image_cache` for the
  non-tile path; that path doesn't own state across frames so the
  fix would require either a stateful wrapper or moving the cache
  up into the caller.
- **No native-compositor handoff (axiom 14).** Confirmed loss as
  predicted in §2.4. Servo doesn't use this today; the v1.5 fallback
  in §recommendation (whole-frame vello + post-render tile slicing)
  remains an option if Firefox-style native compositing becomes
  required.

**Cleanup outcome (2026-05-04):**

After Phase 7' integration, the batched WGSL rasterizer was retired
on `main`:

- `netrender/src/batch.rs` (608 lines) deleted
- `netrender/src/image_cache.rs` (170 lines) deleted
- `Renderer::prepare` / `render` / `prepare_direct` /
  `prepare_tiled` / `render_dirty_tiles*` /
  `build_tile_composite_draw` / `ensure_gradient_pipelines` /
  `insert_image_gpu` removed
- `PreparedFrame` / `FrameTarget` / `ResourceRefs` /
  `ColorAttachment` / `DepthAttachment` / `DrawIntent` /
  `RenderPassTarget` removed
- `netrender_device`'s `brush_solid` / `brush_rect_solid` /
  `brush_image` / `brush_gradient` pipeline factories + WGSL
  sources + bind-group layouts + tests retired (the crate dropped
  from 2394 → 730 lines)
- 11 redundant batched-path tests deleted; remaining tests run
  through `render_vello`
- The legacy upstream WebRender code (`webrender_api`, `wrench`,
  `wr_glyph_rasterizer`, `examples`, `wrshell`,
  `example-compositor`, `fog`, `peek-poke`, `wr_malloc_size_of`,
  `ci-scripts`) was removed from the workspace and the working
  tree (preserved on the `webrender-wgpu-upstream` side worktree)

Net: -90,000 lines on `main` across the cleanup, leaving netrender
(6,034) + netrender_device (730) ≈ 6,764 lines of live Rust. Vello
is the sole rasterizer.

### 11.9 `FontBlob` unified to `peniko::Blob<u8>` (2026-05-04) — **CLEARED**

`netrender::FontBlob` originally held `Arc<Vec<u8>>` plus a `u32`
font-collection index, and `vello_rasterizer::emit_glyph_run`
wrapped it in a fresh `peniko::Blob::new(...)` per glyph run, per
render. Two consequences:

1. **Vello's font atlas couldn't dedup across frames.** Vello keys
   font atlas slots on `Blob::id()`. `Blob::new` mints a unique id
   at construction; reconstructing the blob every render meant
   every frame's font lookup hit a fresh id and re-uploaded.
2. **The parley adapter copied bytes per `push_layout` call** —
   `Arc::new(font_data.data.data().to_vec())` allocated a fresh
   `Vec<u8>` of the TTF size, since `parley::FontData::data` was
   `peniko::Blob<u8>` and `FontBlob.data` was `Arc<Vec<u8>>`.
   Different shape, no conversion path that preserved id.

Resolution: changed `pub data: Arc<Vec<u8>>` to `pub data:
peniko::Blob<u8>` (re-exported via `netrender::peniko::Blob`).
Construction sites in tests now wrap with `Blob::new(Arc::new(..))`
once. `emit_glyph_run` clones the blob (Arc + id copy, no bytes).
Parley adapter clones `font_data.data` directly, no `to_vec()`.

This deliberately leaks `peniko` into `netrender::scene`'s public
API. The earlier doc claim "the wrapper exists so netrender's Scene
API doesn't leak peniko types" was undermined by the rasterizer
already round-tripping through `peniko::Blob` per render and
defeating its dedup. The honest fix is to align the type, accept
the public-API surface, and re-export `peniko` for consumer access.

Receipts:

- All 79 workspace tests pass after the change (the same set that
  passed before, including the parley adapter's `shape_and_paint`
  binary and the renderer integration tests).
- Construction-site updates in
  `netrender/tests/p10prime_a_glyph_api.rs` (5 sites),
  `netrender/tests/p10prime_b_glyph_render.rs` (1 site), and
  `netrender_text/src/lib.rs` (1 site, now `font_data.data.clone()`).
- `vello_rasterizer.rs::emit_glyph_run` simplified from
  `Blob::new(blob.data.clone())` to `blob.data.clone()`.

Side effect: `netrender` now `pub use vello::peniko;` so consumers
can build `FontBlob` without a separate `vello`/`peniko` dep.

### 11.10 Variable-radius box-shadow blur (2026-05-04) — **CLEARED**

`Renderer::build_box_shadow_mask` previously took a `blur_step: f32`
(texel-space sample distance for one fixed 2-pass blur). The 5-tap
binomial kernel saturates at small effective blur — pushing the
step up past ~2 px per tap produces visible 5-tap quantization
instead of a smooth Gaussian, so the API couldn't honestly serve
CSS-style `box-shadow: 0 0 12px` requests.

Resolution: signature is now `blur_radius_px: f32` (CSS-pixel
units). `blur_kernel_plan` picks a per-pass step capped at 2 px
and a pass count `N = ceil((σ_target / step)²)` where
`σ_target = blur_radius_px / 2` (WebKit/Mozilla convention; the
spec is ambiguous, the comment in `renderer/mod.rs` flags this).
`build_box_shadow_mask` then chains `1 + 2N` render-graph tasks:
mask, then N alternating H/V `brush_blur` passes. Pass count
capped at 50 — large blurs that exceed it would benefit from the
classic downscale-blur-upscale trick, not implemented yet.

Receipts:

- Five unit tests (`blur_plan_tests`) cover the planner: zero
  radius, σ at the cap boundary, cascade trigger, the σ_total
  invariant, and the pass-count cap.
- `p11c_02_blur_radius_extends_halo` (in
  `netrender/tests/p11prime_c_box_shadow.rs`) renders the same
  shadow source with `blur_radius_px = 2` and `= 16` and asserts
  the larger blur darkens a probe 8 px outside the source by
  ≥ 25 grayscale levels — visible runtime evidence the cascade
  widens the kernel as the radius grows.
- Demo (`demo_card_grid.rs`) bumped from a 1-pass tight blur to
  `blur_radius_px = 12.0` for Card 5; the resulting halo is
  visibly softer and extends further than the previous output.

Math notes (for future maintainers):

- One 5-tap binomial pass with step = `k` pixels has σ = `k`
  (variance = `k²` — the kernel weights `[1, 4, 6, 4, 1] / 16`
  applied at offsets `[-2k, -k, 0, k, 2k]`).
- Cascading N H+V pairs accumulates variance: `σ_total = k · √N`.
- The empirical receipt at the probe matches Gaussian-edge falloff
  `0.5 · erfc(d / (σ√2))` to within bilinear-sampler precision.

### 11.17 C-architecture readiness — `compose_into` + registries (2026-05-04) — **CLEARED**

Background: graphshell-shaped consumers building multiple netrender
viewports per frame have three architecture options (per the
recommendation discussion this session):

- **A.** Each consumer owns its own `vello::Renderer` and renders to
  its own texture. Cross-consumer interaction = texture sampling.
- **B.** Consumers share one `vello::Renderer` via `Mutex`. Each
  still renders to its own texture; renders serialize at the lock.
- **C.** A single `vello::Scene` per frame, composed from N
  consumers via `Scene::append`, rendered once. Atlas slots dedup
  across consumers via `peniko::Blob::id()`. Cross-consumer
  interaction = bytewise scene composition.

The decision was: don't ship the consumer side of A→B→C yet (no
multi-consumer code paths exist), but **make the netrender side
C-ready now** so when graphshell decides on C the renderer is
already speaking the protocol. The work landed in this finding.

**What changed:**

1. **`ImageData.bytes` unified to `peniko::Blob<u8>`** (analog to
   §11.9's `FontBlob` unification). Required for cross-consumer
   atlas dedup: vello keys atlas slots on `Blob::id()`, which is
   stable through `Arc`-shared bytes but not through fresh
   `Vec<u8>` clones. New constructors `ImageData::from_bytes` and
   `ImageData::from_blob` cover the common cases. 8 construction
   sites updated across tests and the demo.

2. **`netrender::FontRegistry`** (`registry.rs`). HashMap from
   `(Blob::id(), font_index)` → `FontId`. Threaded through
   `netrender_text::push_layout_with_registry` (new function);
   the existing `push_layout` becomes a thin wrapper that builds
   a fresh registry per call. Consumers that build many layouts
   into one Scene per frame share one registry → one entry in
   `scene.fonts` per unique font, regardless of call count.
   Receipts: 3 unit tests (dedup within call, separate distinct
   blobs, separate distinct collection indices).

3. **`netrender::ImageRegistry<K>`** (`registry.rs`). HashMap from
   consumer-supplied key `K: Eq + Hash` → `ImageKey`. The
   consumer-key shape acknowledges that "is image A the same as
   image B" is a consumer-domain question (same URL? content
   hash?) we can't answer — we just provide the bookkeeping.
   Receipts: 3 unit tests (dedup by consumer key, distinct keys
   allocate distinct ImageKeys, `get` doesn't insert).

4. **`VelloTileRasterizer::compose_into`** — the C entry point.
   Same tile-cache update + master-scene composition as `render`,
   but appends the result into a caller-provided `vello::Scene`
   with a caller-provided `Affine` instead of rendering to a
   texture. Internal: factored a private `build_master_scene`
   helper that both `render` and `compose_into` call. Receipts:
   3 integration tests:
   - `compose_into_01_identity_matches_render` — pixel-exact
     match (within ±1 channel) between rendering directly and
     composing-then-rendering at identity transform. Pins the
     contract that `compose_into` is a refactor of the inner
     steps of `render`, not a different code path.
   - `compose_into_02_transform_translates_content` — translate
     transform shifts content by exactly that translate.
   - `compose_into_03_two_consumers_share_atlas` — two
     `VelloTileRasterizer`s composing scenes that reference the
     *same* `Arc`-shared image bytes produce the same Blob id in
     each rasterizer's image cache. The cross-consumer dedup
     signal vello's atlas keys on is reachable.

**What this enables:**

- Graphshell can hold one `vello::Renderer` at app boot, give
  netrender consumers a `&mut vello::Scene` to compose into, and
  do a single `render_to_texture` per frame. No cross-consumer
  texture-sampling boundary; one GPU submit; atlas slots shared
  across panes.
- An animating embedded surface (graph node moving across canvas
  with embedded webview content) re-rasterizes from vector data
  every frame instead of resampling a fixed texture — sharp at
  any zoom and any motion.
- Live thumbnails for a navigator: append a pane's tile-Scenes
  into the swatch's master Scene with a scale transform; one
  rasterization, no texture readback.

**What it does *not* enable on its own:**

- Concurrent N-consumer encoding under B is still serialized at
  the renderer Mutex; only C avoids that.
- Cross-consumer image-data sharing only kicks in when consumers
  hand the same `Arc`-shared bytes (or use a shared
  `ImageRegistry`). Two consumers that each load the same favicon
  from disk into separate `Vec`s still get separate atlas slots
  — by design (we don't try to content-hash bytes for them).

**Workspace state after this finding:** 114 tests passing
(was 105; +9: 6 registry, 3 compose_into), 0 failures,
0 clippy warnings, 0 build warnings.

### 11.11 Unified painter-order op list (2026-05-04) — **CLEARED**

Pre-refactor `Scene` carried six per-type Vecs (`rects`, `strokes`,
`gradients`, `images`, `shapes`, `glyph_runs`) and the rasterizer
walked them in a fixed cross-type order: rects → strokes →
gradients → images → shapes → glyph runs. Painter order was
implicit in primitive *type*, not consumer push order.

The first iteration of the demo's Card 6 made the failure mode
concrete: a magenta "badge" rect pushed *after* an image painted
*under* the image, because rects-before-images is a property of the
type-Vec design regardless of consumer intent. The matching note in
`p11prime_c_box_shadow.rs::p11c_01` flagged the same shape: a drop
shadow image had to land over (rather than under) its associated
card body, since rects come first.

Resolution: replaced the six Vecs with one `pub ops: Vec<SceneOp>`
where

```rust
pub enum SceneOp {
    Rect(SceneRect),
    Stroke(SceneStroke),
    Gradient(SceneGradient),
    Image(SceneImage),
    Shape(SceneShape),
    GlyphRun(SceneGlyphRun),
}
```

Every `Scene::push_*` helper appends one variant; the rasterizer
iterates `ops` once and dispatches per match arm. Tile-cache
dependency hashing (`hash_tile_deps`) and the per-tile filter
(`filter_scene_to_tile`) collapsed similarly — one walk over `ops`
replaces the six separate walks. Convenience iterators
`Scene::iter_rects`, `iter_strokes`, … re-expose the per-type view
where consumers want it (currently used only by tests).

`SceneOp` is now in the public surface alongside `Scene`.

Receipts:

- `netrender/tests/op_list_painter_order.rs` — three new tests:
  `op_order_01` proves a rect pushed after an image paints on top
  (the previous design failed this); `op_order_02` is the symmetric
  case (anchors the contract from the other side); `op_order_03`
  is a structural check that `Scene::ops` accumulates one entry per
  push helper, in call order, with the right variant per primitive
  kind.
- Demo Card 6: the badge rect now visibly paints over the image —
  the rendered PNG is the runtime-visible regression switch.
- Demo Card 5: the drop-shadow image is now pushed *before* the
  card body via a `ShadowDef` parameter to `build_cards`, so it
  sits under the card as CSS expects. Pre-refactor the shadow
  always painted over because images came after rects/gradients
  by type.
- Full workspace: 88 tests passing (was 85 + 3 new).

Migration was tightly scoped: 22 push-call sites in `scene.rs`, 3
iteration sites (`vello_rasterizer.rs`, `tile_cache.rs`,
`vello_tile_rasterizer.rs::filter_scene_to_tile`), and 3 test
files reading per-type Vecs (rewritten to use `iter_*`
accessors). No primitive structs changed; the variants are
pure carriers.

This refactor unblocks Phase 12b' (nested groups) — once Scene
holds an op list, push/pop scope ops slot in as additional
variants without further structural change.

### 11.12 Hit testing (2026-05-04) — **CLEARED**

Open question 3 in the original plan ("hit testing — what's the
return shape?") had been deferred pending consumer pull; the
op-list refactor (§11.11) made it the natural next step since
"top-most primitive at point" maps directly onto "last entry in
`Scene::ops` whose AABB contains the point."

API: `netrender::hit_test::{hit_test, hit_test_topmost,
HitResult, HitOpKind}` (re-exported at the crate root).

```rust
pub fn hit_test(scene: &Scene, point: [f32; 2]) -> Vec<HitResult>;
pub fn hit_test_topmost(scene: &Scene, point: [f32; 2]) -> Option<HitResult>;
```

The stack form is the primitive: returns every primitive covering
the point in top-most-first order. `hit_test_topmost` is the
short-circuiting common case for "what did the user click on."
`HitResult` carries an `op_index` (stable for the scene's lifetime)
and a `HitOpKind` tag mirroring `SceneOp` variants.

**Why a stack, not a single hit:** event bubbling, pick-through-
transparency, drag selection, hover targeting on overlay stacks —
all need to traverse from topmost down. Servo / WebRender's hit
test returns a stack with a short-circuit option; we follow that
shape. `single = .first()` is the special case; the reverse isn't
true.

Precision: AABB-level only.

- Rect / image / gradient: world-space AABB of the primitive's
  local rect, transformed via `scene.transforms`.
- Stroke: AABB inflated by `stroke_width / 2`. The interior of a
  stroked rect counts as a hit (typically what UI consumers want).
- Shape: bounding box of the path. Per-segment point-in-polygon
  is a future addition when consumer pull surfaces it.
- Glyph run: combined AABB of glyph origins, inflated by
  `font_size`. Per-glyph hit-testing needs real font metrics.

`clip_rect` (when set) gates inclusion: a point outside the clip's
AABB does not hit, even if the primitive AABB covers it. Rounded-
corner clips test against their AABB; refining the corner regions
is future work.

Receipts: 7 unit tests in `netrender/src/hit_test.rs::tests` —
empty scene, inside-rect, outside-rect, three-deep stack ordering,
top-most short-circuit, clip-rect exclusion, and mixed-kind stack.
Full workspace 95 tests passing (+7 vs §11.11's 88).

Future refinements (deferred):

- Per-glyph hit testing using the font's outline tables.
- Per-segment point-in-polygon for `SceneOp::Shape`.
- Honoring rounded-rect clip corners precisely (currently AABB).
- Coordinate-space helper for window-to-scene mapping (consumers
  do this themselves today).

### 11.13 Display-list format — discussion (2026-05-04)

After the op-list refactor (§11.11) the question "what's a
display list in this codebase" mostly answers itself: `Vec<SceneOp>`
*is* the display list. Two follow-up questions remain about the
consumer-facing shape; this section captures the design space so a
real consumer can pick.

**What the shape looks like in adjacent projects.** Cross-checked
to inform the decision, not to copy any of them wholesale:

- **WebRender display list** (Servo / Firefox): a flat
  `Vec<DisplayItem>` with rich CSS-shaped variants — `StackingContext`,
  `ScrollFrame`, `ClipChain`, `BoxShadow`, plus the leaf primitives.
  Tuned for cross-process bincode serialization (Servo's content
  process builds it, the GPU process consumes it). Heavy for a
  graphshell-scoped consumer that doesn't have a process boundary.
- **Skia `SkPicture`**: a recorded sequence of canvas calls,
  played back on demand. Same record-and-replay shape as our op
  list, with serialization layered on top. Validates the
  flat-op-list design.
- **Flutter layer trees**: compositor-shaped, not display-list-
  shaped. Different problem; not directly applicable.
- **SVG / CSS painter model**: document order = paint order; the
  spec is itself a flat-list-with-stacking-contexts model. Our
  op list maps onto it directly except for stacking contexts (the
  `push_layer` / `pop_layer` ops we'd add for §12b').

**Three options for the consumer-facing shape:**

A. **Push-helper-only (status quo).** Consumers call
   `scene.push_rect(...)` etc. `Scene::ops` is public for read,
   but consumers don't construct `SceneOp` variants directly;
   they go through the typed helpers. The display list is
   implicit — there's no "format" the consumer hands in.

   *Best for:* ad-hoc scenes, immediate-mode UI loops.

B. **`Vec<SceneOp>` is the canonical format.** Consumers can
   either use push helpers or build `Vec<SceneOp>` directly and
   replace `Scene::ops` wholesale. Recording is `scene.ops.clone()`;
   replay is `scene.ops = recorded`. Mutation is direct Vec
   indexing.

   *Best for:* persistent / mutable display lists, recording UIs,
   editor-shaped consumers that want to manipulate the list
   between frames. This is what `SkPicture`-shaped uses look like.

C. **Higher-level `DisplayItem` enum that lowers to SceneOp.**
   A semantic layer: variants like `Card { bounds, color, border }`
   that the consumer composes, with a translator emitting
   `Vec<SceneOp>`. Decouples the consumer's domain types from
   netrender's primitive types.

   *Best for:* a structured document model where consumer
   "intent" is meaningfully bigger than netrender's primitives
   (browser-style content, full DOM-equivalent representations).

**Recommendation:** ship Option B explicitly when a real consumer
needs it; don't pre-build C.

Concretely: `SceneOp` is already public and clonable. Treat it as
the canonical format. If a consumer surfaces "I want to record /
replay / serialize a scene" we add a thin recorder API
(`scene.snapshot() -> Vec<SceneOp>`, `scene.replay(&[SceneOp])`)
that's literally a Vec clone + assign. No format design work
required — the data type is the format.

Reject C until a real consumer actually has document types whose
mismatch with `SceneOp` is costly. If graphshell ends up wanting
e.g. `Node`-level display items (with edges, ports, labels as
substructure), that's a graphshell crate, not netrender — same
boundary as parley → netrender_text. The display list at the
netrender boundary stays primitive-shaped.

**Don't:** model on WebRender's display list. Stacking contexts
and scroll frames belong to a CSS-conformance project, which this
isn't. The op-list-with-future-push/pop-layer-variants design is
what we have, and it's what we should ship.

### 11.14 Nested layers + arbitrary-path clips (2026-05-04) — **CLEARED**

The op-list refactor (§11.11) ended with the explicit observation
that `push_layer` / `pop_layer` slot in as additional `SceneOp`
variants. Done in this pass:

```rust
pub enum SceneClip {
    None,
    Rect { rect: [f32; 4], radii: [f32; 4] },
    Path(ScenePath),  // Phase 9b'
}

pub struct SceneLayer {
    pub clip: SceneClip,
    pub alpha: f32,
    pub blend_mode: SceneBlendMode,
    pub transform_id: u32,
}

// New variants in `SceneOp`:
SceneOp::PushLayer(SceneLayer),
SceneOp::PopLayer,
```

CSS analogues map cleanly: `opacity` → `SceneLayer::alpha`,
`mix-blend-mode` → `SceneLayer::blend_mode`,
`clip-path` / rounded `overflow: hidden` → `SceneClip` variants,
`isolation: isolate` is the implicit effect of any non-trivial
layer.

The 9b' arbitrary-path clip is a sub-case of 12b': a layer with
`SceneClip::Path(ScenePath)`, alpha 1.0, blend mode Normal. No
separate per-primitive path-clip needed; the layer mechanism
covers it because layers can wrap one primitive just as well as
many.

Rasterizer dispatch: `SceneOp::PushLayer` → `vscene.push_layer`,
`SceneOp::PopLayer` → `vscene.pop_layer`. Debug-builds assert
push/pop balance at scene-translation time. Empty layers (no
inner ops between push and pop) are valid and produce no pixels.

Tile cache: `hash_push_layer` mixes the layer's clip / alpha /
blend / transform into the per-tile dependency hash, and
`SceneOp::PopLayer` contributes a marker byte. Dirty-tracking
treats layer changes as global (every tile inside the layer's
clip-AABB invalidates) — conservative but correct; refining to
clip-AABB-bounded invalidation is future work.

Tile filter (`filter_scene_to_tile`): always includes layer
push/pop ops in the filtered scene so balance is preserved per
tile. Layer's own clip narrows what pixels can be touched anyway.

Receipts (`netrender/tests/p12b_nested_layers.rs`):

- `p12b_01_alpha_layer_fades_inner_content` — alpha 0.5 layer
  wrapping a red rect over white bg produces mid-pink pixels.
- `p12b_02_rect_clip_layer_culls_outer_pixels` — rect clip culls
  pixels outside.
- `p12b_03_rounded_clip_layer_clips_corners` — rounded-rect clip
  produces visible corner clipping.
- `p9b_01_path_clip_layer_culls_outside_path` — triangle-shaped
  `ScenePath` clip wrapping a full-frame rect: only the triangle
  paints.
- `p12b_04_nested_layers_compose` — outer alpha + inner rect clip
  combine correctly (alpha-faded red inside the clip; bg color
  outside).

Plus 2 new hit-test unit tests:

- `layer_ops_skipped_in_hit_walk` — `PushLayer` / `PopLayer` ops
  don't generate hits themselves.
- `per_glyph_hit_returns_glyph_index` (see §11.15).

Caveat: hit testing does not yet honor a layer's clip (an inner
op is hit even if the layer's clip would have culled its pixels).
Documented in `hit_test.rs`'s module doc; future work is a
clip-stack-aware walk. Today's behavior is conservative — consumer
can post-filter the stack if they need clip-respecting hits.

### 11.15 Per-glyph hit testing (2026-05-04) — **CLEARED**

`HitResult` gained a `glyph_index: Option<usize>` field. For a
[`HitOpKind::GlyphRun`] hit, it's the index of the specific glyph
whose approximate AABB contains the point, or `None` if the point
is in the run's overall AABB but doesn't land on any individual
glyph (e.g., trailing whitespace or inter-glyph gap). `None` for
all other kinds.

Per-glyph AABB (no font metrics required): each glyph at
`(x, y)` gets a box

```text
(x, y - font_size, x + advance, y + font_size * 0.25)
```

where `advance = next_glyph.x - this_glyph.x`, or `font_size` for
the last glyph. A `0.25 * font_size` floor on advance keeps
combining marks / narrow glyphs clickable. This sketches an em-
box top-to-shallow-descender; real font metrics (via skrifa, which
parley already pulls in transitively) would tighten the box.
Deferred until a consumer needs the precision; for "click on this
character" UI the approximation is enough.

Receipt: `hit_test::tests::per_glyph_hit_returns_glyph_index` —
constructs a 3-glyph run at known x positions, hits each glyph's
box, verifies the returned `glyph_index`. Also confirms
`glyph_index = None` for non-glyph-run hits.

See §11.99 below for the consolidated open-items catalogue
(per-glyph metric refinement, point-in-polygon for shapes, etc.).

### 11.16 Polish sweep (2026-05-04) — **CLEARED**

Closed in one batch:

- **Edition bump.** All three workspace crates (`netrender`,
  `netrender_device`, `netrender_text`) moved from edition `2018`
  to `2021`. Unblocks `{var}` capture syntax in `format!` /
  `assert!`, IntoIterator-for-arrays, and the prelude additions
  (`TryFrom`, `TryInto`, `FromIterator`).
- **`Scene::clear_ops()`** — drops the op list without touching
  `fonts`, `transforms`, or `image_sources`. Lets streaming
  consumers do "rebuild ops per frame, reuse asset palette"
  without the boilerplate.
- **Layer-clip-aware hit testing.** `hit_test` and
  `hit_test_topmost` now run a forward pre-pass that tracks the
  active layer-clip stack at each op index, then a reverse pass
  that skips ops whose visibility is occluded by an enclosing
  layer's clip. Two new tests
  (`layer_clip_culls_inner_op_outside_clip`,
  `nested_layer_clips_intersect`) pin the contract: nested clips
  intersect, an outer clip culls inner ops correctly. AABB-only
  for non-axis-aligned clip shapes (rounded-rect corners and
  arbitrary path interiors register as visible at AABB level —
  same conservative tradeoff as elsewhere).
- **Decoration painting in `netrender_text`.** The parley adapter
  now emits underline / strikethrough rects from
  `Style::underline` / `Style::strikethrough` and the run's
  `RunMetrics`. Painting order matches the CSS text-decoration
  spec (underline → glyphs → strikethrough). Receipt at
  `netrender_text_03_decorations_emit_rects` checks the rect
  count, the brush colors, and the painter-order invariant.

105 tests passing across the workspace; 0 failures.

### 11.18 Color emoji / COLR fonts (2026-05-06) — **CLEARED**

Roadmap [B3 verification probe](2026-05-04_feature_roadmap.md):
*"vello + skrifa already handle COLR layer rendering on the glyph
path; we likely get this for free."*

**Verified.** A probe loading Segoe UI Emoji on Windows
(`C:\Windows\Fonts\seguiemj.ttf`, 12.4 MB), shaping `"😀🎉🌈"` via
parley at 48 px, rendering through `Renderer::render_vello`, and
reading back pixels measures **91% chromatic ratio** (4118 of 4524
painted pixels have channel divergence > 32 / 255). That is
overwhelmingly above the 5% threshold separating "COLR layers
honored" from "achromatic silhouette only." vello's GPU glyph
path renders peniko's COLR-decoded layers without any netrender-
side work.

Receipt at
[`netrender_text/tests/pb3_color_emoji_probe.rs`](../netrender_text/tests/pb3_color_emoji_probe.rs).
Skipped vacuously on hosts without one of the canonical emoji
font paths (Segoe UI Emoji / Apple Color Emoji / Noto Color
Emoji); CI that wants to enforce this should bundle Noto under
`tests/data/`.

No netrender-side work item. Re-run the probe on text-stack
changes (vello / skrifa / parley bumps) as a cheap regression
canary.

### 11.19 Selection rects + caret helpers (2026-05-06) — **CLEARED**

Roadmap [B1](2026-05-04_feature_roadmap.md): selection highlight +
caret emission for nematic's Gemini/Gopher/Scroll viewers,
Markdown editors, and feed readers.

`netrender_text` now exposes:

- `selection_rects(&Layout, Range<usize>) -> Vec<[f32; 4]>` — one
  rect per visual line that the byte range touches; thin wrapper
  over `parley::Selection::geometry`. Bidi-correct (RTL runs
  produce the right line-anchored bands by parley's own logic).
- `caret_rect(&Layout, byte_index, Affinity, width) -> [f32; 4]` —
  caret rectangle at a byte position; thin wrapper over
  `parley::Cursor::geometry`. Caret blink is consumer-side
  (alternate paint / no-paint at the platform's cadence); we just
  return the shape.

Both pure CPU, no GPU dependency. Receipts at
[`netrender_text/tests/pb1_selection_and_caret.rs`](../netrender_text/tests/pb1_selection_and_caret.rs)
cover collapsed ranges (empty), single-line bands, multi-line
bands ordered top-to-bottom, caret position at start, monotonic
caret advance through text, stable caret height across the same
line, and partial-vs-full line widths.

The roadmap had B1 framed as consumer-pull-gated ("nematic ships
shaped text via parley and asks for selection rects"). Closer
look at parley's API showed the trigger was protective rather
than technical: `parley::Selection::geometry` and
`parley::Cursor::geometry` already exposed exactly the right
shape, so wrapping them as netrender_text helpers was a
no-speculation ~30-line job.

### 11.20 Path-precise hit testing for shapes + path/rounded clips (2026-05-06) — **CLEARED**

Roadmap [R2 + R3](2026-05-04_feature_roadmap.md): tighten
`hit_test` from AABB-conservative to path-precise for arbitrary
`SceneOp::Shape` ops and for `SceneClip::Path` / rounded-rect
`SceneClip::Rect` clips.

Both fixes are thin wraps around `kurbo::Shape::contains`:

- `op_contains_point` for `SceneOp::Shape` now AABB-pre-passes,
  inverse-transforms the world point to the shape's local space,
  builds a `BezPath` from the `ScenePath`, and calls `contains`.
- `clip_aabb_contains_point` for `SceneClip::Path` does the same
  (BezPath::contains in local space). For `SceneClip::Rect` with
  non-zero radii, it builds a `kurbo::RoundedRect` and calls its
  `contains`. Sharp axis-aligned rects skip the path-precise check.
- Non-invertible transforms (degenerate scale, etc.) fall back to
  AABB-conservative — same protective default as before, just
  scoped to the cases where the inverse can't be computed.

The `transform_to_affine` and `build_bez_path` helpers from
[`vello_rasterizer`](../netrender/src/vello_rasterizer.rs) were
promoted from module-private to `pub(crate)` so `hit_test` can
reuse them.

Receipts: 8/8 in
[`netrender/tests/pr2_pr3_path_precise_hits.rs`](../netrender/tests/pr2_pr3_path_precise_hits.rs)
covering triangle centroid hits, AABB-corner-but-outside-triangle
misses, transformed-shape path-precision, rounded-rect clip
corner-cutout misses, sharp-rect-clip unchanged behavior, path
clip path-precise, and a combined shape-inside-path-clipped-layer
case. The original AABB-only tests in `hit_test::tests` still
pass (the AABB pre-pass + path-precise refinement is a strict
narrowing).

This was another consumer-pull-gated item where the upstream API
was already in shape — a no-speculation ship-now per the
"consumer-pull gates need a sanity check" feedback memory.

### 11.21 Inline-box walker in `netrender_text` (2026-05-06) — **CLEARED**

Roadmap [R6](2026-05-04_feature_roadmap.md): expose a per-line
walker that surfaces glyph runs and inline-box placements in
visual order so consumers (graphshell-shaped, nematic, …) can
paint inline images / nested widgets / embedded layouts without
re-deriving line geometry.

`netrender_text` now exposes:

- `push_layout_with_inline_boxes(scene, registry, layout, origin,
  on_inline_box)` — single integrated walker. Glyph runs flow into
  the scene with the same logic as `push_layout` (font dedup,
  decorations, positioning); each `PositionedLayoutItem::InlineBox`
  fires the callback with a typed `InlineBoxPlacement` carrying
  scene-space coordinates (origin already applied), the
  consumer-supplied id, width, and height. Items emerge in parley's
  visual order (top-to-bottom by line, left-to-right within a line
  after BiDi reordering); inline boxes and glyph runs interleave
  in their natural order.
- `InlineBoxPlacement { x, y, width, height, id }` — scene-space
  placement record.

The plain `push_layout` / `push_layout_with_registry` entry points
are now thin wrappers around the inline-box-aware walker with an
empty callback — same behavior as before, no inline-box surface
exposed, no glyph-run emission duplicated. A new `emit_glyph_run`
internal helper holds the shared body.

Receipts: 6/6 in
[`netrender_text/tests/pr6_inline_box_walker.rs`](../netrender_text/tests/pr6_inline_box_walker.rs)
covering metadata round-trip (id, dimensions), origin-as-translation-
delta, glyph-run emission alongside callbacks, multi-box visual-
order ordering, the no-inline-box thin-wrapper case, and box-x in
layout bounds.

Same consumer-pull-gate sanity-check pattern: parley's
`PositionedLayoutItem` was already in shape; the wrap was no-
speculation work.

### 11.22 R9-canary wired (2026-05-06) — **CLEARED (trigger detector only; R9 itself remains blocked)**

Roadmap [R9-canary](2026-05-04_feature_roadmap.md): wire a CI
tripwire that signals the moment vello's GPU compute path starts
honoring `peniko::Gradient::interpolation_cs`. The wrap itself
(R9: `Scene::interpolation_color_space` field) stays parked until
the canary turns green.

Implementation:

- `linear-light-canary` cargo feature on the netrender crate; off
  by default so normal builds skip the canary entirely.
- `p1prime_03_canary_linear_light_is_honored` test gated under
  that feature, asserting the **fixed** behavior (LinearSrgb
  gradient midpoint differs from default by ≥ 16/255 per channel).
- Today the canary is **RED**: `mid_default = mid_linear =
  [127, 0, 128, 255]`, max_chan_diff = 0. Vello's GPU compute
  path still hard-codes `to_alpha_color::<Srgb>()` per
  `vello_encoding/src/ramp_cache.rs:86,97`.
- CI usage:
  `cargo test --features linear-light-canary -p netrender
   p1prime_03_canary_linear_light_is_honored`. Run on every
  vello-dep bump; failure today is informational, not a build
  block.

The canary panics with a loud RED-state message describing
exactly what's still missing and why R9 stays parked. When it
turns GREEN it prints a follow-up notice telling the next reader
to ship the R9 wrap and retire both the canary and the twin
`p1prime_03_gradient_default_is_srgb_encoded`. The two flip
together: when the canary greens, the twin starts failing because
the LinearSrgb-equals-default invariant breaks.

R9 itself remains [open on the roadmap](2026-05-04_feature_roadmap.md)
— this entry only clears the trigger-detector wiring.

### 11.23 Per-glyph hit testing via skrifa metrics (2026-05-06) — **CLEARED**

Roadmap [R1](2026-05-04_feature_roadmap.md): replace the em-box
approximation in `glyph_run_per_glyph_hit` with real font-supplied
glyph bounds.

`hit_test::glyph_run_per_glyph_hit` now opens the font via
`skrifa::FontRef::from_index(blob.data.data(), blob.index)` and
queries `GlyphMetrics::bounds(GlyphId)` per glyph. Bounds come back
in font (y-up) space; we mirror around the glyph origin's y to
match netrender's screen (y-down) convention.

Em-box fallback covers three cases: the `font_id == 0` sentinel,
font-parse failures (corrupt or empty bytes), and glyphs without
outline bounds (notably COLR emoji where the outline table is
empty — color emoji glyphs still hit the run-level AABB and the
em-box fallback is fine for those).

Added `skrifa = "0.42"` as a direct netrender dep (matching the
version vello pulls transitively).

Receipt at
[`netrender/tests/pr1_per_glyph_hit_metrics.rs`](../netrender/tests/pr1_per_glyph_hit_metrics.rs)
(3/3): a probe at the descender tail of a real 'g' hits under real
metrics where it would have missed under em-box; an above-glyph
point misses; a sentinel-font glyph still hits via em-box.

### 11.24 Image cache for the simple rasterizer (2026-05-06) — **CLEARED**

Roadmap [R4](2026-05-04_feature_roadmap.md): mirror
`VelloTileRasterizer::image_data` for the simple (non-tile) path.

New `vello_rasterizer::VelloRasterizer` struct holds
`image_data: HashMap<ImageKey, ImageData>` and
`image_overrides: HashMap<ImageKey, ImageData>` across calls.
`scene_to_vello(&mut self, scene)` refreshes the cache against
`scene.image_sources` (insert new, evict missing, no work for
unchanged keys), merges Path A + Path B, and calls a new
`scene_to_vello_with_cache` (extracted from
`scene_to_vello_with_overrides`).

Same Path B `register_texture` / `unregister_texture` interface as
the tile rasterizer. Existing `scene_to_vello` /
`scene_to_vello_with_overrides` free functions are unchanged
(back-compat).

Receipt at
[`netrender/tests/pr4_simple_rasterizer_image_cache.rs`](../netrender/tests/pr4_simple_rasterizer_image_cache.rs)
(7/7): cache fills on first call, stays stable across identical
calls, grows on new keys, evicts dropped keys, register/unregister
round-trips.

### 11.25 Stroke decorations (2026-05-06) — **CLEARED**

Roadmap [C1](2026-05-04_feature_roadmap.md): line caps, joins, and
dash patterns plumbed through to kurbo's stroke.

`SceneStroke` gained `cap: SceneStrokeCap`, `join: SceneStrokeJoin`,
`dash_pattern: Vec<f32>`, and `dash_offset: f32`. New netrender-
owned enums (`SceneStrokeCap` = Butt / Round / Square,
`SceneStrokeJoin` = Bevel / Miter / Round) keep peniko/kurbo out of
the Scene API. Mapped 1:1 to `kurbo::Stroke::with_caps` /
`with_join` / `with_dashes` in `emit_stroke`. Existing helper
constructors (`push_stroke`, `push_stroke_rounded`,
`push_stroke_full`) default to Butt / Miter / no-dashes. New
`Scene::push_stroke_decorated` takes cap / join / dash_pattern as
explicit arguments.

Tile-cache `hash_stroke` extended to include cap, join, dash
pattern, and dash offset — changes to any of these invalidate
covered tiles.

Receipt at
[`netrender/tests/pc1_stroke_decorations.rs`](../netrender/tests/pc1_stroke_decorations.rs)
(8/8): defaults documented, push_stroke_decorated applies args,
each of cap / join / dash_pattern / dash_offset triggers tile
invalidation, unchanged decorations keep tiles clean.

### 11.26 SceneOp::Pattern (2026-05-06) — **CLEARED**

Roadmap [C2](2026-05-04_feature_roadmap.md): repeated-tile fill
primitive.

New `ScenePattern { tile: ImageKey, extent: [f32; 4], scale: f32,
transform_id, clip_rect, clip_corner_radii }` struct, exposed as
`SceneOp::Pattern(ScenePattern)`. New `Scene::push_pattern(tile,
extent, scale)` helper.

Translation: `emit_pattern` builds an
`ImageBrush::new(img).with_extend(Extend::Repeat)` and fills the
extent rect. The `scale` is threaded as a brush_transform
(`Affine::scale(scale)`), so a unit step in image-pixel space
becomes `scale` units in scene-local space — a tile is
`image_size * scale` wide. Negative or zero `scale` clamps to 1.0
defensively.

Hit testing: `HitOpKind::Pattern` added; `op_contains_point` uses
the AABB of the extent rect.

Tile-cache: `hash_pattern` includes tile key, extent, scale,
transform, clip. `filter_scene_to_tile` intersects on extent
world-AABB. `dump_ops` (A1 inspector) labels the new op.

Receipt at
[`netrender/tests/pc2_pattern_op.rs`](../netrender/tests/pc2_pattern_op.rs)
(8/8): push appends a Pattern op, iter_images / iter_rects don't
see it, tile/extent/scale changes invalidate, hit-testing reports
Pattern, dump_ops labels it.

### 11.27 Variable fonts axis interpolation (2026-05-06) — **CLEARED**

Roadmap [C4](2026-05-04_feature_roadmap.md): thread variable-font
axis values through to vello's glyph path.

`SceneGlyphRun` gained `font_axis_values: Vec<(SceneFontAxisTag,
f32)>` where `SceneFontAxisTag = [u8; 4]` (ASCII bytes per the
OpenType spec; e.g., `*b"wght"` for the weight axis). User-space
values (e.g., 100, 400, 700 for weight) flow into the new
`compute_normalized_coords` helper which:

1. Parses the font with `skrifa::FontRef`.
2. Calls `font.axes().location(settings)` to do user→normalized
   conversion (skrifa's existing path; respects each axis's
   user-space range and avar mapping).
3. Extracts the `F2Dot14` coords as raw `i16` bits — the
   representation `vello::NormalizedCoord` (= i16) consumes.

`DrawGlyphs::normalized_coords` then receives the slice when
non-empty; empty axis values keep the font at its default location
(common case, no overhead).

New `Scene::push_glyph_run_variable` helper takes axis values
explicitly. `Scene::push_glyph_run` and `push_glyph_run_full`
default the field to empty.

Tile-cache `hash_glyph_run` includes axis values so weight / width
animations invalidate covered tiles.

Receipt at
[`netrender/tests/pc4_variable_fonts.rs`](../netrender/tests/pc4_variable_fonts.rs)
(7/7): default empty-axis-values, push_glyph_run_variable
applies args, each of value/tag/add invalidates, unchanged keeps
tiles clean. Plus a GPU smoke that loads Bahnschrift on Windows,
renders 'B' at wght = 300 / 400 / 700, and asserts bold paints
visibly more ink than light (the receipt the roadmap asked for).

## 11.99 Open items — moved (2026-05-05)

The catalogue of deferred refinements that originally lived here
has been folded into the feature roadmap as
[Phase R in `2026-05-04_feature_roadmap.md`][roadmap-r] so all
open items live in one place.

The originally-deferred items (12c' backdrop filter, 13'
compositor handoff, linear-light blending) all activated 2026-05-05;
their canonical entries now live on the roadmap as **D1**, **D3**,
**R9** respectively. The activation-history record is preserved in
[`archive/2026-05-05_deferred_phases.md`](archive/2026-05-05_deferred_phases.md).
The path (b′) design for D3 lives in
[`2026-05-05_compositor_handoff_path_b_prime.md`](2026-05-05_compositor_handoff_path_b_prime.md).

When a wart fix from Phase R lands, record it as a `§11.x —
CLEARED` finding here and remove it from the roadmap. When a
deferred-phase item lands (D1 / D3 / R9), do the same and update
the relevant follow-up plan.

[roadmap-r]: 2026-05-04_feature_roadmap.md

## 12. Phase mapping under this plan

Renumbered; "Phase X' " is the vello-path equivalent of the parent
plan's Phase X.

**Status legend:** ✅ delivered · 🚧 partial · ⏳ pending

- ✅ **Phase 0.5'**: crate split (`netrender` + `netrender_device`).
  Delivered before vello work began.
- ✅ **Phase 1'**: first-light + oracle smoke green. Three probes
  in `p1prime_vello_first_light` cleared the §11.6 runtime spikes;
  five p2 oracle PNGs round-trip byte-exactly through vello via
  `p1prime_oracle_regreen`. §11.7 captures the findings.
- ✅ **Phase 2'**: rect ingestion + transforms + axis-aligned clips.
  Receipts at `p2prime_vello_rects` (3 probes) and the re-greened
  `p3_transforms` (7 tests; 5 byte-exact, 2 with vello-captured
  oracles for rotation cases).
- ✅ **Phase 3'**: subsumed into Phase 2' — `transform_id` +
  `clip_rect` flow through the same translator.
- ✅ **Phase 4'**: depth / ordering. Vello handles painter order
  natively; the parent plan's depth pre-pass for opaques was
  dropped entirely. No receipt needed beyond what Phase 2'
  scenes already cover.
- ✅ **Phase 5'**: image primitives, all three sub-phases:
  - 5a image translator (full UV, alpha tint)
  - 5b chromatic tints via Mix::Multiply + SrcAtop
  - 5c Path B `register_texture` for GPU-resident image sources
- ✅ **Phase 6'**: render-task graph (delivered before vello).
  Vello does NOT slot in as a per-tile rasterization task as the
  pre-spike draft envisioned; the bridge is `insert_image_vello`
  for graph outputs to feed into vello scenes (see §11.8). Drop-
  shadow receipt (`p6_02`) re-greened through `render_vello`.
- ✅ **Phase 7'**: picture caching via Masonry pattern. See §11.8
  for full findings + cleanup outcome. The §2.2 `Rasterizer`
  trait was dropped in favor of direct `VelloTileRasterizer`
  ownership on `Renderer`.
- ✅ **Phase 8'**: gradients. `p8prime_vello_gradients` covers
  linear / circular-radial / elliptical-radial / conic with
  N-stop ramps.
- ✅ **Phase 9'**: path-shaped clips.
  - **9a' rounded-rect clips** delivered (2026-05-04): every Scene
    primitive carries `clip_corner_radii: [f32; 4]` in addition to
    `clip_rect`; non-zero radii produce a `kurbo::RoundedRect`
    clip via vello `push_layer`. Receipt at
    `p9prime_rounded_clip` covers rect / image / gradient.
  - **9b' arbitrary BezPath clips** delivered via the layer
    mechanism (§11.14): `SceneClip::Path(ScenePath)` inside a
    `SceneLayer` opens a vello layer with the path as its clip
    shape. Receipt at `p9b_01_path_clip_layer_culls_outside_path`.
- ✅ **Phase 10'**: text via `Scene::draw_glyphs` + skrifa. Layout
  stays embedder-side per §4.4. Two slices delivered:
  - 10a' Scene API plumbing — `FontBlob`, `Glyph`,
    `SceneGlyphRun`, `Scene::push_font` + `push_glyph_run`,
    translator `emit_glyph_run`, tile-cache hash + AABB filter.
    Receipt: `p10prime_a_glyph_api` (5 data-structure probes).
  - 10b' real-font GPU smoke. Loads Arial (or DejaVu / Liberation
    on non-Windows) from a system font path, registers it,
    renders 5 glyphs at 32px through `render_vello`, reads back
    and verifies non-zero painted pixels. Skipped vacuously if
    no known system font path exists. Receipt:
    `p10prime_b_glyph_render`.

  Netrender doesn't bundle a font (license / repo-size tradeoff);
  consumers needing deterministic CI text rendering bundle a
  permissive TTF (Roboto, Inter, etc.) under `tests/data/` per
  their own discretion. Layout (shaping, BiDi, line breaking,
  font fallback) stays embedder-side: Servo lowers via its
  existing `gfx` + harfrust + inline-layout stack; embedders
  without an existing layout layer are pointed at parley.
- ✅ **Phase 11'**: borders / box shadows / line decorations.
  Three slices delivered:
  - 11a' rect / rounded-rect strokes (`SceneStroke` +
    `push_stroke_*` helpers, vello-native `Scene::stroke`).
    Receipt: `p11prime_a_strokes`.
  - 11b' arbitrary path fills + strokes (`SceneShape` with
    `ScenePath` of `PathOp` enum, optional `fill_color` + optional
    `stroke`). Receipt: `p11prime_b_paths`.
  - 11c' box-shadow ergonomic helper
    (`Renderer::build_box_shadow_mask`). Wraps the
    `cs_clip_rectangle` mask + separable `brush_blur` render-graph
    chain into a single call; caller composites the resulting
    `ImageKey` via `push_image_full_rounded` with a tinted alpha.
    Receipt: `p11prime_c_box_shadow`. The render-graph filter
    callbacks (`clip_rectangle_callback`, `blur_pass_callback`,
    `make_bilinear_sampler`) were promoted from
    `tests/common/mod.rs` to the public `netrender::filter`
    module to support this helper.
- 🚧 **Phase 12'**: compositing correctness. Core shipped; carve-outs
  tracked as roadmap items.
  - **12a' scene-level alpha + blend mode** ✅ delivered (2026-05-04):
    `Scene.root_alpha` and `Scene.root_blend_mode: SceneBlendMode`
    fields apply a single outer `push_layer` wrap. Maps to vello's
    `BlendMode { mix, compose: SrcOver }` with mix variants Normal
    / Multiply / Screen / Overlay / Darken / Lighten. No outer
    layer added when at defaults. Receipt:
    `p12prime_a_scene_compositing` (4 probes).
  - **12b' nested groups** ✅ delivered (2026-05-04) via the op-list
    refactor (§11.11) + `SceneOp::PushLayer`/`PopLayer` (§11.14).
    `SceneLayer` carries `{ clip, alpha, blend_mode, transform_id }`;
    layers nest. Op-list painter order is consumer push order, so
    "render this stack of primitives at 50% alpha as a unit" is
    just a push/pop pair. Receipt: `p12b_nested_layers` (4 tests).
  - **12c' backdrop filters** ⏳ deferred. Roadmap entry: **D1**.
    Reads pixels under the element; vello's render-to-texture
    always overwrites the entire target. Multi-pass rendering
    (snapshot under-pixels → filter → composite over) is the shape
    of the fix. Architectural change; lift on consumer pull.
  - **Filter chains** (drop-shadow, brightness, etc.) beyond the
    existing `Renderer::build_box_shadow_mask` (11c'). The
    render-graph + insert_image_vello pattern handles one-off
    filters today; ergonomic helpers land per consumer demand.
  - **Linear-light blending** (§6.3, Pitfall #2) ⏳ upstream-blocked.
    Roadmap entry: **R9** (with R9-canary as the trigger detector).
    `mix-blend-mode: linear-light` and linear-light gradient
    interpolation depend on vello's GPU compute path honoring
    `peniko::Gradient::interpolation_cs`. Tracked at `p1prime_03`;
    not ours to work around.
- 🚧 **Phase 13'**: native-compositor handoff (axiom 14) via path (b′).
  Sub-phases 5.1–5.4 shipped on netrender's side (commit
  `9447a852b`); 5.5 (servo-wgpu adapter) pending in separate
  workspace. Recovers most of the trivial-handoff loss flagged at
  §2.4 without forking. Roadmap entry: **D3**. Full design:
  [`2026-05-05_compositor_handoff_path_b_prime.md`](2026-05-05_compositor_handoff_path_b_prime.md).
  The §2.4 v1.5 fallback (whole-frame vello + post-render tile
  slicing) has been superseded; see §2.4 banner.

**What's left in the phase mapping (post-§11.17, 2026-05-06):**

- **Phase 12c'** (backdrop filters → roadmap D1): architectural
  change, gated on consumer pull. Modern frosted-glass nav bars hit
  this; static content doesn't.
- **Phase 13'** (native-compositor handoff → roadmap D3): netrender
  side complete; servo-wgpu adapter (5.5) is the remaining work,
  out-of-repo.
- **Linear-light blending** (→ roadmap R9): upstream-blocked on
  vello's GPU compute path; R9-canary will fire when vello honors
  the field.

Each of these has its trigger, design shape, and done condition
recorded on [`2026-05-04_feature_roadmap.md`](2026-05-04_feature_roadmap.md);
the activation-history record is in
[`archive/2026-05-05_deferred_phases.md`](archive/2026-05-05_deferred_phases.md).

Everything else from 0.5'–12b' has shipped with receipts.
13' is netrender-complete (5.1–5.4); 5.5 lives in servo-wgpu.

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

   *Update 2026-05-06 (path b′ partial recovery).* Cross-frame
   GPU-work skipping is now restored at *surface* granularity
   (consumer-declared compositor surfaces skip their blit when
   clean), via
   [`2026-05-05_compositor_handoff_path_b_prime.md`](2026-05-05_compositor_handoff_path_b_prime.md)
   sub-phases 5.1–5.4 (shipped, commit `9447a852b`). Vello itself
   still re-runs the unioned encoding every frame — that part is
   unchanged and remains upstream-blocked. Net: tile-grid skipping
   still missing; surface-grid skipping recovered.

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
