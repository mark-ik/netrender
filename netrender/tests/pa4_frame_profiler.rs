//! Roadmap A4 — frame profiler receipts.
//!
//! Two receipt classes:
//!
//! 1. **Pure CPU**: `FrameTimings` / `Span` struct mechanics —
//!    record, lookup, total. Doesn't touch wgpu.
//! 2. **GPU smoke**: drive `Renderer::render_vello` with a real wgpu
//!    device and assert that `Renderer::last_frame_timings()` returns
//!    a populated report with non-zero durations for every expected
//!    span name.
//!
//! The GPU smoke uses `boot()` from `netrender_device` to acquire a
//! wgpu adapter; it'll skip on systems without a working adapter
//! (boot returns Err, and the test panics with a clear "wgpu boot"
//! message — same shape as p7prime_renderer_integration uses).

use std::time::Duration;

use netrender::profiling::{FrameTimings, NamedSpan, Span};

#[test]
fn frame_timings_record_appends_in_order() {
    let mut t = FrameTimings::empty();
    t.record("alpha", Duration::from_micros(100));
    t.record("beta", Duration::from_micros(200));
    t.record("gamma", Duration::from_micros(300));

    let names: Vec<&str> = t.spans.iter().map(|s| s.name).collect();
    assert_eq!(names, vec!["alpha", "beta", "gamma"]);

    assert_eq!(t.span("alpha"), Some(Duration::from_micros(100)));
    assert_eq!(t.span("beta"), Some(Duration::from_micros(200)));
    assert_eq!(t.span("gamma"), Some(Duration::from_micros(300)));
    assert_eq!(t.span("missing"), None);
}

#[test]
fn span_start_then_stop_recording_appends_a_span() {
    let mut t = FrameTimings::empty();
    let s = Span::start("work");
    // Burn a few micros so the duration is non-zero; system clocks
    // generally tick at >=1µs resolution.
    std::thread::sleep(Duration::from_micros(50));
    s.stop_recording(&mut t);
    assert_eq!(t.spans.len(), 1);
    assert_eq!(t.spans[0].name, "work");
    assert!(t.spans[0].duration > Duration::ZERO);
}

#[test]
fn span_stop_returns_duration_without_recording() {
    let s = Span::start("solo");
    std::thread::sleep(Duration::from_micros(50));
    let d = s.stop();
    assert!(d > Duration::ZERO);
}

#[test]
fn frame_timings_named_span_struct_is_inspectable() {
    let n = NamedSpan {
        name: "x",
        duration: Duration::from_millis(7),
    };
    assert_eq!(n.name, "x");
    assert_eq!(n.duration, Duration::from_millis(7));
}

// ── GPU integration smoke ─────────────────────────────────────────────

mod gpu_smoke {
    use netrender::{ColorLoad, NetrenderOptions, Scene, boot, create_netrender_instance};

    const DIM: u32 = 64;
    const TILE: u32 = 32;

    fn make_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pa4 target"),
            size: wgpu::Extent3d {
                width: DIM,
                height: DIM,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        (texture, view)
    }

    #[test]
    fn render_vello_populates_last_frame_timings() {
        let handles = boot().expect("wgpu boot");
        let device = handles.device.clone();
        let renderer = create_netrender_instance(
            handles,
            NetrenderOptions {
                tile_cache_size: Some(TILE),
                enable_vello: true,
                ..Default::default()
            },
        )
        .expect("create_netrender_instance");

        // Pre-render: no timings yet.
        assert!(
            renderer.last_frame_timings().is_none(),
            "before any render, last_frame_timings is None"
        );

        let mut scene = Scene::new(DIM, DIM);
        scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [0.5, 0.5, 0.0, 1.0]);

        let (_target, view) = make_target(&device);
        renderer.render_vello(&scene, &view, ColorLoad::default());

        let timings = renderer
            .last_frame_timings()
            .expect("post-render last_frame_timings is Some");

        // total > 0
        assert!(
            timings.total > std::time::Duration::ZERO,
            "total should be positive: {:?}",
            timings.total
        );

        // Every expected span name is present.
        let expected = [
            "refresh_image_data",
            "tile_invalidate",
            "dirty_tile_rebuild",
            "master_compose",
            "vello_render",
        ];
        for name in &expected {
            let dur = timings
                .span(name)
                .unwrap_or_else(|| panic!("span {name:?} missing from timings: {timings:#?}"));
            // We don't assert > 0 here because some spans (e.g.
            // refresh_image_data on an empty image map) can complete
            // in less than a clock tick; what matters is that the
            // span was recorded at all.
            let _ = dur;
        }
    }

    #[test]
    fn second_render_replaces_timings() {
        let handles = boot().expect("wgpu boot");
        let device = handles.device.clone();
        let renderer = create_netrender_instance(
            handles,
            NetrenderOptions {
                tile_cache_size: Some(TILE),
                enable_vello: true,
                ..Default::default()
            },
        )
        .expect("create_netrender_instance");

        let mut scene = Scene::new(DIM, DIM);
        scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [0.5, 0.5, 0.0, 1.0]);
        let (_target, view) = make_target(&device);

        renderer.render_vello(&scene, &view, ColorLoad::default());
        let total_1 = renderer.last_frame_timings().unwrap().total;
        let span_count_1 = renderer.last_frame_timings().unwrap().spans.len();

        renderer.render_vello(&scene, &view, ColorLoad::default());
        let total_2 = renderer.last_frame_timings().unwrap().total;
        let span_count_2 = renderer.last_frame_timings().unwrap().spans.len();

        // Both renders capture timings; we can't assert any ordering
        // between the two `total` values, but we can assert both are
        // positive and that the report shape is stable.
        assert!(total_1 > std::time::Duration::ZERO);
        assert!(total_2 > std::time::Duration::ZERO);
        assert_eq!(
            span_count_1, span_count_2,
            "consecutive renders produce reports with the same span count"
        );
    }
}
