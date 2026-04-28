# Tile-with-Spacing wgpu Validation Error — 2026-04-24

## Status

**Fixed 2026-04-25** in commit `68755a60f` (`Fix tile-with-spacing wgpu validation
error: align color formats`). Full reftest sweep confirms 464 passing / 1 failing
with no `Rgba8Unorm → Bgra8Unorm` validation error line. See Root Cause and Fix
sections below.

## Summary

A single wgpu validation error fired once during a full `reftest reftests`
sweep on `--wgpu-hal-headless`. The signature was:

```
[ERROR webrender::device::wgpu_device] wgpu flush_encoder validation error: Validation Error

Caused by:
  In a CommandEncoder, label = 'frame encoder'
    Source format (Rgba8Unorm) and destination format (Bgra8Unorm) are not copy-compatible (they may only differ in srgb-ness)
```

## Trigger

`wrench/reftests/image/tile-with-spacing.yaml`. Reproduced reliably in
isolation:

```bash
cargo run -p wrench --features wgpu_backend -- \
  --wgpu-hal-headless reftest reftests/image/tile-with-spacing.yaml
```

The reftest reported `REFTEST INFO | 1 passing, 0 failing` even with the error
present because the invalid copy was silently dropped by the wgpu validation
layer and both the test and reference scenes were equally affected.

## Scope

Pre-existing on branch tip `3e112b83a` (`Bug 2018549 - Improve wrench reftest
manifest parsing`), before the 2026-04-24 audit commits.

## Root Cause

Three surfaces used in `copy_texture_to_texture` paths had inconsistent color
formats on the wgpu path:

- **Shared texture atlas** (`color8_linear`): `Bgra8Unorm` — set by
  `color_cache_formats = BGRA8` in `renderer/init.rs`.
- **Dynamic color render targets**: `Rgba8Unorm` — hardcoded as
  `ImageFormat::RGBA8` in `render_task_graph.rs:463`.
- **Picture cache tiles**: `Rgba8Unorm` — hardcoded as `ImageFormat::RGBA8`
  in `picture_textures.rs:192`.

The tile-with-spacing code path in `prim_store/image.rs` creates a `Blit`
render task (via `RenderTask::new_blit`) that copies from a dynamic
`Rgba8Unorm` render target into the `Bgra8Unorm` shared atlas slot. wgpu's
`copy_texture_to_texture` rejects this as format-incompatible. On GL the same
logical operation succeeded silently because GL treats RGBA8 and BGRA8 as the
same internal format.

The first fix attempt (changing dynamic render targets to `Bgra8Unorm` via
`color_render_target_format()`) caused 22 regressions because resolve ops
were then copying from `Rgba8Unorm` picture tiles into `Bgra8Unorm` dynamic
resolve targets — the same class of mismatch, just moved.

## Fix

All three surfaces are now derived from the same source:

1. `resource_cache.rs`: added `color_render_target_format()` returning
   `self.texture_cache.shared_color_expected_format()`.
2. `render_task_graph.rs:463`: replaced hardcoded `ImageFormat::RGBA8` with
   `resource_cache.color_render_target_format()`.
3. `picture_textures.rs`: added `color_format: ImageFormat` field/parameter
   so tiles use the format passed at construction.
4. `renderer/init.rs`: both construction sites (GL and wgpu paths) now pass
   `color_cache_formats.internal` to `PictureTextures::new(...)`.

On wgpu all three surfaces are `Bgra8Unorm` → every `copy_texture_to_texture`
call is format-compatible. On GL all three surfaces follow
`preferred_color_formats().internal` as before (GL's permissive blit behavior
is unchanged). Unit tests pass because `new_for_testing` uses `RGBA8` for all
three consistently.
