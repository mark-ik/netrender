# Session Brief — 2026-04-28 / 2026-04-29

State of the `idiomatic-wgpu-pipeline` branch after the 2026-04-29
adapter-groundwork commit, the A2.X.5 misstep + revert, and the
refactor to a pipeline-first migration plan. Snapshot for orientation;
actionable sequencing lives in the new plan.

---

## Where we're at

**Branch shape**: `idiomatic-wgpu-pipeline` off `upstream/upstream`,
tracking `origin/idiomatic-wgpu-pipeline`. P0 (webrender side) just
landed. Pre-P0 tag `pre-p0` at `aa1850ed7` for servo-wgpu pinning.

**Plans in play**:

- **Parent plan**:
  [`2026-04-28_idiomatic_wgsl_pipeline_plan.md`](2026-04-28_idiomatic_wgsl_pipeline_plan.md).
  Jump-ship to a clean wgpu-native fork of `upstream/upstream`.
  Authored WGSL only, no GL backend, no SPIR-V intermediate, no
  artifact pipeline. Architecture patterns §4.6–4.11.
- **Active migration plan**:
  [`2026-04-29_pipeline_first_migration_plan.md`](2026-04-29_pipeline_first_migration_plan.md).
  Pipeline-first, family-by-family. Embedder owns the wgpu device.
  Data-as-texture carriers get deleted, not migrated. Per-family
  draw-loop dispatch during transition; D phase deletes GL.
- **Superseded follow-up**:
  [`2026-04-28_renderer_body_wgpu_adapter_plan.md`](2026-04-28_renderer_body_wgpu_adapter_plan.md).
  Textures-first ordering preserved GL anti-patterns; "narrowest
  first callsite" was a fiction. A1 / A2.X.0–4 / A2.3.0 work landed
  on it survives intact as the wgpu-native foundation.

**What landed (still real after the refactor)**:

| Slice | Status | Receipt |
|---|---|---|
| Main S0 | ✅ | branch + version bump (0.68.0) + 6 prior plans superseded + push |
| Main S1 | ✅ | `boot_clear_readback_smoke` — wgpu boot + 4×4 clear + readback |
| Main S2 | ✅ | `render_rect_smoke` exercising §4.6 storage / §4.7 uniform+immediate / §4.8 record+flush / §4.9 override |
| Main S3 | ✅ | 5 oracle PNGs at 3840×2160 from upstream/0.68 + GL via wrench, sibling worktree |
| Main S4 | ⏳ 1/5 | `oracle_blank_smoke` matches `blank.png` exactly, tolerance 0; remaining 4 gated on new plan phase D |
| Foundational A1 | ✅ | `WgpuDevice` fulcrum; `ensure_brush_solid` lazy-cache pattern |
| Foundational A2.X.0–4 | ✅ | `pass.rs` (DrawIntent / RenderPassTarget / depth attachment / encode_pass bridge / encoder lifecycle) |
| Foundational A2.0 / A2.1.0 | ✅ | `WgpuTexture` + create / upload + format map (kept for legit textures only) |
| Foundational A2.3.0 | ✅ | `WgpuDevice::read_rgba8_texture`; oracle harness uses it |
| ❌ A2.X.5 | reverted | independent `WgpuDevice::boot()` was a hack — embedder must own the wgpu device. Reverted as `40661cd22`; original `ad655dc09` preserved in branch history. |
| **P0 — Embedder wgpu handoff** | ✅ webrender side | `WgpuHandles` struct, `WgpuDevice::with_external`, `Renderer.wgpu_device`, new `create_webrender_instance(gl, wgpu, …)` signature, `RendererError::WgpuFeaturesMissing`. `core::boot()` + `WgpuDevice::boot()` gated behind `cfg(test)`. Servo-wgpu side outstanding (pin against `pre-p0` tag until its call-site updates). |
| **P1.1 — brush_solid storage-buffer shape** | ✅ | brush_solid WGSL now reads from `PrimitiveHeader` + `gpu_buffer_f` storage buffers (mirrors GL `prim_shared.glsl::PrimitiveHeader` collapsed into one std430 struct, plus `fetch_from_gpu_buffer_*`'s `vec4<f32>` table). `ALPHA_PASS` WGSL override replaces `MAX_PALETTE_ENTRIES`. `DrawIntent::uniform_offset` → `dynamic_offsets: Vec<u32>`. |
| **P1.2 — Transform storage buffer** | ✅ | `Transform { m, inv_m }` storage at bind slot 1 (mirrors GL `transform.glsl::Transform`, 128 bytes std430). Vertex shader applies `transforms[header.transform_id & 0x003fffff].m` to the corner. Smoke feeds identity matrices. |

**Concrete artifacts**:

- 11-module `webrender/src/device/wgpu/` scaffold (mod, core,
  format, buffer, texture, shader, binding, pipeline, pass, frame,
  readback, adapter)
- Wgpu module owns: device boot (test only), lazy brush pipeline
  cache, texture create / upload, command encoder lifecycle, pass
  replay (`encode_pass`), and RGBA8 readback staging
- 7 wgpu device-side tests passing in ~2s
- 5 captured oracle PNG / YAML pairs in `webrender/tests/oracle/`
- A reusable oracle harness (`load_oracle_png`, `count_pixel_diffs`)
  plus adapter-backed readback (`WgpuDevice::read_rgba8_texture`)
- A `webrender-wgpu-oracle` worktree on `upstream/0.68` with a
  local-only wrench patch for clap 3 compatibility (documented in
  the oracle README)
- ~10 wgpu 29 surface-API gotchas captured across S2 / A1 / A2 plan
  sections (`PUSH_CONSTANTS`→`IMMEDIATES`, `var<push_constant>`→
  `var<immediate>`, `RenderPassColorAttachment::depth_slice` and
  `multiview_mask`, `PushConstantRange`→`immediate_size`,
  `bind_group_layouts` now sparse, etc.)

---

## Where we're going

**Critical path** (per new plan §6):

1. ~~**P0 — Embedder wgpu handoff.**~~ ✅ webrender side landed
   2026-04-29. Servo-wgpu side outstanding — pins against `pre-p0`
   tag (`aa1850ed7`) until its call-site updates land.
2. **P1 — `brush_solid` end-to-end pilot.** Sub-slices (P1.1 ✅,
   P1.2 → P1.9). First family migrated.
   Largest single slice; forces every architectural decision in
   parent §4.6–4.11 to land at once: storage-buffer reshape of
   `gpu_cache` / `transforms` / `prim_headers` for `brush_solid`'s
   consumption, §4.7 uniform hierarchy in production, §4.8
   record-then-flush in the renderer body, §4.9 override
   specialization (alpha vs. opaque), §4.11 async pipeline compile +
   on-disk cache, render-target attachment, per-family draw-loop
   dispatch. Receipt: `brush_solid`-only oracle scene matches.
3. **P2 — Brush family expansion.** `brush_image`, `brush_blend`,
   `brush_mix_blend`, `brush_linear_gradient`, `brush_opacity`,
   `brush_yuv_image`. Sampled-texture machinery, sampler cache,
   texture-cache integration as `wgpu::Texture`. `ExternalTexture`
   materializes here.
4. **P3 — `ps_quad` family.** Textured / gradient / radial / conic /
   mask / mask-fast-path quads. More override specialization.
5. **P4 — Clip-mask family** (`cs_clip_*`). First cache-task render
   pass (clip masks → render-target textures used as inputs to
   subsequent draws). Depth attachment policy under realistic load.
6. **P5 — Cache-task family** (gradient / blur / scale / svg-filter).
7. **P6 — Border / line cache tasks.**
8. **P7 — Text family** (`ps_text_run` + dual-source). Glyph atlas
   as `wgpu::Texture` (or texture array per parent §S6 sub-task).
9. **P8 — Composite / debug / utility.** After P8, every shader
   family runs through wgpu; GL branch still compiled but unreached.
10. **D — Delete GL backend.** `gl.rs`, `gleam` dep, GLSL sources,
    `swgl/`, VAO / VBO / PBO / FBO / Program / ProgramCache /
    Capabilities, `dither_matrix_texture`, data-carrier 2D
    textures (`gpu_buffer_texture_f/i`, `transforms_texture`,
    `prim_header_texture`, `vertex_data_textures`),
    `Renderer::device: Device`, `gl: Rc<dyn gl::Gl>` parameter.
    Per-family dispatch collapses to a single wgpu path.

**Honest scope estimate**: P0 is days. P1 is multi-week (it lands
the entire architectural pattern). P2..P8 are multi-day to
multi-week each, parallelizable in places. D is days once everything
is migrated.

---

## Fruitful sidequests

Things that aren't on the critical path but unblock, accelerate, or
de-risk later work:

1. **Servo-wgpu integration verification before P0.** Confirm Servo's
   wgpu device shape matches the `WgpuHandles` we plan to take from
   it. Cheaper to surface mismatches now than at P0 entry.
2. **WebGPU CTS gate (Main S5).** Runs alongside P slices without
   conflict. Target a small conformance lane first: buffers,
   render_pass, bind_groups, blend, depth_stencil, vertex_state.
3. **WGSL `override` variant collapse exploration.** Author one
   duplicate shader-family pair as override-specialized WGSL. Validates
   the §4.9 plan without touching renderer control flow. P1 exercises
   this anyway, but ahead-of-time recon is cheap.
4. **`wgpu::PipelineCache` spike** (§4.11). The on-disk cache mechanic
   lands in P1; a small standalone spike de-risks the cache key shape
   decision (open question Q4).
5. **Oracle harness hardening.** Keep `blank` exact, but design the
   tolerance / reporting shape for non-blank scenes before the
   remaining four S4 images come online via P slices.
6. **`RenderBundle` experiment for tile replay** (parent §Q12,
   pipeline-first plan §10 future). Potential frame-time win after
   picture-cache rendering is reachable; not blocking.

---

## Potential pitfalls

1. **Each P slice is multi-day to multi-week.** Renderer-body surgery
   against a 5,316-LOC god object. The work has moved out of design
   and into careful surgery; expect fewer lines per turn and more
   compile / debug cycles per slice.
2. **P0 changes Servo's call site.** Coordinate via a pre-P0 tag on
   `idiomatic-wgpu-pipeline` for servo-wgpu to pin against until both
   sides land.
3. **No GL-shaped compatibility layer.** The plans intentionally
   reject a wgpu-backed clone of `gl.rs::Device`. A2.X.5's revert
   was the most visible reminder; the same trap waits at every
   migration site that wants to "just shim the GL shape." Don't.
4. **No data-carrier preservation.** `dither_matrix_texture` is the
   poster child: 64-byte 8×8 R8 tables don't become `WgpuTexture`,
   they become WGSL `const` (or get inlined into the shaders that
   read them). Same logic for `gpu_cache`, `transforms`,
   `prim_headers`, `gpu_buffer_texture_f/i`, `vertex_data_textures`.
   These are storage buffers in idiomatic wgpu, not 2D textures.
5. **Per-family dispatch is transitional code.** It lives across
   P1..P8 and gets slimmer with each slice. D collapses it. Until D,
   both backends compile and the dispatch `match` exists.
6. **Storage-buffer size limits.** `max_storage_buffer_binding_size`
   typically 128 MB portable. `gpu_cache` may push this; existing
   paging logic carries forward but the access path changes.
   Sized at P1 entry.
7. **Depth / clear semantics must stay explicit.** wgpu load / store
   ops are pass-begin decisions. GL-style late clears and
   `invalidate_depth_target()` calls become `RenderPassTarget`
   policy or the migration accidentally preserves mutable
   framebuffer state in a new disguise.
8. **Servo-wgpu may break during renderer-body edits.** Keep
   checkpoints green, coordinate pinning. Pitfall #2 is the
   specific instance for P0; this is the recurring version.
9. **Oracle PNGs are platform-dependent.** Current exact match is
   only proven for `blank` on the capture machine. Non-blank scenes
   may need documented tolerances; text / image scenes still need
   asset / font handling.
10. **wgpu API churn remains real.** wgpu 29 already produced
    ~10 surface-API gotchas. Future major bumps can move the ground
    under the adapter; keep version notes close to code.
11. **Scope gravity.** Glyph arrays, RenderBundles, pipeline cache
    deep-dives, CTS, servo smoke — all tempting adjacent work. GL
    deletion (D) is the real finish line. Sidequests should
    de-risk migration or stay explicitly optional.

---

## Bottom line

Design is over; the wgpu module foundation is in place and seven
tests are green. **P0 (embedder wgpu handoff) landed on the
webrender side 2026-04-29**: `create_webrender_instance` now takes a
`WgpuHandles` parameter, `Renderer.wgpu_device` is installed via the
embedder's wgpu primitives (no second boot), `RendererError::WgpuFeaturesMissing`
surfaces adapter mismatches, and `boot()` is `cfg(test)` only. The
renderer body has not yet been migrated — `Renderer.device: Device`
(GL) is still what every callsite uses. `gl.rs` still owns the
GPU surface for production rendering.

The next milestone is P1 (`brush_solid` end-to-end through the
renderer body) — the architectural-pattern slice that the rest of
the plan reuses. Forces every §4.6–4.11 decision to land at once
for one family. Multi-week.

The project's three principles — idiomatic wgsl/wgpu backend, no
hacks, no unnecessary GL structure carryover — are the bar the
new plan is calibrated against.
