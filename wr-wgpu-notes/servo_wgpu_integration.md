# Servo wgpu Backend Integration Guide

How to set up a Servo checkout to test the WebRender wgpu backend.
Tested and confirmed working at DPR=1 and DPR=2 on Windows (2026-04-02).

## Prerequisites

- A working Servo build environment (`mach build` succeeds)
- A local checkout of the webrender repo with the `wgpu_backend` feature
  (the webrender repo in `../webrender` relative to your Servo checkout)

## How it works

Servo's painter creates a WebRender renderer during init. The GL path uses
`create_webrender_instance()`. For wgpu, we use
`create_webrender_instance_with_backend()` with
`RendererBackend::Wgpu { instance, surface, width, height }`.

The wgpu surface is created from the winit window's raw handles. All GL
operations (make_current, clear_background, prepare_for_rendering, etc.)
must be skipped when using wgpu, because WebRender presents directly to the
wgpu surface.

To activate: `SERVO_WGPU_BACKEND=1 cargo run --bin servoshell`

## Step-by-step setup

Starting from a clean Servo checkout (e.g. `servo/servo` main branch):

### Step 1. Point Cargo at your local webrender

In the workspace `Cargo.toml`, enable the `wgpu_backend` feature and
uncomment the local path overrides in `[patch.crates-io]`:

```toml
# In [workspace.dependencies]:
webrender = { version = "0.68", features = ["capture", "wgpu_backend"] }

# In [patch.crates-io]:
webrender = { path = "../webrender/webrender" }
webrender_api = { path = "../webrender/webrender_api" }
wr_malloc_size_of = { path = "../webrender/wr_malloc_size_of" }
```

Then run `cargo update -p webrender -p webrender_api -p wr_malloc_size_of`
to regenerate `Cargo.lock`.

### Step 2. Expose raw window handles from RenderingContext

**File:** `components/shared/paint/rendering_context.rs`

Add two trait methods with default impls:

```rust
// In the RenderingContext trait:
fn raw_window_handle(&self) -> Option<raw_window_handle::RawWindowHandle> {
    None
}
fn raw_display_handle(&self) -> Option<raw_window_handle::RawDisplayHandle> {
    None
}
```

Store the raw handles in `WindowRenderingContext` at construction time:

```rust
// Add fields to WindowRenderingContext:
raw_window_handle: raw_window_handle::RawWindowHandle,
raw_display_handle: raw_window_handle::RawDisplayHandle,

// In WindowRenderingContext::new(), before the surfman Connection call:
let raw_display_handle = display_handle.as_raw();
let raw_window_handle = window_handle.as_raw();

// Implement the trait methods:
fn raw_window_handle(&self) -> Option<raw_window_handle::RawWindowHandle> {
    Some(self.raw_window_handle)
}
fn raw_display_handle(&self) -> Option<raw_window_handle::RawDisplayHandle> {
    Some(self.raw_display_handle)
}
```

Forward both methods from `OffscreenRenderingContext` to its parent context.

### Step 3. Add wgpu backend selection to the Painter

**File:** `components/paint/painter.rs`

**3a.** Add a field to `Painter`:

```rust
/// Whether this painter uses the wgpu backend (skips GL operations).
use_wgpu: bool,
```

**3b.** In `Painter::new()`, branch on `SERVO_WGPU_BACKEND`:

```rust
let use_wgpu = std::env::var("SERVO_WGPU_BACKEND").is_ok();

// Extract webrender_options and notifier BEFORE the branch
let webrender_options = webrender::WebRenderOptions { /* ... same as before ... */ };
let notifier = Box::new(RenderNotifier::new(painter_id, paint.paint_proxy.clone()));

let (mut webrender_renderer, webrender_api_sender) = if use_wgpu {
    let size = rendering_context.size();
    let (wgpu_instance, surface) = match (
        rendering_context.raw_window_handle(),
        rendering_context.raw_display_handle(),
    ) {
        (Some(raw_window_handle), Some(raw_display_handle)) => {
            let instance = webrender::wgpu::Instance::default();
            #[allow(unsafe_code)]
            let surface = unsafe {
                instance.create_surface_unsafe(
                    webrender::wgpu::SurfaceTargetUnsafe::RawHandle {
                        raw_display_handle,
                        raw_window_handle,
                    },
                )
            }
            .expect("Failed to create wgpu surface from window handles");
            (Some(instance), Some(surface))
        }
        _ => (None, None),
    };

    webrender::create_webrender_instance_with_backend(
        webrender::RendererBackend::Wgpu {
            instance: wgpu_instance,
            surface,
            width: size.width,
            height: size.height,
        },
        notifier,
        webrender_options,
        None,
    )
    .expect("Unable to initialize WebRender with wgpu backend.")
} else {
    webrender::create_webrender_instance(
        webrender_gl.clone(),
        notifier,
        webrender_options,
        None,
    )
    .expect("Unable to initialize WebRender.")
};
```

**3c.** Guard GL-only operations with `if !self.use_wgpu`:

```rust
// After painter construction:
if !use_wgpu {
    painter.assert_gl_framebuffer_complete();
    painter.clear_background();
}

// In Painter::paint(), before rendering:
if !self.use_wgpu {
    if let Err(error) = self.rendering_context.make_current() {
        error!("Failed to make the rendering context current: {error:?}");
    }
    self.assert_no_gl_error();
    self.rendering_context.prepare_for_rendering();
}

// Also guard clear_background() before renderer.render():
if !self.use_wgpu {
    self.clear_background();
}
```

**3d.** In `resize()`, guard GL resize and always notify the renderer:

```rust
if !self.use_wgpu {
    if let Err(error) = self.rendering_context.make_current() {
        error!("Failed to make the rendering context current: {error:?}");
    }
    self.rendering_context.resize(new_size);
}
if let Some(renderer) = self.webrender_renderer.as_mut() {
    renderer.resize_surface(new_size.width, new_size.height);
}
```

### Step 4. Skip GL paint in the GUI layer

**File:** `ports/servoshell/desktop/gui.rs`

The GUI's `paint()` calls surfman present, which overwrites wgpu output.
Add an early return at the top of `Gui::paint()`:

```rust
pub(crate) fn paint(&mut self, window: &Window) {
    if std::env::var("SERVO_WGPU_BACKEND").is_ok() {
        return;
    }
    // ... existing GL paint code unchanged
}
```

### Step 5. Increase stack size (Windows only)

**File:** `.cargo/config.toml`

Debug builds with the wgpu path can overflow the default 1MB stack.
Add under the existing `[target.x86_64-pc-windows-msvc]` section:

```toml
rustflags = ["-C", "link-args=/STACK:8388608"]
```

### Step 6. Build and run

```bash
cargo build --bin servoshell
SERVO_WGPU_BACKEND=1 cargo run --bin servoshell
```

## HiDPI notes

Servo handles HiDPI by pushing a 2x reference frame transform
(`painter.rs` ~line 688) rather than setting `global_device_pixel_scale`
(hardcoded to 1.0 in `frame_builder.rs:684`). This is the same for both
GL and wgpu paths — no Servo-side changes needed.

The webrender wgpu backend must select the `GLYPH_TRANSFORM` shader
variant for `TransformedAlpha`/`TransformedSubpixel` glyph formats
at DPR > 1. See `wr_wgpu_debug_plan.md` for details on this fix.

## Reference commits

These changes were developed on the `webrender-wgpu-patch` branch in the
`servo-graphshell` fork. The substantive commits:

- `d530bba` — Wire wgpu backend selection via SERVO_WGPU_BACKEND env var
- `a37edf5` — Skip GL operations in painter when using wgpu backend
- `6bff6c8` — Create wgpu surface from raw window handles

The gui.rs paint skip and .cargo/config.toml stack size were uncommitted
working changes at time of documentation.

## What is and isn't wgpu-related

Only these files need wgpu-specific changes:

- `Cargo.toml` — feature flag + local path overrides
- `components/shared/paint/rendering_context.rs` — raw handle trait methods
- `components/paint/painter.rs` — backend selection + GL guards
- `ports/servoshell/desktop/gui.rs` — skip GL present
- `.cargo/config.toml` — stack size (Windows debug builds only)

Other diffs in the `webrender-wgpu-patch` branch (accesskit changes in
`gui.rs`/`headed_window.rs`, version bumps, etc.) are upstream drift from
merge timing, not wgpu-related.
