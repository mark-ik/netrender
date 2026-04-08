# P9 Progress Report — wgpu Renderer Implementation

**Date**: 2026-04-04
**Branch**: `wgpu-device-renderer`
**Last commit**: `c86d2f0bd notes: P8 progress report` (uncommitted P9 changes in `renderer/mod.rs`)

---

## Reftest Results

**Current (P9)**: 261/413 pass (63.2%) — 152 failures
**Previous (P8)**: 261/413 pass (63.2%) — 152 failures

No net change in pass/fail counts. The session focused on MixBlend investigation
and infrastructure for two-pass rendering. The changes are correct (no regressions)
but don't yet fix the blend failures.

### Per-category breakdown

| Category           | Pass | Total | Fail | Delta vs P8 |
|--------------------|------|-------|------|-------------|
| aa                 | 1    | 3     | 2    | 0 |
| backface           | 8    | 10    | 2    | 0 |
| blend              | 8    | 23    | 15   | 0 |
| border             | 12   | 22    | 10   | 0 |
| boxshadow          | 10   | 16    | 6    | 0 |
| clip               | 10   | 17    | 7    | 0 |
| compositor         | 2    | 4     | 2    | 0 |
| compositor-surface | 3    | 7     | 4    | 0 |
| crash              | 1    | 2     | 1    | 0 |
| filters            | 48   | 70    | 22   | 0 |
| gradient           | 64   | 80    | 16   | 0 |
| image              | 21   | 26    | 5    | 0 |
| mask               | 6    | 11    | 5    | 0 |
| performance        | 0    | 1     | 1    | 0 |
| scrolling          | 19   | 26    | 7    | 0 |
| snap               | 1    | 3     | 2    | 0 |
| split              | 10   | 16    | 6    | 0 |
| text               | 21   | 48    | 27   | 0 |
| tiles              | 1    | 2     | 1    | 0 |
| transforms         | 12   | 23    | 11   | 0 |

---

## What This Session Worked On (P9)

### 1. Two-pass MixBlend rendering scheme

**File**: `renderer/mod.rs`, picture_cache loop and all_targets loop

Added a two-pass MixBlend scheme to both render target types:

**Picture cache tiles** (lines ~3001-3203):
- Pass 1 (`LoadOp::Clear`): Draw all non-MixBlend batches (opaque + alpha)
- Backdrop copy: `encoder.copy_texture_to_texture()` from picture tile to
  readback texture (outside render pass)
- Pass 2 (`LoadOp::Load`): Draw only MixBlend batches with the readback
  (color0) and source child picture (color1)

**Texture cache / alpha / color targets** (lines ~2660-2846):
- Same pattern: detect `has_mix_blend`, defer MixBlend batches in pass 1,
  copy backdrop, draw MixBlend in pass 2

Key design decisions:
- wgpu cannot do framebuffer reads inside a render pass (unlike GL), so we
  must end pass 1, do the copy, and start pass 2 with `LoadOp::Load`
- Depth buffer is preserved across passes (`LoadOp::Load` for depth in pass 2)
- `pic_has_mix_blend` / `has_mix_blend` flags gate the two-pass path

### 2. Loop reorder: all_targets before picture_cache

**File**: `renderer/mod.rs`, `draw_passes_wgpu()` (lines ~2042-3209)

Reordered the per-pass loops so that `texture_cache + alpha + color` targets
render BEFORE `picture_cache` targets. This matches the GL path ordering:

```
for pass in frame.passes:
  1. texture_cache targets  (dependencies: e.g. child pictures)
  2. alpha targets          (dependencies: e.g. intermediate surfaces)
  3. color targets
  4. picture_cache targets  (consumers: use textures from 1-3)
```

Previously our wgpu path had picture_cache first, which meant MixBlend's
`color1` (child picture texture) hadn't been rendered yet when used.

### 3. Diagnostic investigation

Confirmed via debug prints that for `multiply.yaml`:
- `[BD]` backdrop copy executes: `src=CacheTextureId(1) dst=CacheTextureId(3) src=(25,25,50,50) dst=(0,0,50,50)`
- `[MB2]` MixBlend pass 2 executes: `color0=CacheTextureId(3) color1=CacheTextureId(3)` both in cache
- The picture tile is `CacheTextureId(1)` (own texture from `picture_textures` pool)
- The readback and source child picture share the same atlas `CacheTextureId(3)`
- No wgpu validation errors visible in logs

Despite all of this executing, the test output is white (255,255,255,255) where
it should be green (0,255,0,255). The entire 100x100 test area (10000 pixels)
has max diff 255.

---

## Failing Blend Tests (15)

All tests that actually invoke `BrushMixBlend` fail. Tests that only use
isolation / stacking contexts (no mix-blend-mode) pass.

MixBlend tests (all fail):
- multiply, multiply-2, multiply-3
- difference, difference-transparent, repeated-difference
- darken, lighten
- large

Non-MixBlend blend tests that also fail (likely other causes):
- isolated, isolated-with-filter, isolated-premultiplied-2
- mix-blend-invalid-backdrop, raster-roots-1
- backdrop-filter-blend-container

---

## Hypotheses for the Remaining Blend Failure

### H1: Coordinate space mismatch (most likely)

The backdrop copy shows `src=(25,25,50,50)` but at HiDPI factor 2, the CSS
bounds `[25, 25, 50, 50]` should map to device pixels `(50, 50, 100, 100)`.
If `readback_origin` from `ReadbackTask` is in device pixels (it's typed as
`DevicePoint`) but the picture tile was rendered in layout/CSS coordinates,
the copy reads from the wrong region.

Need to verify: what coordinate space does `content_origin` and `readback_origin`
actually use in the render task graph? And what coordinate space is the ortho
projection in the picture tile's render pass using?

### H2: The MixBlend shader output is discarded by depth test

If `has_opaque = true` (the 100x100 green rect is opaque), pass 2 uses
`WgpuDepthState::TestOnly`. The MixBlend fragment must have a smaller z-value
than the opaque green rect wrote in pass 1. If the z ordering puts MixBlend
"behind" the green rect, its fragments are discarded.

To test: temporarily force `alpha_depth = WgpuDepthState::None` in the pass 2
MixBlend loop and see if the output changes.

### H3: The MixBlend shader never actually runs

`ensure_pipeline()` might silently fail (returning a wrong/dummy pipeline)
or the shader module for `BrushMixBlendAlpha` might have a compilation issue.
The `log::warn` in `record_draw` would fire if the pipeline isn't found, but
maybe it's compiled to a wrong variant.

To test: add `eprintln!` inside `record_draw` confirming the pipeline was
found and `draw_indexed` was called.

### H4: Sampler/UV coordinates are wrong

The `TEX_SIZE(sColor0).xy` and `TEX_SIZE(sColor1).xy` uniforms in the shader
are set to the RENDER TARGET size (the picture tile), not the SOURCE TEXTURE
size (CacheTextureId(3)). If the UV calculation uses tex_size to normalize
pixel coordinates, sampling from a differently-sized texture would produce
wrong UVs.

This is a strong candidate: the `create_target_uniforms(target_w, target_h)`
creates a `tex_size_buf` matching the picture tile texture, but sColor0 and
sColor1 are in CacheTextureId(3) which may be a different size.

---

## Architecture Notes — Updated

### MixBlend rendering pipeline (wgpu path)

```
For picture_cache tiles with MixBlend:
  Pass 1 (Clear):
    - opaque batches (front-to-back, depth write)
    - alpha batches (back-to-front, depth test) EXCEPT MixBlend
  
  Between passes (outside render pass):
    - For each MixBlend batch:
      copy_texture_to_texture(picture_tile → readback_texture)
  
  Pass 2 (Load):
    - MixBlend batches only
    - color0 = readback texture (backdrop copy)
    - color1 = child picture texture (rendered in all_targets loop)
    - Shader: brush_mix_blend.glsl computes blend in fragment shader
    - GPU blend: PremultipliedAlpha (standard over-composite)

For texture_cache/alpha targets with MixBlend:
  Same pattern, but backdrop source = target.texture_id via
  get_target_texture() instead of cache_tex_id
```

### Debug prints currently in code

- `[BD]` — backdrop copy in picture_cache MixBlend (line ~3113)
- `[MB2]` — MixBlend pass 2 texture bindings (lines ~3185-3199)

These should be removed once the blend issue is resolved.

---

## Next Steps (Priority Order)

1. **Test H2 (depth)**: Force `WgpuDepthState::None` for MixBlend pass 2 draws
   and re-run multiply.yaml. Quick test, high signal.

2. **Test H4 (tex_size)**: Check if `tex_size_buf` in the MixBlend draw uses
   the picture tile size vs the color0/color1 texture size. The brush_mix_blend
   vertex shader uses `TEX_SIZE(sColor0)` to convert pixel→UV coordinates.
   If this is wrong, all UV lookups sample garbage.

3. **Test H1 (coordinates)**: Add eprintln for `readback_origin`, `backdrop_screen_origin`,
   and `backdrop_rect` values. Compare with device_pixel_scale to verify
   coordinate space.

4. **Pixel readback debugging**: Use `read_texture_pixels` on CacheTextureId(1)
   after pass 1 to verify the picture tile has green content. Then on
   CacheTextureId(3) after the backdrop copy to verify the copy worked.

5. **Remove debug prints** and commit P9 once blend is fixed.

6. **C4 render pass sharing** — plan at `logical-whistling-mochi.md`

7. **Text / dual-source blending** — 27 text failures

---

## File Changes Summary

### `webrender/src/renderer/mod.rs` (+573 / -190 uncommitted)

- `draw_passes_wgpu()`: Reordered `all_targets` loop before `picture_cache`
- `draw_passes_wgpu()` all_targets loop: Added `has_mix_blend` detection,
  two-pass rendering with backdrop copy for MixBlend batches
- `draw_passes_wgpu()` picture_cache loop: Added `pic_has_mix_blend` detection,
  two-pass rendering with backdrop copy from picture tile texture
- Debug eprints for `[BD]` and `[MB2]` (temporary)
