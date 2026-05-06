# Plan: Typed Pipeline Metadata for wgpu Backend

## Context

The wgpu backend identifies shader pipelines using string tuples like
`("ps_text_run", "ALPHA_PASS,GLYPH_TRANSFORM,TEXTURE_2D")`. This caused the
DPR=2 text bug — a wildcard match arm silently selected the wrong shader
variant. Replacing these stringly-typed keys with a flat enum gives us
compiler exhaustiveness checks that prevent this bug class entirely.

## Design: Single Flat `WgpuShaderVariant` Enum

A single enum rather than factored `ShaderName` + `ShaderConfig` enums, because
the config flags are NOT freely combinable — each shader only uses specific
configs. A product space would create hundreds of invalid combinations. The flat
enum makes every valid variant explicit and every invalid variant unrepresentable.

The enum covers the ~35 shader variants actually generated under wgpu feature
flags (DITHERING + DEBUG). No GL-only variants (TEXTURE_RECT, ADVANCED_BLEND,
DUAL_SOURCE_BLENDING, ANTIALIASING, REPETITION) are included.

## Enum Variants

```
// Brush shaders (opaque + alpha)
BrushSolid, BrushSolidAlpha,
BrushImage, BrushImageAlpha,
BrushBlend, BrushBlendAlpha,
BrushMixBlend, BrushMixBlendAlpha,
BrushLinearGradient, BrushLinearGradientAlpha,
BrushOpacity, BrushOpacityAlpha,
BrushYuvImage, BrushYuvImageAlpha,

// Text (the two variants that matter)
PsTextRun, PsTextRunGlyphTransform,

// Quads
PsQuadTextured,
PsQuadGradient, PsQuadRadialGradient, PsQuadConicGradient,
PsQuadMask, PsQuadMaskFastPath,

// Prim
PsSplitComposite,

// Clip
CsClipRectangle, CsClipRectangleFastPath, CsClipBoxShadow,

// Cache tasks
CsBorderSolid, CsBorderSegment, CsLineDecoration,
CsFastLinearGradient,
CsLinearGradient, CsRadialGradient, CsConicGradient,
CsBlurColor,
CsScale,

// Debug
DebugColor, DebugFont,

// Utility (texture cache ops)
PsClear, PsCopy,

// Debug overdraw variants (compiled but rarely used at runtime)
BrushSolidDebugOverdraw, BrushBlendDebugOverdraw, BrushMixBlendDebugOverdraw,
BrushLinearGradientDebugOverdraw, BrushOpacityDebugOverdraw, BrushOpacityAntialiasing,
BrushOpacityAlphaAntialiasing, BrushOpacityAntialiasingDebugOverdraw,
BrushImageDebugOverdraw, BrushImageAlphaRepetition, BrushImageRepetition,
BrushImageRepetitionDebugOverdraw, BrushImageAlphaRepetitionDebugOverdraw,
BrushYuvImageDebugOverdraw,
PsTextRunDebugOverdraw, CsBlurAlpha,
```

This is large. Pragmatic approach: **start with only the variants actually used
at runtime** (the first ~35 above). DEBUG_OVERDRAW and ANTIALIASING/REPETITION
variants can map via `from_shader_key()` → `None` and be compiled but not
indexed in the typed cache. They're only used with `DebugFlags::SHOW_OVERDRAW`
which the wgpu path doesn't support yet.

**Core variants (what we actually type): ~38 variants.**

## Bridge to Build-Time Strings

`WGSL_SHADERS: HashMap<(&str, &str), WgslShaderSource>` stays unchanged (no
build.rs changes). The enum provides:

- `shader_key(self) -> (&'static str, &'static str)` — enum → string pair
- `from_shader_key(name, config) -> Option<Self>` — string pair → enum (None for debug-overdraw etc.)
- `instance_layout(self) -> Option<&'static [InstanceField]>` — replaces the string match in create_all_pipelines

## File Changes

### 1. `webrender/src/device/wgpu_device.rs`

- **Define `WgpuShaderVariant`** near `WgpuBlendMode`/`WgpuDepthState` (~line 51)
  - Derive `Debug, Clone, Copy, PartialEq, Eq, Hash`
  - `shader_key()`, `from_shader_key()`, `instance_layout()` methods
  - `all()` → const array for test coverage

- **Change `shaders` HashMap** (line 230):
  `HashMap<WgpuShaderVariant, ShaderEntry>` (was `(&str, &str)`)

- **Change `pipelines` HashMap** (line 231-232):
  `HashMap<(WgpuShaderVariant, WgpuBlendMode, WgpuDepthState, wgpu::TextureFormat), WgpuProgram>`

- **Change `draw_instanced()` signature** (line 1551):
  `variant: WgpuShaderVariant` replaces `shader_name: &str, config: &str`
  Pipeline key: `(variant, blend_mode, depth_state, target_format)`
  Shader lookup: `self.shaders.get(&variant)`

- **Update `create_all_pipelines()`** (line 2352):
  Iterate WGSL_SHADERS, call `from_shader_key()`, skip None, store by variant.
  Replace string-match instance layout with `variant.instance_layout()`.

- **Update `create_pipeline_for_blend()`** (line 2269):
  `variant: WgpuShaderVariant` replaces `name: &str, config: &str`.
  Label: `format!("{:?}", variant)`.

### 2. `webrender/src/device/mod.rs`

- Add `WgpuShaderVariant` to public re-exports (line 39).

### 3. `webrender/src/renderer/mod.rs`

- **Change `batch_key_to_pipeline_key()`** (line 2199):
  Return `WgpuShaderVariant` instead of `(&str, &str)`.
  ```rust
  BatchKind::TextRun(glyph_format) => match glyph_format {
      GlyphFormat::TransformedAlpha | GlyphFormat::TransformedSubpixel =>
          WgpuShaderVariant::PsTextRunGlyphTransform,
      _ => WgpuShaderVariant::PsTextRun,
  }
  ```

- **Update `draw_batch!` macro** (~line 2005):
  `let variant = Self::batch_key_to_pipeline_key(...)` → pass to `draw_instanced`.

- **Update `pattern_to_shader` closure** (~line 2439):
  Return `WgpuShaderVariant` instead of `(&str, &str)`.

- **Update all direct `draw_instanced` calls** (~13 call sites):
  Replace string pairs with enum variants:
  - `"cs_clip_rectangle", ""` → `CsClipRectangle`
  - `"cs_clip_rectangle", "FAST_PATH"` → `CsClipRectangleFastPath`
  - `"cs_clip_box_shadow", "TEXTURE_2D"` → `CsClipBoxShadow`
  - `"cs_blur", "COLOR_TARGET"` → `CsBlurColor`
  - `"cs_scale", "TEXTURE_2D"` → `CsScale`
  - `"cs_border_solid", ""` → `CsBorderSolid`
  - `"cs_border_segment", ""` → `CsBorderSegment`
  - `"cs_line_decoration", ""` → `CsLineDecoration`
  - `"cs_fast_linear_gradient", ""` → `CsFastLinearGradient`
  - `"cs_linear_gradient", "DITHERING"` → `CsLinearGradient`
  - `"cs_radial_gradient", "DITHERING"` → `CsRadialGradient`
  - `"cs_conic_gradient", "DITHERING"` → `CsConicGradient`

- **Update `draw_cs!` and `draw_cs_blend!` macros** (~line 2575):
  Change `$shader:expr, $config:expr` to `$variant:expr`.

### 4. No changes to `build.rs` or `webrender_build/`

## Implementation Order (build-green at each step)

1. **Define the enum** in wgpu_device.rs with shader_key/from_shader_key/instance_layout.
   Re-export in device/mod.rs. Build succeeds — nothing uses it yet.

2. **Migrate WgpuDevice internals**: change shaders + pipelines HashMaps,
   draw_instanced signature, create_all_pipelines, create_pipeline_for_blend.
   This breaks the call sites temporarily.

3. **Migrate renderer call sites**: batch_key_to_pipeline_key return type,
   draw_batch macro, pattern_to_shader, all direct draw_instanced calls,
   draw_cs/draw_cs_blend macros.

4. **Build and verify.**

Steps 2-3 are effectively one atomic commit since changing draw_instanced's
signature breaks all callers. Can do as a single commit.

## Verification

- `cargo build --bin servoshell` from servo-graphshell (full compile)
- `SERVO_WGPU_BACKEND=1 cargo run --bin servoshell` — visual check that text,
  images, solid backgrounds, gradients all render correctly at DPR=1 and DPR=2
- No string shader names remain in renderer/mod.rs draw paths (grep check)
