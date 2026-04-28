# P8 Progress Report — wgpu Renderer Implementation

**Date**: 2026-04-03
**Branch**: `wgpu-device-renderer`
**Last commit**: `f68b6feaf wgpu: add resolve_ops and clip_masks dispatch (P8)`

---

## Reftest Results

**Current (P8)**: 261/413 pass (63.2%) — 152 failures
**Previous (P7)**: 225/366 pass (61.5%) — 141 failures

Note: total count increased because some tests that were previously crashing the
harness (encoder-invalid panic) now complete. Net new passes = +36.

### Per-category breakdown

| Category           | Pass | Total | Fail | Delta vs P7 |
|--------------------|------|-------|------|-------------|
| aa                 | 1    | 3     | 2    | 0 |
| backface           | 8    | 10    | 2    | 0 |
| blend              | 8    | 23    | 15   | **+7** |
| border             | 12   | 22    | 10   | 0 |
| boxshadow          | 10   | 16    | 6    | 0 |
| clip               | 10   | 17    | 7    | 0 |
| compositor         | 2    | 4     | 2    | (harness diff) |
| compositor-surface | 3    | 7     | 4    | 0 |
| crash              | 1    | 2     | 1    | 0 |
| filters            | 48   | 70    | 22   | 0 |
| gradient           | 64   | 80    | 16   | **+10** |
| image              | 21   | 26    | 5    | 0 |
| mask               | 6    | 11    | 5    | 0 |
| performance        | 0    | 1     | 1    | 0 |
| scrolling          | 19   | 26    | 7    | **+12** |
| snap               | 1    | 3     | 2    | 0 |
| split              | 10   | 16    | 6    | 0 |
| text               | 21   | 48    | 27   | **+7** |
| tiles              | 1    | 2     | 1    | 0 |
| transforms         | 12   | 23    | 11   | 0 |

---

## What This Session Added (P8)

### 1. `resolve_ops` — parent-to-child surface copies
**File**: `renderer/mod.rs` texture cache target loop in `draw_passes_wgpu()`

Added before the render pass opens (like GL's `handle_resolve()`). Copies from
a source picture task's render target to a destination picture task's render
target using `encoder.copy_texture_to_texture()`.

Key details:
- Uses `content_size` for blur-expanded destination targets (not task rect size)
- Computes intersection in layout space, scaled to device pixels independently
  for source and destination (handles different DPR cases)
- Validates: zero-size copies skipped, self-blit (same texture ID) skipped,
  out-of-bounds source/dest coordinates skipped
- Destination texture looked up via `get_target_texture()` (may differ from
  the current loop's target)

This enables backdrop-filter and picture-cache surface resolution.

### 2. `clip_masks` — ClipMaskInstanceList dispatch
**File**: `renderer/mod.rs` texture cache target loop in `draw_passes_wgpu()`

Added inside the render pass after `alpha_batch_containers`. Dispatches all
six fields of `ClipMaskInstanceList`:

| Field | Shader | Blend |
|---|---|---|
| `mask_instances_fast` | `PsQuadMaskFastPath` | MultiplyClipMask |
| `mask_instances_fast_with_scissor` | `PsQuadMaskFastPath` | MultiplyClipMask + per-draw scissor |
| `mask_instances_slow` | `PsQuadMask` | MultiplyClipMask |
| `mask_instances_slow_with_scissor` | `PsQuadMask` | MultiplyClipMask + per-draw scissor |
| `image_mask_instances` | `PsQuadTextured` | MultiplyClipMask + color0 texture |
| `image_mask_instances_with_scissor` | `PsQuadTextured` | MultiplyClipMask + per-draw scissor + color0 |

Instance data: `MaskInstance` = 32 bytes (PrimitiveInstanceData 16 + 4×i32 16),
matching the pre-defined `MASK_INSTANCE_LAYOUT`. `image_mask_instances` use
`PrimitiveInstanceData` (16 bytes), matching `PRIMITIVE_INSTANCE_LAYOUT`.

Also added `WgpuShaderVariant` to the `use crate::device::` import in
`draw_passes_wgpu()`.

---

## Known Gaps / Remaining Failure Sources

### High impact (many failures)

1. **text (27 failures)**: Subpixel text rendering requires
   `BlendMode::SubpixelDualSource` which needs `wgpu::Features::DUAL_SOURCE_BLENDING`.
   Currently falls back to `PremultipliedAlpha`.

2. **blend (15 failures)**: Still has blend-mode surface failures. Some may be
   from `resolve_ops` not correctly resolving the backdrop copy (the source
   texture may not have been rendered yet when the copy happens). Also:
   `MixBlend` shaders need the correct backdrop texture binding.

3. **transforms (11 failures)**: Mostly `large-raster-root`, `raster-root-large-mask`,
   `raster-root-huge-scale`, `non-inversible-world-rect` — these produce blank
   output (not just wrong), suggesting missing render target initialization or
   clipping.

4. **gradient (16 failures)**: 16 still fail despite 64 passing. The remaining
   failures likely involve radial/conic gradients in contexts requiring
   resolve_ops or specific blend modes.

### Medium impact

5. **scrolling (7 failures)**: 19 now pass, 7 still fail. The failures may
   involve complex scroll containers that require proper clipping or
   alpha-pass surfaces.

6. **Encoder-invalid panics**: Some tests still hit wgpu validation errors
   (caught by `catch_unwind`). Likely source: depth attachment format mismatch
   in specific render target configurations.

7. **`target.resolve_ops` dest texture lookup**: The current implementation
   looks up `dest_task.get_target_texture()` which may resolve to a texture
   that hasn't been populated yet (render order issue).

### Low impact / deferred

8. **brush_image antialiasing/repetition variants**: Not mapped.
9. **`PsQuadMaskFastPath`**: Defined but tests show no improvement from adding
   it vs PsQuadMask — may indicate the fast path isn't being taken in test cases.

---

## Architecture Notes

### Rendering pipeline (wgpu path) — updated

```
draw_passes_wgpu()
  for pass in frame.passes:
    1. Picture cache tiles (per tile):
       - take encoder, create render pass with optional depth
       - opaque batches (front-to-back, depth write)
       - alpha batches (back-to-front, depth test)
       - return encoder

    2. Texture cache / alpha / color targets (per target):
       a. Blits (copy_texture_to_texture, before render pass)
       b. Resolve ops (copy_texture_to_texture, before render pass)
       c. Render pass (with optional depth if needs_depth()):
          - cs_* cache tasks (borders, gradients, blurs, scalings, SVG filters)
          - Clip batch list primary (None blend)
          - Clip batch list secondary (MultiplyClipMask)
          - Quad batches (prim_instances by PatternKind)
          - Alpha batch containers (opaque + alpha)
          - ClipMaskInstanceList (MultiplyClipMask for all variants)
       d. return encoder

    3. Flush encoder

  Composite pass:
    - Color tiles (solid rectangles)
    - Textured tiles (CompositeFastPath shader)
    - Readback to staging buffer
```

---

## Next Steps

1. **Diagnose blend failures** — check if `resolve_ops` ordering is correct
   (backdrop source may need to be resolved before the blend target renders)
2. **Diagnose transform blank-output failures** — check large-raster-root path
3. **Text / dual-source blending** — investigate `wgpu::Features::DUAL_SOURCE_BLENDING`
4. **C4 render pass sharing** — the plan file at `logical-whistling-mochi.md`
   describes merging all draws to the same target into one render pass
5. **Servo integration** — test wgpu path through Servo's compositor
