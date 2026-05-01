# P0 — `Device` Method Assignment Table

Date: 2026-04-30
Branch: `spirv-shader-pipeline`
Status: working doc — review and refine before touching code

Source: every `pub fn` inside `impl Device` in `webrender/src/device/gl.rs`
(block starts at line 1544; next impl ~4377). Verified count of `pub fn`
in the file is 168, of which ~110 are on `Device`; the rest belong to
`Texture`, `Program`, `ProgramCache`, `FormatDesc`, `ReadTarget`,
`DrawTarget`, `UploadPbo`, etc. and stay on those types.

Each method is assigned to one of:

- **`GpuFrame`** — frame lifecycle, capabilities, parameters, queries
- **`GpuShaders`** — program/pipeline/uniform-location lifecycle
- **`GpuResources`** — texture/buffer/sampler/FBO/PBO/VAO/VBO ownership + upload
- **`GpuPass`** — per-pass binding, state, draw, blit, readback
- **concrete-only** — stays on `GlDevice`, not on any trait (GL-internal
  helper, GL-typed return, or constructor)

`?` next to a method means the assignment is plausible but uncertain and
worth confirming during P0 implementation when call sites become visible.

---

## `GpuFrame` (~22 methods)

Frame lifecycle and global-ish state queries. Anything that the renderer
asks once per frame or once per device-creation belongs here.

| Method | Line | Notes |
|---|---|---|
| `begin_frame` | 2260 | Returns `GpuFrameId` |
| `end_frame` | 3867 | |
| `reset_state` | 2183 | |
| `set_parameter` | 2083 | Takes `&Parameter` |
| `clamp_max_texture_size` | 2112 | |
| `max_texture_size` | 2117 | |
| `surface_origin_is_top_left` | 2121 | |
| `get_capabilities` | 2125 | Returns `&Capabilities` |
| `preferred_color_formats` | 2129 | |
| `swizzle_settings` | 2133 | |
| `depth_bits` | 2141 | |
| `max_depth_ids` | 2151 | |
| `ortho_near_plane` | 2155 | |
| `ortho_far_plane` | 2159 | |
| `required_pbo_stride` | 2163 | |
| `upload_method` | 2167 | |
| `use_batched_texture_uploads` | 2171 | |
| `use_draw_calls_for_texture_copy` | 2175 | |
| `batched_upload_threshold` | 2179 | |
| `supports_extension` | 4142 | wgpu impl returns `false` for GL extension names |
| `echo_driver_messages` | 4146 | wgpu impl drains validation queue |
| `report_memory` | 4235 | Backend-specific accounting |
| `depth_targets_memory` | 4249 | |

---

## `GpuShaders` (~10 methods)

Program/pipeline ownership, uniform-location lookup, sampler binding setup.
"Program" maps to a wgpu `RenderPipeline` keyed by (SPIRV module, vertex
layout, baked state).

| Method | Line | Notes |
|---|---|---|
| `create_program` | 3152 | Returns `Program` |
| `create_program_linked` | 3136 | |
| `link_program` | 2528 | |
| `delete_program` | 3130 | |
| `get_uniform_location` | 3217 | Returns `UniformLocation` |
| `bind_shader_samplers` | 3200 | One-time per-program sampler unit setup |
| `compile_shader` ? | 2219 | Returns raw `GLuint` — concrete-only candidate; the trait should likely operate on `Program` only |

Three pass-state methods that touch programs are placed in `GpuPass` instead:
`bind_program`, `set_uniforms`, `set_shader_texture_size`. Reasoning: these
update active-pipeline state per draw call; in wgpu they map to
`set_pipeline`, uniform-buffer writes, and bind-group binding, all of
which are render-pass operations.

---

## `GpuResources` (~30 methods)

Resource ownership and upload. Anything that allocates a GPU object,
frees one, or moves bytes onto the GPU.

| Method | Line | Notes |
|---|---|---|
| `create_texture` | 2690 | |
| `delete_texture` | 3095 | |
| `delete_external_texture` | 3126 | |
| `copy_entire_texture` | 2812 | GPU-to-GPU copy |
| `copy_texture_sub_region` | 2834 | |
| `invalidate_render_target` | 2889 | |
| `invalidate_depth_target` | 2915 | |
| `reuse_render_target` | 2928 | Generic over `T: Texel` |
| `create_fbo` | 2480 | |
| `create_fbo_for_external_texture` | 2485 | |
| `delete_fbo` | 2504 | |
| `create_pbo` | 3250 | |
| `create_pbo_with_size` | 3258 | |
| `delete_pbo` | 3336 | |
| `create_vbo` | 3550 | Generic |
| `delete_vbo` | 3560 | Generic |
| `allocate_vbo` | 3588 | |
| `fill_vbo` | 3606 | |
| `create_vao` | 3565 | Takes `&VertexDescriptor` |
| `create_vao_with_new_instances` | 3637 | |
| `delete_vao` | 3576 | |
| `create_custom_vao` | 3519 | |
| `delete_custom_vao` | 3545 | |
| `update_vao_main_vertices` | 3657 | Generic |
| `update_vao_instances` | 3667 | Generic |
| `update_vao_indices` | 3725 | Generic |
| `upload_texture` | 3365 | Returns `TextureUploader` |
| `upload_texture_immediate` | 3380 | Generic |
| `map_pbo_for_readback` | 3307 | Returns `BoundPBO` |
| `attach_read_texture` | 3470 | |
| `attach_read_texture_external` | 3464 | |
| `required_upload_size_and_stride` | 3344 | Pure query — could equally live on `GpuFrame`; kept here next to the other upload paths |

Open question: `VAO`, `VBO<T>`, `PBO`, `CustomVAO`, and `Texture` are
currently concrete `gl.rs`-defined types. The trait needs associated types
so each backend can use its own representation, e.g. `type Vao;`,
`type Vbo<T>;`, `type Pbo;`, `type Texture;`. Generic associated types
(`type Vbo<T>: ...`) keep the generic-over-`T` upload signatures working.

---

## `GpuPass` (~40 methods)

Per-pass binding, state, draw commands, blits, readback. In wgpu, all of
these are operations on a `RenderPass` (or, for blit/readback, on the
encoder outside a pass). The wgpu impl will internally manage encoder /
pass lifetimes; the trait surface stays declarative.

| Method | Line | Notes |
|---|---|---|
| `bind_read_target` | 2410 | |
| `bind_read_target_impl` ? | 2396 | Looks like internal helper; concrete-only candidate |
| `reset_read_target` | 2430 | |
| `bind_draw_target` | 2442 | |
| `reset_draw_target` | 2436 | |
| `bind_external_draw_target` | 2508 | |
| `bind_program` | 2671 | wgpu: `set_pipeline` |
| `set_uniforms` | 3221 | wgpu: write to uniform buffer + bind group |
| `set_shader_texture_size` | 3236 | Per-pass uniform write |
| `bind_vao` | 3483 | wgpu: bind vertex/instance/index buffers |
| `bind_custom_vao` | 3487 | |
| `bind_texture` | 2370 | wgpu: bind group with texture view |
| `bind_external_texture` | 2383 | |
| `clear_target` | 3894 | Clears bound draw target |
| `enable_depth` | 3939 | Takes `DepthFunction` |
| `disable_depth` | 3945 | |
| `enable_depth_write` | 3949 | |
| `disable_depth_write` | 3954 | |
| `disable_stencil` | 3958 | |
| `set_scissor_rect` | 3962 | |
| `enable_scissor` | 3971 | |
| `disable_scissor` | 3975 | |
| `enable_color_write` | 3979 | |
| `disable_color_write` | 3983 | |
| `set_blend` | 3987 | Master enable/disable |
| `set_blend_mode` | (new) | One method, takes `BlendMode` enum (collapses 16 `set_blend_mode_*` methods at lines 4016-4110); GL impl matches on enum and dispatches to existing internal per-mode helpers, no behavior change |
| `draw_triangles_u16` | 3738 | |
| `draw_triangles_u32` | 3761 | |
| `draw_indexed_triangles` | 3820 | |
| `draw_indexed_triangles_instanced_u16` | 3843 | |
| `draw_nonindexed_points` | 3784 | |
| `draw_nonindexed_lines` | 3802 | |
| `blit_render_target` | 3053 | |
| `blit_render_target_invert_y` | 3073 | |
| `read_pixels` | 3400 | Reads from current draw target |
| `read_pixels_into` | 3412 | |
| `read_pixels_into_pbo` | 3275 | |
| `get_tex_image_into` | 3436 | Reads from a texture, not the draw target — could equally be `GpuResources`; kept here with the other read paths |

Blend-mode collapse confirmed (see Decisions section): one
`set_blend_mode(BlendMode)` replaces the 16 `set_blend_mode_*` methods.
GL impl keeps its existing internal helpers; the trait method dispatches
on the enum.

---

## Concrete-only — stays on `GlDevice`, not on any trait

| Method | Line | Reason |
|---|---|---|
| `new` | 1545 | Constructor — different signature per backend |
| `gl` | 2075 | Returns `&dyn gl::Gl` — GL-only |
| `rc_gl` | 2079 | Returns `&Rc<dyn gl::Gl>` — GL-only |
| `gl_describe_format` ? | 4177 | Returns `FormatDesc` — currently GL-specific; check renderer call sites |

If `gl_describe_format` turns out to be used cross-backend, lift it to
`GpuFrame` with a backend-neutral return type.

---

## Trait-shape decisions

### Decided

1. **Associated types + GATs, not generic parameters.** `Texture`, `Program`,
   `Vao`, `Pbo`, `CustomVao`, `TextureUploader`, `BoundPbo` are associated
   types on the relevant trait. Generic-over-`T` methods (`create_vbo<T>`,
   `update_vao_main_vertices<V>`, etc.) use GATs (`type Vbo<T>;`). Each
   backend picks one concrete type per associated type; consumers stay
   short (`fn foo<D: GpuResources>(d: &mut D)`). Pitfall noted: GATs
   restrict object-safety, but we'll use trait bounds, not `dyn`, so this
   doesn't bite.

2. **Trait hierarchy via supertraits.** `GpuPass` needs `Program`,
   `Texture`, `Vao` to express its bind methods, and those associated
   types live on `GpuShaders` and `GpuResources`. So:

   ```rust
   pub trait GpuFrame { /* ... */ }
   pub trait GpuResources: GpuFrame { /* ... */ }
   pub trait GpuShaders: GpuFrame { /* ... */ }
   pub trait GpuPass: GpuShaders + GpuResources { /* ... */ }
   ```

   Consumer that wants the full surface bounds on `GpuPass`. Backends
   implement all four. Splits stay meaningful: a renderer module that only
   cares about resources can write `<D: GpuResources>` and won't have
   pass-state methods in scope.

3. **Blend-mode collapse.** Replace the 16 `set_blend_mode_*` methods with
   one `set_blend_mode(BlendMode)` taking an enum (we already have
   `MixBlendMode` for the advanced family — extend or wrap it). The GL
   impl can keep its existing internal per-mode functions, just match on
   the enum and dispatch. Renderer call-site change is mechanical: the
   ~one place that calls `set_blend_mode_premultiplied_alpha()` becomes
   `set_blend_mode(BlendMode::PremultipliedAlpha)`.

4. **`bind_program` placement.** `GpuPass`. Reasoning in conversation:
   matches wgpu's `RenderPass::set_pipeline`, keeps the draw-call sequence
   (`bind_program → set_uniforms → bind_texture → bind_vao → draw_*`) on
   one trait, and `bind_*` methods that take resource references are the
   established pattern (`bind_texture(&Texture)`, `bind_vao(&VAO)`).

### Deferred to implementation

1. **`ProgramCache` access on the trait.** Currently passed in via
   `Device::new`. Wgpu backend will want a pipeline cache too. Decide
   shape (trait method, associated type, or shared concrete type) when
   adding the wgpu skeleton in P1 — easier with two impls visible.

2. **`upload_texture` returning `TextureUploader<'a>`.** Lifetime ties
   uploader to `&mut self`. With GAT lifetimes
   (`type TextureUploader<'a> where Self: 'a;`) this works. Confirm during
   actual method move.

3. **`gl_describe_format` cross-backend usage.** Check renderer call sites
   when moving — if used cross-backend, lift to `GpuFrame` with a
   backend-neutral return type; otherwise leave concrete-only on
   `GlDevice`.

---

## P0a receipts (post-implementation findings)

Captured after the four impl commits landed on `Device`. Use these to plan
P0b/c/d and the wgpu-side work.

### R1. Delegation pattern is not permanent

Every trait method body is `Device::method(self, ...)` — i.e. the trait impl
calls the inherent method. Inherent and trait methods coexist, both defined.
Justified for P0a (zero risk to existing renderer call sites) but doubles
the surface. Folds away later when (a) renderer code migrates to trait
bounds and (b) the wgpu impl lands. At that point each method body either
moves into the trait impl with the inherent removed, or stays as
delegation if the inherent has GL-specific signature gaps not on the trait.
Reversible. Noted as not-permanent so future readers don't enshrine it.

### R2. GL-typed values in trait signatures: lift vs. generalize

Eight types currently leak from `gl.rs` into trait signatures. Two
strategies for backend-neutralizing them:

| Type | Strategy | Notes |
|---|---|---|
| `DepthFunction` | **Lift** to backend-neutral module | Pure enum; maps directly to `wgpu::CompareFunction` |
| `TextureFilter` | **Lift** | Pure enum; maps to `wgpu::FilterMode` |
| `TextureSlot` | **Lift** | Wraps `usize`; pure data |
| `Texel` | **Lift** | Trait for raw-byte conversion; implementation-agnostic |
| `FBOId` | **Associated type** (`type RenderTargetHandle;`) | Wraps `gl::GLuint`; wgpu has no FBO concept (just textures) |
| `ReadTarget` | **Associated type** | Variants reference `FBOId`; cascades from above |
| `DrawTarget` | **Associated type** | Same — variants reference `FBOId` |
| `Stream<'a>` | **Associated type (GAT)** | Contains `VBOId` (GL handle); wgpu equivalent is buffer + layout pair |
| `ExternalTexture` | **Associated type** | Currently used in `bind_external_texture` on trait + `delete_external_texture` inherent-only — split is inconsistent; lift via `type ExternalTexture;` when wgpu impl lands |

So 4 lift cleanly (move to a new `device/types.rs` or similar), 4 need
associated types with cascading effects on dependent enums. Defer until P1
(wgpu skeleton) when both impls are visible — easier to design the
associated-type shape with two consumers than one.

### R3. `attach_read_texture_external` and `delete_external_texture` callers confirmed

Grep for callers (excluding the trait doc comment):

- `attach_read_texture_external` → [`webrender/src/renderer/mod.rs:4904`](../webrender/src/renderer/mod.rs#L4904) (`self.device.attach_read_texture_external(gl_id, target)`)
- `delete_external_texture` → [`webrender/src/renderer/mod.rs:4533`](../webrender/src/renderer/mod.rs#L4533) (`self.device.delete_external_texture(ext)`)

Both are called from cross-backend renderer code (`renderer/mod.rs`), not
GL-specific paths. Implications:

- For P0 (GL-only) they work as inherent methods; renderer call sites
  resolve against the concrete `Device`.
- When wgpu impl lands, these call sites either need (a) associated-type
  generalization on the trait (per R2 — `type ExternalTexture` solves
  `delete_external_texture`; `type ExternalTextureId` solves
  `attach_read_texture_external`), or (b) cfg-gating to backend-specific
  branches in `renderer/mod.rs`.
- (a) is cleaner if external-texture interop has a coherent wgpu equivalent
  (it does: `wgpu::Texture` for owned, `wgpu::TextureView` for views).

### R4. GAT lifetime asymmetry verified

Confirmed by reading struct definitions:

- `BoundPBO<'a>` (gl.rs:717) borrows from `&'a mut self` *and* `&'a PBO` —
  the pbo's pointer is GL-bind-state-dependent. Trait GAT correctly
  declared as `type BoundPbo<'a> where Self: 'a;`.
- `TextureUploader<'a>` (gl.rs:4632) contains only `pbo_pool: &'a mut UploadPBOPool`
  and a `Vec<PixelBuffer<'a>>`. It does not borrow `self`. Trait GAT
  correctly declared without `where Self: 'a`.

Trait signatures preserve the inherent semantics exactly; no GAT bug
hiding here.

### R5. BlendMode enum shape recommendation (for P0b)

Three candidate shapes for the collapse:

**Option A — Flat enum (recommended).** One variant per existing method,
mechanical 1:1 mapping for renderer call sites:

```rust
// illustrative
pub enum BlendMode {
    Alpha,
    PremultipliedAlpha,
    PremultipliedDestOut,
    Multiply,
    SubpixelPass0,
    SubpixelPass1,
    SubpixelDualSource,
    MultiplyDualSource,
    Screen,
    PlusLighter,
    Exclusion,
    ShowOverdraw,
    Max,
    Min,
    Advanced(MixBlendMode),
}
fn set_blend_mode(&mut self, mode: BlendMode);
```

15 variants (one is parameterized). Renderer edit pattern: replace
`device.set_blend_mode_premultiplied_alpha()` with
`device.set_blend_mode(BlendMode::PremultipliedAlpha)`. Diff is large
but mechanical.

**Option B — Hierarchical enum** (groups subpixel modes):

```rust
// illustrative
pub enum BlendMode {
    Alpha, PremultipliedAlpha, PremultipliedDestOut, Multiply,
    Screen, PlusLighter, Exclusion, Max, Min, ShowOverdraw,
    Subpixel(SubpixelMode),
    Advanced(MixBlendMode),
}
pub enum SubpixelMode { Pass0, Pass1, DualSource, Multiply }
```

Pro: groups related modes; clearer intent. Con: nested call sites
(`BlendMode::Subpixel(SubpixelMode::Pass0)`); only 4 subpixel modes,
grouping doesn't pay much.

**Option C — Two-method split** — keep `set_blend_mode_advanced(MixBlendMode)`
separate, fold the other 15 into one `set_blend_mode(SimpleBlendMode)`.
Pro: discriminates simple vs. parameterized at the type level. Con:
partially undoes the collapse benefit.

Recommendation: **Option A**. Simplest, most direct, lowest cognitive
overhead. The grouping in Option B doesn't earn its complexity given the
small subpixel cohort. Option C splits work that should naturally unify.

## P0a trait surface — skim summary

Snapshot of what landed in P0a (commits `e08462a08` → `e69fffc48`). Use this
as a quick reference instead of reading the full per-trait method tables
above.

### Hierarchy

```text
GpuFrame                                          ← lifecycle, capabilities, queries
  ├── GpuResources: GpuFrame                      ← texture/buffer/sampler/FBO/PBO/VAO ownership
  ├── GpuShaders:   GpuFrame                      ← program/uniform-location lifecycle
  └── GpuPass:      GpuShaders + GpuResources     ← per-pass binding, state, draw
```

`Device` (in `gl.rs`) implements all four; renderer code untouched and
continues to call inherent methods (Rust prefers inherent over trait in
method resolution).

### `GpuFrame` — 23 methods, 0 assoc types

Lifecycle (`begin_frame` → `GpuFrameId`, `end_frame`, `reset_state`),
parameters (`set_parameter`), 16 capability/config queries (`get_capabilities`,
`max_texture_size`, `swizzle_settings`, `supports_extension`, depth/ortho,
upload config, etc.), 3 diagnostics (`echo_driver_messages`, `report_memory`,
`depth_targets_memory`).

### `GpuShaders` — 6 methods, 2 assoc types

Assoc: `type Program;`, `type UniformLocation;`.

Methods: `create_program`, `create_program_linked`, `link_program`,
`delete_program`, `get_uniform_location`, `bind_shader_samplers<S>`. Three
program-state methods (`bind_program`, `set_uniforms`,
`set_shader_texture_size`) live on `GpuPass` instead — per-draw, not
lifecycle.

### `GpuResources` — 30 methods, 7 assoc types (3 GATs)

Assoc: `type Texture;`, `type Vao;`, `type CustomVao;`, `type Pbo;`, plus
GATs `type Vbo<T>;`, `type BoundPbo<'a> where Self: 'a;`,
`type TextureUploader<'a>;` (no `where Self: 'a` — borrows from `pbo_pool`).

Texture (7): `create_texture`, `delete_texture`, `copy_entire_texture`,
`copy_texture_sub_region`, `invalidate_render_target`,
`invalidate_depth_target`, `reuse_render_target<T: Texel>`.
FBO (3): `create_fbo`, `create_fbo_for_external_texture`, `delete_fbo`.
PBO (3): `create_pbo`, `create_pbo_with_size`, `delete_pbo`.
VAO (5): `create_vao`, `create_vao_with_new_instances`, `delete_vao`,
`create_custom_vao`, `delete_custom_vao`.
VBO (4, generic): `create_vbo<T>`, `delete_vbo<T>`, `allocate_vbo<V>`,
`fill_vbo<V>`.
VAO updates (3): `update_vao_main_vertices<V>`,
`update_vao_instances<V: Clone>`, `update_vao_indices<I>`.
Upload (3): `upload_texture<'a>`, `upload_texture_immediate<T: Texel>`,
`map_pbo_for_readback<'a>`.
Misc (2): `attach_read_texture`, `required_upload_size_and_stride`.

### `GpuPass` — 47 methods, no new assoc types

Render targets (5): `bind_read_target`, `reset_read_target`,
`bind_draw_target`, `reset_draw_target`, `bind_external_draw_target`.
Programs (3, per-pass): `bind_program`, `set_uniforms`,
`set_shader_texture_size`.
Bindings (4): `bind_vao`, `bind_custom_vao`, `bind_texture<S>`,
`bind_external_texture<S>`.
Clears (1): `clear_target`.
Depth/stencil (5): `enable_depth(DepthFunction)`, `disable_depth`,
`enable_depth_write`, `disable_depth_write`, `disable_stencil`.
Scissor/colour-write (5): `set_scissor_rect`, `enable_scissor`,
`disable_scissor`, `enable_color_write`, `disable_color_write`.
Blend (17, P0b collapses 16 → 1): `set_blend(bool)` plus 16
`set_blend_mode_*` methods.
Draw (6): `draw_triangles_u16/u32`, `draw_indexed_triangles`,
`draw_indexed_triangles_instanced_u16`, `draw_nonindexed_points/lines`.
Blits (2): `blit_render_target`, `blit_render_target_invert_y`.
Readback (4): `read_pixels`, `read_pixels_into`, `read_pixels_into_pbo`,
`get_tex_image_into`.

### Concrete-only on `Device`

- `new` — constructor, signature differs per backend
- `gl()`, `rc_gl()` — GL context access
- `gl_describe_format` — returns GL-specific `FormatDesc` (cross-backend
  usage not yet checked)
- `attach_read_texture_external` — raw `gl::GLuint` argument; called from
  `renderer/mod.rs:4904`
- `delete_external_texture` — `cfg(feature = "replay")`; called from
  `renderer/mod.rs:4533`

### Non-obvious decisions (cross-reference)

| # | Decision | See |
| --- | --- | --- |
| 1 | Delegation pattern (inherent + trait both exist) | R1 |
| 2 | 8 GL-typed values in trait signatures (4 lift, 4 → assoc types) | R2 |
| 3 | 2 inherent-only methods called from cross-backend renderer code | R3 |
| 4 | GAT lifetime asymmetry verified | R4 |
| 5 | 16 raw blend-mode methods kept on trait for P0a; collapse in P0b | R5 |

### Counts

| Trait | Methods | Assoc types |
| --- | --- | --- |
| `GpuFrame` | 23 | 0 |
| `GpuShaders` | 6 | 2 |
| `GpuResources` | 30 | 7 (3 GATs) |
| `GpuPass` | 47 | 0 (inherited) |
| **Trait surface total** | **106** | **9** |
| Concrete-only on `Device` | 5 | — |

P0b's blend-mode collapse drops `GpuPass` from 47 → 32 methods (15 fewer
trait entries; the inherent methods stay as internal dispatchers).
