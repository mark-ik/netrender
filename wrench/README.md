# wrench

`wrench` is a tool for debugging webrender outside of a browser engine.

## Build

Build `wrench` with `cargo build --release` within the `wrench` directory.

## headless

`wrench` has an optional headless mode for use in continuous integration. To run in headless mode, instead of using `cargo run -- args`, use `./headless.py args`.

## `show`

If you are working on gecko integration you can capture a frame via the following steps.

* Visit about:support and check that the "Compositing" value in the "Graphics" table says "WebRender". Enable `gfx.webrender.all` in about:config if necessary to enable WebRender.
* Hit ctrl-shift-3 to capture the frame. The data will be put in `~/wr-capture`.
* View the capture with `wrench show ~/wr-capture`.

## wgpu backends

Wrench supports multiple wgpu rendering backends (requires the `wgpu_backend` feature):

* `--wgpu` — windowed wgpu rendering using a native surface
* `--wgpu-hal` — wgpu-hal backend with host-owned device (factory closure pattern)
* `--wgpu-hal-headless` — headless wgpu-hal rendering with no window or display server required

Example:

```bash
cargo run --release --features wgpu_backend -- show --wgpu test.yaml
cargo run --release --features wgpu_backend -- --wgpu-hal-headless reftest
```

The headless mode creates a wgpu adapter without a surface and renders to
offscreen textures. This is useful for CI environments and automated testing.

## `reftest`

Wrench also has a reftest system for catching regressions.

* To run all reftests, run `script/headless.py reftest`
* To run specific reftests, run `script/headless.py reftest path/to/test/or/dir`
* To examine test failures, use the [reftest analyzer](https://hg.mozilla.org/mozilla-central/raw-file/tip/layout/tools/reftest/reftest-analyzer.xhtml)
* To add a new reftest, create an example frame and a reference frame in `reftests/` and then add an entry to `reftests/reftest.list`

### wgpu reftests

Run reftests with different backends:

```bash
# GL (default, uses ANGLE on Windows)
cargo run --release -- reftest

# wgpu windowed
cargo run --release --features wgpu_backend -- --wgpu reftest

# wgpu headless (no window needed)
cargo run --release --features wgpu_backend -- --wgpu-hal-headless reftest
```

The reftest harness auto-applies fuzzy tolerance (max_difference <= 4) when
running with a wgpu backend, since minor precision differences between GL
and wgpu are expected.
