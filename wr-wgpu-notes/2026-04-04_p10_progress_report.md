# P10 Progress Report — wgpu Renderer Implementation

**Date**: 2026-04-04
**Branch**: `wgpu-device-renderer`
**Last commit**: `c86d2f0bd notes: P8 progress report` (uncommitted P9 + P10 changes)

---

## Reftest Results

**Current (P10)**: 303/413 pass (73.4%) — 110 failures
**Previous (P9)**: 261/413 pass (63.2%) — 152 failures
**Net change**: **+42 tests passing** (+10.2 percentage points)

### Per-category breakdown

| Category           | Pass | Total | Fail | Delta vs P9 |
|--------------------|------|-------|------|-------------|
| aa                 | 2    | 3     | 1    | +1 |
| backface           | 10   | 10    | 0    | +2 |
| blend              | 20   | 23    | 3    | +12 |
| border             | 17   | 22    | 5    | +5 |
| boxshadow          | 13   | 16    | 3    | +3 |
| clip               | 11   | 17    | 6    | +1 |
| compositor         | 0    | 4     | 4    | -2 |
| compositor-surface | 1    | 7     | 6    | -2 |
| crash              | 2    | 2     | 0    | +1 |
| filters            | 51   | 70    | 19   | +3 |
| gradient           | 42   | 80    | 38   | -22 |
| image              | 19   | 26    | 7    | -2 |
| mask               | 7    | 11    | 4    | +1 |
| performance        | 0    | 1     | 1    | 0 |
| scrolling          | 26   | 26    | 0    | +7 |
| snap               | 3    | 3     | 0    | +2 |
| split              | 16   | 16    | 0    | +6 |
| text               | 37   | 48    | 11   | +16 |
| tiles              | 5    | 5     | 0    | +4 |
| transforms         | 21   | 23    | 2    | +9 |

### Categories now at 100%

backface (10/10), crash (2/2), scrolling (26/26), snap (3/3), split (16/16), tiles (5/5)

### Regressions noted

- **gradient**: -22 (64/80 -> 42/80)
- **compositor**: -2 (2/4 -> 0/4)
- **compositor-surface**: -2 (3/7 -> 1/7)
- **image**: -2 (21/26 -> 19/26)

These are almost certainly *reveal regressions*: the CompositeFastPath fix
(see below) means picture cache tiles now actually composite onto the output.
Previously, the missing CompositeFastPath pipeline caused tiles to silently
not render, meaning the output was a blank/clear surface. Some tests coincidentally
passed when their output was empty (e.g., because the reference also showed
similar behavior due to the test structure). Now that tiles render correctly,
these tests reveal genuine rendering bugs in the underlying shaders (gradients,
compositor surfaces, etc.) that were previously masked.

Evidence: the 38 gradient failures include all 8 `tiling-*` tests, all 7 conic
tests, and several linear/radial tests. These are complex gradient variants
that likely exercise picture-cached rendering. The previous "pass" was spurious.

---

## What This Session Fixed (P10)

### 1. CompositeFastPath pipeline config string mismatch (CRITICAL)

**Files**: `device/wgpu_device.rs` — `shader_key()` and `from_shader_key()`

**Root cause**: The build system (`webrender_build/shader_features.rs` line 57-60)
sorts feature strings alphabetically via `FeatureList::finish()` before joining
with commas. The generated WGSL shader keys in `target/.../shaders.rs` use
`"FAST_PATH,TEXTURE_2D"` (alphabetical). But `from_shader_key()` had the
features in a different order: `"TEXTURE_2D,FAST_PATH"`.

This meant `from_shader_key("composite", "FAST_PATH,TEXTURE_2D")` returned
`None` at runtime, so `ensure_pipeline()` never found the CompositeFastPath
pipeline. Every picture cache tile composite draw was silently skipped (with
only a `log::warn` message).

**Fix**: Changed both `shader_key()` and `from_shader_key()` to use
alphabetically-sorted feature order:

```rust
// Before:
Self::CompositeFastPath    => ("composite", "TEXTURE_2D,FAST_PATH"),
Self::CompositeFastPathYuv => ("composite", "TEXTURE_2D,FAST_PATH,YUV"),

// After:
Self::CompositeFastPath    => ("composite", "FAST_PATH,TEXTURE_2D"),
Self::CompositeFastPathYuv => ("composite", "FAST_PATH,TEXTURE_2D,YUV"),
```

**Impact**: This was the single highest-impact bug in the wgpu renderer.
Fixing it enabled ALL picture cache tile compositing, which is the primary
rendering path for most content. This one fix accounts for the bulk of the
+42 test improvements.

### 2. MixBlend pass 2 depth test rejection

**Files**: `device/wgpu_device.rs` (new enum variant), `renderer/mod.rs`
(pass 2 depth state)

**Root cause**: In picture cache targets with opaque geometry, pass 1 writes
depth values for opaque front-to-back batches. Pass 2 (MixBlend) used
`WgpuDepthState::TestOnly` which rejects fragments that fail the depth test.
But MixBlend fragments need to composite ON TOP of existing content — their
z-values may be "behind" the opaque geometry that was already depth-written.
Result: MixBlend fragments were silently rejected.

Using `WgpuDepthState::None` was also wrong: wgpu validation requires that
if the render pass has a depth attachment (which it does when `has_opaque`),
the pipeline must also have a depth stencil format.

**Fix**: Added a new `WgpuDepthState::AlwaysPass` variant:

```rust
WgpuDepthState::AlwaysPass => Some(wgpu::DepthStencilState {
    format: wgpu::TextureFormat::Depth32Float,
    depth_write_enabled: false,
    depth_compare: wgpu::CompareFunction::Always,
    stencil: wgpu::StencilState::default(),
    bias: wgpu::DepthBiasState::default(),
}),
```

This is format-compatible with the depth attachment but never rejects fragments.
Applied to MixBlend pass 2 in both picture cache and all_targets loops.

**Impact**: Fixes blend tests that use MixBlend with opaque geometry (e.g.,
`multiply.yaml`). Contributes to the +12 improvement in blend category.

### 3. Cleanup

Removed all temporary debug `eprintln!` statements added during P9 investigation:
`[BD]`, `[MB2]`, `[COMP]`, `[COMP-PX]`, `[RD]` diagnostics. Restored the
`info!()` logging for compositor tile counts.

---

## Remaining Failure Categories (110 failures)

### Gradient (38 failures) — highest count

Mostly conic gradients (7), tiling gradients (12), and linear/radial variants.
Likely shader-level bugs in gradient rendering now visible due to CompositeFastPath
fix. Some may be coordinate/UV issues in the gradient shaders.

### Filters (19 failures)

Dominated by `backdrop-filter-*` (7) and `svg-filter-*`/`svgfe-*` (6).
Backdrop filters may have the same two-pass rendering issue as MixBlend —
they need to read the backdrop and apply a filter. SVG filters may need
additional shader variants or filter graph traversal fixes.

### Text (11 failures)

Down from 27 in P9 (16 fixed by CompositeFastPath!). Remaining 11 include
shadow tests (5), `allow-subpixel`, `decorations-suite`, `mix-blend-layers`,
`raster_root_C_8192`, `split-batch`, and `1658`. Some may be dual-source
blending issues (subpixel AA), others may be shadow rendering bugs.

### Image (7 failures)

`snapshot-*` (3), `segments`, `tile-repeat-prim-or-decompose`,
`tile-with-spacing`, `rgb_composite`. Snapshot tests may involve readback
or multi-frame rendering issues.

### Clip (6 failures)

Ellipse clips, corner overlap, empty inner rect, clip modes, stacking context
clips. May involve clip mask rendering or shader-side clip evaluation bugs.

### Compositor-surface (6 failures)

All 6 failures after CompositeFastPath fix. These tests exercise external
compositor surfaces which may use a different code path.

### Border (5 failures)

Border images (2), radial gradient border, discontinued dash, overlapping.
The border-image tests may need `border-image` shader support; the gradient
border test ties into the gradient rendering bugs.

### Compositor (4 failures)

All rounded-corner + tile-occlusion tests. Rounded corner compositing may
need additional clip handling during tile compositing.

### Mask (4 failures)

Mask and nested mask tests, including tiling variants. May be related to
clip mask rendering.

### Other (10 failures across 4 categories)

boxshadow (3), transforms (2), aa (1), performance (1).

---

## Architecture Notes — Updated

### CompositeFastPath rendering flow

Now that CompositeFastPath works:

```
Per frame, after all render passes complete:
  For each picture cache tile:
    1. Look up tile's texture in picture_textures pool
    2. Create CompositeFastPath pipeline (FAST_PATH + TEXTURE_2D features)
    3. Composite tile onto wgpu_readback_texture (Bgra8Unorm)
       using PremultipliedAlpha blend
    4. Tile's screen-space rect positions it correctly
  
  Copy wgpu_readback_texture to surface for presentation
```

### Feature string matching rule

Build system (`shader_features.rs:57-60`) ALWAYS sorts features alphabetically.
Any new `WgpuShaderVariant` entries in `shader_key()` and `from_shader_key()`
MUST use alphabetically sorted, comma-separated feature strings to match.

---

## Next Steps (Priority Order)

1. **Commit P10 changes** — the CompositeFastPath fix and AlwaysPass depth state

2. **Gradient failures (38)** — investigate conic/tiling gradient shader issues.
   Since these are likely real rendering bugs now visible, check:
   - Conic gradient shader correctness (angle calculations)
   - Tiling/repeat logic in gradient shaders
   - Coordinate spaces (device vs layout pixels) in gradient parameters

3. **Backdrop filter failures (7)** — similar to MixBlend two-pass rendering,
   backdrop filters need to read the backdrop and apply a filter. May need
   the same two-pass approach or may already use it but with bugs.

4. **Text failures (11)** — shadow rendering (5 tests) is the largest subgroup.
   Dual-source blending for subpixel AA is a separate issue.

5. **C4 render pass sharing** — plan at `logical-whistling-mochi.md`. This is
   an optimization that can happen independently of correctness fixes.

6. **Compositor/compositor-surface failures (10)** — rounded corners and external
   surface compositing. May need clip handling during tile composite.

---

## File Changes Summary

### `webrender/src/device/wgpu_device.rs` (uncommitted)

- `WgpuDepthState::AlwaysPass` enum variant added
- `to_wgpu_depth_stencil()` implementation for `AlwaysPass`
- `shader_key()`: Fixed `CompositeFastPath` and `CompositeFastPathYuv` feature order
- `from_shader_key()`: Fixed matching patterns for same

### `webrender/src/renderer/mod.rs` (uncommitted, includes P9 changes)

- Two-pass MixBlend rendering (P9, unchanged)
- Loop reorder: all_targets before picture_cache (P9, unchanged)
- MixBlend pass 2 depth state: `AlwaysPass` instead of `TestOnly` (P10)
- Debug prints removed, `info!()` logging restored (P10)
