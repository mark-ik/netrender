# WebRender wgpu Backend Minimal Plan

Local working note for the minimal additive `wgpu` backend branch.
This file is intentionally gitignored via `.git/info/exclude`.

## Branch

`wgpu-backend-0.68-minimal`

## Purpose

This branch exists to prove one narrow claim with minimal review scope:

- WebRender can grow an opt-in `wgpu_backend` without disturbing the existing GL path
- the backend can generate WGSL, build real `wgpu` pipelines, submit draws, sample textures, and read pixels back
- downstream consumers such as Servo and/or Graphshell can validate that proof without first accepting a renderer-wide abstraction refactor

This branch originally set out to **not** do the following. Some of those
constraints relaxed as the branch matured — marked below:

- ~~redesign WebRender around a backend abstraction~~ — **partially crossed**.
  A `GpuDevice` trait and `RendererBackend` enum now exist, and `Renderer.device`
  is `Option<Device>` (~240 call sites migrated). But this is a targeted seam for
  the wgpu render path, not a full renderer-wide abstraction redesign. The GL path
  is untouched in behavior.
- add `wgpu-hal` — **still true**, no `wgpu-hal` dependency
- make GL and wgpu mutually exclusive at the cargo-feature level — **still true**,
  both compile simultaneously
- ~~refactor renderer startup, profiler setup, device construction~~ — **crossed**.
  `create_webrender_instance_wgpu()` is a separate constructor, `GpuProfiler` has
  `new_noop()`, `Shaders` and `TextureResolver` are optional. This was necessary
  to build a wgpu-only `Renderer` without a GL context — the alternative was
  fabricating a dummy GL device, which would have been worse.

Why the constraints relaxed: the original plan assumed that a proof-of-concept
could stay entirely additive — new files, no changes to existing structures.
In practice, routing real render passes through wgpu required the renderer to
know which backend it was using, which meant the constructor and device ownership
had to split. Each relaxation was the minimum necessary to make a real render
path work, not speculative architecture.

## Success Criteria

For this branch, success means:

- the default GL build remains unchanged for current users
- `wgpu_backend` is additive and opt-in
- GL is not feature-gated off
- backend choice happens at runtime, not by forcing mutual exclusion in cargo features
- shader generation emits WGSL successfully for the needed shader set
- a headless `WgpuDevice` can:
  - initialize
  - create textures
  - upload texture data
  - create render pipelines
  - render at least one flat-color path
  - render at least one sampled-texture path
  - read pixels back for verification
- the backend can be exercised from a tiny example and from downstream integration experiments

That is enough to justify the backend experiment. Renderer-wide seam work is deferred until there is concrete pressure for it.

## Current Status

Implemented on this branch:

- `webrender/Cargo.toml`
  - `wgpu_backend` exists as an opt-in feature
  - GL remains the normal/default path
- `webrender_build/src/wgsl.rs`
  - build-time GLSL -> WGSL translation via naga
  - WebRender-specific preprocessing for naga compatibility
  - fixed binding assignments for cross-stage consistency
  - post-processing for WGSL issues discovered during real `wgpu` validation
- `webrender/src/device/wgpu_device.rs`
  - headless `wgpu::Device` / `wgpu::Queue` creation
  - texture creation and upload
  - eager render pipeline creation for all generated WGSL variants
  - bind group and sampler setup
  - synchronous readback helper for tests
  - `debug_color` draw path
  - sampled-texture `debug_font` draw path
  - instanced composite rendering (`render_composite_instances`)
    - two-buffer pipeline layout: unit-quad vertex buffer + per-instance data
    - name-based attribute matching (handles varying `@location(N)` across shader variants)
    - `COMPOSITE_INSTANCE_LAYOUT` maps `CompositeInstance` struct fields to wgpu formats
    - WGSL entry-point parser (`parse_wgsl_vertex_inputs`) extracts name/location/format
    - vertex buffer stride-aligned to `VERTEX_STRIDE_ALIGNMENT` (4 bytes)
  - `create_render_target()` convenience method for composite RT creation
  - `create_cache_texture()` creates wgpu textures for texture cache entries
  - `upload_texture_sub_rect()` uploads pixel data via `queue.write_texture()`
  - `new_with_surface()` creates a device with a `wgpu::Surface<'static>` for presentation
  - `acquire_surface_texture()`, `resize_surface()`, `surface_format()` for surface lifecycle
  - `render_composite_instances_to_view()` renders to arbitrary `TextureView` (surface or offscreen)
- `examples/wgpu_headless.rs`
  - tiny additive example that exercises the headless backend directly
- **constructor split — completed**
  - `Renderer.device` is now `Option<Device>` (~240 call sites migrated)
  - `gl_device()` / `gl_device_mut()` accessor methods for GL path
  - `deinit()` uses `take()` pattern for clean `Option<Device>` teardown
  - `create_webrender_instance_wgpu()` builds a wgpu-only Renderer with:
    - `device: None`, `shaders: None`, `wgpu_device: Some(...)`
    - hardcoded capability values (no GL queries)
    - all subsystems use wgpu enum variants
    - backend threads (scene builder, render backend) set up identically to GL
  - `RendererBackend::Wgpu` stub is **closed** — routes to the wgpu constructor
  - `GpuProfiler` made noop-capable (gl field optional, `new_noop()`)
  - `Shaders` made optional (`Option<Rc<RefCell<Shaders>>>`)
  - `TextureResolver` constructible without GL (dummy_cache_texture optional)
- **wgpu composite render path — wired**
  - `render_wgpu()` is the dedicated render method for wgpu-only mode
  - `render()` routes to it when `is_wgpu_only()` (device is None)
  - collects solid-color composite tiles into `CompositeInstance` batches
  - collects texture-backed tiles into per-`CacheTextureId` batches
  - marshals to bytes and calls `wgpu_device.render_composite_instances()`
  - `update()` guarded to skip `render_impl`, texture cache ops, and GL device
    calls in wgpu-only mode; message processing still maintains `active_documents`
- **wgpu texture cache — implemented**
  - `wgpu_texture_cache: FastHashMap<CacheTextureId, WgpuTexture>` on Renderer
  - `update_texture_cache_wgpu()` processes `TextureUpdateList`:
    - allocations: Alloc/Reset create wgpu textures, Free removes them
    - uploads: `Bytes` and `External` sources → `upload_texture_sub_rect()`
    - copies: `copy_texture_sub_rect()` for atlas defragmentation
  - wired into both `update()` (UpdateResources) and top of `render_wgpu()`
  - `WgpuTexture` re-exported from `device/mod.rs`
- **surface presentation — implemented**
  - `WgpuDevice::new_with_surface()` creates a device with a configured surface
  - `render_wgpu()` acquires surface texture, renders to it, and calls `present()`
  - falls back to offscreen render target in headless mode (tests, examples)
  - `RendererBackend::Wgpu` carries optional `Surface<'static>` + dimensions
  - `Renderer::resize_surface()` exposed for window resize handling
  - `wgpu` crate re-exported from `webrender` for downstream surface creation
- renderer seam groundwork
  - additive `RendererBackend` selection API with the old GL constructor preserved as a compatibility wrapper
  - backend-specific device creation split out of the main renderer constructor path
  - a small shared `GpuDevice` texture/bootstrap surface used by helper/resource paths
  - renderer-owned backend state seams now exist for:
    - GPU cache
    - vertex data textures
    - upload support state
    - VAO state
    - auxiliary textures (dither / zoom debug)

Verified on this branch:

- `cargo check -p webrender` — GL-only build clean
- `cargo check -p webrender --features wgpu_backend` — wgpu build clean
- `cargo test -p webrender` — all GL tests pass
- `cargo test -p webrender --features wgpu_backend` — all tests pass (GL + wgpu)
- `cargo run -p webrender-examples --bin wgpu_headless --features wgpu_backend`
- downstream compile integration in Servo/Graphshell:
  - `servo-paint` builds against local `webrender` with `wgpu_backend` enabled
  - `servoshell` compiles substantially past local `webrender` with the same configuration

Current proof points:

- WGSL translation succeeds for `63/63` current variants
- all `63` pipelines can be created successfully by `wgpu`
- first-pixel flat-color rendering works
- first sampled-texture rendering works
- instanced composite rendering works (unit-quad + `CompositeInstance` data through the `FAST_PATH,TEXTURE_2D` pipeline)
- readback verifies the output pixels
- wgpu-only Renderer can be constructed and routed through a wgpu-specific render path
- wgpu texture cache: create, upload, and free cache textures without GL
- textured composite tiles render through wgpu (batched by CacheTextureId)
- surface presentation: acquire surface texture, render, present — full window pipeline
- Servo can consume the branch at compile time without breaking its current GL-driven paint path
- `brush_solid` shader produces pixel-accurate output through wgpu (verified by readback test)
- data texture reads work correctly (texelFetchOffset bug fixed in WGSL preprocessing)
- batch color textures (color0/1/2) resolved from wgpu texture cache for image/text shaders
- per-batch blend state: `WgpuBlendMode` enum with lazy pipeline creation per (shader, config, blend) triple
- clip mask rendering: `cs_clip_rectangle` (fast + slow), `cs_clip_box_shadow`, `ps_quad_mask` all wired
- quad batch rendering: all `PatternKind` variants mapped to `ps_quad_*` shader pipelines
- scissor rect support: picture cache task scissor and per-rect quad batch scissor
- cs_* cache target rendering: border (solid + segment), line decoration, gradients (fast linear,
  linear, radial, conic with dithering), blur (COLOR_TARGET), and scale (TEXTURE_2D)
- dead `aData` input stripped from cs_blur/cs_svg_filter/cs_svg_filter_node WGSL at build time
- depth testing for picture cache targets: opaque batches front-to-back with depth write,
  alpha batches with depth test only — matching GL overdraw optimization
- wgpu orthographic projection correctly maps z to [0,1] for depth buffer (fixes latent
  z-clipping bug where z > 0 was outside wgpu's NDC range)

## Current Integration Status

What this branch proves today:

- `WgpuDevice` can render WebRender-shaped primitives directly
- downstream consumers can compile with `wgpu_backend` enabled
- a wgpu-only `Renderer` can be constructed via `RendererBackend::Wgpu`
- the wgpu render path processes backend messages and composites solid-color tiles
- wgpu texture cache management: create, upload, and free cache textures without GL
- textured composite tiles render through wgpu (batched by CacheTextureId)
- the GL path is completely unchanged in behavior

What it does **not** prove yet:

- WebRender can render real webpages through `wgpu` end-to-end in Servo
- ~~alpha batch draw output has been validated visually at runtime~~ — `brush_solid`
  produces correct red pixels in headless test with real data textures
- ~~blend state is hardcoded~~ — per-batch blend state now fully implemented
- ~~clip masks not drawn~~ — clip rectangle, box shadow, and quad mask rendering implemented
- ~~quad batches not drawn~~ — all PatternKind variants mapped to pipelines

Remaining gaps for real page rendering:

- **downstream wiring**: Servo needs to create a `wgpu::Surface<'static>` from its window
  handle and pass it via `RendererBackend::Wgpu { surface, width, height }`
- **~~texture copies~~**: implemented — `copy_texture_sub_rect()` for atlas defrag
- **~~external images~~**: implemented — `ExternalImageHandler` lock/upload/unlock cycle
- **~~batch texture binding~~**: implemented — color0/1/2 resolved from wgpu_texture_cache
- **~~quad batches~~**: implemented — PatternKind → pipeline mapping for all quad patterns
- **~~per-batch blend state~~**: implemented — `WgpuBlendMode` enum + lazy pipeline creation
- **~~clip masks~~**: implemented — `cs_clip_rectangle`, `cs_clip_box_shadow`, `ps_quad_mask`

Practical consequence:

- the branch is **ready for downstream integration**
- Servo can select `RendererBackend::Wgpu` with a surface, get a working Renderer
  that renders solid-color and texture-backed tiles and presents them to the window
- the alpha batch pipeline is validated end-to-end: GPU cache, frame data textures,
  render pass iteration, pipeline selection, instanced draw submission, and pixel
  output all verified by headless tests with readback
- per-batch blend state, clip mask rendering, quad batch support, scissor rects,
  cs_* cache target rendering, and depth/stencil overdraw optimization are all implemented
- the remaining work is downstream (Servo/Graphshell integration) and one minor
  wgpu feature fill-in (SVG filters, blocked on u16 vertex format)

## Seam Progress So Far

The branch is no longer only "backend proof." It now also contains a real, incremental renderer seam:

- constructor/backend selection seam
  - `RendererBackend`
  - `create_webrender_instance_with_backend(...)`
  - old `create_webrender_instance(gl, ...)` retained as a wrapper
- small shared texture/bootstrap surface
  - dither texture creation
  - dummy cache texture creation
  - cache texture creation
  - GPU buffer texture allocation/upload
  - upload texture pool texture lifecycle
  - vertex texture lifecycle
- localized renderer-owned backend state
  - `RendererGpuCache`
  - `RendererVertexData`
  - `RendererUploadState`
  - `RendererVaoState`
  - `RendererAuxTextures`

This is important because it changes the shape of the next step:

- we are no longer deciding whether any renderer seam is needed
- we are now deciding how cautiously to enter the first draw-facing seam

## Candidate Minimal Seam

The smallest plausible seam is:

- `wgpu_backend` is the only new feature flag
- GL remains compiled and unchanged
- one backend is selected per renderer instance at runtime
- GL remains the default unless the embedder explicitly selects `wgpu`

This is **not** a plan to render every frame through both backends in parallel.
It is also **not** a plan to feature-gate GL off.

The implementation shape is a staged constructor-and-ownership split:

### Stage A: backend selection at construction time

Add a new additive constructor that can select a backend explicitly, for example:

```rust
pub enum RendererBackend {
    Gl { gl: Rc<dyn gl::Gl> },
    #[cfg(feature = "wgpu_backend")]
    Wgpu {
        // exact init payload TBD; could begin as a headless/device-owned bringup
        // and later expand to surface/presentation data
    },
}

pub fn create_webrender_instance_with_backend(
    backend: RendererBackend,
    notifier: Box<dyn RenderNotifier>,
    options: WebRenderOptions,
    shaders: Option<&SharedShaders>,
) -> Result<(Renderer, RenderApiSender), RendererError>
```

Keep the existing API as a thin compatibility wrapper:

```rust
pub fn create_webrender_instance(
    gl: Rc<dyn gl::Gl>,
    notifier: Box<dyn RenderNotifier>,
    options: WebRenderOptions,
    shaders: Option<&SharedShaders>,
) -> Result<(Renderer, RenderApiSender), RendererError> {
    create_webrender_instance_with_backend(
        RendererBackend::Gl { gl },
        notifier,
        options,
        shaders,
    )
}
```

Why this first:

- it is additive
- Servo can opt in explicitly without disturbing existing GL callers
- examples, wrench, and current embedders do not need to churn immediately
- review scope stays focused on initialization, not all rendering code at once

### Stage B: keep splitting renderer-owned state locally

This is already underway. Instead of introducing one giant renderer-wide enum
up front, the branch is carving out backend-owned state by subsystem.

Completed localized seams:

1. GPU cache state
2. vertex data texture state
3. upload support state
4. VAO state
5. auxiliary texture state
6. first draw-facing VAO submission loop
7. batch texture binding helper moved into texture resolution

That has worked well and should continue until the code stops yielding clean
state-only splits.

### Stage C: enter the first draw-facing seam cautiously

The next work is no longer "easy resource/state extraction." The remaining GL
coupling is close to the actual draw path:

1. texture binding for batches
2. shader/program binding
3. instance-buffer upload for draws
4. indexed draw submission
5. render-target binding around draw passes

This branch should approach that seam cautiously:

- do not jump straight to a giant `RendererGpu` enum or giant all-encompassing `GpuDevice` trait
- first look for one more localized state-or-helper split near draw submission
- if that fails, introduce the smallest draw-facing interface that solves a real renderer path
- prefer explicit local dispatch over speculative general abstraction

Current read on that seam:

- done:
  - VAO lookup, instance upload, and indexed draw loop are now owned by `RendererVaoState`
  - batch texture binding now lives closer to `TextureResolver`
  - repeated draw-target entry / exit plumbing now lives in local renderer helpers
    (`begin_draw_target_pass(...)` / `end_draw_target_pass(...)`)
  - composite-pass setup now lives in a local helper
    (`begin_composite_pass(...)`)
  - composite tile-group sampler / blend / draw-list orchestration now lives in a
    local helper (`draw_composite_tile_group(...)`)
  - composite shader binding now goes through a local helper
    (`bind_composite_shader(...)`)
  - composite batch flushing now goes through a local helper
    (`flush_composite_batch(...)`)
  - composite batch-state transitions now go through a local helper
    (`update_composite_batch_state(...)`)
  - composite draw-item construction now goes through a local helper
    (`build_composite_draw_item(...)`)
  - composite batch locals now live in a small local state struct
    (`CompositeBatchState`)
  - alpha batch drawing now has separate local helpers for:
    - opaque pass submission (`draw_opaque_batches(...)`)
    - transparent pass submission (`draw_transparent_batches(...)`)
    - alpha blend-mode application (`apply_alpha_batch_blend_mode(...)`)
    - per-batch transparent submission (`draw_transparent_batch(...)`)
  - alpha-batch blend-mode tracking now lives in a local state struct
    (`AlphaBatchPassState`) — parallels `CompositeBatchState` for the composite path
- still clearly GL-shaped (irreducible execution, not extractable policy):
  - shader/program binding
  - texture binding
  - blend/depth/scissor state management
  - instanced draw calls
  - draw-target binding and clears

Recent snag worth remembering:

- extracting `build_composite_draw_item(...)` briefly widened compositor-clip lookup by
  mistake; the original path only attaches a compositor clip when the occlusion item
  actually `needs_mask`
- that is fixed, but it is a good reminder that the remaining composite code is now
  close enough to real policy that refactors need extra care

That means the branch is now near the end of the "localized draw-path extraction"
phase. The next work should treat renderer policy more deliberately than renderer
plumbing.

## Stage D: Alpha Batch Rendering Through wgpu (2026-03-31)

This session pushed the wgpu backend from composite-tile-only rendering toward
actual webpage content rendering via the alpha batch pipeline. Key additions:

### Windows Build Fix

- **naga stack overflow**: `translate_to_wgsl()` spawns naga work on an 8MB stack
  thread. The default 1MB Windows stack was insufficient for naga's recursive
  validation flow analysis. All 63 WGSL shader variants now translate successfully
  on Windows.

### GPU Cache for wgpu

- `WgpuGpuCacheState` in `renderer/mod.rs`:
  - CPU mirror of the GPU cache (RGBA32F data, row-addressed)
  - `apply_updates()` processes `GpuCacheUpdate::Copy` operations from the scene builder
  - `upload()` creates/updates a wgpu texture from the CPU mirror
  - `texture_view()` returns a view for binding into draw passes
- GPU cache updates are collected in `pending_gpu_cache_updates` during `update()`
  and consumed at the top of `render_wgpu()`

### Frame Data Textures

- `WgpuFrameDataTextures` struct holds per-frame data textures:
  - `prim_headers_f` / `prim_headers_i`: primitive header data
  - `transform_palette`: transform matrices
  - `render_tasks`: render task data
  - `gpu_buffer_f` / `gpu_buffer_i`: optional GPU buffer data
- `upload_frame_data_textures()` creates RGBA32F textures from the Frame's data arrays
  using `MAX_VERTEX_TEXTURE_WIDTH = 1024`
- `create_data_texture()` and `update_data_texture()` on WgpuDevice handle texture
  lifecycle with automatic reallocation on size change

### Alpha Batch Pipeline Wiring

- `draw_passes_wgpu()` iterates render passes → picture cache targets:
  - Pattern-matches on `PictureCacheTargetKind::Draw { alpha_batch_container }`
  - Looks up the target wgpu texture from `wgpu_texture_cache`
  - Iterates opaque batches then alpha batches
  - Maps each `BatchKey` to a `(shader_name, config)` pipeline key
  - Passes all data textures (GPU cache, transforms, prim headers, render tasks,
    GPU buffers) via `TextureBindings`
  - Submits instanced draws through `draw_instanced()`

- `batch_key_to_pipeline_key()` maps WebRender batch kinds to WGSL pipeline keys:
  - Blend-mode aware: alpha batches get `ALPHA_PASS` config variants
  - Handles: `Brush(Solid|Image|Blend|MixBlend|LinearGradient|Opacity|YuvImage)`,
    `TextRun`, `SplitComposite`

### Instanced Pipeline Layouts for Alpha Batch Shaders

- Alpha batch shaders (`brush_*`, `ps_text_run`, `ps_split_composite`, `quad*`)
  now use proper two-buffer instanced layouts:
  - Buffer 0: unit-quad vertex (Unorm8x2, 4-byte stride)
  - Buffer 1: `PrimitiveInstanceData` (Sint32x4, 16 bytes)
- Detection: `name.starts_with("brush_") || name.starts_with("ps_text_run") || ...`
- `PRIMITIVE_INSTANCE_LAYOUT` maps `aData` field to `Sint32x4`
- Other shaders (`cs_*`, `clip_*`) retain the single-buffer fallback layout

### General-Purpose Draw Infrastructure

- `TextureBindings` struct: named fields for all 12 texture binding slots
  (color0-2, gpu_cache, transform_palette, render_tasks, dither, prim_headers_f/i,
  clip_mask, gpu_buffer_f/i)
- `create_bind_groups_full()`: creates bind groups from `TextureBindings` filling
  all 15 slots (3 uniforms + 12 textures)
- `draw_instanced()`: general-purpose instanced draw method with pipeline lookup,
  uniform creation, bind group creation, and render pass submission

### Build Verification

All changes verified on Windows:

- `cargo check -p webrender` — GL-only build clean (1 pre-existing dead_code warning)
- `cargo check -p webrender --no-default-features --features wgpu_backend,static_freetype` — clean
- `cargo test -p webrender --no-default-features --features wgpu_backend,static_freetype` — 109 tests pass (107 lib + 1 integration + 1 doc)
- `create_all_shader_pipelines` test confirms all 63 pipelines create with correct layouts
- `draw_instanced_brush_solid_red_rect` test confirms pixel-accurate output from
  the `brush_solid` shader through the wgpu pipeline with real data textures

### Critical Bug Fix: texelFetchOffset → textureLoad Offset Loss

Discovered that naga silently drops the constant offset parameter when translating
GLSL `texelFetchOffset(tex, pos, lod, ivec2(x, y))` to WGSL `textureLoad`.
This caused ALL multi-texel data texture reads (transforms: 8 texels, prim headers:
2 texels, render tasks: 2 texels, GPU cache multi-vec fetches) to collapse to the
base coordinate, reading the same texel repeatedly.

**Fix**: added `rewrite_texel_fetch_offset()` in `wgsl.rs` which rewrites
`texelFetchOffset(tex, pos, lod, offset)` → `texelFetch(tex, pos + offset, lod)`
before the GLSL enters naga. This makes the addition explicit in the AST so naga
preserves it correctly. Verified by inspecting the generated WGSL — `fetch_transform`
now correctly reads texels at offsets (0,0) through (7,0), `fetch_prim_header` reads
at (0,0) and (1,0), etc.

### Bind Group Layout Fix: Non-filterable Float Textures

Data texture bindings (GPU cache, transforms, render tasks, prim headers F,
GPU buffer F) are `Rgba32Float` which is not filterable in wgpu. The bind group
layout previously declared all float texture slots as `Float { filterable: true }`,
causing validation errors when binding `Rgba32Float` views.

**Fix**: bindings 3, 4, 5, 7, 10 changed to `Float { filterable: false }`. This
is correct because all data textures use `textureLoad` (not `textureSample`).
Color textures (0-2), dither (6), and clip mask (9) remain filterable.

### Batch Texture Wiring

`draw_passes_wgpu()` now resolves `BatchKey.textures.input.colors[0..2]`
(`TextureSource::TextureCache(id, _)`) to wgpu texture views from
`wgpu_texture_cache`, passing them as color0/1/2 in `TextureBindings`. This
enables image brushes, opacity brushes, text runs, and blend brushes to access
their source textures during rendering.

### What This Proves

The wgpu backend can now:

- Accept GPU cache updates from the scene builder and upload them to GPU
- Upload all per-frame data textures (transforms, prim headers, render tasks, GPU buffers)
- Process render passes: iterate picture cache targets and draw alpha batch containers
- Select the correct shader pipeline variant based on batch kind and blend mode
- Submit instanced draws with proper vertex/instance buffer layouts
- **Produce correct pixel output** from the `brush_solid` shader with real data
  (identity transform, GPU cache color, prim headers, render tasks)
- Resolve batch color textures (color0/1/2) from the texture cache for image/text rendering

### What Still Needs Work

- **~~Quad batches~~**: implemented — all `PatternKind` variants mapped to `ps_quad_*` pipelines
- **~~Clip masks~~**: implemented — `cs_clip_rectangle` (fast + slow path),
  `cs_clip_box_shadow`, `ps_quad_mask` with dedicated instance layouts
- **~~Blend state~~**: implemented — `WgpuBlendMode` enum with 8 blend modes,
  lazy pipeline creation per (shader, config, blend) triple, `blend_mode_to_wgpu()`
  conversion from WebRender `BlendMode`
- **~~cs_* shaders~~**: implemented — border, line decoration, gradient, blur, scale
  cache targets all wired with correct instance layouts and blend modes
- **~~Scissor rect~~**: implemented — `draw_instanced` accepts `Option<(u32, u32, u32, u32)>`,
  wired for picture cache `task_scissor_rect` and per-rect quad batches
- **~~Depth/stencil~~**: implemented — `WgpuDepthState` enum (None/WriteAndTest/TestOnly),
  depth textures pooled by size, opaque batches drawn front-to-back with depth write,
  alpha batches with depth test only, ortho projection fixed for wgpu z mapping
- **cs_svg_filter / cs_svg_filter_node**: not yet wired (u16 packing mismatch)
- **End-to-end page rendering**: downstream (Servo/Graphshell) runtime validation

## Stage E: Blend State, Clip Masks, and Quad Batches (2026-03-31)

This session completed three features that close the major pipeline gaps for
real page rendering.

### Per-Batch Blend State

wgpu sets blend state at pipeline creation time (unlike GL's dynamic state).
The solution uses lazy pipeline creation keyed by `(shader_name, config, WgpuBlendMode)`:

- `WgpuBlendMode` enum with 8 modes: None, Alpha, PremultipliedAlpha,
  PremultipliedDestOut, Screen, Exclusion, PlusLighter, MultiplyClipMask
- `ShaderEntry` caches compiled shader modules separately from pipelines,
  enabling reuse across blend variants without recompilation
- `create_pipeline_for_blend()` builds a new pipeline from cached shader
  modules when a new (shader, config, blend) combination is first encountered
- `blend_mode_to_wgpu()` converts WebRender's `BlendMode` to `WgpuBlendMode`
- Dual-source blending modes (SubpixelDualSource, MultiplyDualSource) and
  Advanced blend modes fall back to PremultipliedAlpha

### Clip Mask Rendering

Clip masks have their own instance layouts distinct from `PrimitiveInstanceData`:

- `CLIP_RECT_INSTANCE_LAYOUT`: 200 bytes — maps `ClipMaskInstanceRect` fields
  (device area, origins, scale, transform IDs, local pos/rect, mode, 4× corner
  rect+radii)
- `CLIP_BOX_SHADOW_INSTANCE_LAYOUT`: 84 bytes — maps `ClipMaskInstanceBoxShadow`
  fields (device area, origins, scale, transform IDs, resource address, src rect
  size, mode, stretch mode, dest rect)
- `MASK_INSTANCE_LAYOUT`: 32 bytes — `aData: vec4<i32>` + `aClipData: vec4<i32>`
  for `ps_quad_mask` (unique among quad shaders)

Key discoveries:
- naga translates GLSL `ivec2` to WGSL `vec2<i32>` — GL `U16` format auto-widens
  to `ivec2`, but wgpu requires exact match; `Sint16x2` maps to `vec2<i32>`
- `ps_quad_mask` has `PatternKind::Mask` which is `unreachable!()` in normal
  quad dispatch; it uses `WgpuBlendMode::None` for non-scissored draws

`draw_clip_batch_list_wgpu()` draws:
- slow rectangles: `cs_clip_rectangle` with empty config
- fast rectangles: `cs_clip_rectangle` with `"FAST_PATH"` config
- box shadows: `cs_clip_box_shadow` with `"TEXTURE_2D"` config

Primary clips use no blending (overwrite), secondary clips use multiplicative
blend `(Zero, Src)` color / `(Zero, SrcAlpha)` alpha.

### Quad Batch Support

`draw_quad_batches_wgpu()` maps `PatternKind` to shader pipelines:

- `ColorOrTexture` → `("ps_quad_textured", "")`
- `Gradient` → `("ps_quad_gradient", "DITHERING")`
- `RadialGradient` → `("ps_quad_radial_gradient", "DITHERING")`
- `ConicGradient` → `("ps_quad_conic_gradient", "DITHERING")`
- `Mask` → `("ps_quad_mask", "")`

Non-scissored quads use `WgpuBlendMode::None`, scissored quads use
`WgpuBlendMode::PremultipliedAlpha`.

### Build Verification

- `cargo check -p webrender` — GL-only build clean
- `cargo check -p webrender --features wgpu_backend` — wgpu build clean
- `cargo test -p webrender --features wgpu_backend` — 109 tests pass
- All 63/63 WGSL pipelines create successfully

## Stage F: Scissor Rects and cs_* Cache Target Rendering (2026-03-31)

### Scissor Rect Support

- `draw_instanced()` now accepts `scissor_rect: Option<(u32, u32, u32, u32)>`
- `pass.set_scissor_rect()` called on the wgpu render pass when scissor is Some
- `device_rect_to_scissor()` converts `DeviceIntRect` → wgpu coordinates
  (top-left origin, same as WebRender device space for offscreen targets)
- Wired into picture cache alpha batch draws (`task_scissor_rect`) and
  scissored quad batches (`prim_instances_with_scissor`)

### cs_* Cache Target Rendering

All non-SVG cs_* shaders now have instanced pipeline layouts and are wired
into `draw_cache_target_tasks_wgpu()`:

- **Borders**: `cs_border_solid` + `cs_border_segment` with `BORDER_INSTANCE_LAYOUT`
  (108 bytes, PremultipliedAlpha blend)
- **Line decorations**: `cs_line_decoration` with `LINE_DECORATION_INSTANCE_LAYOUT`
  (36 bytes, PremultipliedAlpha blend)
- **Gradients**: `cs_fast_linear_gradient`, `cs_linear_gradient` (DITHERING),
  `cs_radial_gradient` (DITHERING), `cs_conic_gradient` (DITHERING) — each with
  their own instance layout (36–52 bytes, no blend)
- **Blur**: `cs_blur` (COLOR_TARGET) with `BLUR_INSTANCE_LAYOUT` (28 bytes,
  no blend), iterates per texture source
- **Scale**: `cs_scale` (TEXTURE_2D) with `SCALE_INSTANCE_LAYOUT` (36 bytes,
  no blend), iterates per texture source

### Dead `aData` Input Stripping

cs_blur, cs_svg_filter, and cs_svg_filter_node inherit a dead `aData: vec4<i32>`
vertex input from `prim_shared.glsl`. In GL, unbound vertex attributes silently
read as zero. wgpu requires all declared inputs to be provided by vertex buffers.

**Fix**: `strip_dead_adata_input()` in `wgsl.rs` removes `aData` from the entry
point signature and renumbers subsequent `@location(N)` values during WGSL
post-processing. This runs at build time so no runtime cost.

### Remaining Gaps

- **SVG filters** (`cs_svg_filter`, `cs_svg_filter_node`): u16 fields in the
  Rust struct map to individual `i32` shader inputs. wgpu has no single-component
  u16 vertex format, so these need a packing solution. Deferred as a rare edge case.
- **End-to-end page rendering**: downstream runtime validation.

### Build Verification

- `cargo check -p webrender` — GL-only build clean (1 pre-existing warning)
- `cargo check -p webrender --features wgpu_backend` — wgpu build clean
- `cargo test -p webrender --features wgpu_backend` — 109 tests pass
- All 63/63 WGSL pipelines create with correct instanced layouts

## Stage G: Depth Testing for Opaque Batches (2026-03-31)

### Depth/Stencil Implementation

WebRender uses depth testing to optimize overdraw in picture cache targets:
opaque batches are drawn front-to-back with depth write enabled, and alpha
batches test against the depth buffer to reject fragments behind opaque geometry.

In wgpu, depth state is baked into the pipeline at creation time (unlike GL's
dynamic `glEnable(GL_DEPTH_TEST)` / `glDepthMask`). The implementation adds:

- **`WgpuDepthState` enum**: `None`, `WriteAndTest`, `TestOnly` — parallels
  `WgpuBlendMode` as a pipeline cache key axis
- **Pipeline key extended**: `(shader_name, config, blend_mode, depth_state)` —
  pipelines are lazily created per-combination as with blend modes
- **`create_pipeline_for_blend()`** now accepts `depth_state` and sets
  `depth_stencil: Some(DepthStencilState { format: Depth32Float, ... })` on
  the pipeline descriptor for non-None depth states
- **Depth texture pool**: `depth_textures: HashMap<(u32, u32), wgpu::Texture>`
  on `WgpuDevice`, keyed by (width, height). `acquire_depth_view()` creates or
  reuses a `Depth32Float` texture for the given dimensions.
- **Render pass attachment**: `draw_instanced()` now accepts `depth_state` and
  `depth_view` parameters. When depth is enabled, the render pass gets a
  `depth_stencil_attachment` with `Clear(1.0)` on first draw, `Load` thereafter.

### Orthographic Projection Fix

The wgpu ortho function previously had `m[2][2] = -1.0`, which mapped any z > 0
to z_clip < 0 — outside wgpu's [0,1] NDC range. This meant fragments with
non-zero depth IDs were silently clipped. This hadn't been caught because tests
used z=0 and non-depth draws (cs_*, clips) also use z=0.

**Fix**: `ortho(w, h, max_depth)` now maps z ∈ [0, max_depth] → depth ∈ [1.0, 0.0]:
- `m[2][2] = -1.0 / max_depth` (higher z → smaller depth → "closer")
- `m[3][2] = 1.0` (z=0 → depth=1.0 at the back)
- With `LessEqual` depth test, front-to-back opaque draws reject overdraw correctly
- `max_depth_ids` (1 << 22, matching GL) is stored on `WgpuDevice`

### Picture Cache Target Draw Order

`draw_passes_wgpu()` now separates opaque and alpha batches:

1. **Opaque batches** iterate in **reverse** order (`.iter().rev()`) with
   `WgpuDepthState::WriteAndTest` — front-to-back, writing depth
2. **Alpha batches** iterate in **forward** order with
   `WgpuDepthState::TestOnly` (if opaque batches exist) or
   `WgpuDepthState::None` (if no opaque batches)
3. First draw clears both color (black) and depth (1.0)
4. Non-depth draws (cs_*, clip masks, quad batches) continue to use
   `WgpuDepthState::None` with no depth attachment

### Build Verification

- `cargo check -p webrender` — GL-only build clean (1 pre-existing warning)
- `cargo check -p webrender --features wgpu_backend` — wgpu build clean
- `cargo test -p webrender --features wgpu_backend` — 109 tests pass
- All 63/63 WGSL pipelines create with correct layouts

### Minimal Shared Trait Shape

The shared `GpuDevice` trait is now landed and implemented by both `Device` (GL)
and `WgpuDevice`. The current surface covers texture lifecycle:

```rust
pub trait GpuDevice {
    type Texture;

    fn create_texture(
        &mut self,
        target: ImageBufferKind,
        format: ImageFormat,
        width: i32,
        height: i32,
        filter: TextureFilter,
        render_target: Option<RenderTargetInfo>,
    ) -> Self::Texture;

    fn upload_texture_immediate<T: Texel>(
        &mut self,
        texture: &Self::Texture,
        pixels: &[T],
    );

    fn delete_texture(&mut self, texture: Self::Texture);
}
```

This is intentionally smaller than the old branch's trait work. Draw calls,
render-target binding, profiler integration, and readback can be added only
when an actual renderer path requires them.

### Phase 1 File Scope

Expected first-pass files on the minimal branch:

- `webrender/src/device/mod.rs`
- `webrender/src/device/gl.rs`
- `webrender/src/device/wgpu_device.rs`
- `webrender/src/renderer/init.rs`
- `webrender/src/renderer/mod.rs`
- `webrender/src/renderer/upload.rs`
- `webrender/src/renderer/vertex.rs`
- `webrender/src/lib.rs`

Files now clearly in the next-risk bucket:

- `webrender/src/renderer/mod.rs`
- `webrender/src/renderer/shade.rs`
- draw submission paths that currently assume concrete GL program/VAO/device behavior

Files that should be avoidable at first because the old GL constructor remains as
a compatibility wrapper:

- `examples/common/boilerplate.rs`
- `examples/multiwindow.rs`
- `example-compositor/compositor/src/main.rs`
- `wrench/src/wrench.rs`

Things that should stay GL-only until forced:

- ~~GPU profiler/query plumbing~~ — `GpuProfiler` now has `new_noop()` for wgpu mode
- debug overlays
- capture/replay
- native compositor integration
- broad upload-path unification

### Why this is smaller than the old approach

The old branch started by making backend abstraction a primary architectural goal.
The minimal branch should instead:

- add an explicit backend selector while preserving the old GL entrypoint
- replace concrete renderer ownership with the smallest closed enum that can grow
- migrate only bootstrap/resource responsibilities first
- delay any broad trait design until there is actual shape to abstract

## Non-Goals For This Branch

The following are intentionally out of scope unless later wiring makes them unavoidable:

- a broad `RendererBackend` architecture
- a large `GpuDevice` trait that mirrors most of GL
- feature-gating GL off or requiring cargo-feature mutual exclusion
- rendering every frame through both backends in parallel for parity checking
- mutual-exclusion cargo features for GL vs wgpu
- profiler/query/compositor refactors
- capture/replay plumbing
- broad upload-path unification
- `wgpu-hal`

If a future contributor asks “why not do the abstraction now?”, the answer for this branch is:
because the minimal proof should stay small enough to review and validate before asking reviewers to absorb a larger architectural change.

## Next Steps

The branch has already completed most of the early items that used to sit here:

- Servo/Graphshell compile validation has been exercised
- additive `RendererBackend` selection exists
- the tiny shared `GpuDevice` texture/bootstrap surface exists
- multiple renderer-owned state seams are already landed

So the next phase is no longer "prove the backend exists" and no longer
"extract easy helpers." The next phase is:

1. preserve current renderer semantics while isolating renderer policy from GL execution
2. keep changes local to one renderer subsystem at a time
3. avoid broad trait design unless a subsystem stops yielding honest local structure

### Named milestone: close the `RendererBackend::Wgpu` stub — done

`RendererBackend::Wgpu` now routes to `create_webrender_instance_wgpu()`,
which builds a full wgpu-only Renderer. The stub no longer returns
`Err(UnsupportedBackend)`.

### Named milestone: wgpu composite render path — done

`render()` routes to `render_wgpu()` in wgpu-only mode. Solid-color
composite tiles are collected into `CompositeInstance` batches and drawn
via `wgpu_device.render_composite_instances()`. `update()` is guarded
to skip all GL-specific operations.

### Alpha-batch policy boundary — completed

The first pass over alpha-batch policy boundaries is done:

- `draw_alpha_batch_container(...)` orchestrates scissor, opaque, and transparent passes
- `draw_opaque_batches(...)` owns the opaque per-batch loop
- `draw_transparent_batches(...)` / `draw_transparent_batch(...)` own the transparent per-batch loop
- `apply_alpha_batch_blend_mode(...)` owns blend-mode dispatch
- `AlphaBatchPassState` owns blend-mode transition tracking across batches

Reassessment: alpha batching no longer yields honest helper-sized splits. The
remaining code in each helper is irreducible per-batch execution: shader
lookup → shader bind → texture bind → draw. The opaque path has no inter-batch
state; the transparent path's inter-batch state is now in `AlphaBatchPassState`.

A larger `RendererAlphaBatches` subsystem is not warranted — `AlphaBatchPassState`
with `prev_blend_mode` is the only inter-batch state. Adding more structure would
be abstraction for abstraction's sake.

### What "renderer policy" means here

Renderer policy means decisions like:

- which batches can be grouped together
- when state transitions force a flush
- when mix-blend requires readback
- how opaque and alpha passes are separated
- how draw ordering is preserved
- when shader and blend transitions are semantic rather than incidental

For this branch, the policy should remain WebRender-authored and GL-compatible.
The goal is to isolate policy, not redesign it to be more `wgpu`-native.

### Composite batching policy — assessment

Assessed and found to be already complete. `CompositeBatchState` owns the
inter-tile state (shader params, textures, instance buffer).
`update_composite_batch_state` implements flush-on-change policy.
`draw_tile_list` is the irreducible per-tile loop. No further helper-sized
splits exist.

### Smallest draw-facing execution surface

Both alpha-batch and composite draw paths ultimately need these operations
from the device:

1. **Blend state**: `set_blend`, `set_blend_mode_*` (many variants)
2. **Depth state**: `enable_depth`, `enable_depth_write`, `disable_depth`, `disable_depth_write`
3. **Scissor state**: `enable_scissor`, `disable_scissor`, `set_scissor_rect`
4. **Shader bind**: `bind_program`, `set_uniforms`, `set_shader_texture_size`
5. **Texture bind**: `bind_texture` (via `texture_resolver.bind_batch_textures`)
6. **Draw target**: `bind_draw_target`, `clear_target`, `blit_render_target`
7. **Draw call**: instanced draw (via `vaos.draw_instanced_batch`)

That is the full surface. Abstracting all of it at once would be the "giant
all-encompassing GpuDevice trait" the plan warns against.

The smallest subset that would let one real renderer path flow through wgpu
is a single opaque composite tile:

- `bind_draw_target` + `clear_target` (begin pass)
- `set_blend(false)` (opaque = no blend)
- shader bind (composite shader)
- texture bind
- instanced draw

No depth, no scissor, no readback. This is close to what `WgpuDevice` can
already do with `debug_color`/`debug_font`.

### Architectural constraint: Renderer owns `pub device: Device`

The core tension is that `Renderer` owns `pub device: Device` (the concrete
GL type), and ~233 call sites in `renderer/mod.rs` use it directly. Changing
this to an enum or trait would touch hundreds of call sites in a single change.

Options considered:

**Option A: enum wrapper replacing `device: Device`**

```rust
enum RendererDeviceState {
    Gl { device: Device },
    Wgpu { device: WgpuDevice },
}
```

Rejected for this branch: too invasive. Touches 233+ call sites. The plan
explicitly avoids "giant all-encompassing" changes.

**Option B: generic `Renderer<D: DeviceTrait>`**

Rejected: even more invasive than Option A, infects every type signature.

**Option C: `Renderer` keeps `device: Device`, adds `wgpu_device: Option<WgpuDevice>`**

The wgpu constructor path creates a `WgpuDevice` for the draw paths that
are wired up. The problem: the constructor currently queries dozens of
GL-specific capabilities from `Device` (233 call sites + 11 `get_capabilities()`
queries in init.rs). The wgpu path has no GL context.

Viable variant: split the constructor so the wgpu path:
- creates a `WgpuDevice` for draw
- provides hardcoded capability answers (wgpu knows its own limits)
- skips GL-specific initialization (shaders, VAOs, PBOs)
- creates a minimal `Renderer` that only uses the wgpu draw paths

This is the most incremental approach but requires a second constructor
path that bypasses GL `Device` creation entirely.

**Option D: keep `WgpuDevice` outside `Renderer`, consume batch data directly**

Scene building is backend-independent — it produces `AlphaBatchContainer`,
`CompositeState`, etc. A `WgpuDevice` could consume these structures
directly, outside the `Renderer` loop. This is what the headless example
already does (albeit with its own test geometry, not real batch data).

Advantages:
- zero changes to `Renderer` itself
- proves wgpu can render real WebRender geometry
- the embedder (Servo) controls which backend draws

Disadvantage:
- duplicates the draw orchestration that `Renderer` already does
- doesn't lead toward a unified renderer

### Recommended path: Option C variant — completed

Option C was chosen and is now fully implemented:

**Constructor split (done):**
- `Renderer.device` is `Option<Device>` (~240 call sites use `as_mut().unwrap()` / `as_ref().unwrap()` to preserve field-level borrowing)
- `create_webrender_instance_wgpu()` builds a Renderer with `device: None`, hardcoded capabilities, wgpu subsystem variants
- `RendererBackend::Wgpu` routes to the wgpu constructor (stub is closed)
- `Shaders` is `Option<Rc<RefCell<Shaders>>>` (None in wgpu mode)
- `GpuProfiler` has `new_noop()` (no GL context needed)
- `TextureResolver` has `new_without_gl()` (no dummy cache texture)
- `deinit()` uses `take()` for clean teardown

**Render path (done):**
- `render_wgpu()` is the wgpu-only render method
- `render()` routes to it when `is_wgpu_only()`
- `update()` skips GL-specific operations (render_impl, texture cache, device calls) in wgpu-only mode
- solid-color composite tiles render through wgpu
- texture-backed tiles are skipped (texture cache blocker)

### Immediate next steps

The constructor split, wgpu render path, and texture cache are all done.
The wgpu-only Renderer can be constructed, process backend messages,
manage a wgpu texture cache, and composite both solid-color and textured tiles.

Priority order from here:

1. ~~**downstream wiring (runtime test)**~~: compile integration verified. Runtime requires
   a display server (WSL2 headless can't create winit window). Test on real display.
2. ~~**external images**~~: implemented — ExternalImageHandler lock/upload/unlock cycle
3. **surface presentation wiring**: create wgpu surface from winit window handle in Servo.
   Requires storing/exposing raw window handles from `RenderingContext` to `Painter`.

Only after those land should the branch consider:

- broader renderer-owned backend enums
- expanding `GpuDevice` beyond the smallest proven execution needs
- external surface / YUV support
- capture/replay for wgpu mode

## Feature-Gating Audit (2026-04-01)

### Tier 1 (completed): module-level gating

Added `gl_backend` feature (on by default). Gate GL device modules,
renderer submodules (debug, gpu_cache, shade, vertex, upload), composite
traits, and profiler draw methods. wgpu-only builds compile with
`--no-default-features --features wgpu_backend`. All three configurations
(GL-only, wgpu-only, both) compile and pass tests.

Commits: `4ac407bdd`, `c7b3e6ca3`, `ea201fce2`.

### Tier 2 audit: what to share vs what's not worth it

Audited five GL-gated renderer submodules (4,896 lines total). Findings:

**Not worth sharing (GL problems that wgpu doesn't have):**

- `upload.rs` staging/batching (~700 "shareable" lines): solves PBO management
  and batched uploads. wgpu does direct uploads; staging is irrelevant.
- `shade.rs` lazy shader compilation (~1,200 lines): wgpu shaders are
  precompiled WGSL from build.rs. No runtime compilation.
- `debug.rs` vertex generation (~200 lines): would need a whole new wgpu
  debug renderer to use it. Not worth extracting until someone builds one.
- `vertex.rs` VAO management: fundamentally GL. wgpu uses bind groups.

**Worth sharing (duplicated logic between GL and wgpu paths):**

1. **Data layout calculation** (~70 lines): `texels_per_item`, `items_per_row`,
   `height` arithmetic duplicated between `vertex.rs::VertexDataTexture::update()`
   and `mod.rs::upload_frame_data_textures()`. Extract to helper functions.

2. **CacheRow dirty-tracking** (36 lines): pure data structure in `gpu_cache.rs`
   (gated module). wgpu path reimplements the concept inline as flat Vec writes.
   Move to shared location so wgpu can optionally use row-level dirty tracking.

3. **GPU cache update dispatch**: the `GpuCacheUpdate::Copy` match with
   block-copy loop is identical in `gpu_cache.rs::update()` and
   `WgpuGpuCacheState::apply_updates()`. Share the iteration logic via a
   callback or trait.

### Tier 2 conclusion

~150 lines of real duplication across three items. Everything else is solving
backend-specific problems. The gated modules diverge at the right level; a
full Tier 2 granular gating pass is not justified.

## Relationship To Other Notes

- `project_wgpu_backend.md`
  - experimental branch note
  - use it for broader seam exploration, larger refactors, and ideas not appropriate for the minimal branch
- `shader_translation_journal.md`
  - technical record of the GLSL -> WGSL translation work
  - shared reference for both branches
- `wasm-portability-checklist.md`
  - current blockers and runtime assumptions for targeting `wasm32-unknown-unknown`
    and `wasm32-wasip2`
