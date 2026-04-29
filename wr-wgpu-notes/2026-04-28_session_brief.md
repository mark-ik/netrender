# Session Brief — 2026-04-28

State of the `idiomatic-wgpu-pipeline` branch at the close of the
2026-04-28 session. Snapshot for orientation; not a plan.

---

## Where we've come from

**Branch shape**: `idiomatic-wgpu-pipeline` off `upstream/upstream`,
with 11 session commits on top of the user's prior
`82faa2fb4 the notes are back in town`. HEAD: `6ebd3e638`.

**Plans landed in session**:

- **Main plan**:
  [`2026-04-28_idiomatic_wgsl_pipeline_plan.md`](2026-04-28_idiomatic_wgsl_pipeline_plan.md).
  Jump-ship from `spirv-shader-pipeline` to a clean wgpu-native fork
  of `upstream/upstream`. Authored WGSL only, no GL backend, no SPIR-V
  intermediate, no artifact pipeline. Architecture patterns §4.6–4.11.
  Six prior plans superseded.
- **Adapter plan**:
  [`2026-04-28_renderer_body_wgpu_adapter_plan.md`](2026-04-28_renderer_body_wgpu_adapter_plan.md).
  Spawned at S4-1/5 closure when recon quantified the renderer-body
  integration scope (169 self.device.* callsites, 57 methods,
  ~11.6k LOC). Slices A0–A8 walk webrender's renderer body from a
  GL-shaped `Device` boundary to a wgpu-native `WgpuDevice` boundary.

**Slices closed**:

| Slice | Status | Receipt |
|---|---|---|
| Main S0 | ✅ | branch + version bump (0.68.0 across the workspace) + 6 plans superseded + push |
| Main S1 | ✅ | `boot_clear_readback_smoke` — wgpu boot + 4×4 clear + readback |
| Main S2 | ✅ | `render_rect_smoke` exercising §4.6 storage / §4.7 uniform+immediate / §4.8 record+flush / §4.9 override |
| Main S3 | ✅ | 5 oracle PNGs at 3840×2160 from upstream/0.68 + GL via wrench, sibling worktree |
| Main S4 | ⏳ 1/5 | `oracle_blank_smoke` matches `blank.png` exactly, tolerance 0; remaining 4 gated on adapter A8 |
| Adapter A0 | ✅ | translation table for ~25 GL-shaped types in plan §11 |
| Adapter A1 | ✅ | `WgpuDevice` fulcrum; `ensure_brush_solid` lazy-cache pattern |
| Adapter A2.0 | ✅ | `WgpuTexture` + `WgpuDevice::create_texture(&TextureDesc)` |
| Adapter A2.1.0 | ✅ | `WgpuDevice::upload_texture`; `image_format_to_wgpu`; surfaced A2.X dependency |
| Adapter A2.X.0 | ✅ | `pass.rs` refactored: `DrawIntent` carries pipeline+bind_group, `flush_pass` takes `Option<Color>` |

**Concrete artifacts**:

- 11-module `webrender/src/device/wgpu/` scaffold (mod, core,
  format, buffer, texture, shader, binding, pipeline, pass, frame,
  readback, adapter)
- 7 wgpu tests passing in 1.84s
- 5 captured oracle PNG/YAML pairs in `webrender/tests/oracle/`
- A reusable load-render-diff harness (`load_oracle_png`,
  `readback_target`, `count_pixel_diffs`)
- A `webrender-wgpu-oracle` worktree on `upstream/0.68` with a
  local-only wrench patch for clap 3 compatibility (documented in
  the oracle README)
- ~10 wgpu 29 surface-API gotchas captured across S2 / A1 / A2 plan
  sections (PUSH_CONSTANTS→IMMEDIATES, var<push_constant>→
  var<immediate>, RenderPassColorAttachment::depth_slice and
  multiview_mask, PushConstantRange→immediate_size,
  bind_group_layouts now sparse, etc.)

---

## Where we're going

**Critical path (the real engineering)**:

1. **Adapter A2.X.1+ — renderer/* per-callsite pass-encoding migration**.
   The next slice that actually edits `renderer/mod.rs` lines.
   Foundational because the bind sites for every texture-lifecycle
   migration depend on wgpu-native pass encoding. Multi-turn.
2. **Adapter A2.1, A2.2, A2.5 — texture-lifecycle migrations**.
   Once A2.X is in place, dither / zoom-debug / blit migrations drop
   in. Each is a per-callsite chunk.
3. **Adapter A3–A7 — vertex / pipeline / render-target / upload /
   query path migrations**. Same pattern as A2: design seed first
   (already partly done in `wgpu/*` modules), per-callsite migration
   second.
4. **Adapter A8 — re-export flip**. `device/mod.rs` switches from
   `pub use self::gl::*;` to `pub use self::wgpu::*;`. Compile errors
   light up remaining residue. Once green, **closes parent plan §S4**:
   the remaining four oracle scenes start passing as the renderer
   body now flows through `WgpuDevice`.
5. **Main S5 — WebGPU CTS gate**. Independent of S4; can run in
   parallel.
6. **Main S6 — full shader corpus** (~50 WGSL shaders). Gated on
   §S4 closure for visual receipts; corpus authoring can begin
   anytime after S5.
7. **Main S7 — servo-wgpu integration smoke**.
8. **Main S8 — external corpus coverage** (WPT slice or Interop subset).
9. **Main S9 — delete GL** (gleam, gl.rs, query_gl.rs, glsl-to-cxx,
   swgl, authored GLSL tree). The receipt that the jump-ship is real.

**Honest scope estimate**: A2.X.1+ through A8 is multi-week to
multi-month engineering work. The branch's design phase (this session)
is closing; the implementation grind is the next phase.

---

## Fruitful sidequests

Things that aren't on the critical path but unblock, accelerate, or
de-risk later work. Pickable in any order, mostly independent:

1. **WebGPU CTS gate (Main S5)** — runs alongside renderer-body
   migration without conflict. Adds an API-correctness gate distinct
   from the pixel-correctness gate S4 covers. Targeted subset:
   buffers, render_pass, bind_groups, blend, depth_stencil,
   vertex_state. Concrete deliverable: `cargo test --test cts_subset`.
2. **WGSL `override` variant collapse exploration**. Today
   `WgpuShaderVariant::ALL` enumerates ~50 variants. Many differ only
   by parameter (alpha vs. opaque, fast-path vs. full,
   dual-source toggle). Authoring one variant pair as
   override-specialized in S6 style validates the §4.9 plan early —
   no renderer body changes.
3. **Parallel mini-renderer for the remaining four S4 oracle
   scenes**. Ships visible S4 progress without waiting for the
   renderer body migration. Costs: violates §5 spirit ("renderer
   body adapts to wgpu, not parallel paths"); may need an explicit
   revision of §5 to land. Option (B) from the strategic question
   I surfaced.
4. **Async pipeline compilation + `wgpu::PipelineCache`** (§4.11).
   Wire up disk-backed pipeline cache so second-run boots are fast.
   Setup work; pays off when S6's corpus expands.
5. **Servo-wgpu integration verification**. The sibling repo
   `c:\Users\mark_\Code\repos\servo-wgpu` patches webrender to our
   local path. With our 0.68 version bumps, the patch should resolve;
   confirm `cargo check -p servo` (or whatever entry point) still
   compiles. Defends against silent breakage during renderer-body
   migration.
6. **`wgpu::RenderBundle` for picture-cache tile replay** (Main §Q12,
   adapter §Q4). Investigate as a separate experiment; potential
   frame-time win once the renderer body's picture cache is reachable.
7. **Texture-array glyph cache** (Main §Q11). Move the glyph atlas
   from single-texture to layered texture array. Optimization; not
   blocking. Ahead-of-time for when text scenes come back online.
8. **Push to origin**. Haven't pushed any of the 11 session commits.
   `git push origin idiomatic-wgpu-pipeline`. One-time, low-risk,
   makes the work visible / shareable / backed up.

---

## Potential pitfalls

Things that could invalidate work or stall progress:

1. **Renderer body migration impedance is bigger than the recon
   measured.** 169 callsites + 57 methods quantifies the
   *callsites*, not the *interdependence*. Many state-machine assumptions
   don't have wgpu-native equivalents (e.g., `bind_texture`'s
   per-call binding model). Each "small" migration may cascade
   further than scoping suggested.
2. **§5 "no GL-shape preserved" + "no parallel paths" makes
   incremental migration mechanically hard.** Forced into atomic
   per-chunk edits where each commit has a green build. Prevents
   easy-but-wrong intermediate states. Slow.
3. **Servo-wgpu integration may break mid-migration.** Servo-wgpu
   patches webrender to `../webrender-wgpu/webrender`. Renderer-body
   work in flight may temporarily break Servo's build until the
   migration is consistent. Coordinate with servo-wgpu side; consider
   tagging a stable pre-migration commit for them to pin until A8
   lands.
4. **Oracle PNGs are platform-dependent.** Captured on NVIDIA
   RTX 4060 / OpenGL 3.2 / driver 591.86 / Windows 11. Different
   hardware (different drivers, AMD, Intel, MoltenVK) may produce
   subtly different output. Tolerance policy is currently 0 (exact
   match) on `blank`; expect to soften per-scene with documented
   root causes when running on other hardware (per dual-servo plan
   §"No hacks").
5. **wgpu 29 → 30 (and beyond) API churn.** This session found ~10
   surface renames between wgpu versions
   (PUSH_CONSTANTS→IMMEDIATES, `var<push_constant>` →
   `var<immediate>`, `multiview` → `multiview_mask`, etc.). Future
   wgpu major bumps will likely invalidate code we wrote. The
   §4.4 dep-currency note is the planned mitigation; recurring audits.
6. **Parley + glyph atlas ownership** (Main §Q14, recorded
   2026-04-28: webrender-wgpu owns the atlas on the WebRender path).
   This is decided but not implemented. When HTML Lane work surfaces
   in graphshell, the atlas-ownership boundary may need revisiting.
   Cross-repo coordination required.
7. **Image / text oracle scenes deferred.** Need asset-dependency
   handling (Ahem.ttf for text; image fixtures for images). Bringing
   them online surfaces font-rasterizer + asset-loader concerns we
   haven't designed yet.
8. **The clap 3 patches on the oracle worktree are local-only.**
   If the worktree is re-created, the patch in
   `wrench/src/yaml_frame_reader.rs::new_from_args` needs
   re-applying. Documented in `webrender/tests/oracle/README.md`,
   but easy to forget. Failure mode: wrench panics on `args.value_of("keyframes")`.
9. **Big-picture scope.** webrender-wgpu's full renderer-body
   migration is genuinely a multi-month engineering project. If
   priorities shift (graphshell M2/M3 work, servo-wgpu integration,
   verse / nostr / matrix mods), this branch may stall partway.
   No external user is blocked on it; the §S9 GL-deletion receipt
   is the only "done" milestone. Any pause leaves the branch in a
   bounded mid-state.
10. **Auto-mode pacing.** This session ran fast and aggressively.
    Each commit landed in single turns; the cumulative velocity
    burned through the whole design phase in one sitting. The next
    phase (renderer body grinding) is much slower per-turn — many
    turns per slice, many slices per A2.X. Setting expectations
    for that velocity drop is part of staying productive.

---

## Bottom line

The design is committed. The wgpu-native API surface for boot,
texture, pipeline, binding, buffer, pass-encoding, and oracle
comparison is in `webrender/src/device/wgpu/` with seven smoke /
integration tests. Both plans (idiomatic-wgsl + adapter) capture
sequencing, receipts, and ~10 sequenced wgpu 29 surface-API gotchas.

The implementation grind starts at A2.X.1 — the first commit that
edits `renderer/mod.rs`. Multi-turn, multi-week. Worth pacing
deliberately, considering sidequest (S5) parallelism, and pushing
the existing commits to `origin` first.
