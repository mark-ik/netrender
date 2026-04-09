# wgpu Backend Debug Plan

## Goal

Build a wgpu rendering backend for webrender. The GL backend is the reference — it works
and produces correct output. We need the wgpu path to produce **identical visual results**.

## Status: DPR=2 text bug FIXED (2026-04-02)

### DPR=1: Fully working (2026-04-01)

At DPR=1 (no HiDPI), the wgpu pipeline renders text correctly. This proves the
entire pipeline works: WGSL shaders, data texture uploads, ortho projection,
tile rendering, composite-to-surface.

### DPR=2: Text 2x size + wrong colors — FIXED (2026-04-02)

At DPR=2 (HiDPI display), text appeared 2x the correct CSS size and body text
rendered teal instead of dark gray. Both symptoms had the same root cause.

## Root cause (confirmed 2026-04-02)

### The bug: missing `GLYPH_TRANSFORM` shader variant selection

**File**: `webrender/src/renderer/mod.rs`, function `batch_key_to_pipeline_key()`

The wgpu path hardcoded all text batches to use `"ALPHA_PASS,TEXTURE_2D"`:
```rust
BatchKind::TextRun(..) => ("ps_text_run", "ALPHA_PASS,TEXTURE_2D"),
```

This ignored the `GlyphFormat` discriminant carried by `BatchKind::TextRun(GlyphFormat)`.
The GL path (in `shade.rs:519-527`) correctly selects between two shader variants:
- `GlyphFormat::Alpha | Subpixel | Bitmap | ColorBitmap` → `simple` (no glyph transform)
- `GlyphFormat::TransformedAlpha | TransformedSubpixel` → `glyph_transform` (with `GLYPH_TRANSFORM` feature)

### Why this matters at DPR=2

Servo handles HiDPI by pushing a 2x reference frame transform (`painter.rs:688-695`)
rather than setting `global_device_pixel_scale`. WebRender's `global_device_pixel_scale`
is hardcoded to 1.0 (`frame_builder.rs:684`).

When a non-trivial transform is present (like 2x scale), WebRender's text run preparation
(`prim_store/text_run.rs:303-317`) detects it's not a simple 2D translation and:
1. Sets `transform_glyphs = true`
2. Bakes the 2x transform into `FontTransform` → glyphs rasterized at 2x pixel size
3. Sets `raster_scale = 1.0`
4. Emits `GlyphFormat::TransformedAlpha` (or `TransformedSubpixel`)

The `GLYPH_TRANSFORM` shader variant handles this correctly:
```glsl
// GLYPH_TRANSFORM path (correct):
mat2 glyph_transform = mat2(transform.m) * task.device_pixel_scale;
mat2 glyph_transform_inv = inverse(glyph_transform);
// ... compute glyph rect in glyph space, then transform back to local space
RectWithEndpoint local_rect = transform_rect(glyph_rect, glyph_transform_inv);
```

The non-transform path (what wgpu was using) does NOT compensate:
```glsl
// Non-GLYPH_TRANSFORM path (wrong for transformed glyphs):
float glyph_scale_inv = res.scale / glyph_raster_scale;  // = 1.0
// glyph rect in local space = glyph_scale_inv * glyph_pixel_size
// → 1.0 * 46 texels = 46 CSS units → 92 device pixels after 2x transform (2x too big!)
```

### Why colors were also wrong

The `GLYPH_TRANSFORM` and non-transform shader variants have different `v_color` and
`v_mask_swizzle` computation in both vertex and fragment shaders. Using the wrong variant
meant the color/mask logic was also incorrect, producing the teal-instead-of-gray symptom.

### The fix

```rust
BatchKind::TextRun(glyph_format) => {
    match glyph_format {
        GlyphFormat::TransformedAlpha |
        GlyphFormat::TransformedSubpixel => {
            ("ps_text_run", "ALPHA_PASS,GLYPH_TRANSFORM,TEXTURE_2D")
        }
        _ => ("ps_text_run", "ALPHA_PASS,TEXTURE_2D"),
    }
}
```

### Why DPR=1 was unaffected

At DPR=1, there is no 2x reference frame. The transform is identity (a simple 2D
translation), so `transform_glyphs = false`, glyph format is `Alpha` (not `Transformed*`),
and the non-transform shader variant is correct.

## Debugging journey (for reference)

### What was confirmed identical between GL and wgpu
- Frame struct: same transforms, prim headers, render tasks, glyph atlas
- Tile cache DPS: 1.0 in both paths (surface at SpatialNodeIndex(0), same as root)
- World scale factors: (1.0, 1.0) in both paths
- GLSL vs WGSL shader math: identical (verified line-by-line via naga output)
- GPU cache addressing: identical (texelFetch/textureLoad, same UV math)
- Data texture layout: identical (MAX_VERTEX_TEXTURE_WIDTH=1024, same packing)
- Composite tile rects: identical placement on surface

### Key diagnostic data (DPR=2)
```
surface size: 2048x1480 (physical pixels, DPR=2)
tile texture: 1024x512 (Rgba8Unorm)
transform[2]: row0=[2.000,0.000,0.000,0.000] row1=[0.000,2.000,0.000,0.000]
render_task[0]: data=[0.0, 0.0, 1024.0, 512.0, 1.0, 0.0, 0.0, 0.0]
  → task_rect=(0,0)-(1024,512), DPS=1.0, content_origin=(0,0)
user_data=[65535, 0, 0, 0] → raster_scale=1.0
DPS_DIAG: surface[0].device_pixel_scale=1.000, world_scale=(1.000,1.000)
WSF_DIAG: parent=None surface_node=SpatialNodeIndex(0) root_ref=SpatialNodeIndex(0) scale=(1.000,1.000)

# Before fix:
wgpu batch[1]: shader="ps_text_run" config="ALPHA_PASS,TEXTURE_2D" ← WRONG

# After fix:
wgpu batch[1]: shader="ps_text_run" config="ALPHA_PASS,GLYPH_TRANSFORM,TEXTURE_2D" ← CORRECT
```

### Hypotheses explored and ruled out
1. **Tile cache DPS should be 2.0** — No, DPS=1.0 is correct; GL uses same value
2. **Data texture upload mismatch** — No, CPU data identical, layout math identical
3. **Ortho projection difference** — No, both use tile texture dimensions
4. **Composite rect/UV mapping** — No, both use same `get_device_rect()`
5. **GPU cache addressing** — No, verified `get_gpu_cache_uv` identical in WGSL
6. **Surface double-scaling** — No, PerMonitorV2 DPI awareness is set

## All fixes applied so far

1. `texels_per_item` 3→2 for render tasks (mod.rs)
2. Clear color: `draw_instanced` signature → `clear_color: Option<wgpu::Color>`
3. Picture cache tiles: use `picture_target.clear_color` instead of hardcoded black
4. Tile projection: use full texture dimensions, not dirty_rect (mod.rs)
5. **Text at DPR=2**: Select `GLYPH_TRANSFORM` shader variant for `TransformedAlpha`/`TransformedSubpixel` glyph formats (mod.rs `batch_key_to_pipeline_key`)

## Key Files

- `webrender/src/renderer/mod.rs` — wgpu render path + GL render path (reference)
  - `batch_key_to_pipeline_key()` — **the fix location**
  - wgpu tile rendering: lines ~2000-2340
  - GL tile rendering: `draw_picture_cache_target` at line 5277
  - GL composite: `composite_frame` at line 7680, `composite_simple` at line 5969
  - wgpu composite: lines 1600-1800
- `webrender/src/renderer/shade.rs:519-527` — GL shader selection (reference for fix)
- `webrender/src/device/wgpu_device.rs` — draw_instanced, composite pipeline, ortho()
- `webrender/src/picture.rs` — tile cache setup, DPS computation
- `webrender/src/prim_store/text_run.rs:270-338` — font instance update, transform_glyphs logic
- `webrender/src/frame_builder.rs:684` — `global_device_pixel_scale = 1.0` (hardcoded)
- `servo-graphshell/components/paint/painter.rs:688` — 2x HiDPI reference frame
- `webrender/res/ps_text_run.glsl:130-198` — GLYPH_TRANSFORM vs non-transform shader paths
