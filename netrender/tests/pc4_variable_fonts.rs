//! Roadmap C4 — variable-font axis interpolation receipts.
//!
//! Pure CPU. Verifies:
//!
//! 1. `SceneGlyphRun.font_axis_values` defaults to empty when
//!    constructed via the existing `push_glyph_run` helper.
//! 2. `push_glyph_run_variable` applies the supplied axis values.
//! 3. Changing `font_axis_values` invalidates the tile cache (the
//!    rendered weight changes, so the tile hash must change).
//!
//! Visual verification of "three weights in one frame look
//! distinct" requires a variable font on disk (e.g., Bahnschrift
//! on Windows, Roboto Flex on Linux). That GPU smoke lives at the
//! end of this file and skips vacuously when no VF is available.

use std::sync::Arc;

use netrender::peniko::Blob;
use netrender::scene::{Scene, SceneFontAxisTag, SceneOp};
use netrender::tile_cache::TileCache;
use netrender::{FontBlob, Glyph};

const TILE: u32 = 32;

fn make_scene_with_run(axis_values: Vec<(SceneFontAxisTag, f32)>) -> Scene {
    let mut scene = Scene::new(64, 64);
    let font_id = scene.push_font(FontBlob {
        // Empty bytes — only used for hash invariants here, not
        // GPU rendering.
        data: Blob::new(Arc::new(vec![0u8; 1])),
        index: 0,
    });
    scene.push_glyph_run_variable(
        font_id,
        16.0,
        vec![Glyph { id: 1, x: 8.0, y: 24.0 }],
        [1.0, 1.0, 1.0, 1.0],
        axis_values,
    );
    scene
}

#[test]
fn pc4_default_push_glyph_run_has_empty_axis_values() {
    let mut scene = Scene::new(64, 64);
    let font_id = scene.push_font(FontBlob {
        data: Blob::new(Arc::new(vec![0u8; 1])),
        index: 0,
    });
    scene.push_glyph_run(
        font_id,
        16.0,
        vec![Glyph { id: 1, x: 0.0, y: 16.0 }],
        [1.0, 1.0, 1.0, 1.0],
    );

    match scene.ops.last().unwrap() {
        SceneOp::GlyphRun(r) => {
            assert!(
                r.font_axis_values.is_empty(),
                "default push_glyph_run leaves axis values empty"
            );
        }
        other => panic!("expected GlyphRun, got {other:?}"),
    }
}

#[test]
fn pc4_push_glyph_run_variable_applies_axis_values() {
    let scene = make_scene_with_run(vec![(*b"wght", 700.0), (*b"wdth", 75.0)]);
    match scene.ops.last().unwrap() {
        SceneOp::GlyphRun(r) => {
            assert_eq!(r.font_axis_values.len(), 2);
            assert_eq!(r.font_axis_values[0], (*b"wght", 700.0));
            assert_eq!(r.font_axis_values[1], (*b"wdth", 75.0));
        }
        other => panic!("expected GlyphRun, got {other:?}"),
    }
}

#[test]
fn pc4_axis_value_change_invalidates_tile() {
    let mut scene = make_scene_with_run(vec![(*b"wght", 400.0)]);
    let mut cache = TileCache::new(TILE);
    let _ = cache.invalidate(&scene);
    let _ = cache.invalidate(&scene); // unchanged: 0 dirty

    if let SceneOp::GlyphRun(r) = scene.ops.last_mut().unwrap() {
        r.font_axis_values[0].1 = 700.0;
    }
    let dirty = cache.invalidate(&scene);
    assert!(
        !dirty.is_empty(),
        "wght value change should invalidate tiles, got {} dirty",
        dirty.len()
    );
}

#[test]
fn pc4_axis_tag_change_invalidates_tile() {
    let mut scene = make_scene_with_run(vec![(*b"wght", 400.0)]);
    let mut cache = TileCache::new(TILE);
    let _ = cache.invalidate(&scene);
    let _ = cache.invalidate(&scene);

    if let SceneOp::GlyphRun(r) = scene.ops.last_mut().unwrap() {
        r.font_axis_values[0].0 = *b"wdth";
    }
    let dirty = cache.invalidate(&scene);
    assert!(!dirty.is_empty(), "axis tag change invalidates: {}", dirty.len());
}

#[test]
fn pc4_adding_axis_value_invalidates_tile() {
    let mut scene = make_scene_with_run(Vec::new());
    let mut cache = TileCache::new(TILE);
    let _ = cache.invalidate(&scene);
    let _ = cache.invalidate(&scene);

    if let SceneOp::GlyphRun(r) = scene.ops.last_mut().unwrap() {
        r.font_axis_values.push((*b"wght", 700.0));
    }
    let dirty = cache.invalidate(&scene);
    assert!(!dirty.is_empty(), "adding axis values invalidates: {}", dirty.len());
}

#[test]
fn pc4_unchanged_axis_values_keep_tiles_clean() {
    let scene = make_scene_with_run(vec![(*b"wght", 700.0)]);
    let mut cache = TileCache::new(TILE);
    let _ = cache.invalidate(&scene);
    let dirty = cache.invalidate(&scene);
    assert_eq!(dirty.len(), 0, "unchanged axis values: no dirty tiles");
}

// ── GPU smoke (skipped without a variable font on disk) ───────────────

mod gpu_smoke {
    use std::sync::Arc;

    use netrender::peniko::Blob;
    use netrender::{
        boot, create_netrender_instance, ColorLoad, FontBlob, Glyph, NetrenderOptions, Scene,
    };

    const DIM: u32 = 256;
    const TILE_SIZE: u32 = 64;

    /// Try a list of paths likely to hold a variable Latin font.
    /// Returns bytes if any matches.
    fn try_load_variable_font() -> Option<(Vec<u8>, &'static str)> {
        let candidates: &[&str] = &[
            // Windows 10+: Bahnschrift is a variable font.
            r"C:\Windows\Fonts\bahnschrift.ttf",
            // Linux: Roboto Flex (sometimes installed).
            "/usr/share/fonts/truetype/roboto-flex/RobotoFlex.ttf",
            "/usr/share/fonts/roboto-flex/RobotoFlex.ttf",
        ];
        for path in candidates {
            if let Ok(bytes) = std::fs::read(path) {
                eprintln!("pc4: loaded {} ({} bytes)", path, bytes.len());
                return Some((bytes, *path));
            }
        }
        None
    }

    fn make_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pc4 target"),
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

    fn glyph_id_for_char(bytes: &[u8], ch: char) -> Option<u32> {
        use skrifa::MetadataProvider;
        let font = skrifa::FontRef::new(bytes).ok()?;
        Some(font.charmap().map(ch as u32)?.to_u32())
    }

    /// Render a single glyph at `wght = w` and return the row of
    /// pixels at the glyph's vertical center. Used to compare
    /// renders across weight values.
    fn render_glyph_at_weight(w: f32) -> Option<Vec<u8>> {
        let (font_bytes, _) = try_load_variable_font()?;
        let g_id = glyph_id_for_char(&font_bytes, 'B')?;

        let handles = boot().expect("wgpu boot");
        let device = handles.device.clone();
        let renderer = create_netrender_instance(
            handles.clone(),
            NetrenderOptions {
                tile_cache_size: Some(TILE_SIZE),
                enable_vello: true,
                ..Default::default()
            },
        )
        .expect("create_netrender_instance");

        let mut scene = Scene::new(DIM, DIM);
        let font_id = scene.push_font(FontBlob {
            data: Blob::new(Arc::new(font_bytes)),
            index: 0,
        });
        scene.push_glyph_run_variable(
            font_id,
            96.0,
            vec![Glyph { id: g_id, x: 32.0, y: 200.0 }],
            [1.0, 1.0, 1.0, 1.0],
            vec![(*b"wght", w)],
        );

        let (target, view) = make_target(&device);
        renderer.render_vello(&scene, &view, ColorLoad::Clear(wgpu::Color::BLACK));
        Some(renderer.wgpu_device.read_rgba8_texture(&target, DIM, DIM))
    }

    /// Sum the luminance of all painted pixels — a coarse but
    /// reliable signal for "how much ink the glyph painted." Heavier
    /// weights paint more ink, so the sum should monotonically
    /// increase with weight value.
    fn ink_load(bytes: &[u8]) -> u64 {
        bytes
            .chunks_exact(4)
            .map(|c| (c[0] as u64) + (c[1] as u64) + (c[2] as u64))
            .sum()
    }

    #[test]
    fn pc4_three_weights_produce_distinct_ink_loads() {
        let Some(light) = render_glyph_at_weight(300.0) else {
            eprintln!("pc4 GPU smoke: no variable font; skipping");
            return;
        };
        let regular = render_glyph_at_weight(400.0).expect("regular renders");
        let bold = render_glyph_at_weight(700.0).expect("bold renders");

        let light_ink = ink_load(&light);
        let regular_ink = ink_load(&regular);
        let bold_ink = ink_load(&bold);

        eprintln!(
            "pc4: ink load at wght=300: {}, wght=400: {}, wght=700: {}",
            light_ink, regular_ink, bold_ink
        );

        // Bold paints meaningfully more than light. Allow a generous
        // margin (10%) since some VFs have shallow weight ramps.
        assert!(
            bold_ink as f64 > light_ink as f64 * 1.1,
            "bold should paint visibly more ink than light: light={light_ink} bold={bold_ink}"
        );
        // Regular sits between light and bold (weak monotonicity —
        // axis-mapped renders aren't always strictly monotonic but
        // a flagrant violation would surface a wiring bug).
        assert!(
            regular_ink >= light_ink,
            "regular should not paint less than light: light={light_ink} regular={regular_ink}"
        );
        assert!(
            bold_ink >= regular_ink,
            "bold should not paint less than regular: regular={regular_ink} bold={bold_ink}"
        );
    }
}
