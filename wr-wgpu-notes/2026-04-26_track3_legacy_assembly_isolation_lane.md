# Track 3 — Runtime Legacy Source Assembly Isolation Lane

This note captures a concrete slice ladder for Phase 5 closure Track 3 of
`2026-04-18_spirv_shader_pipeline_plan.md`: shrink the runtime legacy
GLSL prefix/main-string assembly helpers until they are reachable only from
the explicit built-in legacy roots (`gpu_cache_update` and its `base` include
closure) and from the explicit shader-override path. Migrated artifact-backed
shaders already bypass these helpers — the work here is to *prove* that
boundary in code rather than just by convention, then trim the public API
surface accordingly.

This is not a new strategy. It is a working punch list. Execute slices in
order; revalidate against the named gates from the parent plan after each.

## Helpers in scope

| Function | File | What it does |
| --- | --- | --- |
| `do_build_shader_string` | [webrender_build/src/shader.rs:117](../webrender_build/src/shader.rs#L117) | Orchestrates prefix + main string concatenation. Public entry point for legacy GLSL assembly. |
| `build_shader_prefix_string` | [webrender_build/src/shader.rs:131](../webrender_build/src/shader.rs#L131) | Emits version directive, feature defines, shader-kind define, platform preamble. |
| `build_shader_main_string` | [webrender_build/src/shader.rs:197](../webrender_build/src/shader.rs#L197) | Resolves main file and `#include` imports via `ShaderSourceParser`. |
| `get_unoptimized_shader_source` | [webrender/src/device/gl.rs:130](../webrender/src/device/gl.rs#L130) | Routes between built-in `UNOPTIMIZED_SHADERS` map (legacy roots) and the override directory. |
| `Device::build_shader_string` | [webrender/src/device/gl.rs:3406](../webrender/src/device/gl.rs#L3406) | Thin device-method wrapper around `do_build_shader_string`; resolves source via `get_unoptimized_shader_source`. |

## Caller categorisation

- **legacy-root**: `ProgramSourceInfo::new` Unoptimized hash path
  ([gl.rs:656–692](../webrender/src/device/gl.rs#L656)) and
  `ProgramSourceInfo::compute_source` ([gl.rs:714](../webrender/src/device/gl.rs#L714)).
- **resource-override**: same `ProgramSourceInfo::new` block when
  `override_path.is_some()` ([gl.rs:666–674](../webrender/src/device/gl.rs#L666)).
- **artifact-backed**: `select_program_source_type`
  ([gl.rs:749–762](../webrender/src/device/gl.rs#L749)) routes migrated shaders
  to `ProgramSourceType::Artifact`; `find_artifact_source`
  ([gl.rs:764–793](../webrender/src/device/gl.rs#L764)) extracts pre-generated
  GLSL from `SHADER_ARTIFACTS`. These callers do not touch the helpers above.
- **build-time**: `webrender/build.rs:write_legacy_unoptimized_shaders`
  ([build.rs:151–201](../webrender/build.rs#L151)) populates
  `UNOPTIMIZED_SHADERS` at compile time using `ShaderSourceParser`. The
  `LEGACY_UNOPTIMIZED_SHADER_ROOTS` constant
  ([build.rs:18](../webrender/build.rs#L18)) is currently `["gpu_cache_update"]`.
- **dead-but-not-removed**: none detected; all five helpers have live callers.

## Slice ladder

Each slice should be one commit. The validation gates named below are the
ones the parent plan already runs; do not invent new tests.

### Slice 1 — Document the legacy boundary in code

Add `// LEGACY GL EXCEPTION PATH` markers above `do_build_shader_string`,
`build_shader_prefix_string`, `build_shader_main_string` in
[webrender_build/src/shader.rs](../webrender_build/src/shader.rs), and a
matching note above `LEGACY_UNOPTIMIZED_SHADER_ROOTS` in
[build.rs](../webrender/build.rs#L18) recording that this constant defines
the *only* shaders for which built-in source assembly survives.

- Files: `webrender_build/src/shader.rs`, `webrender/build.rs`.
- Validation: `cargo test -p webrender --no-default-features --features gl_backend built_in_unoptimized_sources_exclude_artifact_shaders -- --nocapture`.
- Risk: documentation only. Zero functional risk.

### Slice 2 — Add a barrier test for the legacy/artifact split

New test in `webrender/src/device/gl.rs` (or a sibling `tests/` file) that
walks `SHADER_ARTIFACTS` and asserts `select_program_source_type` returns
`ProgramSourceType::Artifact` for every migrated variant when
`override_path` is `None`. Also assert that the only names for which the
Unoptimized path is reachable are the entries in
`LEGACY_UNOPTIMIZED_SHADER_ROOTS` (currently `gpu_cache_update`) plus its
`#include` closure (`base`).

- Files: `webrender/src/device/gl.rs` (test module).
- Validation: the new test, plus
  `cargo test -p webrender --no-default-features --features gl_backend built_in_unoptimized_sources_exclude_artifact_shaders -- --nocapture`.
- Risk: low. Test-only; codifies the invariant the rest of the lane relies on.
- Why second: subsequent slices delete code on the assumption migrated
  shaders never reach the Unoptimized path. The barrier test makes that
  assumption a build-time gate.

### Slice 3 — Consolidate override-vs-built-in routing in `ProgramSourceInfo::new`

`ProgramSourceInfo::new` currently calls `build_shader_main_string` twice
([gl.rs:666–692](../webrender/src/device/gl.rs#L666)) once per override branch.
Extract the override-vs-built-in decision into a single helper that returns a
source resolver and call `build_shader_main_string` once. No semantic change;
just deduplication.

- Files: `webrender/src/device/gl.rs`.
- Validation: `cargo test -p webrender --no-default-features --features gl_backend built_in_unoptimized_sources_exclude_artifact_shaders -- --nocapture`.
- Risk: low. Mechanical consolidation; preserves digest stability (see
  *Gotchas*).

### Slice 4 — Inline `Device::build_shader_string`

`Device::build_shader_string` ([gl.rs:3406](../webrender/src/device/gl.rs#L3406))
is a 15-line wrapper around `do_build_shader_string` with one caller
([gl.rs:714](../webrender/src/device/gl.rs#L714)). Inline it: have the caller
hold the source-resolver closure and invoke `do_build_shader_string`
directly. Delete the wrapper.

- Files: `webrender/src/device/gl.rs`, `webrender_build/src/shader.rs`
  (re-export `do_build_shader_string` if not already public).
- Validation: `cargo test -p webrender --no-default-features --features gl_backend built_in_unoptimized_sources_exclude_artifact_shaders -- --nocapture`.
- Risk: low. Mechanical extraction.

### Slice 5 — Inline prefix assembly into the digest hash path

`build_shader_prefix_string` is small and called from one site in the
Unoptimized hash path ([gl.rs:656](../webrender/src/device/gl.rs#L656)).
Inline its body into the hash path so the runtime no longer depends on the
helper symbol. The function may remain in `webrender_build` for build-time
reuse but loses its only `webrender` runtime caller.

- Files: `webrender/src/device/gl.rs`.
- Validation: `cargo test -p webrender --no-default-features --features gl_backend built_in_unoptimized_sources_exclude_artifact_shaders -- --nocapture`,
  plus a digest-stability check (see *Gotchas*).
- Risk: medium. Hash output must be byte-identical to the pre-inlining
  output, otherwise the program cache key changes and shaders recompile
  cold on first run after upgrade.

### Slice 6 — Make `build_shader_main_string` private

After Slices 3–5, all `webrender`-side callers reach
`build_shader_main_string` indirectly via `do_build_shader_string`. Demote
the function to `pub(crate)` (or fully private) in
`webrender_build/src/shader.rs`. The compiler is now the gate: any remaining
runtime caller will fail the build.

- Files: `webrender_build/src/shader.rs`, `webrender_build/src/lib.rs`
  (drop re-exports if any), and any caller that still needs visibility
  through `do_build_shader_string`.
- Validation: `cargo check -p webrender --no-default-features --features gl_backend`,
  `cargo test -p webrender_build --lib`,
  `cargo test -p webrender --no-default-features --features gl_backend built_in_unoptimized_sources_exclude_artifact_shaders -- --nocapture`.
- Risk: low after Slices 3–5. Will fail to compile if any runtime callsite
  was missed; that failure *is* the proof the surface area shrank.

### Slice 7 (optional) — Rename `shader_source_from_file` to mark legacy I/O

`shader_source_from_file` and friends in
[webrender_build/src/shader.rs](../webrender_build/src/shader.rs) are
filesystem readers used only by build-time legacy assembly and runtime
overrides. Prefix with `legacy_` (or move into a `legacy` module) so the
name itself signals the boundary. Pure rename; no functional change.

- Files: `webrender_build/src/shader.rs`, callers in
  `webrender/src/device/gl.rs` and `webrender/build.rs`.
- Validation: same as Slice 6.
- Risk: low. Optional — defer if Slices 1–6 already satisfy the closure.

## Gotchas

- **Digest stability is the load-bearing invariant.** `ProgramSourceInfo::new`
  hashes prefix → main → version/digest in a specific order. Any
  consolidation in Slice 3 or inlining in Slice 5 must preserve hash output
  byte-for-byte, or the program cache key changes and every user pays a
  cold-cache shader compile on first run after the upgrade. Add an explicit
  before/after digest comparison test for `gpu_cache_update` (the surviving
  legacy root) when running Slices 3 and 5.
- **`ShaderSourceParser` has two callers, not one.** Build-time
  (`write_legacy_unoptimized_shaders` in
  [build.rs](../webrender/build.rs#L151)) and runtime
  (`build_shader_main_string`). Do not delete the parser when the runtime
  caller goes; the build-time caller is still load-bearing for populating
  `UNOPTIMIZED_SHADERS`.
- **Override-path precedence.** `ProgramSourceInfo::new` checks
  `override_path.is_some()` *before* consulting `UNOPTIMIZED_SHADERS`.
  Slice 3 must preserve that ordering or the `--shaders` override flow
  silently breaks.
- **`MAX_VERTEX_TEXTURE_WIDTH_STRING`** ([shader.rs:20](../webrender_build/src/shader.rs#L20))
  is a `lazy_static!` consumed by `build_shader_prefix_string`. Slice 5
  inlines its only runtime caller; the `lazy_static` itself is still used
  by build-time prefix assembly via `do_build_shader_string`. Do not delete
  it as part of Slice 5.
- **No remaining migrated consumers must hit the Unoptimized path.** Slice 2
  encodes this assumption as a test, but it is also the operational
  precondition for Slices 4–6: if any migrated shader (`brush_solid`,
  `brush_yuv_image`, etc.) still falls through to Unoptimized in some
  override-path-absent configuration, those slices delete code that is
  silently still load-bearing. Run the spirv-parity lane (the wgpu-hal
  headless variant from the parent plan's matrix) before merging Slice 6.

## Validation gates reused from the parent plan

These are the existing names from `2026-04-18_spirv_shader_pipeline_plan.md`.
Use them verbatim across the lane:

- `cargo test -p webrender --no-default-features --features gl_backend built_in_unoptimized_sources_exclude_artifact_shaders -- --nocapture`
- `cargo test -p webrender --features wgpu_native shader_artifacts_load_as_wgpu_modules -- --nocapture`
- `cargo test -p webrender --features wgpu_native remaining_runtime_layout_variants_match_metadata_contracts -- --nocapture`
- `cargo test -p webrender_build --features glsl-oracle validate_generated_output_derivations -- --nocapture`
- `cargo run --features wgpu_backend -- --wgpu-hal-headless reftest reftests/spirv-parity` (from `wrench/`)
- `cargo run --features gl_backend --no-default-features -- --gl-hidden reftest reftests/spirv-parity` (from `wrench/`)

When this lane completes, the parent plan's "Track 3 — Runtime legacy source
assembly isolation" line in the *Phase 5 closure tracks* section can be
flipped from "partially complete" to "complete."
