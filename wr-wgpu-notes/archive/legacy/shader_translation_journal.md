---
name: GLSL→WGSL Shader Translation Journal
description: Technical record of all naga workarounds implemented in build.rs to achieve 100% WGSL translation of WebRender's 61 shader variants.
type: technical-journal
---

Archived: kept only as historical context for the retired GLSL-front-end translation path.

# GLSL→WGSL Shader Translation — Technical Journal

This documents every naga limitation encountered and the text-level GLSL
preprocessing workaround applied in `webrender/build.rs` to achieve 100%
translation success (61/61 variants on webrender 0.62, 63/63 variants on webrender 0.68).

All transforms are gated behind `#[cfg(feature = "wgpu_backend")]` and
have zero effect on the existing GL backend.

---

## Score progression

| Stage | Commit | Score | Key fixes |
|-------|--------|-------|-----------|
| 3 (initial) | — | 0/74 | naga can't parse combined `sampler2D` |
| 4b | d12fffbf5 | 24/61 | Sampler split, locations, texture wrappers, feature flag trim |
| 4c | 41eeb5745 | 24/61 | Paren-balanced function scanner (infra for 4e) |
| 4d | d2a3c8afe | 31/61 | Switch fall-through (5 passes) |
| 4e | 6d8d18c3f | 45/61 | Stage-ifdef resolution + definition reorder |
| 4f | 03a065e1a | **61/61** | 7 fixes (see below) |

---

## Preprocessing pipeline (order matters)

```
preprocess_for_naga(glsl, stage)
  │
  ├─ Step 0:  resolve_stage_ifdefs()        — strip inactive WR_VERTEX/FRAGMENT_SHADER blocks
  ├─ Step 0b: decompose_matrix_varyings()   — mat3/mat4 varyings → column vectors
  ├─ Step 0c: decompose_array_struct_stores()— s.field = type[N](...) → element-by-element
  ├─ Pass 1:  sampler split + locations      — sampler2D → texture2D + global_sampler; layout(binding/location)
  ├─ Pass 2:  texture() call rewrite         — texture(sName, → texture(sampler2D(sName, global_sampler),
  ├─ Pass 3:  rewrite_sampler_params()       — functions taking sampler2D → texture2D params
  ├─ Strip:   strip_precision()              — remove highp/mediump/lowp everywhere (incl. #define macros)
  ├─ Fix:     fix_switch_fallthrough()       — 6 sub-passes for WGSL switch compat
  └─ Reorder: move_definitions_before_prototypes()  — fix naga ForwardDependency
```

---

## Workaround catalog

### 1. Combined sampler split (Stage 4b)

**naga error:** `InvalidToken` — naga's GLSL frontend is Vulkan-style only; it doesn't recognize `sampler2D` as a combined type.

**Fix:** Pre-scan for `uniform sampler2D sName;` declarations. Replace type with `texture2D`. Inject `layout(binding=0, set=1) uniform sampler global_sampler;`. Rewrite `texture(sName, uv)` → `texture(sampler2D(sName, global_sampler), uv)`.

**Variants unblocked:** 24 (from 0)

### 2. Interface variable locations (Stage 4b)

**naga error:** `BindingCollision` — naga requires explicit `layout(location=N)` on all interface variables.

**Fix:** `storage_qual()` scanner detects `varying`/`in`/`out`/`attribute` (with prefix qualifiers like `flat`, `PER_INSTANCE`). Assigns sequential locations: `next_attr_loc` for vertex inputs, `next_vary_loc` for varying outputs/inputs (shared counter ensures vertex out matches fragment in), `location=0` for fragment output.

### 3. Precision qualifier stripping (Stage 4b, extended 4f)

**naga error:** `InvalidToken(PrecisionQualifier)` — `highp`/`mediump`/`lowp` are GLES-only, invalid in GLSL 4.50.

**Fix:** `strip_precision()` removes `highp ` / `mediump ` / `lowp ` tokens. Extended in 4f to also strip precision at end-of-line (for `#define YUV_PRECISION highp` patterns where naga's preprocessor expands the macro AFTER our text transforms).

### 4. Switch fall-through (Stage 4d, extended 4f)

**naga error:** `Unimplemented("fall-through switch case block")` — WGSL doesn't support switch fall-through; naga's WGSL writer rejects it.

**Fix:** `fix_switch_fallthrough()` with 6 passes:
- **Pass 1:** Cascade labels (`case A:` / `case B:` sharing body) → duplicate body for each label. Variable declarations in duplicated bodies renamed with `_dupN` suffix using word-boundary replacement.
- **Pass 2:** Block-scoped terminators (`case X: { break; }`) → remove inner terminator, emit original terminator type (break/return/discard) after `}` at case level.
- **Pass 3:** Missing `break` before switch-closing `}` → insert one. Extended in 4f to also insert `break;` before case/default labels when preceding code lacks a terminator.
- **Pass 4:** `default:` in middle of switch → reorder to last position.
- **Pass 5:** Return-only switches → convert to if-else chain.
- **Pass 6:** Mixed break/return switches → replace case-level `return;` with `_naga_early_ret = true; break;`, wrap post-switch code in `if (!_naga_early_ret)`.

**Root cause insight:** naga's case_terminator check uses `get_or_insert` — it locks to the first terminator found. For `case X: { ... break; }`, the Block statement is the first "terminator", not the inner Break. So `ctx.body[idx-1] = Block ≠ Break` → fall_through = true. Pass 2 fixes this by removing the inner terminator so block_terminator is None and the outer break/return correctly sets case_terminator.

**Variants unblocked:** 7 (4d) + 4 (4f)

### 5. Stage-ifdef resolution (Stage 4e)

**naga error:** The function reordering pass saw both vertex AND fragment code because `#ifdef WR_VERTEX_SHADER` / `#ifdef WR_FRAGMENT_SHADER` were unresolved.

**Fix:** `resolve_stage_ifdefs()` strips inactive stage blocks at text level before all other processing. Tracks nested `#ifdef`/`#endif` (including `#endif //comment` variants). The active define is kept, the inactive define's block is removed.

**Side-effect fix:** Also prevents the function scanner from cross-contamination between vertex and fragment code (brush_fs prototype, fragment main() were confusing the vertex-stage reorder).

### 6. Forward dependency reorder (Stage 4e)

**naga error:** `ForwardDependency` — naga assigns function handles in order of first encounter (prototype or definition). Validator requires callee handles < caller handles.

**Fix:** `move_definitions_before_prototypes()` with paren-balanced function scanner:
1. Scan: identify all function prototypes and definitions (handling multi-line signatures, up to 500-line bodies).
2. For prototyped functions whose definitions appear later, compute the "specific block" (shader-specific code after the last driver `main()`) and move it to right before the first driver function definition. This preserves `#define` constant visibility.
3. Reconstruction: skip original prototypes and specific block lines, inserting the specific block at the new position.

**Key insight:** Insertion point must be right before the first driver function DEFINITION (not at the prototype position), so that `#define` constants between prototypes and definitions remain visible to the moved code.

**Variants unblocked:** 14 (4e) + 6 (4f, via 200→500 line limit)

### 7. Matrix varying decomposition (Stage 4f)

**naga error:** `NotIOShareableType` — naga 26 does not set the `IO_SHAREABLE` flag on `Ti::Matrix` types (confirmed in `naga/src/valid/type.rs`), despite GLSL spec allowing matrix varyings.

**Fix:** `decompose_matrix_varyings()`:
1. Scan for `flat varying [precision] matN name;` declarations, tracking `#ifdef` guard context.
2. Replace with N column-vector varyings (`flat varying vecM name_c0; ... name_cN;`) + a plain global `matN name;`.
3. Vertex main(): inject `name_c0 = name[0]; ... name_cN = name[N];` before closing `}`.
4. Fragment main(): inject `name = matN(name_c0, ..., name_cN);` after opening `{`.
5. Guarded varyings (e.g. inside `#ifdef WR_FEATURE_YUV`) get their glue code wrapped in the same `#ifdef`.

**Affected varyings:** `v_color_mat` (mat4), `vRgbFromDebiasedYcbcr` (mat3), `vColorMat` (mat4) — across brush_blend, brush_yuv_image, composite, cs_svg_filter, cs_svg_filter_node.

**Variants unblocked:** 8

### 8. Array-in-struct store decomposition (Stage 4f)

**naga error:** `InvalidStoreTypes { pointer, value }` — naga uses separate type handles for struct-embedded arrays vs standalone array constructors.

**Fix:** `decompose_array_struct_stores()` detects `id.field = type[N](a, b, ...);` and rewrites to `id.field[0] = a; id.field[1] = b; ...`.

**Variants unblocked:** 1 (ps_split_composite)

### 9. sampler2D function parameters (Stage 4f)

**naga error:** `InvalidToken(Identifier("sampler2D"), [Token(RightParen)])` — functions like `sampleInUvRect(sampler2D sampler, ...)` use `sampler2D` as a parameter type and `sampler` as a parameter name (reserved keyword in GLSL 4.50).

**Fix:** `rewrite_sampler_params()`:
1. Detect function definitions with `sampler2D` parameter type.
2. Change parameter type to `texture2D`.
3. Rename the parameter from `sampler` to `_tex` (avoids reserved keyword).
4. Wrap internal `texture(_tex, ...)` calls with `texture(sampler2D(_tex, global_sampler), ...)`.

**Variants unblocked:** 2 (cs_svg_filter, cs_svg_filter_node)

---

## Architecture decisions

**Why text-level transforms instead of modifying GLSL source files?**
The `.glsl` shader files are shared with the GL backend and used across Servo, Firefox, and other WebRender embedders. Modifying them would require extensive testing across all platforms. The `build.rs` transforms are isolated to the wgpu codepath and have zero impact on GL.

**Why not fix naga instead?**
Several of these are genuine naga limitations (IO_SHAREABLE, case_terminator logic, sampler2D parsing). Upstreaming fixes to naga is valuable long-term but would block this project for months. The build.rs workarounds are pragmatic and well-documented — they can be removed if/when naga evolves.

**Why not use SPIR-V as intermediate?**
naga's GLSL→SPIR-V→WGSL path would double the translation steps and introduce additional loss. Direct GLSL→naga IR→WGSL is simpler and lets us diagnose issues at the IR level.

---

## Stage 5 — Fixed binding table (post-translation)

**Problem:** `preprocess_for_naga` assigned binding numbers sequentially with `next_binding` counter.
Since VS and FS are processed independently, the same resource got different binding indices
(e.g., `sGpuCache` = `@binding(7)` in VS, `@binding(5)` in FS). wgpu requires a single
`PipelineLayout` per `RenderPipeline`, so VS/FS must agree on binding numbers.

**Fix:** Replaced sequential counter with `FIXED_BINDINGS` table matching GL `TextureSampler` slot
assignments: sColor0=0 through sGpuBufferI=11, uTransform=12, uTextureSize=13,
u_mali_workaround_dummy=14. Both stages now look up the binding by resource name.

---

## Files

Changes span `webrender/build.rs` (~2460 lines, 16 `#[cfg(feature = "wgpu_backend")]` functions + 2 consts) and `webrender/src/device/wgpu_device.rs` (~640 lines, pipeline creation infrastructure). No shader source files were modified. The GL backend compiles identically.
