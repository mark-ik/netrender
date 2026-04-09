# P11 Progress Report — wgpu Renderer Implementation

**Date**: 2026-04-05
**Branch**: `wgpu-device-renderer`
**Last commits**: `6c4856545` (gpu_buffer textures), `f70fae350` (REPETITION variants)

---

## Reftest Results

**Current (P11)**: 309/413 pass (74.8%) — 104 failures
**Previous (P10)**: 303/413 pass (73.4%) — 110 failures
**Net change**: **+6 tests passing** (+1.4 percentage points)

### Per-category breakdown

| Category           | Pass | Total | Fail | Delta vs P10 |
|--------------------|------|-------|------|--------------|
| aa                 | 2    | 3     | 1    | 0 |
| backface           | 10   | 10    | 0    | 0 |
| blend              | 20   | 23    | 3    | 0 |
| border             | 20   | 22    | 2    | +3 |
| boxshadow          | 13   | 16    | 3    | 0 |
| clip               | 11   | 17    | 6    | 0 |
| compositor         | 0    | 4     | 4    | 0 |
| compositor-surface | 1    | 7     | 6    | 0 |
| crash              | 2    | 2     | 0    | 0 |
| filters            | 50   | 70    | 20   | -1 |
| gradient           | 45   | 80    | 35   | +3 |
| image              | 20   | 26    | 6    | +1 |
| mask               | 7    | 11    | 4    | 0 |
| performance        | 0    | 1     | 1    | 0 |
| scrolling          | 26   | 26    | 0    | 0 |
| snap               | 3    | 3     | 0    | 0 |
| split              | 16   | 16    | 0    | 0 |
| text               | 37   | 48    | 11   | 0 |
| tiles              | 5    | 5     | 0    | 0 |
| transforms         | 21   | 23    | 2    | 0 |

### Categories at 100%

backface (10/10), crash (2/2), scrolling (26/26), snap (3/3), split (16/16), tiles (5/5)

### Regressions

- **filters**: -1 (51→50). `filter-blur-downscaled-task` regressed (max diff 147).
  Likely atlas-layout sensitivity: the gpu_buffer fix means cs_* gradient shaders
  now write real gradient content into the texture cache, which may shift atlas
  packing and affect blur filter readback coordinates.

---

## What This Session Fixed (P11)

### 1. Missing gpu_buffer textures for cs_* gradient shaders (commit `6c4856545`)

**File**: `renderer/mod.rs` — `draw_cache_target_tasks_wgpu()`

**Root cause**: `draw_cache_target_tasks_wgpu()` builds a `base_textures` struct
for cs_* shader dispatches (linear/radial/conic gradient, blur, etc.). This struct
lacked `gpu_buffer_f` and `gpu_buffer_i` bindings. The `CsLinearGradient`,
`CsRadialGradient`, and `CsConicGradient` shaders all call `sample_gradient()` →
`fetch_from_gpu_buffer_2f()` → `texelFetch(sGpuBufferF, ...)`. Without the binding,
the shader sampled from a dummy texture, producing white output.

**Fix**: Added `gpu_buffer_f` and `gpu_buffer_i` to `base_textures`:
```rust
let base_textures = TextureBindings {
    gpu_cache: ctx.gpu_cache,
    gpu_buffer_f: ctx.gpu_buffer_f,
    gpu_buffer_i: ctx.gpu_buffer_i,
    dither: ctx.dither,
    ..Default::default()
};
```

**Impact**: Fixed gradient rendering for all non-fast-linear gradients.
+3 gradient tests, +3 border tests (border gradients), +1 image test.

### 2. BrushImage REPETITION shader variants (commit `f70fae350`)

**Files**: `device/wgpu_device.rs`, `renderer/mod.rs`

**Root cause**: The build system generates `brush_image` WGSL variants with
`WR_FEATURE_REPETITION` (which enables UV tiling via `fract()` in
`brush_image.glsl`). The wgpu backend had no corresponding `WgpuShaderVariant`
entries. All batches with `BatchFeatures::REPETITION` were silently routed to
the non-repeating `BrushImage` variant, causing tiled content to render once
without repeating.

**Fix**:
- Added `BrushImageRepeat` and `BrushImageRepeatAlpha` enum variants
- Added shader_key/from_shader_key mappings with feature strings
  `"ANTIALIASING,REPETITION,TEXTURE_2D"` (alphabetically sorted)
- Changed `batch_key_to_pipeline_key()` to accept `BatchFeatures` parameter
  and route based on `BatchFeatures::REPETITION`
- Updated all 3 call sites to pass `batch.features`

**Impact**: Fixed tiling for gradients and images rendered via BrushImage.
- `tiling-linear-3`: max diff 254→3
- `tiling-radial-3`: max diff 254→4
- `tiling-conic-3`: max diff 223→4
- Other tiling tests also improved but still fail on dithering tolerance

---

## Investigation: Remaining Max-255 Gradient Failures

Investigated the 4 gradient tests with max difference 255 (fundamentally wrong
output, not dithering). **None are gradient shader bugs:**

### 1. `gradient_cache_clamp` (max diff 255, 80000 px)

**State leakage bug.** Passes when run in isolation. Fails when preceded by
other tests in the full suite. The rendering output is pixel-identical to the
reference when captured via `png` subcommand. The reftest runner's readback
or render state is contaminated by a preceding test.

### 2. `conic-color-wheel` (max diff 255, 20918 px)

**Clip mask rendering bug.** The test uses a circular clip (radius=300 on a
300x300 rect, creating a full circle). The wgpu backend renders the gradient
into the full square without applying the circular clip mask. Pixels at the
square corners show gradient colors where the reference has white (background).
This is the same class of bug affecting the 6 `clip` category failures.

### 3. `conic-large-hard-stop` (max diff 255, 2500 px)

**Texture cache tile leakage.** The test renders a 2048x2048 conic gradient
masked by white rects to show only a 250x250 corner. A stray 50x50 yellow
square appears near the bottom-center of the output, absent from the reference.
This is likely a texture cache atlas tile that bleeds through due to incorrect
tile bounds or UV clamping in the large gradient case.

### 4. `radial-border-radius-large` (max diff 255, 995 px)

**Clip mask rendering bug.** Uses a rounded-rect clip (radius=32) on a large
750x500 primitive. Same category as `conic-color-wheel` — the clip mask
isn't fully applied at the rounded corners. Only 995 pixels are affected
(the corner regions).

---

## Remaining Failure Analysis (104 failures)

### Dithering/precision (estimated ~30 tests)

Gradient and conic tests with max diff 3-4, caused by float-to-unorm rounding
differences between GL-rendered reference PNGs and wgpu shader output. Not
fixable without increasing fuzzy tolerances or regenerating references.

### Clip mask rendering (~12 tests)

clip (6), conic-color-wheel, radial-border-radius-large, and likely some
compositor/compositor-surface failures involve clip masks not being correctly
applied. This is a high-value fix target.

### Filters (~20 tests)

Dominated by backdrop-filter (7) and SVG filter (6) failures. Backdrop
filters likely need the same two-pass approach as MixBlend.

### State leakage (gradient_cache_clamp, possibly others)

At least 1 test fails due to render state not being properly reset between
tests. May affect more tests when preceded by specific test patterns.

### Other

text (11), image (6), compositor-surface (6), compositor (4), mask (4),
boxshadow (3), border (2), transforms (2), aa (1), performance (1).

---

## Next Steps (Priority Order)

1. **Clip mask rendering** — fixing the circular/rounded-rect clip mask
   application would fix ~12 tests across gradient, clip, and possibly
   compositor categories. Investigate how clip masks are dispatched in
   the wgpu backend and whether they're correctly sampled during BrushImage
   and gradient rendering.

2. **Backdrop filter** — 7 tests. Similar to MixBlend two-pass rendering.

3. **State leakage** — investigate what render state persists between tests
   causing gradient_cache_clamp to fail in the full suite. May be a
   depth/stencil clear issue or texture cache state.

4. **SVG filters (6)** — may need additional shader variants or filter
   graph processing.

5. **Text failures (11)** — shadow rendering (5), subpixel AA / dual-source
   blending, and other text-specific issues.

6. **C4 render pass sharing** — optimization plan at `logical-whistling-mochi.md`

---

## File Changes Summary

### `webrender/src/device/wgpu_device.rs` (committed as `f70fae350`)

- `BrushImageRepeat` and `BrushImageRepeatAlpha` enum variants
- `shader_key()` / `from_shader_key()` mappings for REPETITION variants
- `instance_layout()` match arm updated

### `webrender/src/renderer/mod.rs` (both commits)

- `draw_cache_target_tasks_wgpu()`: Added `gpu_buffer_f` and `gpu_buffer_i`
  to `base_textures` (commit `6c4856545`)
- `batch_key_to_pipeline_key()`: Added `features` parameter, routes
  `BrushImage` batches based on `BatchFeatures::REPETITION` (commit `f70fae350`)
- Three call sites updated to pass `batch.features`
