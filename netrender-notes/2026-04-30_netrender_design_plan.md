# Netrender Design Plan (2026-04-30)

**Status**: Active. Sole forward-looking plan; supersedes all prior
plans in `netrender-notes/`. The historical plans (pipeline-first
migration, idiomatic-wgsl pipeline, renderer-body adapter) document
the failed attempts to retrofit GL-era webrender; they survive as
record, not as direction.

**Premise**: Build a wgpu-native 2D renderer that ingests display
lists and produces pixels. No GL. No GL-era assumptions. No
retrofitting. Every type, every cache, every trait surface decided
fresh against wgpu's actual model.

---

## 1. Goals

- Render display lists as fast and as correctly as production-era
  webrender did, on wgpu, on the hardware servo-wgpu targets
  (Vulkan / Metal / DX12 / WebGPU; software fallbacks via Lavapipe /
  WARP / SwiftShader).
- Architecture that makes wgpu's actual shape (Send+Sync device,
  Arc-cloneable handles, scoped passes, no global state) the *path
  of least resistance*, not a thing that has to be defended.
- A consumer-ready surface: hand in a `wgpu::Device`/`Queue`,
  configure a `wgpu::Surface`, feed display lists, get pixels.
  Servo-wgpu, Graphshell, anything else.

## 2. Non-goals

- API compatibility with upstream webrender. We keep the display
  list types (lifted from `webrender_api`) but `Renderer` /
  `Frame` / everything else is fresh.
- Multi-process / IPC support beyond what the embedder's own
  threading model demands.
- Anything Gecko-specific. The `gecko` cargo feature, glean
  telemetry, FFI shims — gone, not replicated.
- swgl-style CPU rasterizer. Software fallback is wgpu's own
  software-adapter path (Lavapipe / WARP / SwiftShader),
  embedder-selectable.

## 3. Architectural axioms

These are the rules. If a future decision violates one of these,
the violation must be load-bearing and documented.

1. **Resources are wgpu handles.** `wgpu::Texture`, `wgpu::TextureView`,
   `wgpu::BindGroup`, `wgpu::Buffer`, `wgpu::Sampler`,
   `wgpu::RenderPipeline`. Cloneable Arc handles. Held directly by
   whatever needs them — no `CacheTextureId(u32)` indirection,
   no late binding, no resolution phase.
2. **Stable IDs are for bookkeeping, not for indirection.** When the
   renderer needs a hashable key (batch keys, debug logs, profiling
   spans), it uses stable IDs assigned at resource-creation time
   (`PipelineId`, `BindGroupId`, `TextureId`, `SamplerId`). The
   handle map lives next to the renderer state; IDs never escape
   into a "resolve-later" queue.
3. **No frame-builder/render-thread split unless profiling forces
   it.** wgpu `Device` is `Send+Sync`; `wgpu::Texture` is
   Arc-cloneable. The pre-D split (alloc-list pushed to render
   thread) was a GL artifact. Build single-threaded. Parallelize
   scene-tree build vs. frame-building only if a benchmark demands
   it.
4. **Surface acquisition is per-frame; target views are ephemeral.**
   Embedder configures `wgpu::Surface`, acquires `SurfaceTexture`
   per frame, hands `TextureView` to `Renderer::render(prepared,
   target)`. `PreparedFrame` is independent of the swapchain image
   — testable, snapshot-able, replay-able. **Surface format is
   pinned at Phase 1**, not deferred: oracle PNGs are tolerance-0
   from Phase 2, so the color-space contract has to be in place
   before the first golden is captured. See Phase 1.
5. **Pipelines compiled once, cached by `(family, format, override
   key)`.** WGSL `override` constants are the specialization
   mechanism (parent-plan §4.9). On-disk `wgpu::PipelineCache` for
   warm starts (parent-plan §4.11).
6. **Bind groups built against typed layouts.** Each shader family
   has a known `BindGroupLayout` shape; bind-group construction
   takes typed inputs (storage-buffer references, samplers, views)
   and produces a `BindGroup` ready to be bound. No runtime
   type-checking of bindings.
7. **Storage buffers, not data textures.** Per-primitive headers,
   transforms, render-task data, gpu-cache entries → storage
   buffers indexed by integer. Data textures (RGBAF32 2D as
   structured array) were the GL-driver-permissions workaround;
   wgpu storage buffers are the right shape and don't have row-
   width constraints. Portable
   `max_storage_buffer_binding_size` is ~128 MB, which gpu-cache
   scale eventually exceeds — design shader-side addressing as a
   chunked lookup (`Vec<wgpu::Buffer>` indexed by upper bits of
   the address) from the start, not as a Phase 7+ retrofit. (See
   §7 Q8.)
8. **Render pass is the unit of GPU work.** A pass has one target
   (color + optional depth), a load/store policy declared at begin,
   and a list of draws encoded into an active `RenderPass` scope.
   No "bind framebuffer / issue draws / unbind" pattern. Multiple
   passes per frame for multi-stage rendering (clip mask → main
   scene → compositor); each is its own scoped block.
9. **Frame arena for per-frame allocations.** Bump-allocate
   per-primitive temporaries into a frame-scoped allocator,
   mass-drop at frame end. Don't proliferate `Vec::new()` per
   primitive.
10. **Feature tiering is real.** Baseline wgpu device works for
    Phases 1–9. Optional features (`PUSH_CONSTANTS`,
    `DUAL_SOURCE_BLENDING`, `TIMESTAMP_QUERY`) gate specific
    upgrades; the renderer queries `wgpu::Features` at construction
    and selects pipeline variants accordingly. Boot does not
    require optional features.
11. **WGSL is authored, never translated.** No GLSL→WGSL pipeline.
    No glsl-to-cxx, no glsl-opt, no GLSL `#include` machinery.
    WGSL files in `netrender_device/src/shaders/` are the source
    of truth.
12. **Smoke tests don't define the ABI.** `brush_solid.wgsl` is a
    smoke test that proves the device path. Its primitive layout
    (`PrimitiveHeader`, `a_data: vec4<i32>`, `RenderTaskData`) is
    a GL-era contract and gets re-decided in Phase 2/3, before
    primitive layout calcifies.
13. **`render()` is upload-free.** All texture / buffer writes
    happen during `PreparedFrame` construction. By the time
    `Renderer::render(prepared, target)` runs, every resource the
    draws reference is already on-device. This keeps frame
    pacing predictable: first-visibility uploads (image cache,
    glyph atlas, render-task inputs) are bounded by the
    prepare-phase budget, not surfaced as render-phase spikes.
    The image cache's `get_or_load` is a *prepare-phase* call.
14. **Platform compositor handoff is a constraint, not a Phase 13
    adapter.** IOSurface (macOS), DXGI shared handles (Windows),
    and equivalent platform-handle paths impose texture-creation-
    time flag and feature requirements. Tile-cache textures
    (Phase 7), transient-pool textures (Phase 6), and any other
    texture that may be presented through a `NativeCompositor`
    must be allocated through code paths that *can* request the
    relevant wgpu external-memory features. Phase 13 picks a
    platform; Phases 5–7 reserve the seam. Trait shapes
    (`Compositor`, `NativeCompositor`) land at Phase 0.5 — empty
    bodies, no implementations — so every later phase has the
    surface to defer to. **Every texture-allocating subsystem
    declares its export class** — internal-only,
    compositor-exportable, or undecided — at allocation time.
    Internal-only: blur intermediates, transient-pool scratch
    targets that never leave the renderer. Compositor-exportable:
    tile-cache textures, any framebuffer-shaped output that may
    cross to a `NativeCompositor`. Undecided: phases that
    haven't picked tag explicitly and revisit before Phase 13.
    "We'll figure it out at Phase 13" was Phase D's failure mode
    at smaller scale; the tag forces the decision at the call
    site.
15. **netrender is an in-process renderer.** Multiprocess
    transport, serialization, and cross-process resource
    brokering are out of scope for the renderer core. Resource
    handles are `wgpu::Texture` / `wgpu::Buffer` /
    `wgpu::BindGroup` (axiom 1), not stable IDs designed for
    cross-process resolution; any token type that surfaces in a
    public API is an IPC contamination warning sign. If a future
    consumer needs multiprocess support, the orchestration layer
    (transactions, serialization, resource brokering) lives in
    the *consumer* — Servo's embedding code, Graphshell's host
    crate — not in any crate that ships alongside netrender.
    Otherwise `RenderBackend` reincarnates as
    `netrender_orchestration::FrameBroker` and Phase D's
    deferred-resolve / token-indirection wound reopens.
16. **External resources are local by the time they hit the
    renderer.** Decoded image bytes, glyph rasters, video / YUV
    planes, embedder-provided textures all reach netrender as
    in-process handles — `Arc<[u8]>` for byte buffers,
    `wgpu::Texture` for embedder-owned GPU resources. The
    consumer owns decoding, asynchronous fetch, and lifetime
    arbitration; netrender consumes already-resolved handles.
    Sync rule: an external `wgpu::Texture` handed to netrender
    must outlive any `PreparedFrame` that references it — the
    consumer holds the Arc until the frame's submitted command
    buffer completes. Phases 5 (image cache) and 10a (glyph
    atlas) are the first to exercise this contract; Phase 13
    (native compositor) is where it crosses platform handles.
    The whole point is to keep deferred-resolve / texture-update
    queues out of the renderer's data flow — same anti-pattern
    as the IPC trap (axiom 15) at smaller scale.

## 4. Crate structure

Two crates introduced at Phase 0.5; a third
(`netrender_compositor`) lands when a consumer needs platform
compositor adapters (see Phase 13):

```
netrender-workspace/
├── netrender_device/    -- foundation: WgpuDevice, pass encoding,
│                           pipeline + bind-group factories, buffer
│                           helpers, readback, WGSL shaders
├── netrender/           -- renderer: PreparedFrame, batches, render-
│                           task graph, picture cache, primitive
│                           pipelines, scene-tree → batch translation
└── (future) netrender_compositor/  -- platform compositor adapters,
                                       added when a consumer needs
                                       them
```

`netrender_device` has zero dependencies on `webrender_api`. It
deals in wgpu primitives + bytes + WGSL. Reusable by any consumer
that wants the device + WGSL pipeline pattern without the renderer.

`netrender` depends on `netrender_device` and on `webrender_api`
(for display list types — lifted from disk into the workspace).
Display list types are clean; the renderer-internal types are all
fresh.

The current `netrender/` crate becomes `netrender_device/` (it's
already shaped that way — `device::wgpu` is its only contents
plus a thin `Renderer` wrapper). The thin wrapper migrates to
`netrender/`.

**Crate-split rationale**: The boundary makes "no leaking renderer
guts into the foundation" enforced by package visibility, not by
author discipline. `netrender_device`'s public surface is
intentionally small — see Phase 0.5 for the curated list. The
implementation modules (`binding`, `buffer`, `format`, `frame`,
`pass` internals, `pipeline` internals, `readback`, `shader`,
`texture` internals) are `pub(crate)`. `netrender` consumes the
narrow public API to build frames — its types (`PreparedFrame`,
`Batch`, `RenderTaskGraph`) are private to it.

## 5. Phase plan

Each phase has a smallest-thing-that-works receipt. Each phase
that ships pixels has a golden test. **Don't move past a phase
without its golden.**

### Phase 0.5 — Crate split (1–3 days)

Split current `netrender/` into `netrender_device/` (foundation)
and `netrender/` (renderer shell). Move:

- `device/wgpu/*` → `netrender_device/src/` — **flatten the
  `device::wgpu` namespace.** Today's path is
  `device::wgpu::core::WgpuHandles`; post-split it becomes
  `netrender_device::core::WgpuHandles`. The `wgpu` segment was a
  sub-namespace inside a renderer crate that also held a GL
  device; with `netrender_device` *being* the wgpu crate, the
  segment is redundant.
- WGSL files → `netrender_device/src/shaders/`
- `Renderer` shell → `netrender/src/`
- **Curated public API** in `netrender_device` — not blanket
  `pub`. Public items: `WgpuDevice`, `WgpuHandles`,
  `REQUIRED_FEATURES`, `DrawIntent`, `RenderPassTarget`,
  `ColorAttachment`, `DepthAttachment`, the pipeline-factory
  entry points (e.g. `BrushSolidPipeline` and its constructor),
  and any types those signatures transitively expose. Everything
  else (`binding`, `buffer`, `format`, `frame`, `pass`'s
  `flush_pass`, `pipeline`'s build helpers, `readback`, `shader`,
  `texture`'s helpers, `core` internals beyond `WgpuHandles`)
  stays `pub(crate)`. The boundary is the point of the split;
  defending it is what makes the foundation re-usable instead of
  a grab-bag.
- **Compositor trait shapes** in `netrender` (or a stub
  `netrender_compositor` if it's clear the third crate is
  coming): `trait Compositor` and `trait NativeCompositor` with
  empty bodies and doc-comments describing the contract. No
  implementations. This reserves the seam axiom 14 calls out.
- `tests/angle_shader_validation.rs` — **delete.** It's GL-era
  bit-rot (`extern crate webrender; webrender_build; mozangle` —
  none survive Phase D); it cannot compile against the current
  workspace and cannot be rescued without resurrecting deleted
  crates. Today this test is why `cargo test -p netrender
  --no-run` fails outright; deleting it is what makes the Phase
  0.5 receipt below achievable.
- **Demote `REQUIRED_FEATURES`.** Today
  [core.rs](../../netrender/src/device/wgpu/core.rs) hard-requires
  `IMMEDIATES.union(DUAL_SOURCE_BLENDING)`. `IMMEDIATES` is unused
  — `brush_solid`'s pipeline declares `immediate_size: 0`. Drop
  it. `DUAL_SOURCE_BLENDING` is only needed for the Phase 10
  subpixel-AA pipeline; move the check from boot-time
  `with_external` into the Phase 10 pipeline factory. Post-demote,
  `REQUIRED_FEATURES = wgpu::Features::empty()` — matching axiom
  10's baseline-portability claim. Without this demotion the
  goals statement (line 18-21) is false: today's renderer rejects
  baseline adapters that could run Phases 1–9 cleanly.
- **Preserve the existing oracle corpus.** `tests/oracle/`
  carries five PNG/YAML pairs (`blank`, `rotated_line`,
  `fractional_radii`, `indirect_rotate`,
  `linear_aligned_border_radius`) captured 2026-04-28 from
  `upstream/0.68` GL with full provenance — see
  [tests/oracle/README.md](../../netrender/tests/oracle/README.md).
  Move the directory to `netrender/tests/oracle/` (renderer-side,
  since goldens are scene-level). Don't re-capture; don't treat
  them as missing assets. Phase 2 decides per-scene which survive
  the new primitive ABI.
- `netrender/doc/` (CLIPPING_AND_POSITIONING.md, coordinate-spaces,
  text-rendering, swizzling, blob) and `netrender/res/`
  (`Proggy.ttf`, `area-lut.tga`) — **leave on disk.**
  `area-lut.tga` is load-bearing for box-shadow (Phase 11);
  `Proggy.ttf` for text (Phase 10); the docs are reference
  material. Decide their final home (`netrender_device` vs
  `netrender`) at the phase that actually consumes each.

**Receipt**: `cargo test -p netrender_device --no-run` and
`cargo test -p netrender --no-run` both succeed (not just `cargo
check` — the package is currently not test-buildable because of
the `angle_shader_validation.rs` rot, and the receipt has to
actually clear that). All 7 device-side tests from today's
`device/wgpu/tests.rs` still pass under their new
`netrender_device` paths. `REQUIRED_FEATURES` boots on a
no-optional-features wgpu adapter — verifiable by adapter-feature
introspection in `wgpu_device_a1_smoke`. Oracle corpus rehomed at
`netrender/tests/oracle/` with all five PNG/YAML pairs intact.

### Phase 1 — Surface ↔ skeleton handshake (3–7 days internal; embedder hookup separate)

Embedder hands in `wgpu::TextureView` per frame; we render into
it and return; embedder presents. Define `PreparedFrame { draws:
Vec<DrawIntent>, retained: ResourceRefs }` and `FrameTarget<'a> {
view: &'a TextureView, format, extent }`. Implement
`Renderer::render(prepared, target)` that begins a pass and flushes.

**Color-space pin**: surface format is `Rgba8UnormSrgb`. The
device sRGB-encodes on store; oracle PNGs are captured from
`Rgba8UnormSrgb` framebuffers and compared as sRGB-encoded bytes.
This is the contract the goldens lock in — Phase 2's tolerance-0
diff would otherwise silently bake whichever color space happens
to fall out of the smoke test.

When Phase 7+ introduces a linear `Rgba16Float` intermediate plus
a composite pass, the composite terminates at the same
`Rgba8UnormSrgb` surface format. Goldens captured at Phase 2 stay
valid through that change. Acceptable wart of this Phase 1
choice: blend / gradient math runs in sRGB-encoded space until
the linear intermediate lands, so early color blending is
mathematically wrong-but-consistent. That's the tradeoff for
unblocking the harness now.

**Deletes**: today's `Renderer` carries
`wgpu_render_targets: HashMap<(u32, u32, TextureFormat),
wgpu::Texture>` plus `ensure_wgpu_render_target` and
`read_wgpu_render_target_rgba8`. Zero in-tree callers
(`oracle_blank_smoke` bypasses `Renderer` and uses
`WgpuDevice::boot()` directly). The embedder owns surface
textures in the post-D model — drop the cache and both methods at
this phase.

**Receipt (internal smoke)**: hardcoded test scene (one solid
rect, full extent, red on transparent) renders into a 256×256
**offscreen `wgpu::Texture`** (`device.create_texture` with
`RENDER_ATTACHMENT | COPY_SRC`, the same shape `oracle_blank_smoke`
already uses). Readback matches `oracle/p1_solid_rect.png`. The
target is a caller-supplied `TextureView` — no swapchain in this
receipt. Headless on Lavapipe / WARP / SwiftShader. ~3–7 days.

**Receipt (first embedder hookup)**: servo-wgpu or graphshell
acquires a real `SurfaceTexture` from `wgpu::Surface` and
presents the rendered view through `Renderer::render` against
that surface's `TextureView`. Separate scope from the headless
smoke; the offscreen receipt is what proves the renderer; the
swapchain receipt only proves the embedder integration. Estimate
independently when the consumer is ready.

### Phase 2 — Display list ingestion (rects-only) (1–2 weeks)

Lift `webrender_api` into the workspace. The crate already lives
on disk at `webrender_api/` — Phase D left it there but excluded
it from `[workspace] members`. The "lift" is a `Cargo.toml`
change (add `"webrender_api"` to `members`), not a code move.
Same pattern applies to `wr_glyph_rasterizer` at Phase 10.

Author `Scene`, `PrimitiveStore`, `BatchBuilder`. Walk
`BuiltDisplayList` → solid-rect primitives → batch (single batch,
single pipeline) → DrawIntents. Author golden harness: scene YAML
→ render → PNG diff. Land 5–10 rect-only golden scenes.

**Harness format**: write a fresh minimal YAML schema scoped to
the primitives we actually support (rects in Phase 2, then
extended per family). Don't lift wrench's reader — it carries
upstream's full display-list vocabulary, much of which won't
exist in our renderer for months. Authoring a small parser is
~1–2 days; lifting wrench is a multi-week side-quest.

**Inherited oracles** (preserved at Phase 0.5; promoted here):
the corpus is five frozen PNG/YAML pairs captured 2026-04-28 from
`upstream/0.68` GL — `blank` (already wired), `rotated_line`,
`fractional_radii`, `indirect_rotate`,
`linear_aligned_border_radius`. They're not assets to capture;
they're references we already own. Per-scene Phase 2 work: render
through netrender, diff against the frozen PNG, and either (a)
promote if pixel-equal within tolerance, or (b) demote to
"reference shifted under the new primitive ABI" with a written
note explaining what changed. The corpus's `README.md` documents
re-capture procedure if a scene needs a fresh oracle.

**Re-decide primitive layout here.** The smoke-test brush_solid's
`PrimitiveHeader` was GL-shaped; the post-D form is whatever the
new batch builder actually needs. Likely simpler: per-instance
`{ rect: vec4<f32>, transform_id: u32, color_addr: u32, ... }`
in a per-batch storage buffer; no `picture_task_address`
indirection, no `specific_prim_address` lookup chain.

**Receipt**: 10 rect-only YAML scenes pixel-match captured PNGs
through the netrender pipeline.

### Phase 3 — Transforms + spatial tree + axis-aligned clips (1–2 weeks)

Lift `space.rs`, `spatial_tree.rs`, `transform.rs` math from old
webrender (just the algorithms, not the file). Resolve
display-list transforms → per-primitive matrices. Pass through
the new `Transform` storage buffer. Add axis-aligned clip
rectangles in device space.

**Receipt**: scene with one transform chain (translate + rotate +
scale) + one axis-aligned clip rectangle pixel-matches reference.

### Phase 4 — Batching + depth (2–3 weeks)

This is bigger than it sounds. New work:
- Batch keys: `(PipelineId, BindGroupId, BlendMode, DepthState)`
- Sort opaques front-to-back, alphas back-to-front
- Z-buffer in main pass (depth attachment, format selection,
  write/test variants)
- Pipeline cache shape: `(family, target_format, depth_format,
  blend, write_mask)` keys
- Alpha pass uses depth-test-only (no depth-write)
- `RenderPassTarget` extended to carry depth-attachment info

**Receipt**: 100-overlapping-rect scene with mixed opacity renders
correctly; opaque early-Z visible in profile (fragment count drops
vs. no-Z baseline).

### Phase 5 — Image primitives + image cache (1–2 weeks)

WGSL `brush_image.wgsl`. Image cache:
`HashMap<ImageKey, Arc<wgpu::Texture>>` populated by
`image_cache.get_or_load(key)`. No alloc-queue, no
`TextureUpdateList`. Bind group includes the sampled image view
+ a sampler from a sampler cache.

`get_or_load` is a **prepare-phase call** (axiom 13), not a
render-phase call. It runs during `PreparedFrame` construction:
staging-buffer fill, `queue.write_texture`, and any required
state churn happen there, bounded by the prepare-phase budget.
By the time `Renderer::render(prepared, target)` runs, every
image the draws reference is already on-device. This is what
makes "first visibility" not equal "frame spike."

Image bytes come from the embedder (decoded by them, maybe
asynchronously, delivered as `Arc<[u8]>` to us). Decoding is
upstream. Same prepare-phase contract applies later to the
glyph atlas (Phase 10) and render-task input textures (Phase 6).

**Compositor seam reservation** (axiom 14): cached image
textures may eventually be presented through a `NativeCompositor`
on platforms where direct sampling avoids a final blit. Allocate
through a code path that can request platform-handle import
features when those wgpu features are available; the default
path stays plain `device.create_texture`.

**Receipt**: image-rect scene with checkerboard image source
pixel-matches reference.

### Phase 6 — Render-task graph (2–4 weeks)

Author the graph type fresh. The graph allocates each task's
output target up front from the transient pool (extent + format
keyed reuse) and passes both inputs and the pre-allocated output
into the encode callback:

```rust
struct Task {
    id: TaskId,
    extent: Extent3d,
    format: TextureFormat,
    inputs: Vec<TaskId>,
    encode: fn(&mut CommandEncoder, inputs: &[&TextureView], output: &TextureView),
}
```

Topo-sort, walk, allocate outputs, encode passes in dependency
order. Output ownership stays with the graph / pool — encode
doesn't conjure or return a `TextureView`. Reuse-keyed allocation
is only correct when the pool, not the closure, decides what
gets reclaimed.

Transient pool comes online at this phase. Per-task
`device.create_texture` is the first cut; pool follows once Phase
6's bench shows churn. Pool allocations honor axiom 14: leave
room for platform-handle import flags on tile-cache-bound
formats; the default-path output for an internal blur target
doesn't need them.

**Receipt**: drop-shadow scene (blur-then-sample) pixel-matches
reference within tolerance.

**Status (2026-04-30, delivered)**: `brush_blur.wgsl` (5-tap separable
Gaussian, fullscreen quad VS, `BlurParams { step: vec2<f32> }`
uniform), `RenderGraph { push, execute }` with Kahn's topo-sort and
single-`CommandEncoder` execution, and `ImageCache::insert_gpu`
bridging graph outputs into the scene-compositing path. Receipts
`p6_01` (uniform source invariant under blur, ±2/255) and `p6_02`
(drop-shadow golden) green. Transient pool deferred per the "first
cut = `device.create_texture`" plan in this section. The encode
callback uses `Box<dyn FnOnce>` (closure capture) rather than the
spec's bare `fn` because blur passes need to capture pipeline,
sampler, and step uniform — a function pointer can't carry that
state.

### Phase 7 — Picture caching + tile invalidation (2–4 weeks)

Lift `tile_cache.rs`'s *invalidation* algorithm — frame-stamp
dirty tracking, retain heuristic, dependency tracking. Storage
is `Vec<Arc<wgpu::Texture>>` per tile, GC drops Arcs (wgpu
reference-counts the GPU memory).

Tile metadata preserved per cache entry (texture, device rect,
dirty rect, opacity, transform, clip, z-order) — even if Phase 7
only uses some, Phase 13 wants the rest.

**Receipt**: scrolling test — unchanged frame reuses 100% of
tiles; small scroll only renders newly-exposed strips. Tile
re-render count proportional to scroll delta, not viewport size.

**Implementation plan (2026-04-30)**:

*Sub-phase ladder.* Three slices land in order; the full receipt
closes when all three are green:

- **7A — Invalidation algorithm.** `TileCache` data structure +
  per-tile dependency hash + frame-stamp tracking. No rendering
  integration. Algorithm-level test: dirty count is 0 on identical
  re-prepare; proportional to scroll delta on translated re-prepare.
- **7B — Per-tile rendering.** Dirty tiles render their intersecting
  primitives into `Arc<wgpu::Texture>` via a tile-local orthographic
  projection through the existing `brush_rect_solid` /
  `brush_image` pipelines. No framebuffer compositing yet — tile
  textures exist but aren't sampled.
- **7C — Composite integration.** `prepare()` routes through the
  tile cache when enabled; `PreparedFrame.draws` becomes one
  `brush_image_alpha` draw per tile sampling its cached texture.
  Receipt: `p7` scrolling scene pixel-matches a non-tiled equivalent
  (±2/255) AND re-render count proportional to scroll delta.

*Why staged.* 7A is a pure algorithm and risk-free. 7B exercises
per-tile pipeline reuse with no user-visible change. 7C flips
`prepare()`'s default path — the riskiest edit. Splitting them lets
each receipt verify exactly one thing.

*Defaults.* `tile_size = 256` (configurable via `NetrenderOptions`).
Tile texture format `Rgba8Unorm` (linear); framebuffer stays
`Rgba8UnormSrgb`. Linear tile storage avoids the precision loss of
caching sRGB-encoded values and keeps end-to-end color math identical
to direct rendering (linear write → linear sample → sRGB encode on
framebuffer write).

*Tile-local projection.* For tile `(cx, cy)` with tile_size `T`:
`proj = ortho(world_x ∈ [cx·T, (cx+1)·T], world_y ∈ [cy·T, (cy+1)·T])`.
NDC clipping crops primitives crossing tile bounds; no CPU-side
clipping. Per-tile primitive filter: AABB intersection between
prim.rect (transformed) and tile rect.

*Dependency hash.* `DefaultHasher` (SipHash) over per-prim state
(rect, color, transform_id, clip_rect; +uv, +key for images) of every
primitive intersecting the tile, in painter order. Move / add /
remove flips the affected tiles' hashes; static prims don't.

*Frame-stamp retain.* `current_frame: u64` ticks per `prepare()`;
`tile.last_seen_frame` more than N frames stale → tile evicted (Arc
dropped, wgpu reclaims the GPU memory). Default N = 4.

*Known divergences from direct render.*

- Sub-pixel edge rasterization may differ by ≤1 px on rotated rects
  (axis-aligned rects: exact). 7C tolerance ±2/255 covers this.
- `ImageData` byte changes with stable `ImageKey` are not detected
  by the tile hash — Phase 7 limitation; Phase 8+ may add a
  content-hash track.

*Deferred.* Tile metadata Phase 13 wants (transform / clip / z-order
/ opacity) — Phase 7 stores `world_rect` only, full struct shape lands
when Phase 13 surfaces a concrete consumer. Transient pool: tile
textures are inherently pooled by the cache itself, so the per-task
allocator from Phase 6 doesn't apply here. `prepare_with_tile_cache`
opt-in: only exists during the 7A→7C transition; always-on after 7C.

**Status (2026-04-30, delivered)**: 7A/7B/7C all green.

- 7A: `TileCache` invalidation (`netrender::tile_cache`). 6 tests in
  `p7_tile_cache.rs` covering identical-scene zero-dirty,
  translation-localizes-to-two-tiles, dirty-count independent of
  viewport size, empty-scene stability, color-change locality,
  add/remove locality.
- 7B: `Renderer::render_dirty_tiles(scene, &mut tile_cache)`. Each
  dirty tile renders into an `Arc<wgpu::Texture>` via the existing
  `brush_rect_solid` / `brush_image` pipelines and a tile-local
  orthographic projection. Shared `Depth32Float` texture and shared
  transforms buffer across tiles; per-tile per-frame uniform. NDC
  clipping handles per-tile cropping (per-tile primitive filtering
  is a 7C+ optimization). 4 tests in `p7b_tile_render.rs`.
- 7C: `NetrenderOptions::tile_cache_size = Some(N)` opts in;
  `prepare()` then routes through `prepare_tiled` which invalidates,
  re-renders dirty tiles, and emits one `brush_image_alpha` composite
  draw per cached tile (full-viewport projection, identity transform,
  z=0.5, no tint). 3 tests in `p7c_tile_composite.rs` covering pixel
  equivalence vs. the direct path (no diff at all on the test scene
  — sub-pixel rounding stayed within the ±2/255 budget by luck;
  rotated rects may consume more of it later), unchanged-frame
  zero-dirty, and dirty count proportional to scroll delta across
  three viewport sizes.

The opt-in escape hatch (`tile_cache_size` defaulting to `None`)
remains the API; flipping the default to always-on is a separate
decision tied to Phase 9+ when clip masks consume tile textures.

*Review notes (2026-04-30, post-delivery):*

- **80-byte `ImageInstance` encoding is duplicated.** `batch.rs::emit_image_draws`
  (user images) and `renderer/mod.rs::build_tile_composite_draw` (tile
  composites) both serialize the same struct layout by hand. The next
  pipeline family that consumes `ImageInstance` (image-repeat in Phase 8,
  YUV later) will be the third copy; factor into a `pub(crate)` writer
  before then.
- **`transforms_buf` is built twice per tiled `prepare()`.** Once in
  `render_dirty_tiles` (tile rendering), once in `prepare_tiled`
  (composite). They are identical for a given scene; reuse the rendering
  path's buffer in the composite path.
- **No per-tile primitive filtering yet.** Every dirty tile renders the
  full `scene.rects` + `scene.images` lists, relying on NDC clipping to
  crop. Receipt-correct but wastes vertex shader work proportional to
  `dirty_tiles × scene_prims`. Easy follow-up: pre-bucket prim indices
  by tile coord before the render loop.
- **`TileCache.tiles` is `pub(crate)`.** `Renderer` reaches in directly
  to set `tile.texture` and read `tile.world_rect` from the dirty list.
  Works fine in a single crate, but accessor methods (`iter_dirty_mut`,
  `dirty_world_rects`) would localize the layout knowledge if the
  renderer ever moves to a separate crate.
- **Pixel equivalence on the test scene was bit-exact** (zero channels
  diverged on `p7c_01`), not merely within ±2. The tolerance budget is
  unused on axis-aligned rects with primary + premultiplied-half colors;
  it is reserved for transformed primitives and gradient sampling
  scenarios that Phase 8+ will exercise.
- **Test gaps acceptable for now:** pixel equivalence with images in
  the scene, tile eviction past `RETAIN_FRAMES`, tile size larger than
  viewport, sub-tile-pixel translations. None block Phase 8; queue as
  cleanup after the next family lands.

### Phase 8 — Shader family expansion (~1 week each post-harness)

Each family: WGSL file + pipeline factory + primitive-layout
extension + golden scene. Override-specialized variants where
parameter-only.

Order is gated by upstream-phase dependencies — the families are
not end-to-end parallelizable:

- `brush_blend`, `brush_mix_blend`, `brush_opacity` — pure
  shader and batch work; no upstream gate beyond Phase 4
  batching. Land these first.
- `brush_image_repeat`, `brush_yuv_image` — gate on Phase 5
  (image cache + sampler cache).
- `brush_linear_gradient`, `brush_radial_gradient`,
  `brush_conic_gradient` — fidelity-correct implementations
  rasterize the gradient ramp into an intermediate target and
  sample, gating on Phase 6 (render-task graph). Simpler analytic
  versions can land sooner without the graph; pick per family
  based on fidelity vs. budget.

**Receipt per family**: golden scene pixel-matches.

**Implementation plan (2026-04-30)**:

*First family deviation: gradient before blend/opacity.* The plan
lists `brush_blend` / `brush_mix_blend` / `brush_opacity` as the first
slice ("no upstream gate beyond Phase 4 batching"). In practice, all
three of those families do something meaningful only over a *picture*
(a primitive group rendered to an off-screen target) — without picture
grouping (a Phase 11+ concept), `brush_opacity` is functionally
identical to a rect with `color.a < 1.0`, and `brush_mix_blend` needs
backdrop access we haven't built. Authoring those WGSL files now would
land a pipeline that's redundant with `brush_rect_solid_alpha` until
pictures arrive.

`brush_linear_gradient` is in the *next* slice in the plan ("gates on
Phase 6"), but its analytic form is explicitly called out as
land-anywhere ("simpler analytic versions can land sooner without the
graph"). Phase 6 IS done. The analytic 2-stop linear gradient is a
genuinely new visible primitive — not a pipeline that duplicates an
existing one — so it's the right first Phase 8 family.

The `brush_blend` / `brush_mix_blend` / `brush_opacity` trio gets
deferred to the same phase that introduces pictures (Phase 11 surface
splitting). When that lands, these three become the natural first
families to wire over the picture mechanism.

*Sub-phase ladder.* Each family is its own slice:

- **8A — `brush_linear_gradient` (2-stop, analytic).** WGSL file,
  pipeline factory (depth + alpha variants), `SceneGradient` primitive,
  `Scene::push_linear_gradient` API, batch builder. Receipt: horizontal
  red-to-blue gradient golden, ±2/255.
- **8B — `brush_radial_gradient` (analytic).** Same shape; fragment
  shader computes radial `t` from center + radius. Receipt: radial
  black-to-white golden.
- **8C — `brush_conic_gradient` (analytic).** Fragment shader computes
  angular `t` via `atan2`. Receipt: 4-color conic golden.
- **8D — N-stop ramp.** Generalize 2-stop to a variable-length
  `stops_buffer` (storage buffer) keyed by per-instance `stop_count` +
  `stop_offset`. Backwards-compatible: a 2-stop instance is one entry
  with offsets `[0, 1]`.

Blend/mix-blend/opacity are intentionally *not* in this ladder — see
above.

*`SceneGradient` 2-stop layout (96 bytes).* The struct packs `rect`
(16 bytes), `start_point: vec2` and `end_point: vec2` (16 bytes
combined), `color0` (16), `color1` (16), `clip_rect` (16),
`transform_id: u32`, `z_depth: f32`, and 8 bytes of padding. The
96-byte stride matches `ImageInstance` (80) plus the extra 16 for
the second color, sharing the `transform_id` / `z_depth` / padding
tail so the depth-sorting logic in `build_*_batch` ports unchanged.

*Per-tile filtering still deferred.* The Phase 7 review note about
unfiltered per-tile rendering applies here too: gradient primitives
will be drawn into every dirty tile they touch, NDC-clipped at the
rasterizer. Filter optimization lands when 8+ benchmarks justify it.

*Override specialization (deferred).* The plan calls for override
specialization for parameter-only variants (e.g. premultiplied vs.
straight alpha). 2-stop gradients have only two natural variants
(opaque-only vs. alpha-blend), already cleanly addressed by the
`(color_format, depth_format, alpha_blend)` cache key from Phase 4.
WGSL `override` constants come back when N-stop ramps land in 8D.

**Status (2026-04-30, 8A delivered)**: `brush_linear_gradient.wgsl`
(2-stop analytic, 96-byte instance, flat-interpolated colors +
clip + linearly-interpolated `t`), `BrushLinearGradientPipeline`
with depth + alpha variants cached on `(color_format, depth_format,
alpha_blend)`, `SceneGradient` primitive type,
`Scene::push_linear_gradient` (and a `_full` variant with explicit
transform + clip), `build_gradient_batch` in `batch.rs` (front-to-back
opaque sort, painter-order alpha), unified `n_total = n_rects +
n_images + n_gradients` z assignment so gradients paint in front of
both prior families. `merge_draw_order` extended to take three lists.
Tile cache (`render_dirty_tiles`) updated in lockstep so 7C +
gradients combine correctly. Receipt: `p8a_linear_gradient.rs`,
4 tests — programmatic pixel checks against an `srgb_encode`
reference (no golden files; bit-exact within ±2/255 from sRGB
rounding):

- `p8a_01` horizontal red→blue gradient at 5 columns
- `p8a_02` vertical alpha-fade against opaque-black backdrop
  (RGB carries the alpha signal; framebuffer alpha is always 255
  by the premultiplied blend equation)
- `p8a_03` t-clamp outside the gradient line
- `p8a_04` gradient over an underlying rect (depth-test integration)

Carry-forward from the Phase 7 review notes still applies — the
80-byte `ImageInstance` writer and the gradient's 96-byte instance
writer share the same tail layout (`transform_id` / `z_depth` /
padding) and would benefit from a common `pub(crate)` helper before
the next family lands.

**Status (2026-05-01, 8B delivered)**: `brush_radial_gradient.wgsl`
(2-stop analytic, 96-byte instance with `params: vec4 = (cx, cy, rx,
ry)` replacing linear's `line` field; per-fragment `t = clamp(length
((local_pos - center) / radii), 0, 1)`),
`BrushRadialGradientPipeline` with depth + alpha variants cached on
`(color_format, depth_format, alpha_blend)`, `SceneRadialGradient`
primitive, `Scene::push_radial_gradient` + `_full` API,
`build_radial_gradient_batch` in `batch.rs`. The bind-group layout
function was renamed `brush_linear_gradient_layout` →
`brush_gradient_layout` since linear and radial share the same
3-binding shape. `SceneGradient` was renamed to `SceneLinearGradient`
to make room for the radial sibling, and `Scene.gradients` →
`Scene.linear_gradients`. `merge_draw_order` now takes four lists
(rects, images, linear gradients, radial gradients) — within-frame
linear/radial interleave is *not* preserved (linear always paints
behind radial), documented as a Phase 8 limitation that 8D's unified
gradient list will fix. Tile cache (`render_dirty_tiles`) updated in
lockstep so 7C + radial gradients combine correctly. Receipt:
`p8b_radial_gradient.rs`, 4 tests — circular center-to-boundary mix,
outside-radius clamp to color1, elliptical-radii t equivalence on
both axes, and radial-paints-in-front-of-linear ordering.

**Status (2026-05-01, 8C delivered)**: `brush_conic_gradient.wgsl`
(2-stop analytic, same 96-byte instance + bind-group shape; `params:
vec4 = (cx, cy, start_angle, _pad)`; per-fragment `t = fract((atan2
(dy, dx) - start_angle) / 2π)`), `BrushConicGradientPipeline` with
depth + alpha variants, `SceneConicGradient` primitive,
`Scene::push_conic_gradient` + `_full` API,
`build_conic_gradient_batch` in `batch.rs`, plus the parallel changes
in `prepare_direct`, `render_dirty_tiles`, `merge_draw_order` (now
5 lists), and the unified `n_total` z range. Family painter ordering
across Phase 8: rects → images → linear → radial → conic; the
user-push-order interleaving limitation documented in 8B carries
forward to 8C and is on 8D's plate. Receipt: `p8c_conic_gradient.rs`,
4 tests — quarter-turn cardinal-direction samples, seam discontinuity
(color1 → color0 jump across `start_angle`), uniform-fill collapse
when `color0 == color1`, and conic-paints-in-front-of-radial.

**Phase 8 cleanup carry-forward.** The `pub(crate)` instance-writer
helper called out post-8A still hasn't landed. Three gradient batches
now hand-encode the same 96-byte struct — `rect`, `params`, two
colors, clip, trailing `(transform_id, z_depth, padding)` — with only
the 16-byte `params` slot differing. The cleanup choice is between
factoring an instance-writer helper as a standalone refactor before
any further family lands, or doing it as part of 8D when unifying
linear / radial / conic into a single primitive type with N-stop
ramps. The 8D-as-bundled-cleanup option is likely the better trade
since 8D will rewrite the structs anyway.

**8D implementation plan (2026-05-01)**: deliberate the unification
choices up front since the refactor touches every gradient surface.

*Pipeline specialization via WGSL `override`.* One
`brush_gradient.wgsl` file replaces the three family WGSLs;
`override GRADIENT_KIND: u32` selects per-pipeline behavior
(`0=Linear`, `1=Radial`, `2=Conic`). The pipeline cache key on
`WgpuDevice` extends to `(color_format, depth_format, alpha_blend,
GradientKind)`, yielding 6 cached pipelines per format combo. This
replaces the three separate `BrushLinearGradientPipeline` /
`BrushRadialGradientPipeline` / `BrushConicGradientPipeline` types
with a single `BrushGradientPipeline`.

*N-stop storage buffer.* Bind group grows from 3 to 4 slots; binding
3 is `array<Stop>` where `Stop = { color: vec4<f32>, offset: vec4<f32>
}` (32-byte stride; the `offset.x` carries the [0,1] position and the
remaining 12 bytes pad to vec4 alignment). One stops buffer per
frame, shared across all gradient draw calls; per-instance
`stops_offset: u32` + `stops_count: u32` index into it. Fragment
shader does a linear scan for the segment containing `t` and mixes
between adjacent stops; clamps to first/last for `t` outside the
valid range. 2-stop instances are one entry of `[(0.0, color0), (1.0,
color1)]` — bit-exact equivalent to the Phase 8A-C 2-stop math, so
existing receipts pass without modification.

*Instance struct shrinks 96 → 64 bytes.* Colors move out to the
stops buffer; the per-instance struct keeps `rect`, `params`, `clip`,
and a 16-byte tail of `transform_id` (u32), `z_depth` (f32),
`stops_offset` (u32), and `stops_count` (u32). The gradient batch
builder serializes this once instead of three times, collapsing the
post-8A duplication.

*Painter order across kinds preserved.* `Scene.linear_gradients` /
`radial_gradients` / `conic_gradients` collapse to one
`Scene.gradients: Vec<SceneGradient>`. The batch builder walks that
vec in painter order, grouping consecutive entries with the same
`(kind, alpha_class)` into a single `DrawIntent`. A push sequence of
linear → radial → linear emits three draws (linear, radial, linear)
that respect painter order. Phase 8A-C's family-grouped sort
(linear < radial < conic regardless of push order) is gone.

*Existing 2-stop API preserved.* `Scene::push_linear_gradient`,
`push_radial_gradient`, `push_conic_gradient` (and their `_full`
variants) keep their signatures and now build a 2-stop
`SceneGradient` internally. New `Scene::push_gradient(SceneGradient)`
exposes the general N-stop API. Existing `p8a` / `p8b` / `p8c` tests
pass unmodified.

*Receipt: `p8d_n_stop_gradient.rs`.*

- `p8d_01` 3-stop linear (red → green → blue) — pixel sampling
  along the gradient line matches mix between adjacent stops.
- `p8d_02` uneven offsets (e.g., `[0.0, 0.2, 0.8, 1.0]`) — sub-segment
  spans interpolate correctly.
- `p8d_03` painter order across kinds — radial pushed first, linear
  pushed second; linear paints in front (opposite of Phase 8A-C
  ordering).
- `p8d_04` general API via `push_gradient(SceneGradient { ... })`
  with a custom stops vec.

**Status (2026-05-01, 8D delivered)**: `brush_gradient.wgsl` (one
file replacing the three Phase 8A-C WGSLs; `override GRADIENT_KIND`
selects per-pipeline behavior; `sample_stops` does the N-stop
linear-scan mix in the fragment shader), `BrushGradientPipeline`
(replaces three typed pipelines), `GradientKind` enum,
`build_brush_gradient(...)` parameterized by kind, single
`brush_gradient` cache on `WgpuDevice` keyed by `(color_format,
depth_format, alpha_blend, kind)`, 4-binding `brush_gradient_layout`
adding the FRAGMENT-visible stops storage at binding 3,
`SceneGradient` + `GradientStop` (replaces three `Scene*Gradient`
types), `Scene.gradients` (replaces three typed Vecs),
`Scene::push_gradient` for the general API plus the existing
`push_linear_gradient` / `push_radial_gradient` /
`push_conic_gradient` (and `_full` variants) preserved as 2-stop
convenience methods, single `build_gradient_batch` (replaces three
batch builders) — walks `scene.gradients` in painter order, groups
consecutive same-`(kind, alpha)` entries into single draws,
preserves user push order across kinds. `merge_draw_order` collapses
back to 3 lists. Receipt: `p8d_n_stop_gradient.rs`, 4 tests —
3-stop linear at midpoints of two segments, uneven stop offsets
(0/0.2/0.8/1) sub-segment math, painter order preserved across
linear+radial push, and the general `push_gradient` API with a
4-stop radial. All Phase 8A-C receipts (`p8a` / `p8b` / `p8c`) pass
unmodified — the 2-stop convenience methods route through the unified
path bit-exactly. Three obsolete WGSL files
(`brush_linear_gradient.wgsl`, `brush_radial_gradient.wgsl`,
`brush_conic_gradient.wgsl`) deleted; one new `brush_gradient.wgsl`
added. The post-8A instance-writer-duplication carry-forward is
resolved by collapsing three near-identical builders into one.

### Phase 9 — Clip masks (rounded rects, complex clips) (2–3 weeks)

Render clip masks to off-screen R8 targets (uses Phase 6 graph),
sample in fragment shaders. WGSL `cs_clip_rectangle`,
`cs_clip_box_shadow`, `cs_clip_rectangle_fast_path`. The
clip-mask sampling shape is already drafted in `brush_solid.wgsl`'s
alpha-pass fragment.

**Receipt**: rounded-rect clip + box-shadow clip golden scenes.

**Implementation plan (2026-05-01)**: think through up front because
clip masks are the first phase that combines render-graph
intermediates with per-primitive sampling — and the first phase that
realistically motivates a transient texture pool.

*Sub-phase ladder.*

- **9A — Rounded-rect clip mask.** Single `cs_clip_rectangle.wgsl`
  fragment shader that writes a coverage value (R8) for one rounded
  rect with per-corner radii. Driven by a render-graph task that
  outputs an R8 texture sized to the clip's device-pixel bounds.
  Existing primitives (`brush_rect_solid`, `brush_image`,
  `brush_gradient`) get an optional `clip_mask` binding (texture +
  sampler) and a per-instance `clip_mask_uv: vec4<f32>` so each can
  multiply the sampled coverage into its output alpha.
- **9B — Box-shadow clip mask.** Adds `cs_clip_box_shadow.wgsl`
  (Gaussian-blurred rounded-rect coverage). Two-pass: rasterize the
  rounded rect into an intermediate R8 target, then run
  `brush_blur` H + V over it. Render graph composes the chain.
- **9C — Fast-path rectangular clip.** `cs_clip_rectangle_fast_path.wgsl`
  for axis-aligned non-rounded rects (no per-corner radii); cheaper
  shader, smaller output. Picked at scene-build time when corner
  radii are all zero.

*Primitive-side wiring.* Each existing primitive shader grows a
binding for the clip-mask texture + sampler and a per-instance UV
slot; when the slot is empty (no clip mask), the fragment shader
skips the multiply via an `override HAS_CLIP_MASK: bool` (or a
sentinel UV like `[NaN, NaN, NaN, NaN]`). The override path keeps
the no-clip case bit-exact with Phase 8D's fragments.

*Transient texture pool — comes online here.* Phase 6 deferred this
because per-task `device.create_texture` was sufficient for blur
intermediates that turned over once per drop-shadow scene. Phase 9
multiplies the count by every clipped primitive in a scene: a UI
with N rounded-rect cards generates N R8 mask textures per frame
unless we pool. The pool sits next to the render graph: keyed by
`(extent, format, usage)`, returns `Arc<wgpu::Texture>`; on Arc drop
the texture re-enters the free list (intercepted via a
`PooledTexture` newtype that wraps `Arc<wgpu::Texture>` and on its
`Drop` returns the Arc to a back-channel). This is the API delta
worth getting right at 9A — the rest of 9B / 9C reuse it.

The pool is platform-handle-import-aware (axiom 14): keep the
allocation path optional flags so tile-cache-bound formats can later
opt in to import flags without touching the pool's hot path.

*Render-task graph extension.* `Task::encode` already takes an
output `&wgpu::TextureView`; the graph allocates the texture before
the encode callback (Phase 6 §5). 9A adds: the graph pulls those
allocations from the transient pool when present, falling back to
`device.create_texture` when no pool is configured (so existing
Phase 6 callers — e.g., `RenderGraph::execute(externals)` — keep
working unchanged).

*Bind-group layout changes.* `brush_rect_solid`, `brush_image`, and
`brush_gradient` layouts each grow two bindings: one R8 texture
(filterable: false), one NonFiltering sampler. The instance struct
adds `clip_mask_uv: vec4<f32>` (16 bytes; total stride changes:
rect 64 → 80, image 80 → 96, gradient 64 → 80). Existing tests'
2-stop / no-clip paths set the UV to a sentinel and the override
constant skips the sample.

*Receipt — what the goldens look like.* `p9_01` rounded-rect clip
on a solid rect (golden, ±2/255). `p9_02` box-shadow clip
(rasterize + 2-pass blur via render graph, ±2/255). `p9_03`
rectangular fast-path on a transformed rect (verifies the shader
specialization picks correctly). `p9_04` clip mask + tile cache
interaction (clip-affected tile pixel-equivalent through the tile
path, since 7C composites over the same masks).

**Status (2026-05-01, 9A/9B/9C delivered)**: scope deviated from the
original implementation plan in two ways, both for pragmatic reasons.

*Deviation 1: no primitive-shader integration in 9A.* The plan called
for `brush_rect_solid`, `brush_image`, and `brush_gradient` to grow a
clip-mask binding + per-instance UV slot, gated by `override
HAS_CLIP_MASK`. The receipt instead leans on the existing
render-graph + image-cache + `brush_image` chain (Phase 6's blur
pattern): the mask renders to an `Rgba8Unorm` coverage texture, gets
inserted into the image cache via `insert_image_gpu`, and is drawn
as a tinted `brush_image`. This sidesteps the bind-group / instance-
struct churn across three primitive families. Per-primitive clip
masks (the proper integration) remain as a Phase 11+ item gated on
the picture-grouping work.

*Deviation 2: transient texture pool deferred again.* The plan
flagged 9A as the pool's natural landing spot. With the mask
generation path going through the image cache (which already
owns mask-shaped textures across frames via `Arc<wgpu::Texture>`),
the per-frame allocation pressure is smaller than the plan
anticipated. The render graph still uses per-task
`device.create_texture`. The pool stays deferred until either a
benchmark shows churn or a per-primitive mask flow lands.

*Delivered surface.*

- `cs_clip_rectangle.wgsl` (fullscreen-quad VS, rounded-rect SDF FS,
  `override HAS_ROUNDED_CORNERS` gates the 9C fast path),
  `ClipRectanglePipeline` + `build_clip_rectangle(format,
  has_rounded_corners)`, `WgpuDevice::ensure_clip_rectangle(format,
  has_rounded_corners)` cached on `(format, has_rounded_corners)`.
- 9A receipt `p9a_clip_rectangle.rs` (2 tests): SDF math at
  representative pixels, end-to-end mask-as-tinted-image composite.
- 9B receipt `p9b_box_shadow.rs` (2 tests): chain `cs_clip_rectangle
  → brush_blur (H) → brush_blur (V)` via `RenderGraph`, verify
  edge-softening + drop-shadow halo. No new shader — the chain is
  pure render-graph composition.
- 9C receipt `p9c_clip_fast_path.rs` (2 tests): hard-edged step
  output; pixel-match against the rounded variant at `radius = 0`
  (the fast path is purely an optimization).

*Phase 5 ordering limitation surfaced.* The drop-shadow composite
(p9b_02) wants the foreground rect on top of the shadow image, but
Phase 5's family ordering (rects → images) puts images in front of
rects regardless of push order. The test composites just the shadow
and asserts on its falloff — true rect-on-top-of-image ordering
needs Phase 11 picture grouping.

*Phase 5 image-routing limitation surfaced.* Image instance
classification routes by tint alpha alone (`tint.a >= 1.0` →
opaque/no-blend pipeline, otherwise alpha pipeline). A
fully-opaque tint applied to a *texture* with variable alpha (a
mask) gets routed through the no-blend pipeline and overwrites the
framebuffer — including its alpha channel — wherever the texture
sample is `(0,0,0,0)`. The 9A/9B receipts work around this by using
tint alpha `0.999` to force alpha-blend routing. A "force-alpha"
hint on `SceneImage`, or a peek at the bound texture's content
hash, are both reasonable Phase 11+ fixes.

### Phase 10a — Text (renderer-side: atlas + glyph quads)

Glyph atlas (Phase 5 pattern). Two text shaders:
- `ps_text_run.wgsl` — grayscale AA, baseline, always available.
- `ps_text_run_dual_source.wgsl` — subpixel AA, requires
  `DUAL_SOURCE_BLENDING`. The feature check moves to *this*
  pipeline factory (per Phase 0.5's `REQUIRED_FEATURES` demote);
  fallback to grayscale path when the feature is missing.

Glyph rasterization uses `wr_glyph_rasterizer` (lift from disk
into workspace; outputs CPU bitmaps, wgpu-portable). Atlas
churn / first-visibility uploads happen during `PreparedFrame`
construction (axioms 13, 16); never at render time.

**Shaping is upstream.** Text shaping (script analysis, font
selection, ligature substitution, line breaking, fallback
cascade) is the consumer's responsibility — Servo's embedding
code, Graphshell's host crate, or a shared `cosmic-text`-style
crate the consumer pulls in. netrender consumes shaped glyph
runs (glyph IDs + positions + font handles) and rasterizes them
into the atlas. This is where axioms 15 and 16 land for text:
shaping is a subsystem concern, rasterization-and-paint is a
renderer concern.

**Receipt**: text-run golden scene matches in both grayscale and
(where supported) dual-source variants. Test inputs are shaped
glyph runs authored directly into the harness — no shaping is
exercised through netrender.

### Phase 10b — Browser-grade text correctness

10a paints glyph quads. 10b confronts the gap between "glyphs
appear" and "this is browser-grade text." Each sub-area gets its
own golden:

- **Subpixel AA policy.** When does the dual-source path engage?
  Transform-aware: pure 2D translation = subpixel; rotation /
  non-axis-aligned scale = grayscale. A per-glyph decision the
  renderer makes from the run's transform, not a per-frame mode.
- **Glyph snapping.** Pixel-grid alignment under fractional
  zooms; transform-decomposition rules (translate-only snap vs.
  general-transform no-snap). Goldens at 100% / 125% / 150% zoom.
- **Atlas churn behavior.** Eviction policy when atlas fills:
  LRU with generation stamps. Receipt: scrolling-text scene that
  exceeds atlas size doesn't spike frame time on eviction.
- **Fallback-font behavior.** When the consumer's shaper falls
  back across fonts mid-run, atlas keys distinguish glyph-id-
  by-font; the renderer can't assume one font per run. Receipt:
  mixed-font run renders without glyph collisions in the atlas.

**Receipt per sub-area**: targeted golden scene; some receipts
require synthetic shaped runs that exercise the failure mode
without depending on a particular shaping crate.

### Phase 11 — Borders, line decorations, box shadows (3–6 weeks)

Each: shader family + cached decomposition. Lift `border.rs`,
`box_shadow.rs`, `ellipse.rs`, `line_dec.rs` algorithms (the
math, not the modules).

**Receipt**: border / line / box-shadow golden scenes.

### Phase 12 — Compositing correctness

Headline-primitive completeness ≠ renderer correctness.
WebRender's worst bugs lived at intersections — opacity nested
inside clip inside filter inside scrolled tile. Phase 12
confronts the combinatorics with targeted goldens, not new
primitive families:

- **Filter chains.** SVG / CSS filter graphs (blur, color
  matrix, drop-shadow, composite). Compose through the
  render-task graph (Phase 6) as a subgraph — *not* as a
  deferred-resolve list (axioms 1, 16). Lift `svg_filter`
  algorithm shape from the deletion log; author the executor
  fresh.
- **Nested opacity + clip.** Opacity-with-rounded-clip-with-mix-
  blend; the cross-product the family-by-family Phase 8 work
  doesn't exercise on its own.
- **Group isolation.** Blend / mix-blend-mode isolation
  semantics: when does a group need an isolated intermediate
  vs. blend-against-backdrop? Lift the CSS / SVG decision rules
  (the rules, not the algorithm).
- **Backdrop-style intermediate-heavy cases.** `backdrop-filter`,
  multi-stage filter targets that read from prior pass output.
  These exercise the prepare-phase upload contract (axiom 13)
  through chained intermediate targets — first place the chain
  itself becomes load-bearing.

**Receipt**: a curated suite of intersection goldens (rounded
clip + filter + opacity, scrolled tile + drop-shadow + transform,
nested mix-blend across an isolated group, etc.), each
pixel-matching reference within tolerance. The goldens exist
specifically to prove "primitive families landing" doesn't
silently regress combinations.

### Phase 13 — Native compositor (consumer-driven)

`netrender_compositor` crate. Sibling traits, not parameterized:

- `Compositor` — empty trait shape stubbed at Phase 0.5
  (axiom 14); fleshed out here. Embedder gives us a
  `wgpu::TextureView` per tile; we render into it.
- `NativeCompositor` — empty trait shape stubbed at Phase 0.5;
  fleshed out here. Embedder gives us a platform handle
  (CALayer / IOSurface / DXGI shared handle); we hand them
  rendered tile metadata; they sample / present.

Phase 7's preserved tile metadata feeds either path. Picking the
mode is embedder configuration. Trait *shapes* land at Phase 0.5
so axioms 13/14 have a real seam to defer to during Phases 5–7;
implementations land here.

**Receipt per platform**: macOS CALayer integration (when servo-wgpu
needs it), DirectComposition (when servo-wgpu Windows needs it).

## 6. Cross-cutting concerns

These aren't phases; they live alongside everything.

### Test infrastructure (online from Phase 2)

- Golden scene format: YAML display list → render → PNG pixel
  diff. Tolerance defaults to 0; documented per-scene tolerance
  only on root-cause analysis.
- Oracle directory `netrender/tests/oracle/` already carries five
  PNG/YAML pairs captured 2026-04-28 from `upstream/0.68` GL —
  see [tests/oracle/README.md](../../netrender/tests/oracle/README.md)
  for provenance. Today only `oracle_blank_smoke` is wired through
  the wgpu device; the other four are frozen GL-reference assets
  waiting for the matching primitive to land. Phase 0.5 preserves
  the corpus; Phase 2 promotes scenes one at a time as their
  primitives ship through netrender.
- Smoke tests for individual components (pipeline cache, bind-
  group cache, transient pool) live in `netrender_device/tests/`
  or as `#[cfg(test)]` modules.
- CI runs golden suite + unit tests + headless wgpu (Lavapipe on
  Linux CI image).

### Color contract

One contract, split across phases by *what the renderer does
internally*. The output is constant; the blend space changes
once.

| Element | Phase 1 → Phase 6 | Phase 7+ |
| --- | --- | --- |
| **Surface format** | `Rgba8UnormSrgb` (sRGB-encoded bytes on store) | `Rgba8UnormSrgb` (unchanged) |
| **Internal blend space** | sRGB-encoded (mathematically wrong-but-consistent) | linear `Rgba16Float` intermediate; sRGB-encoded composite to surface |
| **Goldens assert** | sRGB-encoded RGBA8 bytes, tolerance 0 | same bytes, tolerance 0 — composite output matches Phase 2 oracles |

This is what unblocks Phase 2 from capturing goldens before
Phase 7's intermediate exists. The wart: gradients and blends
in Phases 5–6 produce mathematically wrong colors (sRGB math is
not linear math); they're consistently wrong, so the goldens
lock in wrong-but-consistent output. Phase 7's linear
intermediate makes them right; the composite back to
`Rgba8UnormSrgb` keeps Phase 2 goldens valid through the
transition.

The surface format does not move mid-plan. If a future phase
wants HDR or wide-gamut output, that is a *new* contract block,
not a Phase-7-style inner shift.

### Profiling

- `wgpu::QuerySet` (timestamp queries) plumbing in `pass.rs` /
  `frame.rs` from Phase 4. Behind `Features::TIMESTAMP_QUERY`;
  no-op when absent.
- Per-pass timing (depth pre-pass, clip-mask pass, main pass,
  composite pass) reported through a profiler trait the embedder
  consumes.
- Frame stats: draw-call count, primitive count, bind-group
  switches, pipeline switches, texture upload bytes.

### Frame arena

- Per-frame bump allocator owned by `Renderer`. Reset at the
  start of each frame.
- All scene-tree-walking and primitive-construction temporaries
  allocate from the arena.
- Frame-scoped `Vec`s use `allocator_api2` (we already have it
  in deps). The arena impls `Allocator`.

### Feature tiering

- `BASE_FEATURES = wgpu::Features::empty()` — Phases 1–9 work
  with this.
- `OPTIONAL_PUSH_CONSTANTS`, `OPTIONAL_DUAL_SOURCE_BLENDING`,
  `OPTIONAL_TIMESTAMP_QUERY` — checked at `with_external`,
  recorded on `WgpuDevice`. Pipeline construction picks variants
  based on what's available.
- Boot fails only if `BASE_FEATURES` are unavailable.

### Pipeline cache

- `Mutex<HashMap<(family, format, override_key), wgpu::RenderPipeline>>`
  on `WgpuDevice` (pattern already there for `brush_solid`).
- Async compile via `Device::create_render_pipeline_async` once
  Phase 8 has multiple families and async compile pays off.
- On-disk `wgpu::PipelineCache` for warm starts; embedder-supplied
  path.

### Bind-group caching

- Bind groups are cheap to construct but allocating them per draw
  thrashes. Cache by `(layout_id, [resource_id; N], dynamic_offsets)`.
- Frame arena can hold per-frame bind groups; survival rules vary
  per family.

### Transient texture pool

- Per `(extent, format, usage)` keyed pool of `wgpu::Texture`.
  Reset at frame end (mark all as available); reuse on next
  frame's allocation.
- Render-task graph allocator hits this pool first.

### Sampler cache

- `Mutex<HashMap<SamplerKey, Arc<wgpu::Sampler>>>` on `WgpuDevice`.
  Phase 5 onward.

## 7. Open questions / decisions deferred

These are real design questions that don't need to be answered
before Phase 0.5 / Phase 1, but will be load-bearing as later
phases land.

1. **Threading model.** Single-threaded baseline. When (if ever)
   to parallelize scene-tree → frame-build vs. frame-build →
   render? Decision: profile-driven, post-Phase-7.
2. **Picture-cache slice strategy.** Webrender's slice-builder
   is sophisticated (split content into slices based on transform
   boundaries, scroll behaviors, etc.). Lift wholesale, or
   simplify? Decision: lift the algorithm in Phase 7; revisit
   only if it's the bottleneck.
3. **Hit testing.** Webrender computes hit-test info from
   display lists. Embedder consumes. Keep the algorithm in the
   renderer or push it upstream? Decision: **push to consumer,
   but ship the building blocks.** Hit testing is a query
   against display-list state, not a rendering operation, and
   does not belong in netrender's API surface as a `hit_test()`
   method — that's the renderer-as-subsystem slope axiom 15
   guards against.

   Ergonomic contract for the consumer: Phase 3 publishes the
   spatial-tree types (`SpatialTree`, `ScaleOffset`,
   transform-stack inversion, point-in-clip primitives) as
   *public* API on netrender — they're derivable from public
   display-list types anyway, so exposing them adds no internal
   coupling. The consumer composes those primitives into their
   own hit-test layer (Servo's embedding code, Graphshell's
   host crate). What netrender doesn't ship: a `hit_test(x, y)`
   entry point, tag-based filtering policy, or async hit-test
   transaction queues — those are subsystem concerns the
   consumer owns end-to-end.

   Revisit if the building blocks turn out to be too low-level
   for the consumer to assemble without effectively reinventing
   webrender's hit-test layer; in that case the right move is a
   sibling `netrender_hittest` crate, not a method on the
   renderer.
4. **Glyph rasterizer.** `wr_glyph_rasterizer` (lifted from disk)
   vs. fresh implementation vs. delegate to embedder? Decision:
   lift in Phase 10; replace if its API is too GL-flavored on
   contact.
5. **Async pipeline compile.** When does the latency win pay off?
   Decision: enable in Phase 8 once 5+ families exist.
6. **Surface format / color space.** *Decided at Phase 1, not
   deferred* — see Phase 1's "Color-space pin." Surface is
   `Rgba8UnormSrgb`; goldens are sRGB-encoded; Phase 7+'s linear
   `Rgba16Float` intermediate composes back to the same surface
   format so Phase 2 oracles stay valid.
7. **Dynamic-offset uniforms vs. push constants.** Used in old
   webrender for per-draw transforms. wgpu has both; push
   constants are an optional feature. Decision: prefer dynamic-
   offset uniforms (universal); use push constants opportunistically
   when feature present.
8. **Storage-buffer size limits.**
   `max_storage_buffer_binding_size` is typically 128 MB portable.
   Gpu-cache may exceed. Decision: chunked storage bindings (a
   `Vec<wgpu::Buffer>` indexed by upper bits of the address);
   shader does two-step lookup.
9. **Memory budget enforcement.** Pre-D had per-cache budgets
   and LRU eviction. Decision: Phase 7+ adds budget tracking;
   eviction policy per cache.
10. **WGSL structure.** One file per family vs. shared includes
    (parent-plan §4.10 had thoughts on this)? Decision: one
    family per file plus a shared `prim_common.wgsl` for
    PrimitiveHeader / Transform / RenderTaskData WGSL structs.
    No template language; rely on WGSL `override` for variation.

## 8. Reference: lift vs. author fresh

What we keep from old webrender (lift the *algorithm* into the
new module; don't blanket-import the old file):

| Lifted | From | Authored fresh |
|---|---|---|
| Display list types | `webrender_api` | — |
| Spatial-tree math, `ScaleOffset`, transform composition | `space.rs`, `spatial_tree.rs`, `transform.rs`, `util.rs` | — |
| Quad / segment decomposition | `quad.rs`, `segment.rs` | — |
| Clip-rect math | `clip.rs` | — |
| Border / box-shadow / ellipse / line-decoration math | `border.rs`, `box_shadow.rs`, `ellipse.rs`, `line_dec.rs` | — |
| Picture-cache invalidation logic | `tile_cache.rs` | — |
| Render-task-graph topology | `render_task_graph.rs` (concept; type fresh) | — |
| Glyph rasterizer | `wr_glyph_rasterizer` crate (Phase 10) | — |
| Frame allocator | `frame_allocator.rs` | — |
| WGSL prim_common shape | (none — no GL prim_common existed) | yes |
| `Renderer` / `Frame` / `PreparedFrame` | — | yes |
| `PrimitiveStore` | — | yes (post-D layout, not GL's) |
| `BatchBuilder` / `BatchKey` | — | yes |
| `ImageCache` / `GlyphAtlas` / `SamplerCache` / `PipelineCache` / `TransientTexturePool` | — | yes |
| `Compositor` / `NativeCompositor` traits | — | yes (drafted, see Phase 13) |
| All shader families' WGSL | — | yes (`brush_solid.wgsl` already drafted; rest follow) |
| Render-task-graph executor | — | yes |
| Profile / telemetry plumbing | — | yes |

Don't blanket-restore modules. Lift the function or struct that
encodes the algorithm; leave the indirection-token plumbing behind.

## 9. Time + scope estimate

Per the reviewer's adjusted read:

- Phases 0.5–4 (foundation through batched depth-correct rect-only
  rendering): ~2 months focused dev.
- Phase 5–7 (image cache + render-task graph + picture cache): ~2
  months.
- Phase 8 (shader family expansion): ~3 months for the full set
  with goldens being authored alongside.
- Phase 9 (clip masks): ~1 month.
- Phase 10a (text — atlas + glyph quads, shaping upstream):
  ~1 month.
- Phase 10b (browser-grade text correctness — subpixel policy,
  snapping, atlas churn, fallback fonts): ~1–2 months.
- Phase 11 (borders / lines / box shadows): ~2 months.
- Phase 12 (compositing correctness — filter chains, nested
  opacity+clip, group isolation, backdrop): ~1–2 months,
  partially parallelizable with Phase 11 once Phase 9 lands.
- Phase 13 (native compositor): consumer-driven; ~1 month per
  platform once a consumer is ready.

Total focused dev for full webrender-equivalent: **~13 months**.
Static-page demo (rects + transforms + clips + images + simple
text) by month 4–5. Production-quality on a single platform by
month 9–10. Multi-platform native compositing ships when
servo-wgpu / consumer needs it.

## 10. Bottom line

wgpu patterns or bust. Resources are handles, not tokens. Frame
builder is single-threaded until profiling forces otherwise.
Surface is per-frame, target is ephemeral. Pipelines and bind
groups are typed and cached; batches key on stable IDs of cached
items. Storage buffers replace data textures. WGSL is authored,
never translated. Smoke tests don't define the ABI; primitive
layout gets re-decided when the batch builder lands. Feature
tiering is real; baseline boots on baseline hardware. Tests are
golden scenes from Phase 2 onward. Native compositor is consumer-
driven, with a sibling trait for platform-handle handoff.

Start at Phase 0.5: split the foundation crate, set the boundary,
land it before any new code is written.
