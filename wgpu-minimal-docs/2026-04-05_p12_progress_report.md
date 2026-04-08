# P12 Progress Report — wgpu Renderer Implementation

**Date**: 2026-04-05
**Branch**: `wgpu-device-renderer`
**Last commits**: P11 commits + uncommitted P12 changes

---

## Reftest Results

**Current (P12)**: 323/413 pass (78.2%) — 90 failures
**Previous (P11)**: 309/413 pass (74.8%) — 104 failures
**Net change**: **+14 tests passing** (+3.4 percentage points)

### Per-category breakdown

| Category           | Pass | Total | Fail | Delta vs P11 |
|--------------------|------|-------|------|--------------|
| aa                 | 2    | 3     | 1    | 0 |
| backface           | 10   | 10    | 0    | 0 |
| blend              | 20   | 23    | 3    | 0 |
| border             | 20   | 22    | 2    | 0 |
| boxshadow          | 13   | 16    | 3    | 0 |
| clip               | 12   | 17    | 5    | +1 |
| compositor         | 0    | 4     | 4    | 0 |
| compositor-surface | 1    | 7     | 6    | 0 |
| crash              | 2    | 2     | 0    | 0 |
| filters            | 50   | 70    | 20   | 0 |
| gradient           | 45   | 80    | 35   | 0 |
| image              | 20   | 26    | 6    | 0 |
| mask               | 7    | 11    | 4    | 0 |
| performance        | 0    | 1     | 1    | 0 |
| scrolling          | 26   | 26    | 0    | 0 |
| snap               | 3    | 3     | 0    | 0 |
| split              | 16   | 16    | 0    | 0 |
| text               | 48   | 48    | 0    | +11 |
| tiles              | 5    | 5     | 0    | 0 |
| transforms         | 23   | 23    | 0    | +2 |

### Categories at 100%

backface (10/10), crash (2/2), scrolling (26/26), snap (3/3), split (16/16),
tiles (5/5), **text (48/48)**, **transforms (23/23)**

Two new categories reached 100% this session.

---

## What This Session Fixed (P12)

### 1. Compositor clip for picture cache tiles (main fix)

**Files**: `renderer/mod.rs`

**Root cause**: When compositing picture cache tiles onto the final surface,
the wgpu backend passes `CompositeInstance` structs to the composite shader.
Each tile may have a `clip_index` referencing a `CompositorClip` (a rounded
rect that clips the tile during compositing — used for CSS `border-radius`
on the root element or picture cache surfaces).

The wgpu compositor path at `render_composite_instances_to_view()` was
passing `None` as the clip parameter to `CompositeInstance::new()` for ALL
tiles, completely ignoring `tile.clip_index`. This meant the
`rounded_clip_rect` and `rounded_clip_radii` fields in the instance data
were always zeroed, and the composite shader never applied any rounded clip.

The GL path correctly resolves `tile.clip_index` via
`composite_state.get_compositor_clip(index)` and passes the clip to the
instance constructor.

**Fix (Part 1 — pass clip data)**:
```rust
let compositor_clip = tile.clip_index.map(|idx| {
    composite_state.get_compositor_clip(idx)
});
// ... in CompositeInstance::new():
compositor_clip,  // was: None
```

**Fix (Part 2 — use correct shader variant)**:
The composite shader has two variants:
- `CompositeFastPath` (`FAST_PATH,TEXTURE_2D`): Skips UV clamping and
  rounded clip evaluation. Used for unclipped tiles.
- `Composite` (`TEXTURE_2D`): Full path with `vColor` modulation, UV bounds
  clamping, and SDF-based rounded clip evaluation.

The wgpu backend used `CompositeFastPath` for ALL texture-backed tiles.
The `#ifndef WR_FEATURE_FAST_PATH` block in `composite.glsl` (lines 227-242)
contains the rounded clip logic — never executed in the fast path.

Fixed by splitting textured tile batches into two groups:
- Unclipped tiles: continue using `CompositeFastPath`
- Clipped tiles (with `compositor_clip.is_some()`): use `Composite`

**Impact**: Fixed rounded-rect clipping at the compositor level. This is
the clip path used when CSS `border-radius` applies to picture cache tile
compositing, not to individual primitive rendering.

Direct fixes:
- `conic-color-wheel`: max diff 255 -> 4 (circular clip now applied)
- `radial-border-radius-large`: max diff 255 -> 4 (rounded clip now applied)
- `clip-empty-inner-rect`: now passes (was failing)
- `compositor/rounded-corners-3`: max diff improved to 1, 2 pixels

### 2. Clip mask texture binding (from earlier in session)

**Files**: `renderer/mod.rs`

Added `clip_mask` texture binding from `batch.key.textures.clip_mask` to
the `TextureBindings` struct in all three draw locations:
1. `record_alpha_batch` macro (all_targets / color target path)
2. `record_batch` macro (picture_cache path)
3. MixBlend pass 2 standalone draw

This is technically correct for brush-path batches that use per-batch clip
mask textures, but currently all observed batches have `clip_mask=Invalid`
because quad-path primitives handle clips through `QuadRenderStrategy`
(Tiled/Indirect/NinePatch) rather than per-batch clip textures.

### 3. Debug print cleanup

Removed all `eprintln!` debug statements:
- `[PC-O]` and `[PC-A]` prints from picture cache batch loops in `mod.rs`
- `[PQ]` print from `prepare_quad()` in `quad.rs`

**Impact**: The `[PQ]` print was firing for every `prepare_quad()` call,
producing enormous stderr output. This was causing 13 spurious test failures
(11 text, 2 transforms) due to I/O contention or timing effects. The clean
run without debug prints showed these tests passing.

---

## Investigation: Clip Architecture

### How clips work for quad-path primitives

Clips for `Quad(*)` primitives (ConicGradient, RadialGradient, etc.) are
NOT handled via per-batch `clip_mask` textures. Instead:

1. **Frame building** (`prepare_quad` in `quad.rs`): `get_prim_render_strategy()`
   examines the clip chain and returns one of:
   - `Direct`: No mask needed (`clip_chain.needs_mask == false`)
   - `Indirect`: Render to intermediate texture, apply mask via `MaskSubPass`
   - `Tiled`: Decompose into grid tiles; masked tiles rendered indirectly
   - `NinePatch`: Nine-patch decomposition with rounded corner segments

2. **Indirect/Tiled tiles**: Rendered as render tasks via
   `add_render_task_with_mask()`, which creates a `MaskSubPass`. The mask
   is applied by `ps_quad_mask` / `ps_quad_mask_fast_path` shaders during
   `build_sub_pass()`.

3. **Picture cache compositing**: Clips at this level use `CompositorClip`
   (rounded rect with radii), applied by the composite shader's SDF-based
   clip evaluation. This is what was broken and fixed in this session.

### Why `conic-color-wheel` had `needs_mask=false`

The test has a `clip` element with `radius: 300` on a `300x300` rect.
The `NinePatch` strategy check requires `max_corner_width <= 0.5 * rect.width`
(300 <= 150 = false), so NinePatch is rejected. The primitive size check
for tiling requires `size > MIN_QUAD_SPLIT_SIZE`, but in this case
`clip_chain.needs_mask` is false because the clip is applied at the picture
cache surface level, not the primitive level. The clip appears as a
`CompositorClip` on the tile, not in the primitive's clip chain.

---

## Remaining Failure Analysis (90 failures)

### Gradient (35 failures) — highest count
All are dithering/precision issues with max diff 3-4. Two exceptions:
- `gradient_cache_clamp` (max 255): state leakage bug (passes in isolation)
- `conic-large-hard-stop` (max 255): texture cache tile leakage

### Filters (20 failures)
- Backdrop filter: 7 tests (max 255), need two-pass approach like MixBlend
- SVG filters: 6 tests (max 255), may need filter graph processing
- Other: blur chain, scaled blur, component transfer (max 1-147)

### Compositor-surface (6 failures)
External surface compositing issues.

### Clip (5 failures)
- `clip-mode`, `clip-ellipse`, `clip-corner-overlap`: max diff 1 (AA precision)
- `stacking-context-clip`: ColorTargets check failure (not image-based)
- `raster-roots-tiled-mask`: max diff 128 (likely rendering bug)

### Image (6 failures)
Snapshot tests (3), tile-with-spacing, segments, rgb_composite.

### Compositor (4 failures)
- `rounded-corners-1`: ColorTargets check failure
- `rounded-corners-3`: max diff 1, 2px (nearly passing)
- `rounded-rgb-surface`, `tile-occlusion`: max diff 255

### Other
boxshadow (3), mask (4), blend (3), border (2), aa (1), performance (1)

---

## Next Steps (Priority Order)

1. **Backdrop filter** — 7 tests. Similar to MixBlend two-pass rendering.
   Currently the highest-impact category to fix.

2. **SVG filters** — 6 tests. May need additional shader variants or filter
   graph processing.

3. **State leakage** — investigate `gradient_cache_clamp` failure in full suite.

4. **Text shadow rendering** — all text tests pass now, but some were marginal.
   Monitor for regressions.

5. **C4 render pass sharing** — optimization plan at `logical-whistling-mochi.md`

---

## File Changes Summary

### `webrender/src/renderer/mod.rs`

- `render_composite_instances_to_view()`:
  - Resolve `tile.clip_index` via `composite_state.get_compositor_clip()`
  - Pass compositor clip to `CompositeInstance::new()` (was `None`)
  - Split textured tiles into unclipped (`CompositeFastPath`) and clipped
    (`Composite`) batches
  - Updated diagnostic logging to include clipped tile count
- `record_alpha_batch` macro: Added `clip_mask` texture binding
- `record_batch` macro: Added `clip_mask` texture binding
- MixBlend pass 2: Added `clip_mask` texture binding
- Removed `[PC-O]` and `[PC-A]` debug prints

### `webrender/src/quad.rs`

- Removed `[PQ]` debug print from `prepare_quad()`
