# SPIR-V-Centered Shader Pipeline for `webrender-wgpu`

## Summary

Replace the current split shader flow:

- GL runtime consumes assembled/optimized GLSL
- `wgpu` runtime consumes build-generated WGSL

with a single build pipeline whose canonical runtime artifact is:

- SPIR-V stage binaries
- reflected shader metadata
- derived WGSL for `wgpu`
- derived GLSL for GL/GLES targets

The migration will keep authored GLSL as the initial input format, but move both
backends to consume generated artifacts from a shared registry. The plan is phased
so the `wgpu` backend lands first on the new artifact model, then GL/GLES follows
without stranding either backend.

## Key Changes

### 1. Introduce a canonical compiled-shader artifact model

Add a generated shader registry in `shader_source` that replaces the current split
`OPTIMIZED_SHADERS` / `UNOPTIMIZED_SHADERS` / `WGSL_SHADERS` view with one artifact
model per `(shader_name, config)` variant.

New generated types:

- `ShaderArtifactKey { name: &'static str, config: &'static str }`
- `ShaderStageArtifact`
  - `spirv_words: &'static [u32]`
  - `wgsl_source: &'static str`
  - `glsl_sources: TargetGlslSources`
  - `digest: &'static str`
- `ShaderMetadata`
  - vertex inputs by semantic name, location, and format
  - stage resource bindings by semantic resource name and fixed binding slot
  - target/profile availability flags
- `ShaderArtifact`
  - `key`
  - `metadata`
  - `vertex`
  - `fragment`

Artifact storage policy:

- SPIR-V, WGSL, GLSL, and metadata are generated into `OUT_DIR`
- registry code uses `include_bytes!` / `include_str!` to load them
- no generated shader artifacts are checked into the repo

Keep `WgpuShaderVariant` as the runtime-facing typed variant key, but make it resolve
through `ShaderArtifact` instead of `WGSL_SHADERS`.

### 2. Build a single shader compilation pipeline in `webrender_build`

Keep the existing shader assembly layer as the bootstrap input:

- `build_shader_strings` remains the place that expands imports and feature defines
- initial authoring remains `.glsl`
- feature enumeration continues to come from `shader_features.rs`

Add a new compiled-artifact pipeline with this exact flow:

1. Assemble per-stage GLSL from the current source tree and feature matrix
2. Normalize it to the Vulkan/SPIR-V compilation profile
3. Compile GLSL to SPIR-V with `shaderc`/glslang
4. Parse SPIR-V into naga IR for validation, metadata extraction, and WGSL generation
5. Cross-compile the same SPIR-V to target GLSL with SPIRV-Cross
6. Emit generated artifact files plus a registry module

Tool choices:

- GLSL -> SPIR-V: `shaderc`
- SPIR-V -> WGSL: `naga` with `spv-in` + `wgsl-out`
- SPIR-V -> GLSL/ESSL: SPIRV-Cross
- metadata extraction: from the parsed SPIR-V / naga module, not by parsing generated WGSL text

Important build-rule changes:

- move the current WGSL-only preprocessing logic out of the `wgpu`-specific path and split it into:
  - input normalization required before SPIR-V compilation
  - output normalization required after WGSL/GLSL generation
- make the build pipeline profile-aware:
  - canonical SPIR-V profile for shared variants
  - generated GLSL targets for `GL 150`, `GLES 300`
  - `ESSL 100` remains legacy-only until proven supportable through SPIR-V derivation

### 3. Move the `wgpu` backend to artifact-driven runtime setup

Replace all runtime dependence on WGSL text shape with generated metadata.

Required runtime changes:

- `WgpuDevice` stops parsing WGSL entry-point text for vertex inputs
- `parse_wgsl_vertex_inputs` is deleted
- vertex layout construction uses `ShaderMetadata.vertex_inputs`
- pipeline creation uses `ShaderArtifact.vertex.wgsl_source` and `.fragment.wgsl_source`
- fixed binding validation uses reflected metadata to assert the generated WGSL still matches WebRender’s fixed binding model

Digesting/cache behavior:

- the canonical digest source becomes the SPIR-V artifact digest plus target profile marker
- `wgpu` pipeline cache keys continue to use typed shader variants, but the artifact digest is the source-of-truth fingerprint

Acceptance point for this phase:

- all currently enabled `wgpu` variants generate SPIR-V, WGSL, and metadata successfully
- all `wgpu` pipelines create successfully from generated WGSL and metadata
- no runtime WGSL parsing remains

### 4. Move the GL backend to consume generated GLSL

After `wgpu` is stable on the artifact model, switch GL to consume derived GLSL from
the same artifact registry.

Required GL changes:

- `ProgramSourceInfo` and `Device::build_shader_string` stop being the runtime source-of-truth for migrated shader families
- GL program creation loads target-specific generated GLSL from `ShaderArtifact`
- program digests are computed from canonical artifact digests and target profile, not from ad hoc runtime string assembly
- program-cache keys remain target-specific so `Gl` and `Gles` binaries do not collide

Migration boundary for GL:

- first migrate the backend-intersection shader families and all families currently used by `wgpu`
- keep these GL-only families on the legacy GLSL path initially:
  - `TEXTURE_RECT`
  - `TEXTURE_EXTERNAL`
  - `TEXTURE_EXTERNAL_BT709`
  - `TEXTURE_EXTERNAL_ESSL1`
  - `ADVANCED_BLEND_EQUATION`
- maintain a dual registry during the transition:
  - `artifact-backed` variants
  - `legacy-glsl-only` variants

End-state rule:

- artifact-backed variants are consumed by both backends
- legacy-only variants are isolated behind explicit exceptions, not mixed into the main path
- once each remaining GL-only family has a verified SPIR-V route, remove its legacy path

## Implementation Sequence

### Phase 1: Artifact generation side-by-side

- Add a new `webrender_build` module for compiled shader artifacts
- Generate SPIR-V, WGSL, GLSL, and metadata without changing either runtime backend
- Keep current WGSL generation alive until parity checks pass
- Add a per-variant build report that records:
  - GLSL assembled successfully
  - SPIR-V compiled successfully
  - WGSL emitted successfully
  - target GLSL emitted successfully
  - metadata extracted successfully

### Phase 2: Switch `wgpu` to the new registry

- Make `wgpu` consume generated WGSL and metadata
- Remove WGSL text parsing from runtime
- Keep current GL path unchanged
- Fail the build if any `wgpu` variant lacks a complete artifact set

### Phase 3: Switch GL shared variants

- Make GL load generated GLSL for the shared/intersection variant set
- Keep GL-only extension families on the old path
- Change program digesting to artifact-based digests for migrated families
- Preserve current optimized/legacy behavior only for the remaining unmigrated families

### Phase 4: Retire duplicated generation paths

- Remove the old WGSL-only generation path
- Collapse shader registry generation onto the artifact model
- Shrink the legacy GLSL path to only the explicit GL-only exceptions
- Either migrate or permanently quarantine the remaining exception families

## Test Plan

### Build-time checks

- artifact generation succeeds for every variant in `get_shader_features` and `wgpu_shader_feature_flags`
- build fails on any missing stage artifact, missing metadata, or binding mismatch
- generated metadata is deterministic across builds
- artifact digests are stable for unchanged inputs

### `wgpu` validation

- all generated WGSL modules load through `device.create_shader_module()`
- all render pipelines create successfully from generated artifacts
- current `wgpu_backends.rs` coverage still passes
- a parity test confirms generated metadata produces the same vertex layouts as the current hardcoded layout logic

### GL validation

- GL program creation succeeds for all migrated shared variants
- program cache digests remain stable and target-specific
- fallback remains correct for legacy-only GL extension families
- no migrated variant uses runtime GLSL assembly anymore

### Image/regression validation

- run the existing reftest/wrench suite on the migrated shared variants
- compare GL and `wgpu` output against current baseline for:
  - text
  - gradients
  - image brushes
  - composite passes
  - clip shaders
  - render-task/cache shaders
- add a targeted shader-artifact parity test that checks one representative variant from each family emits:
  - valid SPIR-V
  - valid WGSL
  - valid GL target source

## Assumptions And Defaults

- Long-term canonical runtime artifact is generated SPIR-V plus reflected metadata
- Initial authored source remains GLSL; manual SPIR-V authoring is not part of this migration
- Both backends are in scope, but migration is phased: `wgpu` first on the new artifact model, then GL shared variants, then GL-only exceptions
- Generated artifacts remain build outputs only; they are not checked into the repo
- `shaderc` is the GLSL -> SPIR-V compiler, `naga` handles SPIR-V -> WGSL and metadata validation, and SPIRV-Cross handles SPIR-V -> GLSL/ESSL
- `ESSL 100` and other extension-heavy GL-only families stay on the legacy path until their SPIR-V derivation is explicitly validated
- No public embedders-facing renderer API changes are required; the meaningful interface changes are internal generated shader registry/types and backend device initialization logic
