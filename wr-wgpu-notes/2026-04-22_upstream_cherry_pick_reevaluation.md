# 2026-04-22 Upstream Cherry-Pick Re-Evaluation

This note re-evaluates the 2026-04-18 cherry-pick watchlist against the current
state of the local fork and the current state of `upstream/upstream` in
`servo/webrender`.

It is not a new broad integration plan. It is a current-state check intended to
answer a narrower question: what, if anything, should be cherry-picked while the
SPIR-V migration is actively recentering the shader pipeline around authored
SPIR-V and artifact-backed runtime consumption.

## Current Branch Topology

Local remotes still match the expected fork layout:

- `origin = https://github.com/mark-ik/webrender-wgpu.git`
- `upstream = https://github.com/servo/webrender.git`

Relevant branch divergence at the time of this note:

- `upstream/upstream` remains `403` commits ahead and `5` commits behind
  `upstream/0.68`
- `spirv-shader-pipeline` is `140` commits ahead and `0` commits behind
  `upstream/0.68`
- `spirv-shader-pipeline` is `145` commits behind and `0` commits ahead of
  `upstream/upstream` on the shared-history side of the comparison

Practical reading:

- the source branch we planned to cherry-pick from has not materially changed
  since the 2026-04-18 note
- the local branch has moved since then
- the re-evaluation therefore needs to focus on which upstream commits are still
  absent and worth taking during the current migration phase

## Status Of The Original Watchlist

Checked directly against the current local branch and `git cherry` patch
equivalence.

Result:

- none of the original Batch 1 commits are present on `spirv-shader-pipeline`
- none of the original Batch 2 commits are present on `spirv-shader-pipeline`
- none of the original Batch 3 commits are present on `spirv-shader-pipeline`
- all of those commits still exist on `upstream/upstream`
- none of them show up as patch-equivalent changes already absorbed by the local
  branch

That means the old watchlist is still mechanically relevant. The real question is
timing and scope, not whether the candidate commits disappeared.

2026-04-24 recheck before taking any additional upstream picks:

- direct ancestry checks on `spirv-shader-pipeline` still show the branch lacks
  `550c4bec0`, `e489b5906`, `98efa4522`, `f39a3ffba`, `514024da5`, and
  `f2da5f726`; the earlier post-note text claiming those commits were already
  present was stale relative to the current branch state
- treat that stale prose as a note-maintenance issue, not evidence that the
  branch absorbed those upstream changes by patch equivalence
- that ancestry result is not the whole screening result for every candidate:
  on 2026-04-24, `550c4bec0` was still a real missing wrench/parser fix and was
  landed locally, while `e489b5906` turned out to be semantically redundant
  because this branch already carries a richer local snapping rawtest harness in
  `wrench/src/rawtests/snapping.rs`
- the same caveat now also applies to the next two wrench-local candidates:
  `98efa4522` is absent by SHA but its fractional APZ and external-scroll
  snapping coverage is already represented by the current `SCROLL_VARIANTS` and
  `EXTERNAL_SCROLL_VARIANTS` cases in `wrench/src/rawtests/snapping.rs`, and
  `f39a3ffba` is absent by SHA but its manifest-driven invalidation harness is
  already represented by `wrench/invalidation/invalidation.list`,
  `parse_manifest(...)`, `run_list_tests(...)`, and the non-zero exit path in
  `wrench/src/test_invalidation.rs` / `wrench/src/main.rs`
- `9dba98d31` and `2ccee2682` remain deferred: `9dba98d31` does not apply
  cleanly, `2ccee2682` is explicitly a partial undo of it, and the commit body
  points at a separate root-cause fix that is not present in this branch history
- `c01d148dd` remains deferred because its `push_stacking_context` origin
  assertion assumes a broader caller-surface migration than this branch has;
  local example/helper code still uses non-zero stacking-context origins
- `dfd32c78c` remains out because upstream reverted it for reftest failures
- `a5cf9c342` is already represented by the current `spatial_tree.rs`
  snapping logic and is not worth a duplicate port
- `59e2b83c3` is also already represented semantically by
  `normalize_scroll_offset_and_snap_rect(...)`; only its wrench reftest
  coverage was imported locally

## Recommendation

Do not pause the SPIR-V migration to land the old cherry-pick batches wholesale.

The current migration work is recentering the control point in
`compiled_artifacts.rs` and related runtime consumers. Large upstream cherry-pick
batches would mix renderer behavior changes, wrench harness changes, platform
windowing churn, and structural refactors into the same phase. That would make it
harder to interpret parity failures during the migration.

### Take now only if it directly strengthens the migration gate

If we need a small pre-migration cherry-pick batch before the first SPIR-V slice
lands, keep it limited to the commits that most directly improve the wrench-based
parity gate:

1. `550c4bec0` Improve wrench reftest manifest parsing

Do not queue `e489b5906` as the next literal cherry-pick on this branch: the
branch already has a stronger snapping harness than the upstream add, so the
literal cherry-pick only creates add/add conflicts in `wrench/src/rawtests`.

After `550c4bec0`, stop unless the active failure lane specifically needs more
snapping or invalidation coverage.

Conditional adds only if they become directly relevant to the parity slice we are
using and are not already semantically represented on the current branch:

1. `b5dda058e` Add support for testing no invalidation / raster in reftests

These are the only upstream picks that currently have a strong argument for
landing before the shader-pipeline control point is reset.

Probe result for the conditional adds:

- `98efa4522` is no longer a useful literal cherry-pick target on this branch:
  the upstream payload expands the snapping rawtest from simple fractional cases
  to APZ and external-scroll coverage, but the current branch already carries
  that broader coverage directly in `wrench/src/rawtests/snapping.rs`
- `f39a3ffba` is also no longer a useful literal cherry-pick target on this
  branch: the upstream payload adds `wrench/invalidation/invalidation.list`,
  manifest parsing, `run_list_tests(...)`, and a failure-count exit path, and
  those pieces are already present in the current `test_invalidation` harness
- `b5dda058e` is different from the other two: the upstream diff is small, but
  on `spirv-shader-pipeline` it runs into code motion in `webrender/src/picture.rs`
  and the deletion of `webrender/src/renderer/composite.rs`
- the logical payload of `b5dda058e` is still small, but the manual port is no
  longer cheap on this branch because the conflict expands into a large
  `picture.rs` rewrite rather than a narrow local merge
- current recommendation: do not queue `98efa4522` or `f39a3ffba` as the next
  literal upstream picks on this branch; keep `b5dda058e` conditional on a
  concrete need for no-raster assertions rather than folding it into the same
  low-risk mini-batch
- 2026-04-24 follow-up: that concrete need did materialize, but the cheapest
  implementation was not a literal cherry-pick of `b5dda058e`; instead, the
  branch now carries a smaller local `wrench/src/reftest.rs` port that adds a
  `no_raster()` extra-check on top of the already-present
  `RenderResults.did_rasterize_any_tile` plumbing

Probe result on `spirv-shader-pipeline`:

- `550c4bec0` does not apply cleanly as-is, but the conflict is narrow and local
  to `wrench/src/parse_function.rs` and `wrench/src/reftest.rs`
- the needed manual rebase for `550c4bec0` is small: trim parsed arguments in
  `parse_function(...)` and route `fuzzy-if(...)` / `fuzzy-range-if(...)`
  condition failures through the improved manifest-context error path
- `550c4bec0` was then landed locally with that small manual merge
- `e489b5906` is not a good literal cherry-pick target anymore: the branch
  already contains `mod rawtests`, `RawtestHarness::test_snapping()`, and a
  larger `wrench/src/rawtests/snapping.rs` surface that extends beyond the
  upstream two-test infrastructure, so the literal cherry-pick is redundant
  even though ancestry alone says the SHA is absent
- a focused `cargo check -p wrench --all-targets` probe is not a useful accept
  or reject signal right now because the same command already fails on a clean
  baseline worktree in unrelated `webrender/src/renderer/*` code on the current
  branch
- the narrowest useful accept/reject checks are a small parser unit-test pass
  for `wrench/src/parse_function.rs`, followed by a focused
  `cargo run -p wrench --features wgpu_backend -- --wgpu-hal-headless reftest`
  slice such as `reftests/spirv-parity/yuv-composite`

### Revisit after Slice 1 lands

Once the first authored-SPIR-V slice is stable and the parity gate is defined,
revisit the renderer-correctness fixes from the old Batch 1:

No remaining small Batch 1 candidate currently stands above the noise floor the
way `2553cc8dc`, `0ec6773dc`, `721b288c8`, and now `3a827c80e` did. Re-screen
from the then-current parity failures instead of carrying the old list forward
mechanically.

2026-04-24 broadened screen result for the dirty-rect pair:

- `0ec6773dc` and `721b288c8` still addressed real current code paths, but they
  are no longer literal cherry-picks on this branch because the affected logic
  lives in `webrender/src/renderer/mod.rs` instead of the deleted
  `webrender/src/renderer/composite.rs`
- both were landed locally as tiny ports in `renderer/mod.rs`: clip the layer
  compositor's combined dirty rect to `device_size` before storing
  `PartialPresentMode::Single`, and treat the post-clip rect as the emptiness
  gate before binding/presenting the layer surface

2026-04-24 broadened screen result for the next render-task-graph pair:

- `2553cc8dc` was confirmed relevant on this branch and then landed locally as a
  small manual port in `webrender/src/render_task_graph.rs`. The branch now uses
  the upstream `lifetime_group` / `pending_frees` model plus per-pass
  `freed_tasks` deduplication instead of freeing shared surfaces directly from a
  single `Surface.free_after` / `child_task.free_after` pairing.
- focused validation for that port passed via the local render-task-graph unit
  tests (`cargo test -p webrender fg_test_ --lib -- --nocapture`).
- `56715ae36` is no longer a useful independent cherry-pick target. Its
  filter-chain dependency payload is already represented in the current
  `webrender/src/surface.rs` sub-graph finalization logic, including the extra
  dependency wiring for filter-chain outputs, while its remaining
  `render_task_graph.rs` lifetime adjustments overlap with and are effectively
  superseded by the stronger allocator-lifetime model in `2553cc8dc`.
- current recommendation: do not queue `56715ae36` separately; treat this area
  as covered by the local `2553cc8dc` port unless a narrower missing sub-case is
  identified.

2026-04-24 focused wrench follow-up:

- the branch is now able to execute the new `no_raster()` harness path on the
  wgpu headless lane for focused slices.
- `cargo run -p wrench --features wgpu_backend -- --wgpu-hal-headless reftest
  reftests/transforms/raster-root-scaling-2.yaml` completed with
  `REFTEST INFO | 2 passing, 0 failing`, covering both the original equality
  case and the new repeated-render `no_raster()` manifest line.
- `cargo run -p wrench --features wgpu_backend -- --wgpu-hal-headless reftest
  reftests/blend/raster-roots-1.yaml` also completed with
  `REFTEST INFO | 2 passing, 0 failing`, giving a second `no_raster()` proof in
  a different reftest lane.
- `cargo run -p wrench --features wgpu_backend -- --wgpu-hal-headless reftest
  reftests/clip/raster-roots-tiled-mask.yaml` also completed with
  `REFTEST INFO | 2 passing, 0 failing`, adding a third `no_raster()` proof in
  the clip lane.

2026-04-24 fresh failure-driven conic screen:

- a fresh focused rerun of
  `reftests/spirv-parity/radial-conic-gradient-micro` restored the lane's
  current narrow failure surface: `gradient/conic-large.yaml` remains the only
  failing case while `radial-optimized`, `radial-tiling-optimized`, and
  `tiling-conic-2` pass.
- the first obvious historical candidate surfaced by the exact failing asset
  history was `38d976be3` / Bug 1706678 (`Fix cached gradient scaling`), but
  that payload is already represented semantically on this branch: the current
  `webrender/src/prim_store/gradient/conic.rs` already uses the corrected
  `task_size.width / max_size` and `task_size.height / max_size` scale factors.
- the next plausible narrow candidate was `6ab18fa76` / Bug 2013919
  (`Express quad gradient coordinates relative to the primitive's spatial
  node`). It is absent by ancestry, but a minimal conic-only local probe in
  `prim_store/gradient/conic.rs` immediately made the focused lane worse by
  regressing `tiling-conic-2` alongside the existing `conic-large` failure.
- current recommendation: do not queue `6ab18fa76` as a standalone conic pick
  for the active SPIR-V parity lane. It is not a high-confidence narrow fix for
  the remaining `conic-large` mismatch on this branch.
- follow-up local probing against the same focused lane showed the remaining
  `conic-large` mismatch is not solved by a single upstream-style cherry-pick,
  but it is also not a conic-specific opacity or oversized-quad cutoff problem.
  The root cause on this branch is the WGPU picture-cache opaque batching path:
  it recorded opaque batches with `WgpuDepthState::AlwaysPass` even though the
  path comments and the GL renderer require front-to-back opaque rendering with
  depth write/test. Switching that picture-cache path to
  `WgpuDepthState::WriteAndTest` makes the conic-specific workarounds
  unnecessary.
- with the WGPU picture-cache depth fix in place, both previous conic
  workarounds were removed: oversized conics no longer force
  `PrimitiveOpacity::translucent()` in
  `webrender/src/prim_store/gradient/conic.rs`, and
  `webrender/src/prepare.rs` no longer forces `stretch_size > 1024` conics off
  the quad path. The focused `reftests/spirv-parity/radial-conic-gradient-micro`
  slice reports `REFTEST INFO | 4 passing, 0 failing`, and the broader
  `reftests/gradient` directory reports `REFTEST INFO | 80 passing, 0 failing`.
- a later root-cause probe also rejected the most obvious semantic replacement:
  keeping oversized conics on the quad path but forcing them through the
  precise conic pattern in `prim_store/gradient/conic.rs` still failed the same
  focused slice, so the current bug is still in the oversized quad-conic path
  rather than in the batch override or the offline GLSL oracle.

2026-04-24 offline GLSL oracle follow-up:

- the original `webrender_build/src/glsl.rs` fix for `cs_svg_filter` and
  `cs_svg_filter_node` used shader-name / exact-line pruning of dead varyings.
- that is no longer the branch state: the oracle now does a structural,
  pair-level GLES cleanup that seeds liveness from fragment outputs, removes
  dead fragment varying copy chains, and prunes unmatched vertex `_vs2fs_*`
  outputs before ANGLE validation.
- this keeps the repair confined to the offline GLSL oracle / validation
  surface, which matches the current migration plan better than broadening
  runtime normalization or renderer behavior.

2026-04-24 upload-path follow-up:

- `3a827c80e` was confirmed relevant and landed locally as a tiny manual port in
  `webrender/src/renderer/upload.rs`.
- the current branch still had the same root issue as upstream: the batched
  `UploadMethod::PixelBuffer(_)` path called `uploader.stage(...).unwrap()`,
  which would panic instead of falling back when PBO staging failed.
- the local port now falls back to `StagingBufferKind::CpuBuffer` using
  `staging_texture_pool.get_temporary_buffer()` when `stage(...)` returns an
  error.
- executable validation for this change is limited on `spirv-shader-pipeline`
  because the relevant path is GL-only and `gl_backend` is intentionally guarded
  off on this branch, but the touched slice compiled cleanly via
  `cargo check -p webrender`.

### Defer during the current migration slice

Keep these out of the active migration unless a separate integration branch is
created for them:

1. `9dad9f4c8` wrench glutin/winit update
2. `90f95b0f6` extract external image code from `renderer/mod.rs`
3. `67575755c` extract most of the compositing code out of `renderer/mod.rs`

Reason:

- `9dad9f4c8` is still high-churn windowing and dependency work, not a shader
  migration prerequisite
- the structural extraction commits are still refactors, not migration gates
- all three would increase conflict surface in files that are already likely to
  move for other reasons

## Additional Upstream-Only Areas Worth Revisiting Later

Path inspection against `spirv-shader-pipeline..upstream/upstream` also shows
other upstream-only lines that were not the first focus of the 2026-04-18 note
but are worth keeping on the radar once the SPIR-V reset is stable:

- compositor clip and rounded-rect handling:
  - `c55b499f2`
  - `406c97318`
  - `2ee649741`
  - `fc33e4269`
- snapping and fractional-scroll correctness:
  - `514024da5`
  - `9dba98d31`
  - `2ccee2682`
- quad-path / gradient enablement lines and their follow-up toggles:
  - `4d906cd15`
  - `86da54b23`
  - `57960924d`
  - `40d19f688`
  - `6587c0f2f`

These should be triaged separately after the current migration milestone. They
are real upstream deltas, but they are not the right changes to mix into the
control-point reset unless the migration surfaces a specific need.

## Bottom Line

The source branch for cherry-picks is still the same branch the 2026-04-18 note
analyzed. The local branch still lacks every original Batch 1 through Batch 3
candidate. That does not imply we should stop the migration to take them all now.

The current best ordering is:

1. keep moving on the authored-SPIR-V reset
2. if needed, take only a tiny wrench-focused pre-batch that strengthens the
   parity gate
3. after Slice 1 is stable, reassess Batch 1 renderer fixes against the new
   parity baseline
4. keep large wrench platform updates, structural refactors, and quad-path
   enablement work on separate branches or later milestones

Maintenance action:

- whenever this note claims a candidate has landed, tie that statement to a
  date-stamped ancestry or `git cherry` check so the note does not become the
  source of its own branch-drag confusion
