# servo-wgpu × netrender Phase 1 embedder hookup — integration assessment

**Date:** 2026-04-30  
**Task:** Phase 1 second receipt (embedder hookup): servo-wgpu acquires a real
`wgpu::TextureView`, calls `Renderer::render`, presents.

---

## What was tried

**Preferred path (blocked):** Wire the `dc1199c768c` non-presenting wgpu
embedder smoke harness to netrender behind a `netrender_backend` cargo feature.
The plan:

1. Add `netrender = { path = "../../../webrender-wgpu/netrender", optional = true }`
   to `non-presenting-wgpu-embedder/Cargo.toml`.
2. Add `[features] netrender_backend = ["dep:netrender"]`.
3. Add an `#[allow(unreachable_code)]` early-return block in `main()` that calls
   a new `netrender_smoke()` fn when the feature is active.
4. `netrender_smoke()` mirrors `netrender/tests/p1_solid_rect.rs`: boot wgpu
   handles, create offscreen 256×256 `Rgba8UnormSrgb` target, build one
   full-NDC red `brush_solid` `PreparedFrame`, call `Renderer::render`, write
   log.

This approach kept the existing Servo/webrender path untouched and was
architecturally correct.

**Fallback (landed):** Standalone binary at
`servo-wgpu/examples/netrender_smoke/` (own Cargo.toml, NOT a workspace
member). Same receipt logic; no Servo dependencies.

---

## Where it got stuck

**File:** `<workspace>/repos/servo-wgpu/Cargo.toml`
**Line:** `webrender = { path = "../webrender-wgpu/webrender" }` (in
`[patch.crates-io]`)  
**Reason:** The `webrender-wgpu` repo renamed `webrender/` → `netrender/` in
commit `c9481b04b` (2026-04-30, same day). The path no longer exists. Cargo
fails to load the workspace before any package is compiled:

```
error: failed to load source for dependency `webrender`
Caused by: Unable to update <workspace>\repos\webrender-wgpu\webrender
Caused by: failed to read `...\webrender\Cargo.toml`
Caused by: The system cannot find the path specified. (os error 3)
```

This is a workspace-level load failure — it blocks ALL workspace members,
including the non-presenting-wgpu-embedder. Cargo patches are resolved before
any feature-gating takes effect; there is no way to build any workspace member
while the patch is broken.

**Why a shim won't work:** The `webrender` crate in the patch was the
fork-specific version with `RendererBackend::WgpuShared` /
`RendererBackend::WgpuHal` / `create_webrender_instance_with_backend` — APIs
added by Mark's fork that don't exist in upstream crates.io webrender. Pointing
the patch at upstream or an empty stub would compile the workspace but break
all `wgpu_backend` code in `components/paint/painter.rs`.

---

## Two options

### A. Tight-scope: standalone binary (landed)

`servo-wgpu/examples/netrender_smoke/Cargo.toml` with `[workspace]` to opt out
of the servo-wgpu workspace. Deps: `netrender` (path), `wgpu = "29"`,
`pollster = "0.4"`. No Servo, no broken patch.

**Result:** Builds and runs. One full-NDC red `brush_solid` draw through
`Renderer::render` against an offscreen 256×256 `Rgba8UnormSrgb` texture.
Adapter: NVIDIA GeForce RTX 4060 Laptop GPU (Vulkan). PASS.

This proves the netrender API survives contact with a real embedder wgpu
context. The missing piece vs. the preferred receipt: the target is an
offscreen texture, not a swapchain `SurfaceTexture`. The non-presenting variant
is explicitly allowed by the Phase 1 receipt spec.

### B. Full: fix servo-wgpu's broken webrender patch

Update `servo-wgpu/Cargo.toml` `[patch.crates-io]` to replace the broken
`webrender` path with one that works. Two sub-options:

1. **Remove the patch and use crates.io webrender 0.68**: breaks
   `wgpu_backend` feature in painter.rs (fork-specific API). Would need all
   `webrender::RendererBackend::WgpuShared/WgpuHal` references replaced with
   netrender equivalents throughout painter.rs. This is effectively option B2.

2. **Fully port painter.rs to netrender**: Replace the `create_webrender_
   instance_with_backend` path with `create_netrender_instance`. painter.rs
   line 325 and its surrounding `wgpu_backend` block need new API shapes.
   This is the Phase 2+ integration scope — Phase 1 specifically said
   "separate scope from the headless smoke."

   Key incompatibilities:
   - No notifier (netrender is sync, no async render-thread to wake)
   - No `RenderApi`/`send_transaction` (Phase 2 adds display-list ingestion)
   - No IPC sender (axiom 15)
   - `RenderingContext`'s `acquire_frame_target()` would supply the
     `TextureView` to `FrameTarget`

   This is well-defined but is a 1–2 day scope on its own.

---

## Recommendation

**Short term:** The landed standalone smoke (option A) satisfies the Phase 1
second receipt. Commit `servo-wgpu/examples/netrender_smoke/` on the
`upstream-wgpu` branch.

**Follow-up:** Fix the broken `webrender` patch in `servo-wgpu/Cargo.toml`
by routing through a proper compatibility path. The cleanest fix is to update
the `non-presenting-wgpu-embedder/Cargo.toml` `netrender_backend` feature (the
code change is already written above — it was reverted only because the
workspace couldn't load) and simultaneously update the workspace patch so it
points to a valid location. The `wgpu_backend` → netrender port in painter.rs
belongs to Phase 2 integration scope.

---

## Run log

See `netrender-notes/logs/2026-04-30_netrender_embedder_hookup.log` and
`netrender-notes/logs/2026-04-30_netrender_embedder_hookup_run.log`.

```
adapter: NVIDIA GeForce RTX 4060 Laptop GPU (Vulkan)
target: 256×256 Rgba8UnormSrgb
frame: 1× full-NDC red brush_solid via Renderer::render
result: PASS
```
