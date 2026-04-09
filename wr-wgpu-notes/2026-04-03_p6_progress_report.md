# P6 Progress Report — wgpu Renderer Implementation

**Date**: 2026-04-03
**Branch**: `wgpu-device-renderer`
**Last commit**: `da8eb8492 wgpu: fix quad batch routing and composite pass (P6)`
**Uncommitted**: +335 lines across `wgpu_device.rs` and `renderer/mod.rs`

---

## Reftest Results

**Current**: 225/366 pass (61%) — 141 failures
**Previous** (commit da8eb84): 398/714 pass (55.7%) — 316 failures

Note: the two runs use different test harness methods (built-in `wrench reftest` vs
the comparison script), so the denominators differ. The built-in harness skips some
tests that the script counted. The absolute pass count is lower, but the pass *rate*
improved and several previously-blank categories now render.

### Per-category breakdown

| Category           | Pass | Total | Fail | Notes |
|--------------------|------|-------|------|-------|
| aa                 | 1    | 3     | 2    | |
| backface           | 8    | 10    | 2    | |
| blend              | 1    | 11    | 10   | offscreen surfaces still mostly blank |
| border             | 12   | 22    | 10   | |
| boxshadow          | 10   | 16    | 6    | |
| clip               | 10   | 17    | 7    | one test triggers encoder-invalid panic |
| compositor          | 5    | 11    | 6    | |
| compositor-surface | 3    | 7     | 4    | |
| crash              | 1    | 2     | 1    | |
| **filters**        | **48** | **70** | **22** | big improvement from SVG filter + alpha_batch_containers |
| **gradient**       | **54** | **64** | **10** | most now pass via alpha_batch_containers |
| image              | 21   | 26    | 5    | |
| mask               | 6    | 11    | 5    | |
| performance        | 0    | 1     | 1    | |
| scrolling          | 7    | 12    | 5    | |
| snap               | 1    | 3     | 2    | |
| **split**          | **10** | **16** | **6** | SplitComposite routing fix helped |
| text               | 14   | 39    | 25   | subpixel/dual-source blending unsupported |
| tiles              | 1    | 2     | 1    | |
| transforms         | 12   | 23    | 11   | |

---

## What This Session Added (uncommitted P7 work)

### 1. `BatchKind::SplitComposite` routing fix
**File**: `renderer/mod.rs` `batch_key_to_pipeline_key()`

The `_` catch-all was silently routing `SplitComposite` batches to `BrushSolid`.
Now correctly routes to `WgpuShaderVariant::PsSplitComposite`. The match is now
exhaustive (no catch-all).

### 2. `alpha_batch_containers` dispatch for texture cache targets
**File**: `renderer/mod.rs` texture cache target loop in `draw_passes_wgpu()`

This was the highest-impact change. The GL renderer draws `alpha_batch_containers`
for every texture cache / alpha / color render target — these contain the opaque and
alpha batches for offscreen surfaces (isolated stacking contexts, filter effects,
blend modes). The wgpu path was completely skipping them.

Added:
- `has_alpha_batches` check in the skip condition
- `needs_depth` detection via `target.needs_depth()`
- Conditional depth attachment creation (`acquire_depth_view`)
- Full batch dispatch with the same `record_batch!` macro pattern used by picture cache tiles
- Opaque batches drawn front-to-back with depth write, alpha batches back-to-front with depth test
- Scissor rect support from `task_scissor_rect`

### 3. SVG filter shader variants (`CsSvgFilter`, `CsSvgFilterNode`)
**File**: `wgpu_device.rs` + `renderer/mod.rs`

Added both variants to `WgpuShaderVariant` enum with shader key mappings,
`from_shader_key` reverse mappings, and instance layouts.

Key challenge: the `SvgFilterInstance` and `SVGFEFilterInstance` structs use `u16`
fields (kind, input_count, generic_int, extra_data_address) which have no direct
wgpu vertex format mapping. Solution: **CPU-side repacking** to i32 fields before
GPU upload. Each filter dispatch builds a repacked buffer with widened fields.

Dispatch added to `draw_cache_target_tasks_wgpu()` with proper texture bindings
from `BatchTextures`.

### 4. Texture-to-texture blits
**File**: `renderer/mod.rs` texture cache target loop

Added `encoder.copy_texture_to_texture()` for `target.blits` entries, executed
before the render pass begins (as the GL path does with `glBlitFramebuffer`).

Includes validation: skip zero-size copies, self-blits (same texture ID), and
copies that exceed source/target bounds.

### 5. Threaded shader compilation (`create_all_pipelines_threaded`)
**File**: `wgpu_device.rs`

**Root cause**: naga's WGSL parser uses recursive descent and overflows the default
thread stack (~1-2 MB) for large transpiled shaders. `cs_svg_filter_node` is ~3000
lines of WGSL and causes a stack overflow during `device.create_shader_module()`.

**Fix**: `create_all_pipelines` is now called on a dedicated thread with a 16 MB
stack via `std::thread::Builder`. Uses a `SendPtr<T>` wrapper to safely send
`&wgpu::Device` and `&wgpu::PipelineLayout` references across the thread boundary
(the thread is joined before the references go out of scope).

This also fixes a **pre-existing bug** where `wrench --wgpu png` and
`wrench --wgpu reftest` would stack overflow even without any of the other changes.

### 6. Resilient encoder error handling
**File**: `wgpu_device.rs` `flush_encoder()`

`encoder.finish()` panics if any previous command on the encoder failed validation.
This was causing the entire reftest run to abort on the first encoder error.

Fix: wrap `encoder.finish()` in `catch_unwind` and push/pop a wgpu error scope
around the submit. Validation errors are now logged and the test run continues.

---

## Known Gaps / Remaining Failure Sources

### High impact (many failures)

1. **text (25 failures)**: Subpixel text rendering requires `BlendMode::SubpixelDualSource`
   which needs `wgpu::Features::DUAL_SOURCE_BLENDING`. Currently falls back to
   `PremultipliedAlpha`, making subpixel text look like regular alpha text.

2. **blend (10 failures)**: Offscreen blend-mode surfaces need `alpha_batch_containers`
   in the correct render target. Some failures may be from missing `resolve_ops` (the
   parent-to-child copy needed by backdrop-filter).

3. **transforms (11 failures)**: Mix of `SplitComposite` precision issues and missing
   offscreen rendering for preserve-3d contexts.

### Medium impact

4. **`target.resolve_ops`**: Not yet implemented. These are parent-picture-to-child-target
   copies used by backdrop-filter and picture cache surface resolution. Affects some
   filter, blend, and gradient failures.

5. **`target.clip_masks` (ClipMaskInstanceList)**: Not yet dispatched. Only `clip_batcher`
   (rectangle and box-shadow clips) is handled. Image-based clip masks and GPU-driven
   mask operations are skipped.

6. **Encoder-invalid panics**: Some tests produce wgpu validation errors that invalidate
   the encoder. Currently caught by `catch_unwind` but the underlying cause (likely
   depth attachment or format mismatch) should be diagnosed.

### Low impact / deferred

7. **brush_image antialiasing/repetition variants**: The WGSL files for
   `ANTIALIASING,REPETITION,TEXTURE_2D` exist but aren't mapped. Tiled/repeated images
   get the fast-path shader.

8. **`PsQuadMaskFastPath`**: Defined but never dispatched — all quad masks use the
   full `PsQuadMask` variant.

---

## Architecture Notes

### Rendering pipeline (wgpu path)

```
draw_passes_wgpu()
  for pass in frame.passes:
    1. Picture cache tiles (per tile):
       - render pass with optional depth
       - opaque batches (front-to-back, depth write)
       - alpha batches (back-to-front, depth test)
       - uses batch_key_to_pipeline_key() + record_draw()
    
    2. Texture cache / alpha / color targets (per target):
       a. Blits (copy_texture_to_texture, before render pass)
       b. Render pass (with optional depth if needs_depth()):
          - cs_* cache tasks (borders, gradients, blurs, scalings, SVG filters)
          - Clip masks (primary overwrite, secondary multiplicative)
          - Quad batches (prim_instances by PatternKind)
          - Alpha batch containers (opaque + alpha, same as picture cache)
    
    3. Flush encoder
  
  Composite pass:
    - Color tiles (solid rectangles)
    - Textured tiles (CompositeFastPath shader)
    - Readback to staging buffer
```

### Key type mappings

| WebRender type | wgpu equivalent |
|---|---|
| `BatchKind::Brush(*)` | `WgpuShaderVariant::Brush*` / `Brush*Alpha` |
| `BatchKind::Quad(PatternKind)` | `WgpuShaderVariant::PsQuad*` |
| `BatchKind::TextRun(GlyphFormat)` | `PsTextRun` / `PsTextRunGlyphTransform` |
| `BatchKind::SplitComposite` | `PsSplitComposite` |
| `BlendMode::*` | `WgpuBlendMode::*` (7 variants + fallback) |
| `PrimitiveInstanceData` | 16-byte `aData: ivec4` instance buffer |
| `SvgFilterInstance` | Repacked to 32-byte i32 layout |
| `SVGFEFilterInstance` | Repacked to 56-byte i32/f32 layout |

---

## Next Steps

1. **Commit** current work as P7
2. **Diagnose encoder-invalid errors** — likely depth/format mismatch in specific targets
3. **Implement `resolve_ops`** — texture-to-texture copies with render task rect lookup
4. **Implement `clip_masks`** — dispatch `ClipMaskInstanceList` for image-based masks
5. **Text rendering** — investigate `DUAL_SOURCE_BLENDING` feature support
6. **Servo integration** — test the wgpu path through Servo's compositor
