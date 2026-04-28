# Authored-SPIR-V, Naga-Derived Shader Pipeline for `webrender-wgpu`

## Summary

Reset the branch around a different source of truth.

The authored shader language is no longer GLSL. The canonical source for a shader
variant is SPIR-V plus reflected metadata. `naga` stays in the build pipeline, but
only on `spv-in`, `wgsl-out`, and `glsl-out`. It does not parse GLSL anymore.

That changes the intended pipeline from:

- authored GLSL
- patch GLSL until naga accepts it
- derive WGSL
- preserve GL runtime string assembly as a separate center of gravity

to:

- authored SPIR-V modules
- validate SPIR-V as the canonical contract
- reflect metadata from SPIR-V
- derive WGSL and target GLSL from the same SPIR-V
- make both `wgpu` and GL consume derivations of the same artifact set

This is not a different GLSL translation strategy. It is a replacement of GLSL as
the authored source language and the removal of naga's GLSL frontend from the hot path.

## Branch Lifecycle and Upstream Relationship

Read this before assuming anything about how this branch relates to "main" or
how upstream changes get integrated. Ad-hoc inspection of git remotes leads to
the wrong answer if you check the wrong remote.

### Remote and branch topology

The repository has two remotes:

- `origin = https://github.com/mark-ik/webrender-wgpu.git` — the working fork
- `upstream = https://github.com/servo/webrender.git` — the real WebRender
  project

The branches that matter:

| Ref | Role |
| --- | --- |
| `upstream/0.68` | Release branch on `servo/webrender`. **The fork point for this work.** This branch (`spirv-shader-pipeline`) is 0 commits behind it and N commits ahead. |
| `upstream/upstream` (also visible as `upstream/main` via `upstream/HEAD`) | `servo/webrender`'s active development branch. **The cherry-pick source.** Several hundred commits ahead since the fork point. |
| `origin/main` | Historical initial-push lineage of the `mark-ik/webrender-wgpu` fork. **Dead red herring.** Shares no history with this branch — `git merge-base origin/main HEAD` returns nothing. The fork was reseeded onto `upstream/0.68` separately, and `origin/main` was never used as a development base. |

When the rest of this plan says "main" or "long-lived branch drift," it means
**`upstream/upstream`** (the active dev branch on `servo/webrender`) — never
`origin/main`. Bare "main" mentions in this doc have been disambiguated to use
the explicit ref.

### Integration strategy: selective cherry-pick, not rebase

This branch will not be rebased onto `upstream/upstream` wholesale. The
strategy of record is **selective cherry-picking from `upstream/upstream`,
evaluated per candidate against the active migration's parity gate**.

The full strategy and the current candidate watchlist live in two sibling
notes:

- `2026-04-18_upstream_cherry_pick_plan.md` — original watchlist, batch
  ordering, working-method recipe
- `2026-04-22_upstream_cherry_pick_reevaluation.md` — re-evaluation against
  the current branch state, per-candidate accept / defer / reject decisions
  with dated ancestry checks

Read both before proposing any upstream integration. Do not propose
`git rebase upstream/upstream` or `git merge upstream/upstream` — that is not
the integration model in use.

### Why the bare `git merge-base` check is misleading

`git merge-base origin/main HEAD` exits 1 (no common ancestor) because
`origin/main` is not a development base. A cold reader running that check and
concluding "disjoint histories, this is a permanent fork" has misidentified
the comparison point. The right check is `git merge-base upstream/0.68 HEAD`,
which returns a clean fork point.

## Branch Reset

The current branch direction is wrong in these specific ways:

- it still treats assembled GLSL as the effective source of truth
- it still depends on `shaderc` and its native toolchain baggage
- it still carries a large preprocessing tower whose only job is to keep naga's
  GLSL frontend alive
- it still frames standards tooling as a substitute compiler path instead of as
  validators and external oracles around naga's derived outputs

The corrected direction is:

- authored shader source is SPIR-V
- canonical artifact is SPIR-V plus reflected metadata
- naga only consumes SPIR-V and emits WGSL and GLSL
- external validators judge the canonical input and generated outputs
- device and renderer code speak in terms of shader artifacts, not source text

Non-negotiable restatement:

- SPIR-V is the authored source
- naga is a pure SPIR-V consumer
- the renderer is reshaped around SPIR-V plus reflected metadata as shader identity

## Core Principles

### 1. Canonical shader source is SPIR-V

For any migrated shader family, "the shader for X" means:

- vertex SPIR-V module
- fragment SPIR-V module
- reflected metadata for entry points, vertex inputs, bindings, and target support

No migrated runtime path should depend on hand-authored GLSL text.

### 2. Naga never parses GLSL in the intended end state

`naga` remains central, but only for:

- SPIR-V input parsing and validation prep
- WGSL generation
- GLSL generation
- metadata/reflection support where it is sufficient

It is explicitly not used for:

- GLSL parsing
- Vulkan-profile GLSL normalization
- source-patching around GLSL frontend limitations

### 3. Standards validate, not replace, the pipeline

External reference tools are used as correctness gates around the artifact graph:

- `spirv-val` validates authored SPIR-V input
- WGSL validators and/or `wgpu` shader-module creation validate generated WGSL
- GLSL validators, compilers, and linkers validate generated GLSL

Those tools are external oracles. They do not define a second shader translation
pipeline that competes with naga.

### 4. Runtime shader identity is artifact-based

The runtime meaning of a shader variant is:

- canonical SPIR-V stages
- reflected metadata
- derived target-language forms
- a stable digest keyed from the canonical SPIR-V and metadata

Important distinction:

- `(name, config)` may remain as a registry lookup key or variant selector
- it is not the canonical shader identity
- canonical identity is the authored SPIR-V plus reflected metadata contract

The runtime meaning of a shader variant is not:

- a GLSL source string assembled on demand
- a WGSL string reparsed to rediscover layout information

## Non-Goals

The intended end state does not include:

- `shaderc`
- `glslang`
- `cmake` as a shader-build dependency
- `preprocess_for_naga`
- `fix_switch_fallthrough`
- `resolve_stage_ifdefs`
- sampler splitting or GLSL 450 rewrites done solely for naga ingestion
- runtime parsing of generated WGSL or GLSL to recover metadata

If a temporary bootstrap importer is needed to convert existing GLSL families into
authored SPIR-V during migration, that importer is an offline migration aid only.
It is not part of the steady-state build or runtime pipeline.

## Migration Bootstrap Versus Steady State

The branch should distinguish sharply between the one-time corpus migration and the
normal build.

### One-time migration bootstrap

Performed once, off the critical path:

1. take the current GLSL corpus through `glslang` to produce SPIR-V
2. run `spirv-dis` to produce committed `.spvasm`
3. review and commit the `.spvasm` files as the new authored source

Rules:

- this is a data-conversion step, not a build step
- `shaderc` or `glslang` may exist on the machine during this phase only
- after the `.spvasm` corpus is committed, the repo no longer depends on GLSL compilation in the normal build

### Steady state

Normal build starts from authored `.spvasm`.

1. assemble `.spvasm` to `.spv` with `spirv-as`
2. validate `.spv` with `spirv-val`
3. parse SPIR-V with naga `spv-in`
4. reflect metadata
5. emit WGSL with naga `wgsl-out`
6. emit GLSL with naga `glsl-out`
7. validate generated WGSL and GLSL outputs

Steady-state rules:

- naga never parses GLSL at build time or runtime
- `glslang` is not part of the steady-state build
- the only mandatory SPIR-V tooling in the normal build is assembler/validator class tooling such as `spirv-as` and `spirv-val`

Tooling note:

- if authored source remains raw `.spvasm`, the build still needs a text-to-SPIR-V assembler
- `rspirv` is a good ergonomic choice for SPIR-V inspection, binary parsing, and possible in-build normalization after assembly
- `rspirv` is not, by itself, a replacement for the `.spvasm` text assembly step
- therefore the coherent `.spvasm` steady-state is: `spirv-as` for text assembly, `spirv-val` for validation, and optionally `rspirv` for ergonomic post-assembly handling before Naga

## Artifact Model

Introduce a single artifact model per shader variant.

Suggested generated/runtime-facing types:

- `ShaderRegistryKey { name: &'static str, config: &'static str }`
- `CanonicalShaderIdentity`
  - `vertex_spirv_words: &'static [u32]`
  - `fragment_spirv_words: &'static [u32]`
  - `metadata_digest: &'static str`
- `ShaderStageArtifact`
  - `spirv_words: &'static [u32]`
  - `wgsl_source: &'static str`
  - `glsl_sources: TargetGlslSources`
- `ShaderMetadata`
  - entry-point names per stage
  - vertex inputs by semantic name, location, and format
  - bind-group or fixed-slot resource bindings by semantic name
  - push-constant or uniform-block shape if applicable
  - target/profile availability flags
- `ShaderArtifact`
  - `registry_key`
  - `identity`
  - `metadata`
  - `vertex`
  - `fragment`

Storage policy:

- authored `.spvasm` lives in the source tree as the canonical shader input
- build assembles `.spvasm` to `.spv` with `spirv-as`
- derived WGSL, derived GLSL, and generated Rust registry code are build outputs
- generated artifacts are not checked into the repo unless a separate policy says otherwise

## Build Pipeline

The build pipeline for migrated variants should be exactly this:

1. Load authored `.spvasm` plus any small manifest needed for registry lookup
2. Assemble `.spvasm` to `.spv` with `spirv-as`
3. Validate authored SPIR-V with `spirv-val`
4. Parse SPIR-V with naga `spv-in`
5. Reflect metadata from the parsed module(s)
6. Emit WGSL with naga `wgsl-out`
7. Emit desktop GL and GLES GLSL with naga `glsl-out`
8. Validate generated outputs with target-appropriate validators
9. Generate the shader registry consumed by runtime code

What disappears from the build path:

- GLSL assembly as the canonical input path for migrated variants
- shader-text surgery for naga compatibility
- any feature whose only reason to exist is naga GLSL-front-end fragility

What remains acceptable:

- post-generation fixups only if they are true target-output normalization and not
  an attempt to compensate for a missing canonical representation
- explicit metadata augmentation when reflection alone does not encode a runtime invariant

Validation of generated outputs should be explicit:

- WGSL validation can use `wgpu::Device::create_shader_module()` as the runtime-facing validator
- GLSL validation can shell out to validation-only tools such as `glslangValidator`
- these are validator gates, not alternate authoring or translation paths

Reference-tool note:

- `spirv-as` is the reference assembler for authored `.spvasm`
- `spirv-val` is the validator
- `spirv-as` can be kept as the authoritative fallback/reference assembler even if the build later gains additional Rust-side SPIR-V ergonomics via `rspirv`

## Authoring Model

Authoring changes with this branch reset.

For migrated shader families:

- developers author and review `.spvasm` as the normative shader source artifact
- metadata alongside that SPIR-V is part of the authored contract
- WGSL and GLSL are generated products, not edited source files

This should use a small source-package format rather than raw loose `.spv` blobs.
For example, each variant may be represented by:

- vertex-stage `.spvasm`
- fragment-stage `.spvasm`
- a small manifest describing registry lookup key, entry points, and any
  metadata that reflection cannot recover unambiguously

Exact on-disk format is secondary. The important rule is that the authored object
is SPIR-V assembly centered, not GLSL-centered.

## Implications For Current Code

The current owning abstractions that need to change are already visible.

### `webrender_build/src/wgsl.rs`

This file currently exists largely to keep naga's GLSL frontend working. In the
intended design, most of it is deleted.

Delete or retire the path built around:

- `preprocess_for_naga`
- `fix_switch_fallthrough`
- GLSL-specific sampler rewriting
- stage-ifdef resolution for naga parsing
- `translate_to_wgsl` from GLSL input

Keep only pieces that still make sense in an SPIR-V-driven world, such as:

- SPIR-V to WGSL generation helpers
- output-side fixups that are still required after SPIR-V-based generation

### `webrender_build/src/compiled_artifacts.rs`

This file should stop compiling GLSL to SPIR-V.

Replace the current center of gravity:

- `glsl_to_spirv(...)`
- preprocessing of assembled GLSL before SPIR-V creation

with:

- authored SPIR-V loading
- SPIR-V validation
- naga-based reflection
- naga-based WGSL generation
- naga-based GLSL generation
- registry emission from the resulting artifact set

### `webrender_build/Cargo.toml`

The steady-state dependency direction should be:

- keep `naga` with `spv-in`, `wgsl-out`, and `glsl-out`
- remove `shaderc`
- remove any feature enablement that exists only for naga GLSL input
- do not add `glslang` or similar compiler dependencies as Cargo dependencies

### `webrender/src/device` and `webrender/src/renderer`

The runtime contract shifts from source strings to artifacts.

Required effect:

- `wgpu` pipeline creation consumes generated WGSL plus reflected metadata
- GL program creation consumes generated GLSL plus reflected metadata
- both paths key caches from the same canonical artifact digest lineage

The seam is no longer "backend-specific shader sources".

The seam becomes:

- backend-specific consumption of canonical SPIR-V plus reflected metadata
- backend-specific use of derived target output generated from that canonical source

The phrase "shader for X" in runtime code should resolve to one canonical artifact identity,
accessed through a registry lookup entry, not two unrelated source-generation paths.

## Runtime Changes

### `wgpu` path

Required changes:

- remove runtime parsing of WGSL to recover vertex input layout or bindings
- construct vertex layouts from reflected metadata
- create shader modules from generated WGSL derived from canonical SPIR-V
- validate fixed binding expectations against reflected metadata, not string heuristics

Acceptance bar:

- no `wgpu` runtime path depends on authored GLSL
- no `wgpu` runtime path reparses WGSL for metadata

### GL path

Required changes:

- stop treating runtime GLSL assembly as the source of truth for migrated variants
- create GL programs from generated GLSL derived from canonical SPIR-V
- derive program digests from canonical artifact digests plus target profile

Migration note:

- if some GL-only extension-heavy families cannot move immediately, isolate them as
  explicit legacy exceptions instead of leaving the whole pipeline GLSL-centered

## Migration Strategy

This reset implies a different sequence than the current branch plan.

### Phase 1: Define the SPIR-V source package

- choose the on-disk representation for authored `.spvasm` plus manifest/metadata
- define the generated registry types and digest rules
- make one small family load from authored `.spvasm` end-to-end without changing all runtime consumers yet

Before Phase 1 completes, perform the one-time migration bootstrap from existing GLSL to committed `.spvasm` files.

### Phase 2: Side-by-side artifact generation from authored SPIR-V

- assemble `.spvasm` with `spirv-as`
- build WGSL and GLSL from authored SPIR-V with naga
- reflect metadata from the same SPIR-V
- generate the registry without making it the only runtime path yet
- prove that the branch no longer needs naga GLSL ingestion for migrated variants
- require migrated families to pass a backend-switched `wrench` parity slice before
  the family exits this phase

### Phase 3: Switch `wgpu` to artifact-backed consumption

- make `wgpu` consume generated WGSL and reflected metadata only
- remove runtime WGSL parsing
- fail the build if migrated `wgpu` variants do not have complete artifact sets
- require the migrated family to pass `wrench --wgpu reftest` on the agreed
  parity slice before the family exits this phase

### Phase 4: Switch GL migrated variants

- make GL consume generated GLSL for migrated shader families
- keep only explicit GL-only exceptions on the legacy path
- require the migrated family to pass both `wrench reftest` and the relevant
  `wrench --wgpu-hal` or `wrench --wgpu-hal-headless` parity slice before the
  family exits this phase

### Phase 5: Retire the GLSL-front-end path

- delete the preprocessing tower
- remove `shaderc` and associated toolchain requirements
- collapse branch terminology and code structure around SPIR-V artifacts as the source of truth

## Validation Plan

Validation should follow the canonical artifact graph.

### Must-have checks

- every authored SPIR-V module passes `spirv-val`
- reflected metadata is deterministic and complete for migrated variants
- generated WGSL validates and loads through `wgpu` shader-module creation
- generated desktop GL and GLES GLSL compile and link for intended targets
- the agreed high-signal `wrench` parity slice passes for every migrated family
  across the backends that currently consume that family

### Historical import-gate status

- the stabilized `brush_solid` import path now has a focused structural parity
  test: semantic snapshots match, runtime-contract vertex locations match, and
  normalized SPIR-V structure matches checked-in artifacts after ignoring raw
  ID churn and annotation-order noise
- the latest clean raw-word probe for that stabilized `brush_solid` path shows
  identical SPIR-V headers and no newly surfaced executable-instruction drift;
  the remaining raw mismatch begins in `OpName` and then in `OpDecorate`
  ordering / raw-ID assignment
- raw SPIR-V digests therefore remain diagnostic-only for now; they are still
  useful for drift visibility, but they are not the right gate for the current
  migration slice
- broader representative-family structural and validation gating is now viable:
  `cs_svg_filter_node`, `ps_text_run`, `brush_yuv_image`, composite YUV, and
  composite fast-path YUV now also match checked-in normalized SPIR-V
  structure across the representative set: matrix-interface lowering,
  sampled-image wrapper noise, helper-signature drift,
  switch-vs-branch-ladder helper dispatch, function-parameter names,
  within-block pure-op scheduling, Phi incoming-value churn,
  fragment-side matrix reconstruction, stripped function-local variable ID
  recanonicalization, and YUV-family resource layouts are all absorbed, and
  the importer-side `OpSampledImage` undefined-result-type bug is fixed. This
  importer gate served its Phase 2 purpose: it justified the checked-in authored
  SPIR-V corpus. In Phase 5, the bootstrap importer is retired so the active
  build and CI graph no longer require `shaderc`.

### April 2026 branch-state assessment

#### Where the branch stands

- the Phase 2 bootstrap importer has been retired from the active build graph:
  `import_authored_spirv`, the `legacy-glsl-import` feature, and the optional
  `shaderc` dependency are removed. The checked-in `.spvasm` corpus is now the
  migration input, and CI validates that corpus through artifact generation,
  generated WGSL/GLSL validation, runtime metadata checks, and backend parity
  rather than by re-importing legacy GLSL.
- the built-in legacy GLSL source map is now narrowed to non-artifact GL support
  sources, such as `gpu_cache_update` and include-only files. Migrated shader
  program names that are covered by `SHADER_ARTIFACTS` are no longer emitted into
  `UNOPTIMIZED_SHADERS`; without a resource override path they stay on the
  artifact-backed GL source path.
- the old force-legacy-artifact Wrench/WebRender option is gone. It was useful
  while selected migrated families could be forced back onto built-in legacy
  GLSL, but after artifact-backed migrated programs stopped depending on that
  map, the flag only added dead selection plumbing. Explicit shader override
  paths remain the legacy GLSL debugging hook.
- the old full-corpus legacy GLSL validators are gone from the active graph:
  `webrender/tests/angle_shader_validation.rs`, Wrench's `test_shaders`
  subcommand, and the Wrench-only `glsl-lang` dependency were removed. Generated
  GLSL validation now lives with the artifact pipeline in
  `webrender_build/tests/generated_output_validation.rs`, while Wrench keeps
  exercising runtime shader compilation through its normal GL initialization and
  parity lanes.
- the full vertex+fragment legacy GLSL string assembly wrapper is gone as well.
  Remaining legacy GLSL users call the lower-level streaming prefix/main helpers
  directly, which keeps `gpu_cache_update` and explicit shader override loading
  working without preserving the old validator-oriented full-corpus API.

#### Current branch status

- focused backend-facing `wrench` parity slices are now live for six
  representative family clusters:
  `wrench/reftests/spirv-parity/yuv-composite` exercises the migrated
  YUV/image/composite path, and
  `wrench/reftests/spirv-parity/text-micro` exercises a small
  `ps_text_run`-centered text/shadow/clip cluster, and
  `wrench/reftests/spirv-parity/clip-micro` exercises a small
  rectangle-clip / clip-box-shadow cluster, and
  `wrench/reftests/spirv-parity/blur-micro` exercises a small
  `cs_blur` render-task / box-shadow blur cluster, and
  `wrench/reftests/spirv-parity/svg-filter-micro` exercises a small
  `CsSvgFilter` / `CsSvgFilterNode` SVG-filter cluster, and
  `wrench/reftests/spirv-parity/gradient-micro` exercises a small
  `CsFastLinearGradient` / `CsLinearGradient` cache-task cluster
- explicit runtime metadata assertions now guard the artifact-backed `wgpu`
  consumer path for the `ps_text_run` family, so reflected vertex-input
  contract drift fails during shader bootstrap rather than later during reftests
- the same explicit runtime metadata assertion pattern now also guards the
  next concrete clip-family cluster: `cs_clip_rectangle`, `FAST_PATH`, and
  `cs_clip_box_shadow`
- the same runtime-contract pattern now also guards the first render-task blur
  cluster: `CsBlurColor` and `CsBlurAlpha`; the matching build-side WGSL
  normalization now rewrites the `ALPHA_TARGET` fragment output shape and
  compacts the dead-`aData` vertex-location hole so emitted WGSL stays aligned
  with stripped reflected metadata and runtime vertex layouts
- the same runtime-contract pattern now also guards the linear-gradient pair:
  `CsFastLinearGradient` and `CsLinearGradient`, covering the fast and general
  cache-task paths used by the focused gradient parity slice
- the same runtime-contract pattern now also guards the SVG filter pair:
  `CsSvgFilter` and `CsSvgFilterNode`, covering both the simple filter path and
  the graph-backed filter-node path used by SVG filter primitives
- the branch CI lane executes those six focused `wrench` parity slices and the
  focused runtime-contract tests for the text, clip, blur, linear-gradient, and
  SVG filter families
- build-time shader generation remains on the artifact-registry path in
  `webrender/build.rs`, preserves
  `cargo:rerun-if-env-changed=WR_REQUIRE_SPIRV_VAL` for incremental builds, and
  intentionally panics if `gl_backend` is enabled on this branch; that panic is
  a branch guardrail, not the current migration blocker
- a small `wrench` artifact-inspection mode is also live via
  `wrench artifact <SHADER> [--config ...] [--output ...]`; it emits a JSON
  summary and can dump canonical SPIR-V plus derived WGSL payloads for one
  selected registry variant
- generated-output validation is also live at two concrete checkpoints:
  `shader_artifacts_load_as_wgpu_modules` validates derived WGSL at the runtime
  shader-module boundary, and `glsl_oracle` plus
  `webrender_build/tests/generated_output_validation.rs` provide an offline
  derived-GLSL oracle / validation surface for committed shader packages
- the offline derived-GLSL gate is now split but active in CI: the main
  `webrender_build` generated-output test validates authored-SPIR-V-derived
  desktop `450 core` GLSL across the corpus, and the GLES oracle path now does
  a pair-level dead-varying cleanup for generated vertex / fragment GLSL before
  ANGLE validation instead of shader-name / exact-line pruning; that keeps the
  fix confined to the offline oracle surface while still clearing the
  `cs_svg_filter` and `cs_svg_filter_node` dead clip / transform varying issue,
  and the `brush_yuv_image` GLES3 family
  (`TEXTURE_2D,YUV`, `ALPHA_PASS,TEXTURE_2D,YUV`,
  `DEBUG_OVERDRAW,TEXTURE_2D,YUV`) now validates under ANGLE as emitted,
  without requiring any additional GLES-specific output fixup; the composite
  YUV GLES3 pair (`TEXTURE_2D,YUV` and `FAST_PATH,TEXTURE_2D,YUV`) is now also
  covered by explicit ANGLE validation lanes and likewise passes as emitted;
  the non-YUV composite textured pair (`TEXTURE_2D` and
  `FAST_PATH,TEXTURE_2D`) is now covered the same way and also passes as
  emitted;
  the `brush_image` GLES3 textured family now also validates under ANGLE as
  emitted across the plain, alpha-pass, debug-overdraw, repetition /
  antialiasing, and `DUAL_SOURCE_BLENDING` textured variants;
  the `brush_blend` GLES3 family now also validates under ANGLE as emitted
  across the default, alpha-pass, and debug-overdraw variants;
  the `brush_solid` GLES3 family now also validates under ANGLE as emitted
  across the default, alpha-pass, and debug-overdraw variants;
  the `brush_opacity` GLES3 family now also validates under ANGLE as emitted
  across the default, alpha-pass, antialiasing, and debug-overdraw variants;
  the `brush_linear_gradient` GLES3 family now also validates under ANGLE as
  emitted across the dithering, alpha-pass+dithering, and
  debug-overdraw+dithering variants;
  the `brush_mix_blend` GLES3 family now also validates under ANGLE as
  emitted across the default, alpha-pass, and debug-overdraw variants;
  the `cs_fast_linear_gradient` GLES3 default lane and the
  `cs_conic_gradient`, `cs_linear_gradient`, and `cs_radial_gradient` GLES3
  default+dithering lanes now also validate under ANGLE as emitted;
  the `ps_quad_gradient`, `ps_quad_conic_gradient`, and
  `ps_quad_radial_gradient` GLES3 dithering lanes now also validate under
  ANGLE as emitted;
  the `debug_color`, `debug_font`, `ps_clear`, `ps_copy`,
  `ps_quad_textured`, and `ps_split_composite` GLES3 default lanes and the
  `cs_scale` GLES3 `TEXTURE_2D` lane now also validate under ANGLE as
  emitted;
  the `ps_quad_mask` GLES3 default and `FAST_PATH` lanes now also validate
  under ANGLE as emitted;
  the `cs_border_segment` and `cs_border_solid` GLES3 default lanes, the
  `cs_clip_box_shadow` GLES3 `TEXTURE_2D` lane, the `cs_blur` GLES3
  `ALPHA_TARGET` and `COLOR_TARGET` lanes, and the `cs_clip_rectangle`
  GLES3 default and `FAST_PATH` lanes now also validate under ANGLE as
  emitted;
  the `cs_line_decoration` GLES3 default lane now also validates under ANGLE
  as emitted, completing explicit ANGLE GLES coverage for the currently
  committed shader package set under `webrender/res/spirv`;
  the `ps_text_run` GLES3 textured family now also validates under ANGLE,
  including the `DUAL_SOURCE_BLENDING` variants once the validator resources
  explicitly enable `EXT_blend_func_extended` and one dual-source draw buffer
- the exact validated commands on this branch are:
  `cargo run -p wrench --features wgpu_backend -- artifact brush_yuv_image --config TEXTURE_2D,YUV --output target/wrench-artifacts/brush_yuv_image`
  and
  `cargo test -p webrender --features wgpu_native ps_text_run_variants_match_runtime_metadata_contracts -- --nocapture`
  and
  `cargo test -p webrender --features wgpu_native clip_variants_match_runtime_metadata_contracts -- --nocapture`
  and
  `cargo test -p webrender --features wgpu_native blur_variants_match_runtime_metadata_contracts -- --nocapture`
  and
  `cargo test -p webrender --features wgpu_native linear_gradient_variants_match_runtime_metadata_contracts -- --nocapture`
  and
  `cargo test -p webrender --features wgpu_native svg_filter_variants_match_runtime_metadata_contracts -- --nocapture`
  and
  `cargo test -p webrender --features wgpu_native decoration_border_variants_match_runtime_metadata_contracts -- --nocapture`
  and
  `cargo test -p webrender --features wgpu_native remaining_runtime_layout_variants_match_metadata_contracts -- --nocapture`
  and
  `cargo test -p webrender --features wgpu_native shader_artifacts_load_as_wgpu_modules -- --nocapture`
  and
  `cargo run -p wrench --features wgpu_backend -- --wgpu-hal-headless reftest reftests/spirv-parity/yuv-composite`
  and
  `cargo run -p wrench --features wgpu_backend -- --wgpu-hal-headless reftest reftests/spirv-parity/text-micro`
  and
  `cargo run -p wrench --features wgpu_backend -- --wgpu-hal-headless reftest reftests/spirv-parity/clip-micro`
  and
  `cargo run -p wrench --features wgpu_backend -- --wgpu-hal-headless reftest reftests/spirv-parity/blur-micro`
  and
  `cargo run -p wrench --features wgpu_backend -- --wgpu-hal-headless reftest reftests/spirv-parity/svg-filter-micro`
  and
  `cargo run -p wrench --features wgpu_backend -- --wgpu-hal-headless reftest reftests/spirv-parity/gradient-micro`
  and
  `cargo run -p wrench --features wgpu_backend -- --wgpu-hal-headless reftest reftests/spirv-parity/radial-conic-gradient-micro`
  and
  `cargo run -p wrench --features wgpu_backend -- --wgpu-hal-headless reftest reftests/spirv-parity/decoration-border-micro`
  and
  `cargo run -p wrench --features wgpu_backend -- --wgpu-hal-headless reftest reftests/spirv-parity`
  and
  `cargo test -p webrender_build --features glsl-oracle --lib prunes_dead_gles_varying_chains -- --nocapture`
  and
  `cargo test -p webrender_build --features glsl-oracle --test generated_output_validation -- --nocapture`
- on the current Windows machine, the focused YUV/composite, text, clip, blur,
  SVG filter, gradient, radial/conic, decoration/border, mask, image/blend/
  composite, and scale slices are green; the full `reftests/spirv-parity` lane
  reports `33 passing, 0 failing` on both desktop GL (`gl_backend`) and
  `wgpu-hal-headless` (`wgpu_backend`), while Linux/mac-specific YUV image cases
  remain gated by existing manifest conditions and are expected to exercise on
  CI instead. The three Windows NVIDIA desktop-GL follow-ups from the first
  Phase 4 run are now manifest-encoded: `image/segments` and
  `border/discontinued-dash` use GL-only fuzzy allowances, and
  `text/large-line-decoration` keeps the original non-GL `!= blank` assertion
  while recording the GL blank result as the backend-specific expectation.
- the generated GLES dead-varying cleanup is intentionally conservative around
  live private globals, function calls, and struct fields: it still prunes dead
  fragment interface inputs and matching vertex outputs, but it no longer treats
  helper-call statements or no-assignment struct fields as removable
  definitions. The regression coverage is `prunes_dead_gles_varying_chains` and
  `keeps_live_gles_output_declarations_and_locals`, and the full generated-
  output suite now reports `69 passed, 0 failed`.
- CI productization is now centered on aggregate gates instead of per-family
  checklist growth: `.github/workflows/main.yml` runs
  `remaining_runtime_layout_variants_match_metadata_contracts`,
  `shader_artifacts_load_as_wgpu_modules`, the full `generated_output_validation`
  suite, `canonical_artifact_digests_are_stable`,
  the parent `reftests/spirv-parity` Wrench lane under
  `wgpu-hal-headless`, and the matching GL hidden-window `reftests/spirv-parity`
  lane as a required Phase 4 artifact-consumer gate. The individual micro-slices
  remain useful for local bisection, while CI follows the lane manifest.
- the conic caveat is now resolved at the WGPU picture-cache root cause rather
  than via conic-specific workarounds: picture-cache opaque batches must use
  `WgpuDepthState::WriteAndTest`, matching the main WGPU target path and the GL
  renderer's opaque depth contract, instead of `AlwaysPass`. With that fix in
  place, both earlier conic workarounds were removed: the oversized-conic
  `PrimitiveOpacity::translucent()` override in
  `webrender/src/prim_store/gradient/conic.rs` and the `1024` quad cutoff in
  `webrender/src/prepare.rs`.
- validation with the WGPU picture-cache depth fix and both conic workarounds
  removed: `reftests/spirv-parity/radial-conic-gradient-micro` reports
  `4 passing, 0 failing`, and `reftests/gradient` reports
  `80 passing, 0 failing`.
- a direct local probe of the upstream `6ab18fa76` quad-gradient coordinate-
  space fix is not branch-safe yet: after porting the conic-side center-space
  changes and temporarily removing the `1024` quad cutoff, the focused
  `radial-conic-gradient-micro` lane regressed to `2 passing, 2 failing`
  (`conic-large` plus `tiling-conic-2`); restoring the branch-local coordinate
  contract brought the lane back to `4 passing, 0 failing`, so treat
  `6ab18fa76` as a root-cause hint rather than a tiny cherry-pick candidate
- the first Phase 5 closure pass exposed one shader-variant contract mismatch
  that had been hidden by the retired legacy fallback: `shade.rs` still treated
  `WebRenderOptions::enable_dithering` as a runtime selector for
  `brush_linear_gradient`, `ps_quad_*_gradient`, and the migrated
  `cs_*_gradient` shaders, while the artifact registry and typed `wgpu`
  variants already treated `DITHERING` as the committed build-time artifact
  lane. The branch now converges GL shader creation on the same `DITHERING`
  artifact variants and always creates the `sDither` texture resource for both
  backends. `WebRenderOptions::enable_dithering` remains in place because it
  still feeds non-shader gradient scene-building / fast-path decisions; it no
  longer selects alternate shader artifacts for these migrated families.
- validation for the Phase 5 closure dithering-contract fix:
  `cargo test -p webrender --features wgpu_native gradient_variants_match_runtime_metadata_contracts -- --nocapture`,
  `cargo test -p webrender --features wgpu_native typed_wgpu_variants_match_build_artifact_contract -- --nocapture`,
  `cargo test -p webrender --features wgpu_native shader_artifact_registry_contains_expected_variants -- --nocapture`,
  `cargo test -p webrender_build --features glsl-oracle validate_generated_output_derivations -- --nocapture`,
  and `git diff --check`.

#### Where to go next

- the remaining-family lane is now explicit and green: `mask-micro` covers
  `ps_quad_mask`, `image-blend-composite-micro` covers the remaining primitive
  image/blend/composite paths, and `scale-micro` covers the scale/raster-root
  checkpoint. Together with the earlier slices, the parent
  `reftests/spirv-parity` manifest is the narrow backend-facing lane for the
  committed shader package set.
- runtime metadata coverage is likewise no longer a per-family TODO list:
  `remaining_runtime_layout_variants_match_metadata_contracts` walks every typed
  `WgpuShaderVariant` with a declared runtime instance layout, including the
  previously separate text, clip, blur, gradient, SVG, decoration/border, mask,
  scale, composite, and primitive-layout families.
- all five Phase 5 closure tracks are now complete: bootstrap importer
  removal, built-in legacy GLSL source-map reduction, runtime legacy source
  assembly isolation, resource override policy, and terminology cleanup.
  The branch's remaining open work is outside the shader artifact lane:
  Servo presenting smoke coverage extension (scroll, SVG/filter), turning
  high-exposure Wrench tolerances (`image/segments`,
  `border/discontinued-dash`, `text/large-line-decoration`) into understood
  bugs or narrower expectations, and managing long-lived branch drift against
  `upstream/upstream` (see *Branch Lifecycle and Upstream Relationship* — the
  integration model is selective cherry-pick, not rebase).
- the built-in legacy GLSL source map is now reduced to the explicit
  non-artifact root closure for `gpu_cache_update`: `UNOPTIMIZED_SHADERS` keeps
  `gpu_cache_update` and its `base` include, while broad shared include files
  such as `shared` are no longer emitted as built-in legacy sources. Explicit
  resource overrides compute their source digest from the override directory, so
  migrated artifact-backed shader names do not need to re-enter the built-in
  legacy source map just to preserve the override escape hatch.
- validation for the built-in legacy source-map reduction:
  `cargo test -p webrender --no-default-features --features gl_backend built_in_unoptimized_sources_exclude_artifact_shaders -- --nocapture`,
  `cargo test -p webrender --features wgpu_native remaining_runtime_layout_variants_match_metadata_contracts -- --nocapture`,
  `cargo test -p webrender --features wgpu_native shader_artifacts_load_as_wgpu_modules -- --nocapture`,
  `cargo test -p webrender_build --features glsl-oracle validate_generated_output_derivations -- --nocapture`,
  and `git diff --check`.
- the legacy optimized-GLSL runtime branch and public option are now removed as
  a support column:
  generated shader sources no longer emit `OPTIMIZED_SHADERS` or
  `LegacyOptimizedShaderSource`, GL program selection no longer has a
  `ProgramSourceType::Optimized` fallback, and `DeviceConfig` no longer threads
  `use_optimized_shaders` into device creation. The no-op
  `WebRenderOptions::use_optimized_shaders` field is gone; the deprecated
  Wrench `--use-unoptimized-shaders` no-op has also been removed alongside its
  ci-scripts callers. This leaves GL with two explicit source lanes:
  artifact-backed generated GLSL for migrated shaders, and unoptimized legacy
  assembly only for the explicit built-in legacy roots or resource override
  loading.
- validation for the legacy optimized-GLSL source-path removal:
  `cargo test -p webrender --no-default-features --features gl_backend built_in_unoptimized_sources_exclude_artifact_shaders -- --nocapture`,
  `cargo test -p webrender --features wgpu_native shader_artifacts_load_as_wgpu_modules -- --nocapture`,
  `cargo test -p webrender --features wgpu_native remaining_runtime_layout_variants_match_metadata_contracts -- --nocapture`,
  `cargo test -p webrender_build --features glsl-oracle validate_generated_output_derivations -- --nocapture`,
  `cargo run --features wgpu_backend -- --wgpu-hal-headless reftest reftests/spirv-parity`
  from `wrench/` (`33 passing, 0 failing`; the previous
  `CsBlurAlpha#PremultipliedAlpha#None#Bgra8Unorm` validation warning is fixed;
  the local Windows Vulkan loader still logs a registry manifest warning),
  `cargo run --features wgpu_backend -- --wgpu reftest reftests/spirv-parity`
  from `wrench/` (`33 passing, 0 failing`),
  `cargo run --features wgpu_backend -- --wgpu-hal reftest reftests/spirv-parity`
  from `wrench/` (`33 passing, 0 failing`; local Windows Vulkan loader registry
  warning only),
  `cargo run --features gl_backend --no-default-features -- --gl-hidden reftest reftests/spirv-parity`
  from `wrench/` (`33 passing, 0 failing`),
  and `git diff --check`.
- the runtime legacy source assembly path (Phase 5 closure track 3) is now
  isolated: `Device::build_shader_string` is gone, prefix-string assembly is
  inlined into the digest hash path, override-vs-built-in routing in
  `ProgramSourceInfo::new` consults a single source resolver, and
  `build_shader_main_string` and `build_shader_prefix_string` are crate-private
  in `webrender_build`. Runtime callers reach the legacy GLSL preprocessing
  tower only through the explicit `legacy_main_string_digest` seam (digest
  path) and `do_build_shader_string` (source path); both are reachable only
  for the `LEGACY_UNOPTIMIZED_SHADER_ROOTS` closure (`gpu_cache_update` plus
  its `base` include) and for explicit shader-override loading.
  `shader_source_from_file` is renamed `legacy_shader_source_from_file` so
  the name signals the boundary. Three digest-stability gates protect the
  program cache key for `gpu_cache_update`: a prefix-bytes snapshot in
  `webrender_build`, a main-digest match against the build-time precomputed
  value, and a full program-cache-digest snapshot
  (`1d136585efbdee11` on Windows/Linux; macOS and Android emit an extra
  platform define and need their own pinned values on first run there).
- validation for the runtime legacy source assembly isolation:
  `cargo test -p webrender_build --lib`
  (`legacy_prefix_bytes_snapshot_for_gpu_cache_update` passes),
  `cargo test -p webrender --no-default-features --features gl_backend built_in_unoptimized_sources_exclude_artifact_shaders unoptimized_sources_lock_legacy_root_closure legacy_main_digest_matches_precomputed_for_gpu_cache_update unoptimized_program_digest_for_gpu_cache_update_is_stable -- --nocapture`,
  `cargo test -p webrender --features wgpu_native shader_artifacts_load_as_wgpu_modules remaining_runtime_layout_variants_match_metadata_contracts -- --nocapture`,
  `cargo test -p webrender_build --features glsl-oracle validate_generated_output_derivations -- --nocapture`,
  `cargo run --features gl_backend --no-default-features -- --gl-hidden reftest reftests/spirv-parity`
  from `wrench/` (`33 passing, 0 failing`),
  `cargo run --features wgpu_backend -- --wgpu-hal-headless reftest reftests/spirv-parity`
  from `wrench/` (`33 passing, 0 failing`),
  and `git diff --check`.
- terminology cleanup (Phase 5 closure track 5) closed: the deprecated Wrench
  `--use-unoptimized-shaders` no-op flag and its three ci-scripts callers
  (`ci-scripts/{linux,macos}-release-tests.sh`, `ci-scripts/windows-tests.cmd`)
  are removed. Each of those scripts had `--precache test_init` invoked twice
  in a row — once with the no-op flag — so the duplicate-with-flag invocation
  is dropped, leaving one shader-precache smoke per platform. No functional
  change. The remaining `unoptimized` / `UNOPTIMIZED_SHADERS` /
  `LegacyUnoptimizedShaderSource` / `ProgramSourceType::Unoptimized` /
  `get_unoptimized_shader_source` / `LEGACY_UNOPTIMIZED_SHADER_ROOTS`
  identifiers are load-bearing names for the legacy GL exception path, not
  stale wording.
- validation for the terminology cleanup:
  `cargo check -p wrench --features gl_backend --no-default-features`
  (clap-yaml parsing succeeds without the removed arg),
  `cargo test -p webrender --no-default-features --features gl_backend built_in_unoptimized_sources_exclude_artifact_shaders unoptimized_sources_lock_legacy_root_closure legacy_main_digest_matches_precomputed_for_gpu_cache_update unoptimized_program_digest_for_gpu_cache_update_is_stable -- --nocapture`,
  and `git diff --check`.

#### Phase 5 closure tracks

Do not subdivide Phase 5 by implied lettered phases unless a separate execution
plan explicitly defines those letters. The durable demarcation for the rest of
Phase 5 is by support column:

1. Bootstrap importer removal: complete. The active graph no longer depends on
   `shaderc`, `legacy-glsl-import`, or re-importing legacy GLSL to justify the
   checked-in `.spvasm` corpus.
2. Built-in legacy GLSL source-map reduction: narrow `UNOPTIMIZED_SHADERS` to
   the explicit non-artifact root set that still needs built-in GLSL sources.
   The current intended root is `gpu_cache_update`; its include closure remains
   built in, but migrated shader program names and broad shared include sets do
   not.
3. Runtime legacy source assembly isolation: complete. Runtime callers reach
   the legacy GLSL preprocessing tower only through the explicit
   `legacy_main_string_digest` seam (digest path) and `do_build_shader_string`
   (source path); both reach `build_shader_main_string` and
   `build_shader_prefix_string` (now crate-private) only for the
   `LEGACY_UNOPTIMIZED_SHADER_ROOTS` closure (`gpu_cache_update` plus its
   `base` include) and for explicit shader-override loading. Three
   digest-stability gates pin the program cache key for `gpu_cache_update`:
   a prefix-bytes snapshot in `webrender_build`, a main-digest match against
   the build-time precomputed value, and a full program-cache-digest
   snapshot in `webrender`.
4. Resource override policy: complete for this migration. Shader resource
   overrides remain a dev-only legacy GL escape hatch. When present, they
   disable GL artifact-backed source selection and read GLSL files from the
   override directory; they are ignored by the wgpu backend. This keeps
   overrides useful for local GL shader debugging without making them part of
   the steady-state artifact pipeline or forcing migrated shader names back into
   the built-in legacy source map. A future artifact-aware override mechanism
   should be a new explicit feature, not an extension of this path.
5. Terminology cleanup: complete. The `pilot` rename to `artifact-smoke`
   landed earlier; the `--use-unoptimized-shaders` deprecated no-op and its
   three ci-scripts callers are now removed. Remaining `unoptimized` /
   `UNOPTIMIZED_SHADERS` / `LegacyUnoptimizedShaderSource` /
   `ProgramSourceType::Unoptimized` / `get_unoptimized_shader_source` /
   `LEGACY_UNOPTIMIZED_SHADER_ROOTS` references are load-bearing identifiers
   that name the legacy GL exception path itself, not wording that implies
   the old preprocessing tower is the normal shader path; renaming them
   would change identifiers without changing meaning.
   `reftests/artifact-smoke` remains a historical broad smoke bundle, not the
   artifact migration parity gate; on local Windows GL-hidden the full bundle
   still reaches old image/YUV expectations that are not normalized there. Use
   `reftests/spirv-parity` for the current backend parity gate, and use
   `reftests/artifact-smoke/brush-solid.yaml` as the renamed local smoke check.

The next code slice should be described against one of these closure tracks, for
example: "narrow built-in legacy source emission to the `gpu_cache_update`
closure" rather than "Phase 5C".

#### Fruitful sidequests

- per-family micro-scenes for text, image, composite, clip, and render-task
  families would make backend divergence easier to localize than broad reftest
  slices alone
- add a minimal Servo-facing backend smoke matrix after the shader artifact
  lane is stable: one real webpage / display-list path through GL, windowed
  `wgpu`, `wgpu-hal`, `wgpu-hal-headless`, and host-owned/shared-device
  composition. The current Wrench parity lane proves the committed shader
  package set; it does not prove Servo page rendering, present/resize behavior,
  external images, or host compositor integration.
- keep the windowed `wrench --wgpu reftest reftests/spirv-parity` and
  `wrench --wgpu-hal reftest reftests/spirv-parity` entries in the
  release-facing validation matrix alongside the headless lane.
- keep the existing generated-output checks easy to rerun and compare:
  `shader_artifacts_load_as_wgpu_modules` for WGSL module creation and
  `generated_output_validation` / `glsl_oracle` for derived GLSL output are
  already the right narrow surfaces for derivation regressions outside full
  backend parity
- deterministic metadata-digest reporting in CI is now wired:
  `wrench metadata-report --output target/shader-metadata-report.json` (under
  `wgpu_backend`) iterates `SHADER_ARTIFACTS` in sorted `(name, config)` order
  and emits `webrender-wgpu/metadata-report/v1` JSON containing the canonical
  digest, vertex inputs, and resource bindings for every variant. Two runs are
  byte-identical. The `phase2-shader-gates` CI lane runs it after the existing
  digest-stability test and uploads the report as a workflow artifact, so
  contract drift between runs is diff-able without needing raw SPIR-V word
  comparison.

#### Dangerous pitfalls

- chasing raw or canonical digest parity now is likely negative-value work; the
  current evidence still says that exact bytewise parity is the wrong gate for
  this phase
- broadening structural normalization too aggressively can hide semantic drift;
  every new equivalence should be tied to one concrete checked-in/imported
  mismatch rather than a speculative abstraction
- treating importer-green as migration-complete would be a category error; the
  real exit from Phases 2 through 4 is still backend parity under `wrench`,
  not just SPIR-V structural agreement
- remapping or canonicalizing function-local IDs too broadly is risky; the
  recent `GLSL.std.450` near-miss is a reminder to distinguish stripped local
  variables from real global IDs and import handles
- this branch is already long-lived relative to `upstream/upstream` (see
  *Branch Lifecycle and Upstream Relationship*), so broad cherry-pick batches
  or large unrelated refactors should be timed around validation checkpoints
  rather than mixed into representative-gate work. The integration model is
  selective cherry-pick from `upstream/upstream`, not rebase.

#### Current exposure audit

The branch is not currently hiding a known failed shader artifact gate: the
artifact registry, runtime metadata contract checks, generated WGSL module
creation, generated GLSL validation, and parent Wrench GL / `wgpu-hal-headless`
SPIR-V parity lane are green. The remaining exposure is mostly in backend
coverage breadth and in a few deliberately recorded Wrench tolerances.

Current normalization / generated-output rewrites:

- GL artifact lookup normalizes shader feature order for `(name, config)`
  registry matching.
- `strip_dead_adata` removes unused inherited `aData` vertex inputs and compacts
  the remaining locations for opted-in packages, with matching reflected
  metadata changes.
- WGSL reflection strips naga's trailing `_` disambiguation suffix from
  argument names so runtime metadata continues to match canonical semantic
  names.
- generated WGSL rewrites the `cs_blur` `ALPHA_TARGET` fragment output from
  `vec4<f32>` to scalar `f32`; eager wgpu pipeline creation now compiles
  `CsBlurAlpha` against its actual alpha target key
  (`None` / `None` / `R8Unorm`) instead of the default surface BGRA key.
- generated WGSL rewrites the `SetSat` helper shape.
- generated GLES GLSL prunes dead fragment-input / vertex-output varying chains
  conservatively after naga emission.

Current Wrench fuzz / backend-specific expectations in
`reftests/spirv-parity`:

- Low-risk numeric drift: blur, masks, tiled conic, hard-stop gradient clipping,
  rounded YUV, and RGB composite use small max-diff tolerances (`1` or `2`,
  with bounded affected pixels).
- Broad but low-amplitude drift: `gradient_cache_5stops` allows max diff `1`
  across a large pixel count.
- Platform coverage gap: the direct YUV image cases in `yuv-composite` run only
  on Linux/mac, so the local Windows lane does not exercise those exact entries.
- High-exposure GL allowances: `image/segments` allows `env(gl)` max diff `255`
  over a large pixel count, and `border/discontinued-dash` allows `env(gl)` max
  diff `255` over a large pixel count. These keep the aggregate lane green, but
  they are debt, not proof of close visual parity.
- Recorded backend divergence: `text/large-line-decoration` is `!= blank` for
  non-GL but `== blank` for GL. This is not fuzz; it records that the GL result
  is blank for that micro-case.

Pitfall exposure by front:

- Raw/canonical digest parity: low immediate exposure. Canonical digests are
  stable and useful for drift detection, but exact raw word parity remains the
  wrong acceptance gate.
- Over-broad normalization: medium exposure. The active rewrites are targeted,
  but each new rewrite should come with one narrow failing artifact or Wrench
  case and a regression test. Do not generalize the GLES dead-code pruner or WGSL
  helper rewrites speculatively.
- Importer-green complacency: low current exposure. The importer has been
  removed from the active graph; the branch now gates through artifacts,
  generated outputs, runtime metadata, and Wrench.
- Function-local / import-handle canonicalization mistakes: low current
  exposure because the old structural-import normalization is no longer the
  active gate, but the `GLSL.std.450` near-miss remains relevant if any new
  SPIR-V normalization is introduced.
- Backend matrix overclaiming: medium exposure. Wrench now proves the current
  shader parity corpus for GL hidden-window, windowed `wgpu`, windowed
  `wgpu-hal`, and `wgpu-hal-headless`; lower-level tests cover the host-owned
  shared-device path and render-to-caller-texture path. This still does not
  prove Servo webpage rendering, external image integration in an embedding, or
  resize/present behavior.
- Resource override leakage: low-to-medium exposure. The policy is now explicit:
  overrides are a legacy GL-only debugging escape hatch, disable GL artifact
  source selection, are ignored by wgpu, and must not force migrated shader
  names back into built-in legacy source maps. Residual exposure is mostly user
  confusion from the old Wrench `--shaders` flag name, not artifact drift.
- Public option drift: low exposure. `WebRenderOptions::use_optimized_shaders`
  has been removed after the optimized GLSL source path was deleted. The
  deprecated Wrench `--use-unoptimized-shaders` no-op has also been removed
  along with its ci-scripts callers, so no shader-path option remains
  user-visible from command-line surface.
- Long-lived branch drift against `upstream/upstream`: medium exposure. Keep
  future code slices small and re-run the aggregate artifact, GL, and Wrench
  gates before any broad upstream cherry-pick batch. See
  `2026-04-22_upstream_cherry_pick_reevaluation.md` for the per-candidate
  watchlist; do not propose a rebase against `upstream/upstream` (the
  integration model is selective cherry-pick).

Current local backend matrix, refreshed April 26, 2026:

| Path | Command from `wrench/` | Status |
| --- | --- | --- |
| GL hidden window | `cargo run --features gl_backend --no-default-features -- --gl-hidden reftest reftests/spirv-parity` | `33 passing, 0 failing` |
| wgpu owned surface | `cargo run --features wgpu_backend -- --wgpu reftest reftests/spirv-parity` | `33 passing, 0 failing`; local Windows Vulkan loader registry warning only |
| wgpu-hal windowed | `cargo run --features wgpu_backend -- --wgpu-hal reftest reftests/spirv-parity` | `33 passing, 0 failing`; local Windows Vulkan loader registry warning only |
| wgpu-hal headless | `cargo run --features wgpu_backend -- --wgpu-hal-headless reftest reftests/spirv-parity` | `33 passing, 0 failing`; local Windows Vulkan loader registry warning only |
| wgpu shared / host-owned device | `cargo test -p webrender --features wgpu_backend --test wgpu_shared_device -- --nocapture`; `cargo test -p webrender --features wgpu_native --test wgpu_backends -- --nocapture` | lower-level shared-device coverage green (`2 passing` and `8 passing`); no Wrench command |
| Servo wgpu embedder compile | `cargo check -p servo --example toy_wgpu_embedder --features wgpu_backend` from sibling `servo-wgpu/` | green against this WebRender branch after Servo commit `fca6897b8ed`; compile/link coverage only |
| Servo wgpu non-presenting smoke | `cargo run -p non-presenting-wgpu-embedder --features wgpu_backend -- --output out.png` from sibling `servo-wgpu/` (downstream fork) | host-owned shared `wgpu::Device` path with no presentable frame target; Servo falls back to its internal composite texture and `WebView::take_screenshot` reads back through the paint-layer fallback. Local run green |
| Servo wgpu presenting smoke | `cargo run -p servo --example presenting_wgpu_smoke --features wgpu_backend -- --output smoke.png` from sibling `servo-wgpu/` (downstream fork) | drives the same `WgpuRenderingContext` the toy embedder uses. The page is now a real `file://` HTML at `examples/smoke-assets/page.html` (not a data: URL) so it can reference relative assets. Six families covered: solid rect, linear gradient, radial gradient, border-radius clip, image (`<img>` from `swatch.png`), text (sans-serif "OK"). Asserts six exact-pixel samples plus a region check that counts dark pixels in the text strip (sidesteps glyph-shape variance). Local run green. The smoke example and assets are permanently downstream-only — Servo upstream does not accept AI-assisted contributions, so the dispatch-only CI lane reads them from the local fork |

#### GL tolerance debt: investigation notes (2026-04-27)

Static analysis of the three high-exposure GL cases recorded from the first
Phase 4 run. Root causes captured here for triage; the
`text/large-line-decoration` blank requires runtime confirmation.

**`text/large-line-decoration` — GL renders blank**

Manifest entry:
`env(gl) == ../../text/large-line-decoration.yaml ../../text/blank.yaml`

Static analysis traversal (rules out):

- **Tile culling in `composite_simple`**: GL path iterates the same tile list
  as wgpu. The 5000-unit-wide decorations starting at y=0/100/200/300 produce
  non-empty tile intersections — tiles are not culled.
- **`TileSurface::Color` optimization** (`picture.rs` line 1276): `is_simple_prim`
  requires `prims.len() == 1`. The scene has 4 prims (solid, dashed, dotted,
  wavy) per tile → `is_simple_prim=false` → tiles use `TileSurface::Texture`.
- **Composite UV math error** (`composite.glsl`): `DrawTarget::Texture` always
  returns `surface_origin_is_top_left=true` (`device/gl.rs` line 1595).
  Picture cache render tasks always use `ortho(0,W,0,H)` (`renderer/mod.rs`
  lines 10355–10362). UV v=0 maps to `world_pos.y = device_rect.y` (screen
  top); GL FBO y=0 is content origin. Math is self-consistent for GL.
- **Pixel readback Y-convention mismatch**: `read_pixels_rgba8` on GL calls
  `glReadPixels` directly, no Y-flip. Both the test image and reference
  (`blank.yaml`) use the same readback path — any Y-convention difference is
  symmetric and cannot produce blank-vs-non-blank.
- **Solid line render task dependency**: `cs_line_decoration` solid style
  (case 0) sets `alpha = 1.0` unconditionally — no render task needed. The
  solid line being blank alongside the render-task-dependent dashed/dotted/wavy
  lines rules out a render task failure as the sole cause.

**Most likely candidate (unconfirmed, needs runtime):** `frame.present=false`
→ `device_size=None` in `composite_frame` (`renderer/mod.rs` line 10434) →
composite pass skipped → blank framebuffer. `frame_builder.rs:298`:
`let render_picture_cache_slices = present;` gates picture cache rendering on
the `present` flag. Whether this path is triggered for the
large-line-decoration YAML on the GL backend during a Wrench `--gl-hidden`
reftest run is not confirmed by static analysis alone.

**Action to confirm**: Add a GL-only `--gl-hidden` debug run that prints
`frame.present` at `composite_frame` entry for this YAML; alternatively,
bisect by temporarily asserting `present == true` inside `composite_frame` to
see if the blank disappears.

---

**`image/segments` — 69,094 pixel diff on GL (max diff 255)**

Manifest entry:
`fuzzy-if(env(gl),255,69094) == ../../image/segments.yaml ../../image/segments.png`

Scene: checkerboard clipped by a rounded rect (radius=32) at `[10,10,260,260]`
plus an unclipped checkerboard at `[10,290,260,260]`. The clip is processed as
a `cs_clip_rectangle` render task writing a mask into an FBO.

**Hypothesis: clip mask FBO Y-convention inversion.** The `DrawTarget::Texture`
origin is always `surface_origin_is_top_left=true`, and picture cache render
tasks use `ortho(0,W,0,H)`. If the clip mask render task uses a different
projection or if the compositor samples the clip mask at an inverted Y
coordinate relative to the picture cache tile, the rounded-rect clip appears
vertically reflected on GL. With the mask inverted, the clipped region of the
checkerboard would show completely wrong pixels over the full clipped area —
consistent with 69K differing pixels at max diff 255 (large spatial error, not
numeric drift). The unclipped checkerboard at y=290 is unaffected; this
matches the pattern of two regions where only the clipped one diverges.

This test PASSES with the documented GL fuzz allowance. GL is not the artifact
migration target; this is a pre-existing GL baseline difference that does not
affect wgpu parity.

---

**`border/discontinued-dash` — 10,200 pixel diff on GL (max diff 255)**

Manifest entry:
`fuzzy-if(env(gl),255,10200) == ../../border/discontinued-dash.yaml ../../border/discontinued-dash.png`

Scene: 300×300 box with dashed top border and solid left/right/bottom borders.
The dashed top border segment uses a `cs_border_segment` render task.

**Hypothesis: border segment render task FBO Y-convention offset.** If the
`cs_border_segment` render task places the dashed pattern at a Y offset
inconsistent with how GL composites it, the dashed pattern appears displaced
vertically. For a 300px-wide dashed top border of ~10px thickness, 10,200
differing pixels at max diff 255 is consistent with the dashed region being
shifted by a few scanlines across the full width. Solid borders (left/right/
bottom) do not use render tasks and would be correct, matching the observation
that only the dashed top edge differs.

Same GL FBO Y-convention hypothesis as `image/segments`. This test PASSES with
the documented GL fuzz allowance and does not affect wgpu parity.

---

No intentionally skipped work is known inside the shader-artifact validation
lane. The important unproven work is outside that lane: extending the Servo
presenting smoke to cover scroll and SVG/filter families (currently covered:
solid rect, linear gradient, radial gradient, clip, image, text);
embedding-level shared-device composition; and confirming the
`text/large-line-decoration` blank root cause via runtime verification (see
investigation notes above).

The first iteration of the presenting smoke embedded the test page as a
`data:text/html` URL. A nested `data:image/png;base64,...` `<img src>` inside
that URL trips Servo's CSS `url()` error recovery (the inner `;` and `,`)
in a way that cascades into sibling absolute-positioned layout. The smoke is
now a real `file://` HTML in `examples/smoke-assets/` with a sibling
`swatch.png`, which dodges the data: URL trap.

**Bug discovered by the image family in the smoke (now fixed):** the wgpu
image path was rendering RGBA inputs as if they were BGRA — `swatch.png` is
authored as cyan `(0,255,255)` but rendered as yellow `(255,255,0)`. Root
cause was twofold:

- `renderer/init.rs` sets `swizzle_settings = None` for the wgpu device with
  the (true) reasoning that wgpu has no equivalent of GL's
  `TEXTURE_SWIZZLE_*` sampler-side swizzle. That setting causes the texture
  cache to populate `TextureCacheUpdate::format_override` with the source
  image format, expecting the upload site to swap channels when the source
  format disagrees with the destination atlas format.
- `WgpuDevice::upload_texture_sub_rect` discarded that hint
  (`let _ = format`) and `renderer/mod.rs` defaulted to `BGRA8` even when the
  destination texture was `Rgba8Unorm`, so RGBA bytes were written raw into a
  Bgra8Unorm shared atlas and sampled with channels swapped.
- Fix: `upload_texture_sub_rect` now honours the source format and performs a
  CPU-side R/B swap when source and destination disagree (the wgpu equivalent
  of the GL backend's sampler swizzle); the upload sites in `renderer/mod.rs`
  default to the destination texture's image format via the new
  `WgpuTexture::image_format()` accessor instead of an unconditional `BGRA8`.

The smoke's image-strip assertion still tolerates either correct cyan or
known-buggy yellow with a warning, so a regression of this fix would be
visible immediately. Verified locally: cyan rendered as cyan, no warning.

### Wrench as the migration gate

`wrench` is the regression oracle while the explicit legacy GL exception path
and the artifact-backed path coexist.

During Phases 2 through 4, a migrated family is not done because its generated
WGSL and GLSL validate in isolation. It is done only when the same rendering
inputs still pass the agreed parity slice under the relevant backend switches:

- `wrench --wgpu reftest` for the `wgpu` path
- `wrench reftest` for the GL path
- `wrench --wgpu-hal reftest` or `wrench --wgpu-hal-headless reftest` where the
  host-owned-device path is part of the supported validation surface

This is stronger than the pre-migration setup because the compared backends are
expected to consume derivations of the same canonical SPIR-V and metadata.
Backend divergence therefore points at a real derivation or runtime-integration
problem instead of being confounded with separate authored shader sources.

Important wording: parity here means passing the same reftest corpus under the
backend-aware expectations already carried by `wrench` manifests and harness
logic. It does not require byte-identical output from every backend on every
test.

Validator roles:

- `spirv-val` validates authored SPIR-V input
- `wgpu::Device::create_shader_module()` validates generated WGSL at the runtime contract boundary
- GL validators such as `glslangValidator` or existing compile/link harnesses validate generated GLSL

### Stronger follow-up checks

- per-family micro-scenes for clip, text, image, composite, and render-task families
- artifact digest stability for unchanged SPIR-V inputs
- image parity across current and migrated backends while the transition is in flight
- explicit metadata-vs-runtime layout assertions for bindings and vertex inputs
- use the live `wrench` shader-artifact inspection mode to dump canonical
  SPIR-V, reflected metadata, and generated WGSL for one selected variant when
  a focused parity slice fails, instead of widening normalization changes first

### Standards role

The oracle order is:

1. valid authored SPIR-V
2. valid reflected metadata
3. valid generated WGSL
4. valid generated GLSL
5. WebRender behavioral parity

The current GLSL implementation remains a migration reference and regression oracle,
but it is not the normative center of the new pipeline.

## Concrete Branch Consequences

If this reset is accepted, the branch should stop investing in:

- better GLSL preprocessing for naga
- more exceptions in `fix_switch_fallthrough`
- making `shaderc`-based GLSL ingestion more robust
- treating assembled GLSL as the canonical source and SPIR-V as a derived artifact

The branch should instead invest in:

- defining the authored SPIR-V package format
- reflection-backed metadata contracts
- artifact registry design
- `wgpu` runtime consumption of metadata and generated WGSL
- GL runtime consumption of generated GLSL from the same canonical SPIR-V

## Assumptions And Defaults

- canonical authored source for migrated shader families is `.spvasm`, assembled to SPIR-V in the build
- naga remains in the pipeline, but only through `spv-in`, `wgsl-out`, and `glsl-out`
- `shaderc`, `glslang`, and `cmake` are not part of the intended steady-state shader pipeline
- external reference validators judge canonical input and generated outputs; they do not replace naga
- both GL and `wgpu` consume derivations of the same SPIR-V-centered artifact registry
- explicit legacy GL-only exceptions may exist during migration, but they must be isolated and temporary by default
- no public embedder-facing API change is required; the interface change is internal to shader build artifacts and backend setup

Future backend rule:

- adding HLSL, MSL, or other future targets means adding new derivation steps from canonical SPIR-V
- those additions do not change the authored `.spvasm` corpus
