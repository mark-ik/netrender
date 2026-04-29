# Session Brief — 2026-04-28 / 2026-04-29

State of the `idiomatic-wgpu-pipeline` branch after the 2026-04-29
adapter-groundwork continuation **and** the foundational A2.X.5
field-install on `Renderer`. Snapshot for orientation; the actionable
sequencing still lives in the adapter plan.

---

## Where we're at

**Branch shape**: `idiomatic-wgpu-pipeline` off `upstream/upstream`,
tracking `origin/idiomatic-wgpu-pipeline`. HEAD:
`75e125166 session brief`. Current working tree intentionally carries
the uncommitted A2.X / A2.3 adapter-groundwork stack: 7 files,
497 insertions, 311 deletions at the last diff stat. No
`renderer/mod.rs` callsite has been migrated yet.

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
| Adapter A2.X.0 | ✅ | `pass.rs` refactored: `DrawIntent` carries pipeline+bind_group, `flush_pass` owns pass replay |
| Adapter A2.X.1 | ✅ | `RenderPassTarget` / `ColorAttachment` pass descriptor; `oracle_blank_smoke` now goes through `pass::flush_pass` |
| Adapter A2.X.2 | ✅ | `DepthAttachment` pass policy; `pass_target_depth_smoke` covers depth clear + discard through `WgpuDevice::encode_pass` |
| Adapter A2.X.3 | ✅ | `WgpuDevice::encode_pass` bridge; smoke/oracle pass tests now target the adapter surface |
| Adapter A2.X.4 | ✅ | `WgpuDevice::create_encoder` / `submit`; pass receipts use adapter-owned command lifecycle |
| Adapter A2.3.0 | ✅ | `WgpuDevice::read_rgba8_texture`; readback staging moved from tests into `readback.rs` |
| Adapter A2.X.5 | ✅ | `Renderer.wgpu_device: WgpuDevice` + boot wired in `create_webrender_instance`; `RendererError::WgpuBoot`; no callsites changed |

**Concrete artifacts**:

- 11-module `webrender/src/device/wgpu/` scaffold (mod, core,
  format, buffer, texture, shader, binding, pipeline, pass, frame,
  readback, adapter)
- Adapter boundary now owns: device boot, lazy brush pipeline cache,
   texture create/upload, command encoder create/submit,
   pass replay (`encode_pass`), and RGBA8 readback staging.
- 7 wgpu tests passing in 2.03s
- 5 captured oracle PNG/YAML pairs in `webrender/tests/oracle/`
- A reusable oracle harness (`load_oracle_png`, `count_pixel_diffs`)
   plus adapter-backed readback (`WgpuDevice::read_rgba8_texture`)
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

1. **Adapter A2.X.6 — first `renderer/mod.rs` pass-encoding callsite
   migration**. A2.X.5 (foundational install) closed 2026-04-29:
   `Renderer.wgpu_device` is in place, both devices coexist, builds
   green. A2.X.6 picks one path and makes it run end-to-end through
   `self.wgpu_device.encode_pass(...)`. Candidate paths are
   `bind_debug_overlay` (`mod.rs:1507`), texture-cache copy
   (`mod.rs:1983`), or main render-target setup (`mod.rs:3338`). The
   first two are narrower; the main path is more representative but
   touches QCOM tiling, depth-write state, clears, resolves, blits,
   and draw batching in one knot. **First-callsite blocker**: each
   candidate sits behind GL-shaped state (`FBOId`, `Texture`,
   `DrawTarget`) that A2.1+ has not yet migrated. A2.X.6 entry needs
   to choose between (a) parallel wgpu-native plumbing isolated to
   the migrated path, or (b) a `Texture`/`DrawTarget` dual-handle
   bridge so one path runs wgpu-native while the rest stays GL.
2. **Adapter A2.3.1 — renderer read-pixels callsites**.
   The copy-to-buffer machinery is now adapter-owned, but the
   `mod.rs:1262/4614/4619` callsites still sit behind
   `bind_read_target_impl` state. This can proceed once each caller
   can name the source texture/view directly instead of binding a GL
   read target first.
3. **Adapter A2.1 / A2.2 / A2.5 — texture lifecycle, zoom-debug,
   and blit paths**. Dither and zoom-debug remain gated on
   pass-encoding-shaped bind groups. Same-format blits can use
   `copy_texture_to_texture`; scaled/filtering blits need a render
   pass helper.
4. **Adapter A3–A7 — vertex, pipeline, render-target, upload, and
   query migrations**. Same rhythm: keep a small wgpu-native adapter
   surface, route a focused receipt through it, then migrate renderer
   callsites without preserving GL-shaped state.
5. **Adapter A8 — re-export flip**. `device/mod.rs` switches from
   GL to wgpu. Compiler errors light up any remaining residue. Once
   green, parent S4 can close by bringing the remaining oracle scenes
   through the actual renderer body.
6. **Main S5–S9**: CTS gate, full WGSL corpus, servo-wgpu smoke,
   external corpus coverage, then GL deletion. These remain the
   strategic finish line after the renderer boundary has moved.

**Honest scope estimate**: A2.X.6+ through A8 is multi-week to
multi-month engineering work. The work has moved out of design and
into careful renderer-body surgery; expect fewer lines per turn and
more compile/debug cycles per slice.

---

## Fruitful sidequests

Things that aren't on the critical path but unblock, accelerate, or
de-risk later work. Pickable in any order, mostly independent:

1. **Servo-wgpu integration verification.** The sibling
   `servo-wgpu` repo patches webrender to this local path. A2.X.5
   added a second wgpu boot inside `create_webrender_instance`;
   verify Servo's wgpu-context init doesn't conflict before its next
   pull. Pitfall #7 watch.
2. **WebGPU CTS gate (Main S5)**. Runs alongside renderer migration
   without conflict. Target a small conformance lane first: buffers,
   render_pass, bind_groups, blend, depth_stencil, vertex_state.
   Concrete deliverable remains a focused test command rather than a
   full CTS import.
3. **WGSL `override` variant collapse exploration.** Author one
   duplicate shader-family pair as override-specialized WGSL. Validates
   the §4.9 plan without touching renderer control flow.
4. **Pipeline cache / async compilation spike** (§4.11). Small
   adapter-only work that pays off once S6 expands the shader corpus.
5. **Oracle harness hardening.** Keep `blank` exact, but design the
   tolerance/reporting shape for non-blank scenes before the remaining
   four S4 images come online.
6. **RenderBundle experiment for tile replay** (Main §Q12, adapter
   §Q4). Potential frame-time win after picture-cache rendering is
   reachable; not blocking the boundary migration.
7. **Texture-array glyph cache** (Main §Q11). Useful future
   optimization for text-heavy scenes, but do not let it jump the
   critical path until text rendering has a wgpu path again.

---

## Potential pitfalls

Things that could invalidate work or stall progress:

1. **Renderer callsites are interdependent.** 169 `self.device.*`
   callsites and 57 methods count the surface, not the hidden GL state
   coupling. A "single" `bind_draw_target` migration may pull in clear
   policy, depth writes, texture binding, resolves, blits, and profiler
   queries.
2. **No GL-shaped compatibility layer.** The plan intentionally
   rejects a wgpu-backed clone of `gl.rs::Device`. That keeps the
   architecture honest, but it removes the easy path of shimming old
   call shapes one method at a time.
3. **Readback is only half migrated.** `WgpuDevice::read_rgba8_texture`
   handles RGBA8 texture staging, not the renderer's GL-shaped
   read-target binding model, other formats, partial rectangles, or
   caller-owned destination buffers. Do not mark A2.3 closed until
   `read_pixels` / `read_pixels_into` callsites are actually moved.
4. **Depth/clear semantics must stay explicit.** wgpu load/store ops
   are pass-begin decisions. GL-style late clears and
   `invalidate_depth_target()` calls need to become `RenderPassTarget`
   policy, or the migration will accidentally preserve mutable
   framebuffer state in a new disguise.
5. **`cargo fmt` can create broad inherited-WebRender churn.** Prefer
   targeted `rustfmt` on edited files and verify `git diff --name-only`
   afterward. Crate-wide format already produced unrelated churn once.
6. **Servo-wgpu may break during renderer-body edits.** It patches to
   this local webrender. A2.X.5 added a second wgpu boot inside
   `create_webrender_instance` — verify Servo doesn't double-allocate
   or fail adapter selection before its next pull. If a renderer
   migration stays half-done, Servo may fail for unrelated-looking
   reasons. Keep checkpoints green and coordinate pinning if needed.
7. **Oracle PNGs are platform-dependent.** Current exact match is only
   proven for `blank` on the capture machine. Non-blank scenes may need
   documented tolerances, and text/image scenes still need asset/font
   handling.
8. **wgpu API churn remains real.** The branch already hit wgpu 29
   differences (`IMMEDIATES`, `var<immediate>`, `depth_slice`,
   `multiview_mask`, `immediate_size`). Future major bumps can move
   the ground under the adapter; keep version notes close to code.
9. **Scope gravity.** The project has tempting adjacent work
   (glyph arrays, RenderBundles, pipeline cache, CTS, servo smoke),
   but GL deletion is the real finish line. Sidequests should either
   de-risk the migration or stay explicitly optional.

---

## Bottom line

The design phase is over; the adapter boundary is real enough to be
the target for renderer-body work, and as of A2.X.5 the renderer
holds it. `webrender/src/device/wgpu/` owns boot, texture
create/upload, pipeline/binding/buffer helpers, pass target policy,
depth policy, command encoder lifecycle, pass replay, and RGBA8
readback. `Renderer.wgpu_device` boots independently of the GL
`Device` in `create_webrender_instance`. Seven focused wgpu tests
remain green.

The next real milestone is A2.X.6: the first `renderer/mod.rs`
pass-encoding callsite actually migrated to call
`self.wgpu_device.encode_pass(...)`. Treat it as slower, careful
surgery, not another quick scaffolding slice. The first-callsite
recon (parallel-plumbing vs. dual-handle bridge) gates A2.X.6
entry — pick a narrow renderer path and keep it green before
widening.
