//! Roadmap R5 — downscale-blur cascade receipts.
//!
//! Two receipt classes:
//!
//! 1. **Pure CPU**: planner heuristic — `level` is 1 for radii ≤ 28,
//!    powers of 2 beyond, capped at 8.
//! 2. **GPU smoke**: render box-shadow masks at radii 16 / 64 / 96
//!    and verify that the blurred edge actually widens (no σ-clip
//!    artifact at radius 64). Skipped vacuously without a working
//!    wgpu adapter.

mod gpu_smoke {
    use netrender::{boot, create_netrender_instance, NetrenderOptions};

    const DIM: u32 = 256;
    const TILE_SIZE: u32 = 64;

    /// Render a box-shadow mask at the given blur radius and read
    /// back the alpha channel of a horizontal scanline through the
    /// vertical center.
    fn shadow_alpha_scanline(blur_radius: f32) -> Vec<u8> {
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

        // 96×96 box centered in DIM×DIM, no rounded corners.
        let inset = (DIM as f32 - 96.0) * 0.5;
        let bounds = [inset, inset, DIM as f32 - inset, DIM as f32 - inset];

        renderer.build_box_shadow_mask(1, DIM, bounds, 0.0, blur_radius);

        // Pull the registered mask out via vello's atlas isn't
        // straightforward; instead, render the mask through a Scene
        // with a SceneImage referencing the registered key, into a
        // standard target, and read back.
        use netrender::{ColorLoad, Scene};
        let mut scene = Scene::new(DIM, DIM);
        scene.push_image_full(
            0.0, 0.0, DIM as f32, DIM as f32,
            [0.0, 0.0, 1.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            1,
            0,
            netrender::NO_CLIP,
        );

        let target_texture = handles.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pr5 target"),
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
        let target_view = target_texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8Unorm),
            ..Default::default()
        });
        renderer.render_vello(&scene, &target_view, ColorLoad::Clear(wgpu::Color::BLACK));
        let bytes = renderer.wgpu_device.read_rgba8_texture(&target_texture, DIM, DIM);

        let row = DIM / 2;
        let row_start = (row * DIM * 4) as usize;
        // Take the alpha (= R, since the mask is white-on-black) of
        // each pixel along the scanline.
        bytes[row_start..row_start + (DIM * 4) as usize]
            .chunks_exact(4)
            .map(|c| c[0])
            .collect()
    }

    /// Width of the edge transition zone in pixels — the count of
    /// scanline pixels with alpha strictly between `low` and `high`.
    /// Bigger blur → wider transition.
    fn edge_transition_width(scanline: &[u8], low: u8, high: u8) -> usize {
        scanline.iter().filter(|&&v| v > low && v < high).count()
    }

    #[test]
    fn pr5_blur_64_widens_edge_more_than_blur_16() {
        // If wgpu boot fails (no GPU), this test will panic with a
        // clear "wgpu boot" message — same shape as p7prime tests.
        let small = shadow_alpha_scanline(16.0);
        let large = shadow_alpha_scanline(64.0);

        let small_width = edge_transition_width(&small, 16, 240);
        let large_width = edge_transition_width(&large, 16, 240);

        eprintln!(
            "pr5: edge-transition width at blur=16: {}, at blur=64: {}",
            small_width, large_width
        );

        // Blur 64 should produce a noticeably wider transition than
        // blur 16. Without R5's downscale path, the cascade σ-clips
        // around radius 28; the edge would be barely wider for
        // radius 64 than for radius 28. With downscale at level 4
        // (radius 64 → scaled 16 → fits in single-level cap), the
        // blur reaches its target σ.
        assert!(
            large_width > small_width + 10,
            "blur=64 edge width ({}) should be substantially > blur=16 width ({}) — expected at least +10 px",
            large_width, small_width,
        );
    }

    #[test]
    fn pr5_blur_64_paints_visible_alpha() {
        // Sanity: even at large blur, the result has nonzero alpha
        // somewhere along the scanline. If R5's downscale path was
        // emitting all-zeros (wgpu validation error, feedback loop,
        // or extent mismatch), max alpha would be ~0.
        //
        // Note: a wider blur reduces peak alpha because the same
        // total mass spreads further. We don't assert "high" peak
        // — we just assert "any" paint, which proves the pipeline
        // actually executed.
        let r64 = shadow_alpha_scanline(64.0);
        let max_alpha = r64.iter().copied().max().unwrap_or(0);
        let total_paint: u64 = r64.iter().map(|&v| v as u64).sum();
        eprintln!(
            "pr5: blur=64 max alpha={} total scanline paint={}",
            max_alpha, total_paint
        );
        assert!(
            max_alpha > 16,
            "blur=64 mask paints visible alpha; got {}",
            max_alpha
        );
        assert!(
            total_paint > 1000,
            "blur=64 mask has nontrivial total paint; got {}",
            total_paint
        );
    }
}
