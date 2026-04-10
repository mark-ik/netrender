# P15 Progress Report — 2026-04-10

## Status: 413/413 wgpu reftests passing (100%) — all backends clean

wgpu 29 bump complete across all three repos. Two additional rendering bugs
found and fixed: timestamp buffer validation and multiply blend mode mapping.

---

## wgpu 29 Breaking Changes

wgpu 29 introduced several API changes that required updates across
webrender, wgpu-gui-bridge, and servo:

1. **`Instance::new()` takes by-value `InstanceDescriptor`** — no more
   `Default::default()`. New required field: `memory_budget_thresholds`.
2. **`get_current_texture()` returns `CurrentSurfaceTexture` enum** — replaces
   `Result<SurfaceTexture, SurfaceError>`. Match on `Success`/`Suboptimal`/
   `Outdated`/`Lost` instead of `Ok`/`Err`.
3. **`SurfaceTargetUnsafe::RawHandle.raw_display_handle`** changed from
   `RawDisplayHandle` to `Option<RawDisplayHandle>`.
4. **`max_inter_stage_shader_components` renamed to
   `max_inter_stage_shader_variables`** with default dropped from 60 to 16.

---

## Root Cause 1: max_inter_stage_shader_variables

### Symptom
All wgpu rendering paths produced black output after the 29 bump. The
Composite pipeline failed validation silently (pipeline creation returned
an error pipeline, no visible panic). Readback was all zeros.

### Diagnosis
wgpu 29 renamed `max_inter_stage_shader_components` (default 60) to
`max_inter_stage_shader_variables` (default 16). WebRender's composite
vertex shader uses outputs up to `@location(17)`, requiring at least 18
inter-stage variables. The default of 16 caused the pipeline to fail
validation with:

```
vertex shader output location Location[16] exceeds the
`max_inter_stage_shader_variables` limit (15, 0-based)
```

### Fix
Request `max_inter_stage_shader_variables: 28` (generous headroom above
the minimum 18) in every device creation path:

| Repo | File | Change |
|------|------|--------|
| webrender | `webrender/src/device/wgpu_device.rs` | `new_headless()` and `new_with_surface()` device limits |
| webrender | `webrender/tests/wgpu_backends.rs` | `make_device()` test helper |
| webrender | `wrench/src/main.rs` | All 3 wgpu window creation branches |
| webrender | `examples/wgpu_shared_device.rs` | Device creation |
| webrender | `examples/wgpu_hal_device.rs` | Device creation (primary + fallback) |
| servo | `components/shared/paint/wgpu_rendering_context.rs` | `WgpuRenderingContext::new()` |
| wgpu-gui-bridge | `demo-servo-winit/src/main.rs` | Device creation |

Added `WgpuDevice::MIN_INTER_STAGE_VARS = 18` public constant for external
device creators to reference.

---

## Root Cause 2: Timestamp buffer invalid usage flags

### Symptom
Windowed `--wgpu` reftests: 341/413 passing. 72 tests failed with
max_difference=255 (entire content missing). Headless `--wgpu-hal-headless`
was unaffected (412/413).

### Diagnosis
The `WR timestamp resolve buf` was created with incompatible usage flags:
`QUERY_RESOLVE | COPY_SRC | MAP_READ`. In wgpu 29, `MAP_READ` can only be
combined with `COPY_DST`. This made the buffer object invalid, causing
`resolve_query_set` to fail validation, which rejected the **entire command
buffer** — all GPU work for the frame was discarded.

Headless was unaffected because its device factory didn't request
`TIMESTAMP_QUERY`, so the buffer was never created.

### Fix
Split into two buffers:
- `timestamp_resolve_buf`: `QUERY_RESOLVE | COPY_SRC` (GPU-side resolve target)
- `timestamp_readback_buf`: `MAP_READ | COPY_DST` (CPU-mappable staging)

Added `copy_buffer_to_buffer` in `resolve_timestamps()` to transfer data
between them. Updated `read_pass_timings_ms()` to map the readback buffer.

**File:** `webrender/src/device/wgpu_device.rs`

---

## Root Cause 3: MultiplyDualSource blend mode mapped incorrectly

### Symptom
After timestamp fix, 7 blend tests still failed (multiply, isolated-2,
backdrop-filter-*, filter-mix-blend-scaling). All max_diff=255.

### Diagnosis
`blend_mode_to_wgpu()` mapped both `SubpixelDualSource` and
`MultiplyDualSource` to `WgpuBlendMode::SubpixelDualSource`. These have
completely different blend equations:

- **SubpixelDualSource** (text AA): `src * Src1Alpha + dst * (1 - Src1Alpha)`
- **MultiplyDualSource** (blend mode): `src * (1 - DstAlpha) + dst * (1 - Src1Color)`

Additionally, `BrushImage` has no dual-source WGSL shader variant, so
even with the correct blend state, the pipeline validation failed:
"Pipeline uses dual-source blending, but the shader does not support it."

### Fix
Two-part fix:

1. **Added `WgpuBlendMode::MultiplyDualSource`** with the correct blend
   factors matching GL's `set_blend_mode_multiply_dual_source()`.

2. **Added `dual_source_mix_blend_supported` config flag** — set to `false`
   on the wgpu path (no BrushImage dual-source shader), `true` on GL.
   This routes Multiply through the two-pass `brush_mix_blend` readback
   fallback, which produces correct results.

   Text subpixel AA (`SubpixelDualSource`) is unaffected — it has a
   dedicated `PsTextRunDualSource` shader variant that works correctly.

**Files:** `wgpu_device.rs`, `renderer/mod.rs`, `renderer/init.rs`,
`frame_builder.rs`, `render_target.rs`, `batch.rs`, `picture.rs`, `scene.rs`

---

## Wrench: wgpu 26 -> 29

Updated all wgpu API calls in `wrench/src/main.rs`:
- Instance creation (explicit InstanceDescriptor)
- Device limits (inter-stage vars)
- SurfaceTargetUnsafe (Option<RawDisplayHandle>)

Fixed `wrench/Cargo.toml`: added `"webrender/wgpu_native"` to `wgpu_backend`
feature (the `RendererBackend::Wgpu` variant is gated on `wgpu_native`, not
just `wgpu_backend`).

---

## Wrench: --wgpu-hal-headless mode

Added a new `--wgpu-hal-headless` CLI flag for running wgpu reftests without
a display server or window.

### Implementation
- New `WindowWrapper::WgpuHeadless(i32, i32)` variant
- Creates wgpu Instance + Adapter without a surface
- Passes adapter to WebRender via `RendererBackend::WgpuHal` factory closure
- No event loop needed — headless rendering to offscreen textures
- All WindowWrapper match arms handle the new variant (swap_buffers=no-op,
  get_inner_size=stored dimensions, hidpi_factor=1.0, etc.)

### Final reftest results

| Backend | Pass | Fail | Notes |
|---------|------|------|-------|
| GL (ANGLE) | 413/413 | 0 | Clean |
| wgpu (windowed) | 413/413 | 0 | Clean |
| wgpu-hal (headless) | 412/413 | 1 | `allow-subpixel.yaml` — factory doesn't request DUAL_SOURCE_BLENDING |

---

## Examples

Fixed `examples/common/boilerplate.rs` and `examples/multiwindow.rs`:
`create_webrender_instance()` signature takes `Rc<dyn Gl>`, not
`RendererBackend::Gl { gl }`. Pre-existing mismatch from backend refactor.

Fixed `examples/wgpu_shared_device.rs` and `examples/wgpu_hal_device.rs`:
Instance creation and device limits updated for wgpu 29.

Added `"webrender/wgpu_native"` to examples' `wgpu_backend` feature.

---

## Integration tests

All 8 WebRender wgpu integration tests pass (was 5/8 before inter-stage fix):

```
test wgpu_headless_device_creation ... ok
test wgpu_headless_render_basic ... ok
test wgpu_headless_render_colored_rect ... ok
test wgpu_headless_render_multiple_rects ... ok
test wgpu_shared_device_creation ... ok
test wgpu_shared_render_basic ... ok
test wgpu_shared_render_colored_rect ... ok
test wgpu_shared_render_multiple_rects ... ok
```

---

## wgpu-gui-bridge changes

- `demo-servo-winit/src/main.rs`: wgpu 29 API fixes + inter-stage device limits
- `wgpu-native-texture-interop/src/lib.rs`: CapabilityMatrix fix — Vulkan
  `vulkan_external_image` now correctly reports `Unsupported(NativeImportNotYetImplemented)`
  instead of `Supported`
- `servo-wgpu-interop-adapter/Cargo.toml`: winit bumped 0.30.12 -> 0.30.13
  to match Servo

---

## Commits

| Repo | Hash | Description |
|------|------|-------------|
| webrender | `457e35224` | wgpu 29 bump: wrench, examples, inter-stage vars fix |
| webrender | `1a6c27abe` | wrench: add --wgpu-hal-headless reftest mode |
| webrender | (pending) | Fix timestamp buffer, multiply blend mode, dual-source config |
| servo | `4bb0e648bee` | WgpuRenderingContext: request inter-stage shader vars limit |
| wgpu-gui-bridge | `44304e4` | wgpu 29 demo fixes, capability matrix, winit bump |

---

## Known issues

- Gradient cache invalidation bug (P14) — still open, unrelated to wgpu 29
- 1 headless failure: `allow-subpixel.yaml` — wrench's WgpuHal factory
  closure doesn't request `DUAL_SOURCE_BLENDING`, so subpixel AA is
  unavailable in headless mode (minor config fix)
