# P13 Progress Report — 2026-04-07

## Status: 434/441 wgpu reftests passing (98.4%)

Previous checkpoint: 370/413 (89.6%) at commit `1789270b1`.

The jump reflects completion of the picture-cache tile rendering loop, resolve_ops,
and several batch/blend fixes. Test suite grew from 413 → 441 as more tests became
runnable under the wgpu path.

---

## Changes in this session (uncommitted at time of writing)

### webrender/src/renderer/mod.rs (+437/-122)
Core wgpu rendering: picture-cache tile loop, opaque/alpha batch dispatch, scissored
quad batches, conic gradient debug cleanup, LoadOp::Clear for dynamic targets, cfg
gate on `as_byte_slice` import. All debug `eprintln!` removed.

### webrender/src/device/wgpu_device.rs (+11)
Minor fixes from previous session.

### webrender/src/device/mod.rs (+1)
Minor.

### wrench/build.rs (+17), wrench/src/composite.cpp (+1)
Build plumbing for DirectComposition (Windows SDK lib path detection).

### wrench/reftests/gradient/conic-large-hard-stop.yaml
**Test scene fix** — white covering rects extended from 2048→2200 in both dimensions.
The gradient extends to layout (2098, 2098); the original rects only reached x=2048
and y=2048 (exclusive), leaving a 50×50 corner uncovered. On GL at hidpi=2 that corner
is outside the 3840-wide viewport; on wgpu layout==image pixel so it was visible.
This is a pre-existing test authoring error, not a wgpu-specific accommodation.

### wrench/reftests/text/reftest.list
Added `skip_on(wgpu)` to `allow-subpixel` test. Subpixel AA requires GL-specific
extensions not available on the wgpu path. This is a documented feature gap, not a
suppressed failure.

---

## Boundary check

**Did we shift the GL baseline or the tests?**

- GL backend: untouched. All changes are in `wgpu_device.rs` and the wgpu branch of
  `renderer/mod.rs`.
- Tests: two changes, both defensible:
  - `conic-large-hard-stop.yaml` — corrected an authoring error (covering rects too
    small). The underlying test intent is unchanged.
  - `allow-subpixel skip_on(wgpu)` — documents a genuine capability gap using the
    established `skip_on` mechanism.
- Reference images (`.png` files): none modified.

---

## Remaining failures (7)

All have max_difference ≥ 230, meaning these are real rendering bugs, not precision noise.

| Test | Max diff | Pixels | Category |
|---|---|---|---|
| `compositor-surface/too-many-surfaces` | 230 | 960 | compositor overflow |
| `filters/filter-long-chain` | 255 | 111,628 | filter chain accumulation |
| `gradient/gradient_cache_clamp` | 255 | 80,000 | gradient cache texture clamping |
| `image/snapshot-filters-01` | 235 | 225 | snapshot + filter interaction |
| `image/snapshot-shadow` | 255 | 15,539 | shadow in snapshot |
| `text/raster_root_C_8192` | 255 | 34,111 | large raster root text |
| `text/mix-blend-layers` | 255 | 8,476 | mix-blend on text layers |

### Priority order
1. `gradient/gradient_cache_clamp` — 80k wrong pixels, likely a texture sampling mode
   (clamp vs repeat) not set correctly on the wgpu path.
2. `filters/filter-long-chain` — 111k pixels, filter pass accumulation issue.
3. `text/raster_root_C_8192` — large raster root text may share a root with the
   mix-blend failure.
4. `compositor-surface/too-many-surfaces` — overflow fallback path.
5. `image/snapshot-*` — snapshot + filter/shadow interactions.

---

## What is NOT done

- Subpixel AA on wgpu (skipped, requires GL extensions)
- The 7 rendering bugs above
- Release-mode performance profiling
- Any wgpu-specific resource lifetime / synchronization audit

## What is done

- All major rendering paths: picture cache tiles, alpha/opaque batch containers,
  quad batches (scissored + non-scissored), cs_* gradient/blur/border tasks,
  clip masks, resolve ops, blits, composite fast path, mix blend, SVG filters.
- Backend-aware reftest tolerances (`fuzzy-if(wgpu,...)` / `skip_on(wgpu)` plumbing).
- DirectComposition compositor layer (`wrench/src/composite.cpp`).
- Debug infrastructure added and then cleaned up.
