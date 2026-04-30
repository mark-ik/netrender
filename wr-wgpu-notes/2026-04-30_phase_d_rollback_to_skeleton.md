# Phase D Rollback to wgpu Skeleton (2026-04-30)

**Status**: Active. Supersedes
[2026-04-29_pipeline_first_migration_plan.md](2026-04-29_pipeline_first_migration_plan.md)
in its entirety. The pipeline-first plan died on contact with the
GL-thread-model architecture baked into webrender's frame builder.

## What happened

The pipeline-first plan reordered migration around shader families,
each slice migrating one family end-to-end with all of its inputs
(textures, buffers, bindings, pipeline, pass encoding) reshaped to
their idiomatic wgpu form together. Six sub-slices of P1.6 landed
(6a hook, 6b.1 storage-buffer producer, 6b.2 brush_solid arm body,
6c render-target cache, 6d alpha-batch loop, 6b.3a/b hook flip +
readback) before the per-file audit pattern revealed itself as a
rabbit hole one level up: the modules I was "auditing and adopting"
were shaped around GL thread-model assumptions, not GL API calls.
Specifically:

- `texture_cache` / `resource_cache` / `image_source` /
  `picture_textures` / `render_task_cache` / `glyph_cache` — all
  exist to mediate u32-handle / cross-thread / queue-of-commands
  patterns required by the GL single-threaded context model. wgpu's
  `Device` is `Send + Sync`; textures are Arc-cloneable handles any
  thread can hold synchronously. The whole cache layer is GL
  scaffolding.
- `prim_store` / `picture` / `tile_cache` / `batch` /
  `render_task_graph` / `frame_builder` / `scene_building` — embed
  `TextureSource` indirection tokens, `CacheTextureId`,
  `BatchTextures = [TextureSource; 3]`, deferred resolves, and
  pre-allocated render-task IDs. The data flow is shaped around late
  binding of texture handles. Replacing the indirection with wgpu
  `TextureView`s isn't a type substitution — it's a redesign.

The honest read: **the frame-builder architecture is itself a GL
artifact**, not just a user of GL APIs. Audit-and-adopt
file-by-file was preserving the architecture under the illusion of
modernizing it.

## What's deleted

Everything under `webrender/src/` outside the wgpu skeleton:

- All 50+ `webrender/res/*.glsl` shaders, `webrender/build.rs`,
  `webrender_build/`, `swgl/`, `glsl-to-cxx/` (Phase D destructive
  cut)
- `device/gl.rs`, `device/query_gl.rs` (GL device)
- All renderer-body submodules: `renderer/{shade, vertex, upload,
  composite, external_image, debug}.rs`, the 5,600 LOC
  `renderer/mod.rs` and 860 LOC `renderer/init.rs`
- `composite.rs`, `compositor.rs`, `screen_capture.rs`,
  `picture_textures.rs` (GL output side; composite was audited and
  briefly restored, then dropped with the rest of the frame-builder
  layer)
- `texture_cache.rs`, `resource_cache.rs`, `image_source.rs`,
  `render_task_cache.rs`, `glyph_cache.rs` (GL-thread-model
  indirection layer)
- All frame-builder modules: `batch`, `border`, `box_shadow`,
  `clip`, `space`, `spatial_tree`, `command_buffer`, `debug_*`,
  `ellipse`, `filterdata`, `frame_builder`, `freelist`, `gpu_types`,
  `hit_test`, `internal_types`, `intern`, `invalidation`,
  `lru_cache`, `pattern`, `picture`, `picture_composite_mode`,
  `picture_graph`, `prepare`, `prim_store`, `print_tree`,
  `profiler`, `quad`, `render_api`, `render_backend`,
  `render_target`, `render_task`, `render_task_graph`, `scene`,
  `scene_builder_thread`, `scene_building`, `segment`,
  `spatial_node`, `surface`, `telemetry`, `texture_pack`,
  `transform`, `tile_cache`, `util`, `visibility`, `api_resources`,
  `image_tiling`, `rectangle_occlusion`, `frame_allocator`,
  `bump_allocator`, `svg_filter`
- Workspace members: `examples`, `wrench`, `example-compositor`,
  `webrender_api`, `wr_glyph_rasterizer`, `wr_malloc_size_of`,
  `peek-poke`, `fog` (left on disk; not in workspace)
- Cargo deps: `gleam`, `swgl`, `webrender_build`, `glslopt`,
  `mozangle`, `glean`, plus all transitively-only-needed-for-GL
  deps

## What survives

The wgpu skeleton (1,795 LOC total in webrender/src):

| File | LOC | Role |
|---|---|---|
| `lib.rs` | 34 | Crate root, four `pub use` lines |
| `device/mod.rs` | 5 | One `pub mod wgpu;` |
| `device/wgpu/core.rs` | 214 | `WgpuHandles`, `boot()`, feature checks |
| `device/wgpu/adapter.rs` | 191 | `WgpuDevice` + pipeline cache |
| `device/wgpu/pass.rs` | 160 | `DrawIntent`, `RenderPassTarget`, `flush_pass` |
| `device/wgpu/pipeline.rs` | 98 | `build_brush_solid_specialized` |
| `device/wgpu/binding.rs` | 116 | `brush_solid_layout` + bind group factory |
| `device/wgpu/buffer.rs` | 82 | storage / vertex / uniform buffer helpers |
| `device/wgpu/texture.rs` | 47 | `WgpuTexture` |
| `device/wgpu/format.rs` | 29 | `format_bytes_per_pixel_wgpu` |
| `device/wgpu/frame.rs` | 19 | encoder create / submit |
| `device/wgpu/readback.rs` | 69 | RGBA8 read-pixels for tests |
| `device/wgpu/shader.rs` | 10 | WGSL `include_str!` |
| `device/wgpu/mod.rs` | 22 | submodule glue |
| `device/wgpu/shaders/brush_solid.wgsl` | — | authored WGSL |
| `device/wgpu/tests.rs` | 584 | 8 device-side smoke + oracle tests |
| `renderer/mod.rs` | 83 | `Renderer { wgpu_device, wgpu_render_targets }`, `read_wgpu_render_target_rgba8`, `ensure_wgpu_render_target` |
| `renderer/init.rs` | 32 | `create_webrender_instance(handles, options)` |

**Receipt**: `cargo check -p webrender` clean, 6 warnings (all are
`pub fn` helpers awaiting a frame-builder consumer; no GL or
mismatch warnings); 8/8 wgpu device-side tests pass in 1.92s
(`render_rect_smoke`, `render_rect_alpha_smoke`, `oracle_blank_smoke`,
`pass_target_depth_smoke`, `wgpu_device_a1_smoke`,
`wgpu_device_a2_create_texture_smoke`,
`wgpu_device_a21_dither_create_upload_smoke`,
`core::boot_clear_readback_smoke`).

## What's portable from the deletion (lift on demand)

Useful algorithms that lived in deleted modules. Not blanket
restoration — lift the math out into wgpu-native callers as needed:

- **Geometry**: `space.rs` (coordinate system mappers), `transform.rs`
  (`TransformPalette`, `ScaleOffset`), `util.rs` (`extract_inner_rect_safe`,
  `Preallocator`, fast-transform helpers), `spatial_tree.rs`
  (spatial-node graph + transform composition)
- **Primitive math**: `quad.rs` (quad decomposition), `segment.rs`
  (segment generation for partial quads), `clip.rs` (clip rect
  composition), `ellipse.rs`, `border.rs`, `box_shadow.rs`
- **Picture-cache invalidation logic** (which tiles dirty, retain
  heuristic) from `tile_cache.rs` — *just* the invalidation; the
  storage layer was the GL artifact
- **Display list types** in `webrender_api` (still on disk, not in
  workspace) — these are wgpu-portable
- **WGSL shaders** under `device/wgpu/shaders/` (just `brush_solid`
  for now)
- **The composite trait shape** (audited under the prior plan,
  reshaped device-handle-free) — easy to restore when a layer
  compositor wires in

Lift functions out, don't blanket-restore modules. If the math is
40 lines in a 1,000-line file, take the 40 lines and leave the
indirection.

## What's next

Open. Three sketched directions, no commitment:

1. **Thin display-list-to-wgpu renderer for Servo.** Embedder hands
   in a display list + a wgpu surface; we render. Frame builder
   authored wgpu-native: primitives hold `wgpu::TextureView`s,
   render-task graph allocates `wgpu::Texture` synchronously,
   batches key on `wgpu::BindGroup` identity. Same algorithms as
   webrender, different data flow.
2. **Build something narrower than webrender.** Drop the
   render-task-graph indirection and the picture-cache layer
   entirely. Direct display-list → batches → draws. Loses
   webrender's tile-caching speedup but is much smaller. Useful if
   the consumer (Graphshell, etc.) doesn't need the perf scaling
   webrender targets.
3. **Stay at the skeleton until a consumer materializes.** Don't
   write more renderer code without a concrete display list + visible
   target to render against.

The receipt for picking among these is what the consuming embedder
actually needs. Not committing in this doc.

## Lessons

- **GL thread model leaks into data layout, not just API calls.**
  Removing GL calls is easy; removing the indirection tokens those
  calls drove is what required the architecture cut.
- **Audit-and-adopt at the module level preserves architecture
  under the rationale of "the math is portable."** It isn't, when
  the math holds indirection tokens.
- **"Maps cleanly to wgpu" is the test.** Not "could work with
  enough adaptation." If the type that survives is the same shape
  in both worlds, lift it. If the type changes shape, rewrite, don't
  retrofit.
