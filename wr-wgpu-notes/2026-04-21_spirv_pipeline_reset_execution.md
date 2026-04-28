# SPIR-V Pipeline Reset Execution Slice

## Purpose

This note translates the authored-SPIR-V branch reset into concrete implementation work.
It is not a restatement of the architecture plan. It is the execution slice that should
replace the current GLSL-front-end effort.

## Controlling Observation

The current branch still centers migrated variants around this flow:

1. assemble GLSL with `build_shader_strings`
2. rewrite GLSL through `preprocess_for_naga`
3. compile GLSL to SPIR-V with `glsl_to_spirv`
4. reflect and emit WGSL/GLSL from that derived SPIR-V

That is the wrong control point. The new control point is authored SPIR-V plus reflected metadata.

## Execution Slice

### Slice 1: Recenter `compiled_artifacts.rs` on authored SPIR-V

Primary file: `webrender_build/src/compiled_artifacts.rs`

Replace the current entry path in `generate_shader_artifacts(...)`.

Current path:

- assemble GLSL from source tree
- call `preprocess_for_naga(...)`
- write `*.input.glsl`
- call `glsl_to_spirv(...)`
- reflect from derived SPIR-V
- emit WGSL and GLSL from that derived SPIR-V
- hash both canonical and non-canonical intermediates into the digest

Target path:

- load authored vertex and fragment `.spvasm` for the requested registry lookup key
- assemble authored `.spvasm` to `.spv` with `spirv-as`
- validate those modules with `spirv-val`
- reflect metadata from authored SPIR-V
- emit WGSL from authored SPIR-V with naga `spv-in` + `wgsl-out`
- emit desktop GL and GLES GLSL from authored SPIR-V with naga `spv-in` + `glsl-out`
- compute digest from authored SPIR-V bytes, metadata, and explicit target profile markers
- stop storing GLSL-preprocessed input as part of the artifact contract

Identity rule:

- `(name, config)` is only a registry lookup key
- canonical shader identity is SPIR-V plus reflected metadata

Tooling rule:

- if the authored source is plain `.spvasm`, keep `spirv-as` in the steady-state path for text assembly
- use `rspirv` only where its ergonomics actually help: binary parsing, inspection, normalization, or other post-assembly SPIR-V handling
- do not pretend `rspirv` alone replaces the `.spvasm` text assembly step

Concrete code changes:

- delete `glsl_to_spirv(...)`
- introduce `load_authored_spirv_stage(...)`
- introduce `load_authored_shader_package(...)`
- change `ShaderArtifactEntry` to record authored-SPIR-V source paths instead of `vert_glsl_input_path` and `frag_glsl_input_path`
- stop hashing preprocessed GLSL into `digest`
- keep reflection helpers, but rename them away from WGSL-centric wording where appropriate

Immediate acceptance check:

- `generate_shader_artifacts(...)` should be readable without encountering `build_shader_strings`, `preprocess_for_naga`, or `shaderc`
- it should also be readable without treating `(name, config)` as the canonical shader identity

### Slice 2: Collapse `wgsl.rs` to SPIR-V in and output fixups only

Primary file: `webrender_build/src/wgsl.rs`

This file currently contains two different concerns:

- SPIR-V to WGSL generation that can survive
- a large GLSL normalization tower that exists only because naga's GLSL frontend was in the path

Target shape:

Keep:

- `translate_spirv_to_wgsl(...)`
- narrowly-scoped output fixups that are still required after SPIR-V-based generation
- constants or helpers that are runtime metadata contracts rather than GLSL parsing aids

Delete or move out of the hot path:

- `preprocess_for_naga(...)`
- `fix_switch_fallthrough(...)`
- stage `#ifdef` resolution for naga ingestion
- sampler-splitting rewrites for Vulkan-style GLSL parsing
- `translate_to_wgsl(...)` from GLSL input
- `write_wgsl_shaders(...)` if `compiled_artifacts.rs` becomes the only registry producer

Immediate acceptance check:

- `wgsl.rs` should no longer describe GLSL frontend workarounds as a core responsibility

### Slice 3: Remove duplicated GLSL-era generation from `webrender/build.rs`

Primary file: `webrender/build.rs`

The generated `shader_source.rs` schema is already being emitted here, and it still exposes GLSL-era fields such as:

- `vert_glsl_input`
- `frag_glsl_input`
- `vert_glsl`
- `frag_glsl`
- `vert_gles`
- `frag_gles`
- `vert_wgsl`
- `frag_wgsl`

Not all of those fields are wrong, but the schema is flat and source-text-oriented. The reset should move this file toward emitting registry types that explicitly model canonical stages plus derivations.

Target changes:

- define generated types around stage artifacts and metadata, not a bag of parallel strings
- stop exposing preprocessed GLSL input as a first-class generated field
- make the registry layout match the authored-SPIR-V package contract
- keep `SHADER_ARTIFACTS` generation, but change what an artifact is

Immediate acceptance check:

- the generated Rust schema should make it obvious which fields are canonical and which are derived

### Slice 4: Preserve the runtime metadata consumer and tighten it

Primary file: `webrender/src/device/wgpu_device.rs`

This is the part of the branch that is already pointed in the right direction.

Keep and build on:

- metadata-driven vertex input handling
- metadata-driven resource-binding validation
- `validate_artifact_bindings(...)`
- cache lookup by typed variant resolving through `SHADER_ARTIFACTS`

Refine:

- rename local types that still imply WGSL text is the source of truth if they now come from canonical artifact metadata
- ensure the digest lineage used in caches follows canonical SPIR-V, not generated-output incidental text
- keep runtime completely ignorant of how WGSL and GLSL were derived

Immediate acceptance check:

- runtime setup remains metadata-driven and does not regress toward source-text inspection

### Slice 5: Remove steady-state `shaderc` from the build feature graph

Primary file: `webrender_build/Cargo.toml`

Target end state:

- `wgsl` feature keeps `naga`
- `shaderc` is removed
- naga feature selection is reduced to what the SPIR-V-centered pipeline actually uses

Important nuance:

- if an offline one-shot migration tool is needed to convert old GLSL families to authored SPIR-V, that tool must not be wired into the normal build

Immediate acceptance check:

- the normal build does not require `shaderc`, `shaderc-sys`, or its native toolchain

## Authored SPIR-V Package Format

The branch needs an explicit source package contract before code churn starts. A loose pile of `.spv` files is not enough, and raw binary `.spv` is not the right authored format.

Authoring rule:

- `.spvasm` is the authored source
- `.spv` is a build artifact produced by `spirv-as`

## One-time migration bootstrap

Performed once, by the migration effort, off the critical path:

1. take the current GLSL corpus through `glslang` to produce SPIR-V
2. run `spirv-dis` to produce `.spvasm`
3. commit the resulting `.spvasm` files

Constraints:

- this is a data conversion, not a normal build step
- `shaderc` or `glslang` may exist on the machine during this phase only
- after the `.spvasm` corpus is committed, nothing in the repo depends on GLSL compilation during the normal build

## Steady state

Normal build flow:

1. load authored `.spvasm`
2. run `spirv-as` to produce `.spv`
3. run `spirv-val`
4. run naga `spv-in`
5. emit WGSL and GLSL
6. validate generated outputs

If we want better Rust-side ergonomics here, the coherent use of `rspirv` is after step 2:

1. load `.spvasm`
2. assemble with `spirv-as`
3. optionally parse/inspect/normalize via `rspirv`
4. validate with `spirv-val`
5. feed the resulting SPIR-V to Naga

## Proposed on-disk layout

Suggested root:

- `webrender/res/shaders/spirv/`

Suggested variant package layout:

- `manifest.toml`
- `vertex.spvasm`
- `fragment.spvasm`

Example:

- `webrender/res/shaders/spirv/ps_copy/default/manifest.toml`
- `webrender/res/shaders/spirv/ps_copy/default/vertex.spvasm`
- `webrender/res/shaders/spirv/ps_copy/default/fragment.spvasm`
- `webrender/res/shaders/spirv/brush_yuv_image/TEXTURE_2D__YUV/manifest.toml`
- `webrender/res/shaders/spirv/brush_yuv_image/TEXTURE_2D__YUV/vertex.spvasm`
- `webrender/res/shaders/spirv/brush_yuv_image/TEXTURE_2D__YUV/fragment.spvasm`

## Manifest fields

Minimum manifest payload:

- `registry_name`: shader family name used by lookup
- `registry_config_key`: config string used by lookup
- `vertex_entry`: entry point name for vertex stage
- `fragment_entry`: entry point name for fragment stage
- `targets`: generated target set, such as `wgsl`, `gl_150`, `gl_330`, `gles_300`
- `metadata_version`: schema version for generated registry compatibility

Important distinction:

- manifest lookup fields are not shader identity
- shader identity is the assembled SPIR-V plus reflected metadata

Optional manifest payload for invariants reflection may not recover cleanly:

- `legacy_family`: whether the family is still quarantined behind a legacy GL-only path
- `notes`: human-oriented rationale for unusual target constraints
- `required_extensions`: explicit target-side caveats if generation is not enough by itself

## Registry schema

Generated Rust schema should move from a flat text bag toward explicit stage artifacts.

Suggested shape:

```rust
pub struct ShaderRegistryKey {
    pub name: &'static str,
    pub config: &'static str,
}

pub struct CanonicalShaderIdentity {
    pub vertex_spirv_words: &'static [u32],
    pub fragment_spirv_words: &'static [u32],
    pub metadata_digest: &'static str,
}

pub struct GeneratedTargetSources {
    pub wgsl: &'static str,
    pub desktop_gl: &'static str,
    pub gles: &'static str,
}

pub struct ShaderStageArtifact {
    pub entry_point: &'static str,
    pub spirv_words: &'static [u32],
    pub targets: GeneratedTargetSources,
}

pub struct ShaderMetadata {
    pub vertex_inputs: &'static [ShaderVertexInputMetadata],
    pub vertex_resource_bindings: &'static [ShaderResourceBindingMetadata],
    pub fragment_resource_bindings: &'static [ShaderResourceBindingMetadata],
}

pub struct ShaderArtifact {
    pub registry_key: ShaderRegistryKey,
    pub identity: CanonicalShaderIdentity,
    pub metadata: ShaderMetadata,
    pub vertex: ShaderStageArtifact,
    pub fragment: ShaderStageArtifact,
    pub canonical_digest: &'static str,
    pub spirv_validation: &'static str,
}
```

Schema rules:

- canonical fields are clearly distinguished from derived output fields
- target outputs are grouped under the stage they were derived from
- metadata is grouped once at artifact scope, not rediscovered ad hoc from generated text
- digest naming makes it explicit that runtime identity comes from the canonical artifact
- lookup key naming makes it explicit that registry addressing is not artifact identity

## Output validators

Steady-state validation should be phrased as validators on generated outputs, not on authoring.

- SPIR-V input: `spirv-val`
- WGSL output: `wgpu::Device::create_shader_module()` or equivalent WGSL validator path
- GLSL output: validation-only CLI such as `glslangValidator`, or existing compile/link harnesses

These are validator gates, not Cargo dependencies and not alternate authored-source paths.

Assembler note:

- `spirv-as` is the assembler for `.spvasm`
- it is not a validator substitute
- if the build later adopts `rspirv` for ergonomics, that should be layered around or after assembly, not confused with the validator role

## Branch Audit Heuristic

Use this rule for the rest of the branch:

Keep if it strengthens one of these:

- SPIR-V validation
- SPIR-V reflection
- metadata-driven runtime setup
- WGSL generation from SPIR-V
- GLSL generation from SPIR-V
- artifact-backed runtime lookup and cache identity
- validator or harness coverage for generated outputs

Retire if it exists mainly to strengthen one of these:

- naga GLSL parsing
- GLSL preprocessing for naga compatibility
- `shaderc`-based steady-state compilation
- treating assembled GLSL as the source of truth for migrated variants
- recovering runtime layout information by parsing generated shader text

## Initial Keep / Retire Split

Likely keep:

- reflection helpers in `compiled_artifacts.rs`
- generated metadata tables emitted into `shader_source.rs`
- metadata-driven binding validation in `wgpu_device.rs`
- artifact registry lookup in runtime code
- shader-output validation tests that exercise generated GLSL or WGSL as outputs

Likely retire:

- `preprocess_for_naga(...)` and its helper tower
- `translate_to_wgsl(...)` from GLSL source
- `glsl_to_spirv(...)`
- generated artifact fields that treat preprocessed GLSL as first-class runtime data
- any new branch work whose only success criterion is "naga can now parse more of WebRender's GLSL"

## Next audit pass

The next branch-review step should classify actual changed files and symbols into:

- survives as-is
- survives with refactor
- throwaway under the SPIR-V reset
- still valuable only as a migration harness

## Branch Review Against Valid Baselines

Comparison note:

- do not use local `main` here; it is a downstream mirror and not the right ancestry reference for this branch
- the closest branch baseline for the current work is `wgpu-backend-0.68-experimental`
- the broader upstream release baseline is `upstream/0.68`
- the audit below is based primarily on the shader-pipeline tree diff against `wgpu-backend-0.68-experimental`, with `upstream/0.68` used as the larger historical reference point

Relevant changed files observed in that comparison:

- `webrender_build/src/wgsl.rs`
- `webrender_build/src/compiled_artifacts.rs`
- `webrender/build.rs`
- `webrender/src/device/wgpu_device.rs`
- `webrender/tests/angle_shader_validation.rs`
- `webrender_build/Cargo.toml`
- `webrender_build/src/lib.rs`
- `webrender_build/src/shader.rs`
- `webrender_build/src/shader_features.rs`
- multiple planning and progress notes under `wr-wgpu-notes/`

### Survives Mostly As-Is

#### `webrender/src/device/wgpu_device.rs`

Keep:

- metadata-driven vertex input construction via `wgsl_vertex_inputs_from_metadata(...)`
- metadata-driven binding validation via `validate_artifact_bindings(...)`
- runtime lookup through `SHADER_ARTIFACTS`
- pipeline creation keyed by typed shader variant rather than source text

Why it survives:

- this is already on the correct side of the abstraction boundary
- it consumes generated artifacts and metadata instead of depending on GLSL assembly

Required cleanup:

- rename local types that still say "WGSL" when the metadata is no longer WGSL-defined
- ensure digest and cache identity track canonical SPIR-V rather than incidental generated text

#### `webrender/tests/angle_shader_validation.rs`

Keep:

- `validate_shader_artifact_spirv`
- `validate_generated_glsl_artifacts`
- `validate_generated_desktop_glsl_artifacts`
- `validate_angle_legacy_gles_link_smoke`

Why it survives:

- these tests act as output validators and external oracles, which still fit the reset exactly
- they validate generated GLSL and artifact integrity without requiring GLSL to remain the authored source

### Survives With Refactor

#### `webrender_build/src/compiled_artifacts.rs`

Keep the following concepts:

- SPIR-V validation wiring
- SPIR-V reflection helpers
- artifact registry emission
- SPIR-V to WGSL generation
- SPIR-V to GL/GLES generation

Refactor heavily:

- `generate_shader_artifacts(...)`
- `ShaderArtifactEntry`
- digest construction
- on-disk artifact layout

Why it survives only with refactor:

- the file owns the right output model, but its control flow still starts from GLSL assembly and `glsl_to_spirv(...)`
- it currently stores and hashes preprocessed GLSL, which is incompatible with authored SPIR-V as canonical source

#### `webrender/build.rs`

Keep:

- generated `shader_source.rs` emission
- type and registry generation plumbing
- cargo rerun wiring and resource discovery

Refactor:

- the generated `ShaderArtifact` schema
- fields that expose preprocessed GLSL as a first-class artifact payload

Why it survives only with refactor:

- the file is still the registry/type emission point, but the current generated schema is source-text-oriented rather than canonical-artifact-oriented

#### `webrender_build/src/shader_features.rs`

Keep:

- feature-matrix enumeration for variant keys
- `wgpu_shader_feature_flags()` as the current approximation of the backend-intersection/shared set

Refactor:

- terminology so the function describes the artifact-backed shared set rather than specifically the old `wgpu` translation set
- any exclusions that were chosen solely because naga's GLSL frontend could not tolerate them

Why it survives only with refactor:

- variant enumeration is still needed, but the reason for which families are "in" or "out" changes under authored SPIR-V

#### `webrender_build/src/shader.rs`

Potentially keep only as a migration utility:

- `ShaderSourceParser`
- `build_shader_strings(...)`
- include expansion and feature prefix generation

Why it survives only with refactor:

- this remains useful if the project needs a one-shot offline importer from legacy GLSL to authored SPIR-V
- it should not remain on the steady-state build path for migrated families

### Throwaway Under The Reset

#### `webrender_build/src/wgsl.rs`

Retire the GLSL-front-end half of the file:

- `preprocess_for_naga(...)`
- `fix_switch_fallthrough(...)`
- stage `#ifdef` resolution for naga ingestion
- sampler-splitting rewrites
- `translate_to_wgsl(...)` from GLSL input
- `write_wgsl_shaders(...)` if `compiled_artifacts.rs` remains the sole registry producer

Why it is throwaway:

- this code exists to compensate for naga's GLSL frontend
- the reset explicitly removes naga's GLSL frontend from the pipeline

#### `webrender_build/src/compiled_artifacts.rs`

Retire these specific pieces:

- `glsl_to_spirv(...)`
- the `build_shader_strings(...)` entry path inside `generate_shader_artifacts(...)`
- `vert_glsl_input_path` and `frag_glsl_input_path`
- writing `*.input.glsl` as a first-class artifact payload
- hashing preprocessed GLSL and generated text into the canonical digest

Why they are throwaway:

- they preserve GLSL as the effective source of truth
- they bake non-canonical intermediates into the artifact identity

#### `webrender_build/Cargo.toml`

Retire from the normal build:

- `shaderc`
- `shaderc-sys`
- naga `glsl-in`
- any feature wiring whose only purpose is GLSL ingestion in steady state

Why it is throwaway:

- the reset removes `shaderc` and naga GLSL parsing from the intended pipeline

### Valuable As Migration Harness Only

#### `webrender/tests/angle_shader_validation.rs`

Keep as harness-only, not architecture-defining behavior:

- `reduce_generated_ps_copy_link_with_angle`
- `reduce_brush_yuv_image_generated_link_with_angle`

Why harness-only:

- they are useful for debugging generator failures in produced GLES output
- they should not drive architecture decisions about authored source format

#### `webrender/src/device/wgpu_device.rs` test module

Keep the large draw-path tests as migration confidence tools:

- `draw_instanced_brush_solid_smoke`
- `draw_instanced_brush_solid_red_rect`
- `draw_instanced_brush_solid_atlas_subrect`
- related device-level pipeline submission tests

Why harness-only:

- they validate that artifact metadata and runtime binding/layout assumptions still produce functioning draws
- they are not part of the source-format decision and should stay decoupled from it

### Notes Audit

Keep:

- `wr-wgpu-notes/2026-04-18_spirv_shader_pipeline_plan.md`
- `wr-wgpu-notes/2026-04-21_spirv_pipeline_reset_execution.md`
- confirmation and portability notes that describe observed runtime behavior rather than prescribing the old GLSL-front-end design

Likely superseded by the reset:

- `wr-wgpu-notes/archive/legacy/shader_translation_journal.md`
- `wr-wgpu-notes/typed_pipeline_metadata_plan.md` where it assumes WGSL stays the primary runtime-facing source contract
- older implementation plans that frame success as getting more WebRender GLSL through naga's GLSL frontend

Historical only:

- `wr-wgpu-notes/archive/legacy/*`
- archived progress reports that document branch history but should not guide the reset implementation

## Recommended First Code Pass After This Audit

1. Change `webrender_build/src/compiled_artifacts.rs` so the top-level generation path loads authored SPIR-V packages instead of calling `build_shader_strings(...)`, `preprocess_for_naga(...)`, and `glsl_to_spirv(...)`.
2. Shrink `webrender_build/src/wgsl.rs` to SPIR-V input plus output-side fixups only.
3. Refactor the generated `ShaderArtifact` schema in `webrender/build.rs` to distinguish canonical stage artifacts from derived target outputs.
4. Keep `webrender/src/device/wgpu_device.rs` on the metadata-driven path, but rename types and digest handling to match canonical SPIR-V ownership.
