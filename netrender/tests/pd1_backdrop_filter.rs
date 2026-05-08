//! Roadmap D1 — backdrop-filter receipts.
//!
//! Two receipt classes:
//!
//! 1. **Pure CPU**: API surface — `SceneFilter` enum, the
//!    `backdrop_filter` field on `SceneLayer`, and tile-cache hash
//!    distinguishes filtered from unfiltered layers.
//! 2. **GPU smoke**: render a scene with sharp-edged content
//!    underneath a layer carrying `Blur(8.0)` backdrop filter; read
//!    pixels back; verify the post-filter region is **smoother**
//!    (lower local variance) than the same region without the
//!    filter. The receipt the roadmap calls for: "frosted-glass nav
//!    bar over a busy background."

use netrender::scene::{
    Scene, SceneClip, SceneFilter, SceneLayer, SceneOp,
};
use netrender::tile_cache::TileCache;

const TILE: u32 = 32;

#[test]
fn pd1_backdrop_filter_default_is_none() {
    let layer = SceneLayer::clip(SceneClip::None);
    assert!(layer.backdrop_filter.is_none(), "no backdrop filter by default");
    let layer = SceneLayer::alpha(0.5);
    assert!(layer.backdrop_filter.is_none());
}

#[test]
fn pd1_setting_backdrop_filter_invalidates_tile_hash() {
    let mut scene = Scene::new(64, 64);
    scene.push_layer(SceneLayer::clip(SceneClip::Rect {
        rect: [0.0, 0.0, 32.0, 32.0],
        radii: [0.0; 4],
    }));
    scene.push_rect(0.0, 0.0, 32.0, 32.0, [1.0, 0.0, 0.0, 1.0]);
    scene.ops.push(SceneOp::PopLayer);

    let mut cache = TileCache::new(TILE);
    let _ = cache.invalidate(&scene);
    let _ = cache.invalidate(&scene);

    if let SceneOp::PushLayer(l) = &mut scene.ops[0] {
        l.backdrop_filter = Some(SceneFilter::Blur(8.0));
    }
    let dirty = cache.invalidate(&scene);
    assert!(
        !dirty.is_empty(),
        "setting backdrop_filter invalidates: {} dirty",
        dirty.len()
    );
}

#[test]
fn pd1_changing_blur_radius_invalidates_tile_hash() {
    let mut scene = Scene::new(64, 64);
    let mut l = SceneLayer::clip(SceneClip::Rect {
        rect: [0.0, 0.0, 32.0, 32.0],
        radii: [0.0; 4],
    });
    l.backdrop_filter = Some(SceneFilter::Blur(4.0));
    scene.push_layer(l);
    scene.push_rect(0.0, 0.0, 32.0, 32.0, [1.0, 0.0, 0.0, 1.0]);
    scene.ops.push(SceneOp::PopLayer);

    let mut cache = TileCache::new(TILE);
    let _ = cache.invalidate(&scene);
    let _ = cache.invalidate(&scene);

    if let SceneOp::PushLayer(l) = &mut scene.ops[0] {
        l.backdrop_filter = Some(SceneFilter::Blur(16.0));
    }
    let dirty = cache.invalidate(&scene);
    assert!(!dirty.is_empty(), "blur radius change invalidates");
}

// ── GPU smoke ─────────────────────────────────────────────────────────

mod gpu_smoke {
    use netrender::scene::{
        Scene, SceneClip, SceneFilter, SceneLayer, SceneOp,
    };
    use netrender::{boot, create_netrender_instance, ColorLoad, NetrenderOptions};

    const DIM: u32 = 256;
    const TILE_SIZE: u32 = 64;

    fn make_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pd1 target"),
            size: wgpu::Extent3d { width: DIM, height: DIM, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8Unorm),
            ..Default::default()
        });
        (texture, view)
    }

    /// Build a "busy" scene: alternating colored stripes covering
    /// the full canvas. This produces sharp edges that any blur
    /// will visibly smooth.
    fn busy_scene() -> Scene {
        let mut scene = Scene::new(DIM, DIM);
        for i in 0..16 {
            let x0 = i as f32 * (DIM as f32 / 16.0);
            let x1 = x0 + DIM as f32 / 16.0;
            let color = if i % 2 == 0 {
                [1.0, 0.0, 0.0, 1.0]
            } else {
                [0.0, 0.0, 1.0, 1.0]
            };
            scene.push_rect(x0, 0.0, x1, DIM as f32, color);
        }
        scene
    }

    /// Sample local horizontal variance on a scanline through `y`,
    /// across the column range `[x0, x1]`. Each pixel is compared
    /// to its left neighbor; large per-pixel deltas mean sharp
    /// edges (high variance), small deltas mean smooth.
    fn local_variance(bytes: &[u8], y: u32, x0: u32, x1: u32) -> f64 {
        let row_start = (y * DIM * 4) as usize;
        let scanline = &bytes[row_start..row_start + (DIM * 4) as usize];
        let mut total: f64 = 0.0;
        let mut count: u32 = 0;
        for x in x0..x1 {
            if x == 0 {
                continue;
            }
            let curr = (x * 4) as usize;
            let prev = ((x - 1) * 4) as usize;
            let dr = scanline[curr] as i32 - scanline[prev] as i32;
            let dg = scanline[curr + 1] as i32 - scanline[prev + 1] as i32;
            let db = scanline[curr + 2] as i32 - scanline[prev + 2] as i32;
            total += (dr * dr + dg * dg + db * db) as f64;
            count += 1;
        }
        if count == 0 {
            0.0
        } else {
            total / count as f64
        }
    }

    #[test]
    fn pd1_backdrop_blur_smooths_busy_background() {
        let handles = boot().expect("wgpu boot");
        let renderer = create_netrender_instance(
            handles.clone(),
            NetrenderOptions {
                tile_cache_size: Some(TILE_SIZE),
                enable_vello: true,
                ..Default::default()
            },
        )
        .expect("create_netrender_instance");

        // Reference: busy stripes, no backdrop filter.
        let reference_scene = busy_scene();
        let (ref_target, ref_view) = make_target(&handles.device);
        renderer.render_vello(&reference_scene, &ref_view, ColorLoad::Clear(wgpu::Color::BLACK));
        let ref_bytes = renderer.wgpu_device.read_rgba8_texture(&ref_target, DIM, DIM);

        // Filtered: same stripes, with a layer carrying backdrop
        // Blur(12) covering a horizontal band. The band should
        // appear smoother (lower local variance) than the same
        // x-range in the reference.
        let mut filtered = busy_scene();
        let mut layer = SceneLayer::clip(SceneClip::Rect {
            rect: [16.0, 100.0, (DIM - 16) as f32, 156.0],
            radii: [0.0; 4],
        });
        layer.backdrop_filter = Some(SceneFilter::Blur(12.0));
        filtered.push_layer(layer);
        // No content inside the layer — just want the blurred
        // backdrop to show through.
        filtered.ops.push(SceneOp::PopLayer);

        let (filt_target, filt_view) = make_target(&handles.device);
        renderer.render_vello(&filtered, &filt_view, ColorLoad::Clear(wgpu::Color::BLACK));
        let filt_bytes = renderer.wgpu_device.read_rgba8_texture(&filt_target, DIM, DIM);

        let band_y = 128_u32;
        let ref_var = local_variance(&ref_bytes, band_y, 32, DIM - 32);
        let filt_var = local_variance(&filt_bytes, band_y, 32, DIM - 32);

        eprintln!(
            "pd1: reference variance={:.1}, filtered variance={:.1}",
            ref_var, filt_var
        );

        assert!(
            ref_var > 1000.0,
            "reference busy stripes should have high variance, got {ref_var}"
        );
        assert!(
            filt_var < ref_var * 0.5,
            "backdrop-filtered band should have <50% the variance of the reference: ref={ref_var:.1}, filt={filt_var:.1}"
        );
    }
}
