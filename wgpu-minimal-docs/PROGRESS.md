# WebRender wgpu Backend — Progress Summary

## Overview

This document summarizes the work done to implement a wgpu backend for WebRender and integrate it with Servo, enabling host applications to share their wgpu device with the browser engine. The goal is to support graphshell: a spatial graph browser built on egui + Servo that composites web content into its own wgpu render pipeline.

---

## Branch Map

| Branch (WebRender) | Purpose |
|---|---|
| `wgpu-backend-0.68-minimal` | **Main experimental branch** — full wgpu backend + device sharing + docs |
| `wgpu-device-sharing` | Device-sharing API work (now rebased onto minimal) |
| `wgpu-device-renderer` | Snapshot at wgpu reftest parity (413/413 pass) |

| Branch (Servo) | Purpose |
|---|---|
| `webrender-wgpu-patch` | Servo changes for wgpu backend + GL-optional Painter + composite texture API |

---

## Phase 1: wgpu Backend for WebRender (`wgpu-backend-0.68-minimal`)

### What was built

A complete wgpu rendering backend for WebRender, implemented as a second path alongside the existing GL backend. The backend is toggled at runtime by the `SERVO_WGPU_BACKEND=1` environment variable (in Servo) or via `create_webrender_instance_with_backend()`.

Key implementation milestones (all committed on the branch):

| Commit range | Work |
|---|---|
| C3–C4 | Draw context, cached buffers, encoder batching, render pass sharing |
| P6–P7 | Quad batch routing, composite pass, offscreen rendering, SVG filters, blits |
| P8 | resolve_ops, clip masks, offscreen dispatch |
| P9–P10 | CompositeFastPath, MixBlend depth, gpu_buffer textures for cs_* gradients |
| P11 | BrushImageRepeat/RepeatAlpha variants for REPETITION feature |
| P12 | Picture cache blits, resolve_ops completion |
| P13–P14 | Renderer completion pass, conic test scene, subpixel skip (434/441 pass) |
| Parity | Windows/wgpu fuzzy tolerances → 413/413 reftests pass |
| Fixes | Picture cache tile dirty_rect clearing, diagnostic logging removed |

### Reftest results

```
wgpu backend: 413/413 reftests pass (with tolerances for hardware/platform differences)
gl backend:   413/413 (baseline)
```

Tolerances are registered per-backend in `webrender/src/tests.rs` via `FuzzyMatch`.

### Architecture notes

- `RendererBackend` enum controls which path is taken in `Renderer`
- wgpu path: `RendererBackend::Wgpu { instance, surface, w, h }` — Renderer owns the device
- shared path: `RendererBackend::WgpuShared { device, queue }` — host owns the device
- GL code is feature-gated; wgpu path never calls into gleam
- Compositor traits, profiler drawing, GL exports gated behind `#[cfg(feature = "gl")]`

---

## Phase 2: Shared Device API

### What was built

A new `RendererBackend::WgpuShared { device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue> }` variant that lets a host application pass in its existing wgpu device. WebRender initializes without creating its own device or surface; it renders to an internal `wgpu_readback_texture` (Bgra8Unorm).

Key commits:
- `6531eb150` — shared device API: `WgpuShared` variant, `WgpuTexture` type, `Renderer::composite_output()`
- `b8e6b3a6c` — smoke tests for the shared device API
- `654a44a65` — `composite_output()` returning `&WgpuTexture` (texture + size)
- `5615909f2` — proof-of-concept demo (`examples/wgpu_shared_device.rs`)

### `WgpuTexture`

```rust
pub struct WgpuTexture {
    pub texture: wgpu::Texture,   // Bgra8Unorm, COPY_SRC | RENDER_ATTACHMENT
    pub width: u32,
    pub height: u32,
}
```

`Renderer::composite_output() -> Option<&WgpuTexture>` — available after `renderer.render()`.

### Demo: `examples/wgpu_shared_device.rs`

Creates an external device, initializes WebRender with `WgpuShared`, renders a 4-quadrant color test scene, reads back via `read_pixels_into()`, verifies pixel colors. Confirmed working on Windows (NVIDIA).

---

## Phase 3: Servo Integration (`webrender-wgpu-patch` branch)

### GL-optional Painter (`components/paint/painter.rs`)

`webrender_gl` field changed from `Rc<dyn gleam::gl::Gl>` to `Option<Rc<dyn gleam::gl::Gl>>`:
- `None` when `use_wgpu = true`
- All GL calls wrapped in `if let Some(gl) = &self.webrender_gl { ... }`
- `make_current()` skipped in wgpu mode
- `assert_no_gl_error()`, `assert_gl_framebuffer_complete()`, `clear_background()` all tolerate `None`

### Surfman-optional Paint (`components/paint/paint.rs`)

`RenderingContext::connection()` can return `None` for pure-wgpu embedders:
- `PainterSurfmanDetails` insertion wrapped in `if let Some(connection) = ...`
- `PainterSurfmanDetailsMap::remove()` assertion removed (details may be absent)

### Composite texture chain

```
WebRender Renderer::composite_output()
  → Painter::composite_output() -> Option<&WgpuTexture>
  → Paint::composite_texture(painter_id) -> Option<wgpu::Texture>
  → WebView::composite_texture() -> Option<wgpu::Texture>
```

`wgpu::Texture` clone is cheap (Arc bump).

### `WebView::render()`

Embedder must call this from `notify_new_frame_ready` to trigger WebRender. `spin_event_loop()` signals readiness via the delegate but does NOT call `paint.render()` itself.

```rust
pub fn render(&self) { ... paint().render(inner.id) ... }
```

### `RenderingContext` trait additions (`components/shared/paint/rendering_context.rs`)

```rust
fn wgpu_device(&self) -> Option<Arc<wgpu::Device>> { None }
fn wgpu_queue(&self) -> Option<Arc<wgpu::Queue>> { None }
```

Default implementations return `None` for backward compatibility.

---

## Phase 4: wgpu-embedder Demo (`examples/wgpu-embedder`)

A complete Servo embedder that:
1. Creates a winit window + wgpu device (no GL context)
2. Implements `WgpuRenderingContext` (all GL methods `unreachable!()`, `make_current()` returns `Ok(())`)
3. Initializes Servo with `SERVO_WGPU_BACKEND=1`
4. Loads `https://example.com`
5. In `notify_new_frame_ready`: calls `webview.render()`, sets `frame_ready` flag
6. On redraw: calls `webview.composite_texture()`, creates bind group, runs blit pass

### Blit pass

```wgsl
@vertex fn vs_main(@builtin(vertex_index) i: u32) -> ... {
    // fullscreen triangle: covers [-1,1]x[-1,1]
    let x = f32((i << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(i & 2u) * 2.0 - 1.0;
    ...
}
@fragment fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t_diffuse, s_diffuse, in.uv);
}
```

WebRender's `Bgra8Unorm` output is sampled directly into the window surface (also `Bgra8Unorm` on Windows).

### Result

`example.com` rendered in a native winit window, no GL, pure wgpu pipeline. Confirmed working.

---

## Known Issues / Non-Goals (this branch)

- **naga stack overflow** on `cs_svg_filter_node` (~3000-line WGSL): pre-existing upstream bug. Workaround: spawn WebRender thread with 16 MB stack. Not fixed on this branch.
- **Subpixel rendering**: skipped in wgpu path (28 tests excluded from 441 → 413 pass set)
- **WebGL**: not implemented in wgpu path

---

## Next: `wgpu-hal-backend` Branch

The plan is to branch `wgpu-hal-backend` off `wgpu-backend-0.68-minimal` and implement a third backend variant using `wgpu-hal` directly. This enables lower-level host integration (sharing command buffers, render passes, etc.) for the graphshell compositing use case.

The experimental `wgpu-backend-0.68-minimal` branch will eventually have three selectable backends:
- `RendererBackend::Gl` — existing path
- `RendererBackend::Wgpu` / `WgpuShared` — current work
- `RendererBackend::WgpuHal` — future: host provides raw hal device
