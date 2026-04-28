# P14 Progress Report — 2026-04-07

## Status: 434/441 wgpu reftests passing (98.4%) — unchanged from P13

This session focused on diagnosing the `gradient/gradient_cache_clamp` failure.
No new test fixes yet; this is a deep investigation checkpoint.

---

## Investigation: gradient/gradient_cache_clamp

### Symptom
- Reference render (`gradient_cache_clamp_ref.yaml`) is missing gradient 1 (bounds 0,0,400,200)
  when run in the full gradient test suite. Gradients 2 and 3 render correctly.
- When run alone (just this one test), both test and reference pass.
- 80,000 wrong pixels, max_diff=255 — the entire first gradient region is wrong.

### How the gradient system works (relevant path)
1. `optimize_linear_gradient()` (linear.rs:155) decomposes axis-aligned clamped gradients
   into 2-stop segments, each becoming a separate primitive.
2. Constant-color segments (stops[0].color == stops[1].color) get `task_size = (1,1)`.
3. `FastLinearGradientTask { color0, color1, orientation }` + `size: DeviceIntSize`
   forms the render task cache key.
4. `render_task_cache::request_render_task_impl()` checks if cache key exists and
   texture_cache handle is still valid → returns cached task. Otherwise creates new task.

### Key finding: fewer cache requests in suite
- **Alone**: Reference frame makes 4 gradient cache requests (2 constant-color 1x1 + blue→red 1x200)
- **In suite**: Reference frame makes only **2** gradient cache requests (only 2 constant-color 1x1)
- This means the scene builder produces the same decomposition, but the frame builder
  **skips preparing** 2 of the 4 gradient segment primitives.

### Root cause theory: tile cache content deduplication
The test YAML (`gradient_cache_clamp.yaml`) and reference YAML (`gradient_cache_clamp_ref.yaml`)
produce **identical** decomposed segments for gradient 1 (same LinearGradientKey after interning).
The tile cache compares primitive descriptors between frames. Since test→reference is a scene
swap with identical primitives in the same positions, tiles covering gradient 1 are marked
CLEAN (not dirty). Clean tiles don't get command buffer targets, so their primitives are
never passed to `prepare_interned_prim_for_render`, which means `prim_data.update()` is
never called, and no `request_render_task` happens.

But: **clean tiles still reference the previous frame's texture**. If the render task cache
entry's texture was evicted between frames (due to `ClearCaches` or budget pressure from
the 80+ preceding tests), the tile's texture content is stale/missing.

### What was disproved
- **dirty_rects_are_valid = false**: Setting this globally (even disabling the optimization
  after every build_frame) did NOT fix the bug. This only controls whether the compositor
  can skip re-compositing unchanged tiles; it doesn't force tile content to be re-rendered.
  The issue is upstream: primitives in clean tiles don't get prepared at all.

### What was NOT tested yet
- Adding `force_invalidation = true` for the reftest flow (forces ALL tiles dirty every frame)
- Tracking which tiles are dirty vs clean during the reference frame build
- Whether the texture cache entry for the gradient is actually evicted or still valid

### Diagnostic code currently in tree
- `render_task_cache.rs`: `warn!` logging for gradient cache begin_frame drops, request hits/misses
- `renderer/mod.rs`: `warn!` logging for texture lookup misses (from previous session)
- `reftest.rs`: `write_debug_images = true` (writes test/ref PNGs for comparison)

---

## Changes in this session (uncommitted)

### render_task_cache.rs
Added diagnostic `warn!` logging:
- `begin_frame()`: logs entry count before/after, specifically logs gradient entry drops
  with allocation status and frame age
- `request_render_task_impl()`: logs gradient cache key requests with new_entry/needs_render status

### render_backend.rs
**Reverted** experimental dirty_rects changes from earlier in this session:
- Removed `dirty_rects_are_valid = false` in ClearCaches handler
- Restored `dirty_rects_are_valid = true` at line 559 (was changed to false as experiment)
Both proved ineffective and have been fully reverted.

### renderer/mod.rs (from previous session, still present)
Diagnostic logging for texture cache lookups.

### reftest.rs (from previous session, still present)
`write_debug_images = true` at line 986.

---

## Next steps (priority order)

1. **Test `force_invalidation`** in the reftest flow — this should force all tile cache
   tiles dirty every frame, bypassing the content comparison optimization. If this fixes
   `gradient_cache_clamp`, it confirms the tile-cache-skips-clean-primitives theory.

2. **Proper fix**: After ClearCaches, the texture cache is wiped. Any tile that was "clean"
   from content comparison still references a now-freed texture. Either:
   - (a) ClearCaches should invalidate all tile cache tiles (not just dirty_rects), or
   - (b) The tile cache should detect when its backing texture was evicted and self-invalidate.

3. **Investigate `compositor-surface/too-many-surfaces`** — 960 pixels, max_diff=230.
   Not yet started.

4. **Remove diagnostic logging** once fixes are confirmed.

---

## Log files (in wr-wgpu-notes/logs/, gitignored)
- `clamp_diag_alone.log` — alone run, 4 ref requests, PASSES
- `gradient_suite_cache_diag.log` — suite run, 2 ref requests, FAILS
- `gradient_suite_no_dirty.log` — suite with dirty_rects=false, STILL FAILS
- `gradient_full_diag.log` — full suite with allocation diagnostics
- Various `wrench_reftests_gradient__*.png` — debug render outputs
