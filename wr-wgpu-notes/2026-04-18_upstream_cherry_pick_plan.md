# 2026-04-18 Upstream Cherry-Pick Plan

This note captures the recommended cherry-pick plan from `upstream/upstream`
onto `wgpu-backend-0.68-experimental`, using `upstream/0.68` as the baseline.

The goal is not to merge `upstream/upstream` wholesale. The goal is to pull in
upstream changes that are relevant to Servo integration, compositor correctness,
render-task stability, external image handling, and test/tooling coverage.

## Baseline

- Tracking branch: `wgpu-backend-0.68-experimental`
- Reference release branch: `upstream/0.68`
- Candidate source branch: `upstream/upstream`

Observations:

- `wgpu-backend-0.68-experimental` is `0` behind and `131` ahead of
  `upstream/0.68`.
- `upstream/upstream` is `403` ahead and `5` behind `upstream/0.68`.
- The branch should be judged against `upstream/0.68`, not `upstream/main`.

## Working Method

Use short-lived integration branches and land changes in batches.

Recommended pattern:

```powershell
git switch wgpu-backend-0.68-experimental
git switch -c cp/watchlist-batch-1
git cherry-pick -x <commit>
```

Notes:

- Use `-x` so the original commit SHA is preserved in the new commit message.
- Do not combine large batches before checking build/test impact.
- Structural refactors and experimentation should each get their own branch.

## Batch 1: Highest-Confidence Picks

These are the first picks to attempt. They are the best mix of relevance,
contained scope, and likely value for Servo-facing correctness.

1. `2553cc8dc` Fix regression with shared surfaces in render task graph
2. `56715ae36` Fix complex nested render task sub-graphs with filter chains
3. `0ec6773dc` Fix dirty_rect empty check to account for device_size
4. `721b288c8` Clip dirty_rect to device_size in `Renderer::composite_simple()`
5. `3a827c80e` Fall back to CPU buffer when PBO mapping fails
6. `a9a8452a3` Ensure shader source strings are null terminated on Mali-G57
7. `fce14f6f6` Null terminate shader source strings on Adreno 750 GPUs
8. `a1c3f0074` Avoid compiling default composite shader variant when not required

Suggested commands:

```powershell
git switch wgpu-backend-0.68-experimental
git switch -c cp/watchlist-batch-1

git cherry-pick -x 2553cc8dc
git cherry-pick -x 56715ae36
git cherry-pick -x 0ec6773dc
git cherry-pick -x 721b288c8
git cherry-pick -x 3a827c80e
git cherry-pick -x a9a8452a3
git cherry-pick -x fce14f6f6
git cherry-pick -x a1c3f0074
```

Primary conflict hotspots:

- `webrender/src/render_task_graph.rs`
- `webrender/src/surface.rs`
- `webrender/src/renderer/composite.rs`
- `webrender/src/renderer/upload.rs`
- `webrender/src/device/gl.rs`
- `webrender/src/renderer/mod.rs`

Risk:

- Conceptual risk: low
- Mechanical conflict risk: medium to high

## Batch 2: Wrench And Test Infrastructure

These improve the ability to validate invalidation, snapping, and reftest
behavior. They are strongly relevant to Servo integration work, even though
they are not renderer features by themselves.

1. `550c4bec0` Improve wrench reftest manifest parsing
2. `f39a3ffba` Add basic support for file based invalidation tests in wrench
3. `b5dda058e` Add support for testing no invalidation / raster in reftests
4. `e489b5906` Add infrastructure for testing snapping behavior in wrench
5. `98efa4522` Add wrench raw tests support for fractional APZ scrolls
6. `9dad9f4c8` Update wrench to glutin `0.32` / winit `0.30`

Suggested commands:

```powershell
git switch wgpu-backend-0.68-experimental
git switch -c cp/watchlist-batch-2

git cherry-pick -x 550c4bec0
git cherry-pick -x f39a3ffba
git cherry-pick -x b5dda058e
git cherry-pick -x e489b5906
git cherry-pick -x 98efa4522
git cherry-pick -x 9dad9f4c8
```

Notes:

- `9dad9f4c8` is the final reland to prefer over the earlier `a36990b89`.
- `9dad9f4c8` is large enough that it may be worth isolating in its own branch
  after the smaller wrench/test changes are landed.

Primary conflict hotspots:

- `wrench/src/main.rs`
- `wrench/src/reftest.rs`
- `wrench/src/rawtest.rs`
- `wrench/src/test_invalidation.rs`
- `webrender/src/renderer/mod.rs`
- `examples/*`
- `wr_glyph_rasterizer/*`

Risk:

- Small wrench/test picks: moderate
- `9dad9f4c8`: medium to high

## Batch 3: Structural Cleanup

These are not the first picks to land, but they would make future maintenance
and backporting easier by moving compositor and external-image code out of the
largest renderer module.

1. `90f95b0f6` Extract external image code from `renderer/mod.rs`
2. `67575755c` Extract most of the compositing code out of `renderer/mod.rs`

Suggested commands:

```powershell
git switch wgpu-backend-0.68-experimental
git switch -c cp/watchlist-structure

git cherry-pick -x 90f95b0f6
git cherry-pick -x 67575755c
```

Primary conflict hotspots:

- `webrender/src/renderer/mod.rs`
- `webrender/src/renderer/composite.rs`
- `webrender/src/renderer/external_image.rs`

Risk:

- Conceptual risk: low to medium
- Mechanical conflict risk: high

## Batch 4: Compositor Clip Improvements

These look relevant to Servo because they improve compositor clip behavior,
rounded-rect handling, and picture-cache/compositor interaction.

1. `c55b499f2` Allow stacking contexts with group clips to be promoted to compositor clips
2. `406c97318` Support combining multiple rounded-rect clips on picture cache slices
3. `2ee649741` Add overlay rounded clip rect handling to the layer compositor
4. `fc33e4269` Drop overlapped rounded corner in `composite_simple()`
5. `47d7281f8` Use overlay with video rendering with rounded rects if possible

Suggested commands:

```powershell
git switch wgpu-backend-0.68-experimental
git switch -c cp/watchlist-compositor-clips

git cherry-pick -x c55b499f2
git cherry-pick -x 406c97318
git cherry-pick -x 2ee649741
git cherry-pick -x fc33e4269
git cherry-pick -x 47d7281f8
```

Primary conflict hotspots:

- `webrender/src/clip.rs`
- `webrender/src/composite.rs`
- `webrender/src/renderer/mod.rs`
- `webrender/src/tile_cache/mod.rs`

Risk:

- Medium to high

## Batch 5: Scroll And Invalidation Project

This is the most strategic batch rather than the easiest batch. It should be
treated as a focused project, not a routine maintenance pass.

1. `514024da5` Ensure nested sticky-frame scroll offsets are snapped to device pixels
2. `9dba98d31` Ensure fractional external scroll offsets do not affect local snapping
3. `2ccee2682` Fix another scrolling jitter issue with fractional scrolling enabled
4. `7e0341fdb` Change invalidation to be based on raster space

Suggested commands:

```powershell
git switch wgpu-backend-0.68-experimental
git switch -c cp/watchlist-scroll-invalidation

git cherry-pick -x 514024da5
git cherry-pick -x 9dba98d31
git cherry-pick -x 2ccee2682
git cherry-pick -x 7e0341fdb
```

Notes:

- `7e0341fdb` is the relanded raster-space invalidation change to prefer over
  the earlier `29ca2225d`.
- `2ccee2682` partially undoes the effect of `9dba98d31`, so this set should
  be evaluated together.

Primary conflict hotspots:

- `webrender/src/spatial_node.rs`
- `webrender/src/spatial_tree.rs`
- `webrender/src/util.rs`
- `webrender/src/invalidation/*`
- `webrender/src/tile_cache/mod.rs`

Risk:

- High

## Batch 6: Experimental Quad Work

This batch should go to an experiment branch. It may be valuable, but it also
has clearer upstream regression history than the earlier batches.

1. `86da54b23` Port box-shadows to use quad rendering
2. `f86bc4b26` Fix missing update of the device-pixel-scale in `QuadTransformState`
3. `6587c0f2f` Enable precise linear gradients (GPU-only)
4. `57960924d` Use the textured quad shader for image primitives in simple cases
5. `40d19f688` Disable quad image path if the image experiment proves unstable

Suggested commands:

```powershell
git switch wgpu-backend-0.68-experimental
git switch -c cp/watchlist-quad-experiments

git cherry-pick -x 86da54b23
git cherry-pick -x f86bc4b26
git cherry-pick -x 6587c0f2f
git cherry-pick -x 57960924d
```

If image quads become unstable:

```powershell
git cherry-pick -x 40d19f688
```

Notes:

- Do not default to cherry-picking `4d906cd15` yet. That enable-by-default
  quad box-shadow change was reverted upstream.

Primary conflict hotspots:

- `webrender/src/prepare.rs`
- `webrender/src/prim_store/image.rs`
- `webrender/src/renderer/init.rs`
- `webrender/src/renderer/mod.rs`
- `webrender/src/quad.rs`
- `webrender/src/box_shadow.rs`

Risk:

- Medium to high
- Functional regression risk is materially higher than in Batch 1

## Known Hotspot Files

The following files already have significant divergence on
`wgpu-backend-0.68-experimental`, so anything that touches them should be
expected to produce conflicts.

- `webrender/src/renderer/mod.rs`
- `webrender/src/device/gl.rs`
- `webrender/src/renderer/init.rs`
- `webrender/src/renderer/upload.rs`
- `webrender/src/render_task_graph.rs`
- `webrender/src/clip.rs`
- `webrender/src/spatial_tree.rs`
- `webrender/src/surface.rs`
- `wrench/src/main.rs`

## Recommended Landing Order

If the goal is maximum value with minimum chaos, use this order:

1. Land Batch 1.
2. Land Batch 2, excluding `9dad9f4c8`.
3. Land `9dad9f4c8` on a dedicated modernization branch.
4. Land `90f95b0f6`.
5. Decide between Batch 4 and Batch 5 based on current Servo integration pain.
6. Keep Batch 6 isolated as an experiment.

## Commits To Avoid Cherry-Picking Blindly

- `4d906cd15` because upstream later reverted the default-enable path
- Earlier relands/superseded versions when a later final commit exists
- Large reverted upstream experiments unless there is a clear reason to carry
  that branch-specific risk

## Why This Matters To Servo

The most Servo-relevant upstream gains appear to be:

- render-task and shared-surface correctness
- compositor dirty-rect correctness
- stronger invalidation and snapping tests in `wrench`
- more maintainable separation of compositor and external-image code
- optional future gains in quad-based rendering, but behind a more careful gate

This plan should be treated as a practical watchlist for targeted upstream
adoption, not as a request to converge fully with `upstream/upstream`.
