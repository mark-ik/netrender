---
name: WebRender WASM portability checklist
description: Current blockers, target notes, and multithreading guidance for compiling the wgpu backend branch toward wasm32-unknown-unknown and wasm32-wasip2.
type: project
---

# WebRender WASM Portability Checklist

Local working note for the `wgpu-backend-0.68-minimal` branch.
This file is intentionally gitignored via `.git/info/exclude`.

## Purpose

This note answers a narrower question than the main backend plan:

- what still blocks WebRender from compiling and running on WASM targets?
- which blockers are branch-local versus ecosystem/runtime constraints?
- what should we assume about multithreading on browser WASM and WASI?

This is not a promise that WebRender is one small patch away from portable WASM.
It is a checklist for turning the current wgpu backend work into a realistic
WASM path.

## Short Answer

Today, the wgpu backend work makes a WASM direction plausible, but WebRender is
not yet ready to compile-and-run cleanly on:

- `wasm32-unknown-unknown`
- `wasm32-wasip2`

The main blockers are no longer shader translation. They are:

1. remaining GL-shaped renderer paths
2. native-thread assumptions throughout startup and scene/render backend orchestration
3. browser-async `wgpu` initialization versus current blocking constructors
4. default feature and dependency choices that assume native desktop/server environments

## Target Summary

### `wasm32-unknown-unknown`

Best fit for:

- browser tab
- browser extension
- hosted Graphshell / MedNet app

Main constraints:

- `std::thread::spawn` is not a normal supported runtime primitive here
- browser `wgpu` initialization is async
- real threaded WASM depends on shared memory / `SharedArrayBuffer`
- practical thread use on the web requires cross-origin isolation

Current planning assumption:

- this should be the primary portability bar
- if the branch cannot be made sensible here, it is not yet truly browser-portable

### `wasm32-wasip2`

Best fit for:

- component model hosts
- Wasmtime-style embedding
- server-side or runtime-hosted WASM deployments

Strengths:

- much better `std` story than `wasm32-unknown-unknown`

Current limitations:

- GPU/window/surface integration is less direct than the browser path
- the thread story is still not something to assume as production-stable

Current planning assumption:

- treat this as a useful secondary target, not the primary design center

## Blockers In The Current Tree

### 1. GL remains deeply embedded

Even with `GpuDevice`, `RendererBackend`, and the wgpu-only constructor work,
large parts of the crate still assume GL concepts and GL-owned machinery:

- `gleam::gl`
- `query_gl`
- FBO-owned draw/read target paths
- PBO upload pools
- VAO/VBO setup
- GL profiler/query plumbing
- shader behavior keyed off `GlType`

Representative files:

- `webrender/src/device/gl.rs`
- `webrender/src/device/query_gl.rs`
- `webrender/src/renderer/mod.rs`
- `webrender/src/renderer/shade.rs`
- `webrender/src/screen_capture.rs`

Implication:

- compiling the crate for WASM is not just a matter of enabling `wgpu_backend`
- more code must become backend-neutral or be explicitly compiled out for WASM

### 2. The runtime still assumes native threads

WebRender startup still spawns and coordinates multiple native threads:

- scene builder thread
- low-priority scene builder thread
- render backend thread
- Rayon worker pool
- glyph raster work

Representative files:

- `webrender/src/renderer/init.rs`
- `webrender/src/scene_builder_thread.rs`
- `webrender/src/resource_cache.rs`

Implication:

- even if the renderer path becomes fully wgpu-based, the current orchestration
  model does not map directly to browser WASM

### 3. Browser `wgpu` wants async initialization

The current wgpu bringup uses native-style blocking helpers such as:

- `pollster::block_on`

This is acceptable for native and some WASI-like environments, but browser
WASM wants:

- async adapter/device acquisition
- no blocking on the main thread

Implication:

- a browser-facing constructor path likely needs to be async or split-phase
- current constructors are good for proving backend behavior, not yet for final
  browser integration

### 4. Feature defaults are native-oriented

Current `webrender` defaults still assume native environments:

- default feature includes `static_freetype`
- various optional systems assume desktop/server capabilities

Implication:

- a WASM profile should almost certainly start from `--no-default-features`
- then explicitly opt into only the backend/runtime pieces that are meaningful

### 5. Some recent wgpu integration is still GL-shadowed

The branch has progressed beyond pure proof work:

- `RendererBackend`
- `GpuDevice`
- wgpu-only constructor
- composite render path
- surface presentation
- texture cache copy support
- external image uploads

But a lot of renderer state still has a GL-shaped fallback or a GL-owned type
nearby, and several subsystems still assume a concrete GL `Device`.

Implication:

- the branch is on the right path
- it is not yet at the stage where a WASM compile should be expected to work

## Multithreading Story

## Browser WASM

There has been real progress in the ecosystem, but not enough to say:

- “great, WebRender can keep its current native thread model unchanged”

What is true today:

- WebAssembly threads exist
- shared linear memory exists
- browser-side threaded Rust/WASM is possible

What is still operationally important:

- shared memory means `SharedArrayBuffer`
- practical use on the web still depends on cross-origin isolation
- Rust threading on `wasm32-unknown-unknown` is still not a turnkey default setup
- the browser main thread cannot block like a desktop main thread can

Practical consequence for WebRender:

- we probably do not need to permanently collapse WebRender into one thread
- but we also should not assume the current thread model ports over unchanged

More realistic browser-WASM shape:

- worker-based orchestration
- main-thread-thin presentation and event loop
- optional shared-memory path for isolated deployments
- wasm-specific executor/runtime glue instead of direct reuse of native startup

## WASI / Wasmtime

WASI threading is moving forward, but it is still not something to design around
as if it were equivalent to native threads everywhere.

Current assumption:

- `wasm32-wasip2` is better for `std`
- it is not yet a reason to postpone designing a WASM-friendly single-process /
  worker-friendly orchestration model

## Recommended Working Assumptions

For this branch, assume:

1. `wasm32-unknown-unknown` is the primary portability bar
2. browser WGPU requires an async or split-phase constructor
3. WebRender should gain a WASM runtime mode instead of pretending native thread
   startup will map directly
4. GL-only code should either:
   - stay clearly GL-owned, or
   - move behind a backend/runtime seam, or
   - compile out in WASM profiles

## Concrete Next Steps

Priority order:

1. continue removing GL-only assumptions from the wgpu render path
2. identify which startup threads are truly required versus convenience structure
3. sketch a wasm runtime mode:
   - no direct `std::thread::spawn`
   - worker/executor-friendly scene/render backend orchestration
   - async `wgpu` bringup
4. define a minimal WASM feature profile:
   - `--no-default-features`
   - `wgpu_backend`
   - no desktop-only extras by default
5. only after that, begin real `cargo check` target work for:
   - `wasm32-unknown-unknown`
   - `wasm32-wasip2`

## Local Validation Note

Local compile attempts on this machine did not reach meaningful WebRender-level
errors yet because the Rust targets were not installed:

- `wasm32-unknown-unknown`
- `wasm32-wasip2`

So this note reflects:

- current tree inspection
- current branch structure
- current Rust/WASM ecosystem guidance

not a completed target-by-target compile audit.

## Sources

- Rust `wasm32-unknown-unknown` target documentation
- Rust `wasm32-wasip2` target documentation
- `wasm-bindgen` threading guidance
- MDN documentation for shared WebAssembly memory / `SharedArrayBuffer`
- Wasmtime stability notes around WASI proposals

