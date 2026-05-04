/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 10a.1 / 10a.2 / 10a.3 receipt — grayscale text via the
//! renderer-owned glyph atlas + `ps_text_run` pipeline.
//!
//! 10a.1 fixtures hand-author a 5×7 'A' bitmap (no rasterizer
//! dependency). 10a.2 fixtures rasterize the same letter from
//! `Proggy.ttf` via [`netrender::RasterContext`] (a thin
//! `swash::scale::ScaleContext` wrapper). 10a.3 fixtures use the
//! bound-raster API ([`netrender::FontHandle`] +
//! [`netrender::BoundRaster`]) so a multi-glyph run reuses one
//! parsed font + one swash `Scaler` across all of its glyphs.
//!
//! Tests:
//!   p10a1_hand_authored_glyph     — golden: 'A' on transparent
//!   p10a1_pen_position_math       — assert the bitmap lands at the
//!                                   expected pen + bearing position
//!   p10a1_run_groups_glyphs       — two-glyph run shares z + color
//!   p10a2_swash_glyph_nonempty    — sanity: Proggy 'A' rasterizes to
//!                                   a non-empty bitmap with at
//!                                   least one filled pixel
//!   p10a2_swash_glyph_renders     — golden: same Proggy 'A' pushed
//!                                   through the renderer pipeline
//!   p10a3_run_layout              — golden: 'AB' run via BoundRaster
//!                                   (one parse, one Scaler, two
//!                                   glyphs)

use std::path::{Path, PathBuf};
use std::sync::Arc;

use netrender::{
    ColorLoad, FontHandle, FrameTarget, GlyphFormat, GlyphInstance, GlyphKey, GlyphRaster,
    NetrenderOptions, RasterContext, Scene, boot, create_netrender_instance,
};

/// Proggy Clean — bitmap-only font, EBDT strike, included for the
/// 10a.2 swash receipt. Phase 0.5 preserved this on disk for exactly
/// this purpose.
const PROGGY_TTF: &[u8] = include_bytes!("../res/Proggy.ttf");

const VIEWPORT: u32 = 64;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

// ── Fixture: hand-authored 5×7 'A' ─────────────────────────────────

/// Build a 5-wide × 7-tall R8 coverage bitmap of 'A':
/// ```text
/// . # # # .
/// # . . . #
/// # . . . #
/// # # # # #
/// # . . . #
/// # . . . #
/// # . . . #
/// ```
/// `#` = 255 (full coverage), `.` = 0.
fn glyph_a_5x7() -> GlyphRaster {
    const W: u32 = 5;
    const H: u32 = 7;
    let rows = [
        b".###.",
        b"#...#",
        b"#...#",
        b"#####",
        b"#...#",
        b"#...#",
        b"#...#",
    ];
    let mut pixels = Vec::with_capacity((W * H) as usize);
    for row in &rows {
        for &b in row.iter() {
            pixels.push(if b == b'#' { 255 } else { 0 });
        }
    }
    assert_eq!(pixels.len(), (W * H) as usize);
    GlyphRaster {
        width: W,
        height: H,
        // Pen-relative metrics: glyph origin sits at the top-left of
        // the bitmap (bearing_x=0); the baseline is at the bottom of
        // the bitmap (bearing_y=H — every row is above baseline).
        bearing_x: 0,
        bearing_y: H as i32,
        format: GlyphFormat::Alpha,
        pixels,
    }
}

const KEY_A: GlyphKey = GlyphKey { font_id: 0, glyph_id: b'A' as u32, size_x64: 7 * 64 };

// ── Helpers (PNG + render runner) ──────────────────────────────────

fn oracle_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("oracle")
        .join("p10a")
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).expect("create oracle/p10a dir");
    let file = std::fs::File::create(path)
        .unwrap_or_else(|e| panic!("creating {}: {}", path.display(), e));
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().expect("png header");
    writer.write_image_data(rgba).expect("png pixels");
}

fn read_png(path: &Path) -> (u32, u32, Vec<u8>) {
    let file = std::fs::File::open(path)
        .unwrap_or_else(|e| panic!("opening {}: {}", path.display(), e));
    let dec = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = dec.read_info().expect("png read_info");
    let info = reader.info();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    let (w, h) = (info.width, info.height);
    let mut buf = vec![0u8; reader.output_buffer_size()];
    reader.next_frame(&mut buf).expect("png decode");
    (w, h, buf)
}

fn should_regen() -> bool {
    std::env::var("NETRENDER_REGEN").map_or(false, |v| v == "1")
}

fn render_scene(scene: &Scene) -> Vec<u8> {
    let [vw, vh] = [scene.viewport_width, scene.viewport_height];
    let handles = boot().expect("wgpu boot");
    let device = handles.device.clone();
    let renderer = create_netrender_instance(handles, NetrenderOptions::default())
        .expect("create_netrender_instance");

    let target_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p10a target"),
        size: wgpu::Extent3d { width: vw, height: vh, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target_tex.create_view(&wgpu::TextureViewDescriptor::default());

    let prepared = renderer.prepare(scene);
    renderer.render(
        &prepared,
        FrameTarget { view: &target_view, format: TARGET_FORMAT, width: vw, height: vh },
        ColorLoad::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
    );

    renderer.wgpu_device.read_rgba8_texture(&target_tex, vw, vh)
}

fn run_scene_golden(name: &str, scene: Scene) {
    let actual = render_scene(&scene);
    let oracle_path = oracle_dir().join(format!("{name}.png"));
    if should_regen() || !oracle_path.exists() {
        write_png(&oracle_path, scene.viewport_width, scene.viewport_height, &actual);
        println!("  captured oracle: {}", oracle_path.display());
        return;
    }

    let (ow, oh, oracle) = read_png(&oracle_path);
    assert_eq!((ow, oh), (scene.viewport_width, scene.viewport_height),
               "{name}: oracle size mismatch");
    assert_eq!(actual.len(), oracle.len(), "{name}: readback length mismatch");

    let mut diffs = 0usize;
    for (a, b) in actual.chunks_exact(4).zip(oracle.chunks_exact(4)) {
        if a != b {
            diffs += 1;
        }
    }
    assert_eq!(diffs, 0, "{name}: {diffs} pixels differ from oracle");
}

// ── Tests ──────────────────────────────────────────────────────────

/// Receipt: hand-authored 'A' renders at the expected pen position.
/// Pen at (10, 30) with `bearing_y = 7` puts the bitmap top-left at
/// (10, 23) and bottom-right at (15, 30). The glyph is white on a
/// transparent 64×64 background.
#[test]
fn p10a1_hand_authored_glyph() {
    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    scene.set_glyph_raster(KEY_A, glyph_a_5x7());
    scene.push_text_run(
        vec![GlyphInstance { key: KEY_A, x: 10.0, y: 30.0 }],
        [1.0, 1.0, 1.0, 1.0], // premultiplied white
    );
    run_scene_golden("p10a1_hand_authored_glyph", scene);
}

/// Programmatic check (no PNG): the rasterized 'A' should appear in
/// the expected pixel band, and the area outside should be the clear
/// color. Verifies pen + bearing math without depending on the
/// goldens tooling.
#[test]
fn p10a1_pen_position_math() {
    const PEN_X: f32 = 10.0;
    const PEN_Y: f32 = 30.0;
    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    scene.set_glyph_raster(KEY_A, glyph_a_5x7());
    scene.push_text_run(
        vec![GlyphInstance { key: KEY_A, x: PEN_X, y: PEN_Y }],
        [1.0, 1.0, 1.0, 1.0],
    );
    let pixels = render_scene(&scene);

    let stride = (VIEWPORT * 4) as usize;
    let pixel = |x: u32, y: u32| -> [u8; 4] {
        let i = (y as usize) * stride + (x as usize) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };

    // Pen-relative bitmap origin: x0=10, y0 = pen_y - bearing_y = 30 - 7 = 23.
    // Center of the 'A' crossbar is bitmap row 3 → device row 26.
    // All 5 columns of row 3 are filled (`#####`).
    for col in 0u32..5 {
        let p = pixel(10 + col, 26);
        assert!(p[0] > 200, "expected glyph pixel at ({}, 26): got {:?}", 10 + col, p);
    }

    // The hole between the verticals on row 1 (y=24): cols 1-3 of the
    // bitmap are zero. Device cols 11-13 must be transparent clear.
    for col in 1u32..4 {
        let p = pixel(10 + col, 24);
        assert_eq!(p, [0, 0, 0, 0],
                   "expected clear at hole pixel ({}, 24): got {:?}", 10 + col, p);
    }

    // Outside the bitmap: pixel (5, 5) must be the cleared background.
    assert_eq!(pixel(5, 5), [0, 0, 0, 0], "outside-bitmap pixel must be clear");

    // Outside the bitmap on the right: pixel (20, 27) must be clear.
    assert_eq!(pixel(20, 27), [0, 0, 0, 0], "right-of-bitmap pixel must be clear");
}

// ── 10a.2 — swash rasterization (Proggy.ttf) ───────────────────────


/// Sanity: rasterize 'A' from Proggy.ttf and confirm the bitmap is
/// non-empty and at least one pixel is filled. Independent of the
/// renderer pipeline — this is the cross-check that the swash
/// integration itself works before the golden test loads it through
/// `set_glyph_raster` / `push_text_run`.
#[test]
fn p10a2_swash_glyph_nonempty() {
    let mut ctx = RasterContext::new();

    let gid = ctx
        .glyph_id_for_char(PROGGY_TTF, 0, 'A')
        .expect("Proggy.ttf parses");
    assert_ne!(gid, 0, "Proggy must map a glyph for 'A' (got .notdef)");

    // Proggy Clean ships a 13-px ppem bitmap strike. swash's
    // BestFit picks that strike when we ask for 13 px; hint=false
    // since hinting only applies to outline glyphs (Proggy has none).
    let raster = ctx
        .rasterize(PROGGY_TTF, 0, gid, 13.0, false)
        .expect("rasterize 'A' from Proggy.ttf");

    assert!(
        raster.width > 0 && raster.height > 0,
        "rasterized 'A' has non-zero dimensions: {}x{}",
        raster.width,
        raster.height,
    );
    assert_eq!(
        raster.pixels.len(),
        (raster.width * raster.height) as usize,
        "pixels.len matches width*height (R8 single-channel)",
    );
    let filled = raster.pixels.iter().filter(|&&b| b > 0).count();
    assert!(
        filled > 0,
        "rasterized 'A' has at least one filled pixel (got {} of {})",
        filled,
        raster.pixels.len(),
    );

    // Sanity-check the metrics: 'A' at 13 px should be on the order
    // of 5-12 px wide and tall. Bracket loosely; the exact strike
    // sizes are font-specific and not the receipt's concern.
    assert!(
        (3..=20).contains(&raster.width),
        "'A' width plausible at 13 px: {}",
        raster.width,
    );
    assert!(
        (3..=20).contains(&raster.height),
        "'A' height plausible at 13 px: {}",
        raster.height,
    );
}

/// Golden: push the swash-rasterized 'A' through the full netrender
/// pipeline. Receipt that 10a.2's RasterContext output flows
/// unchanged into the same atlas + ps_text_run path that 10a.1
/// proved on a hand-authored bitmap.
#[test]
fn p10a2_swash_glyph_renders() {
    let mut ctx = RasterContext::new();
    let gid = ctx
        .glyph_id_for_char(PROGGY_TTF, 0, 'A')
        .expect("Proggy.ttf parses");
    let raster = ctx
        .rasterize(PROGGY_TTF, 0, gid, 13.0, false)
        .expect("rasterize 'A' from Proggy.ttf");

    let key = GlyphKey {
        font_id: 1, // distinct from KEY_A in 10a.1 tests
        glyph_id: gid as u32,
        size_x64: 13 * 64,
    };
    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    scene.set_glyph_raster(key, raster);
    scene.push_text_run(
        vec![GlyphInstance { key, x: 16.0, y: 32.0 }],
        [1.0, 1.0, 1.0, 1.0], // premultiplied white
    );
    run_scene_golden("p10a2_swash_glyph_renders", scene);
}

/// A two-glyph run shares the run's color and z. Render two adjacent
/// 'A's and verify both bitmaps appear.
#[test]
fn p10a1_run_groups_glyphs() {
    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    scene.set_glyph_raster(KEY_A, glyph_a_5x7());
    scene.push_text_run(
        vec![
            GlyphInstance { key: KEY_A, x: 10.0, y: 30.0 },
            GlyphInstance { key: KEY_A, x: 20.0, y: 30.0 },
        ],
        [1.0, 1.0, 1.0, 1.0],
    );
    let pixels = render_scene(&scene);
    let stride = (VIEWPORT * 4) as usize;
    let pixel = |x: u32, y: u32| -> [u8; 4] {
        let i = (y as usize) * stride + (x as usize) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    // Crossbar of first 'A' at device row 26.
    assert!(pixel(12, 26)[0] > 200, "first 'A' crossbar missing");
    // Crossbar of second 'A' at device row 26, offset by 10 px.
    assert!(pixel(22, 26)[0] > 200, "second 'A' crossbar missing");
    // Gap between glyphs at (16, 26): the first 'A' ended at col 14
    // and the second 'A' starts at col 20, so cols 15-19 row 26 are
    // clear background.
    assert_eq!(pixel(17, 26), [0, 0, 0, 0], "gap between glyphs must be clear");
}

// ── 10a.3 — bound-raster API (FontHandle + BoundRaster) ────────────

/// Sanity: a `BoundRaster` produces the same glyph data as one-shot
/// `RasterContext::rasterize` for the same font + size + glyph.
/// Confirms the bind path doesn't lose information vs. the
/// re-parse-per-call path.
#[test]
fn p10a3_bound_matches_oneshot() {
    let handle = FontHandle::from_static(PROGGY_TTF, 0, 2);
    let mut ctx_a = RasterContext::new();
    let mut ctx_b = RasterContext::new();

    // One-shot path
    let gid = ctx_a
        .glyph_id_for_char(handle.bytes(), handle.font_index(), 'A')
        .expect("Proggy parses (one-shot)");
    let oneshot = ctx_a
        .rasterize(handle.bytes(), handle.font_index(), gid, 13.0, false)
        .expect("rasterize 'A' (one-shot)");

    // Bound path
    let mut bound = ctx_b.bind(&handle, 13.0, false).expect("bind");
    let bound_gid = bound.glyph_id_for_char('A');
    assert_eq!(bound_gid, gid, "glyph id matches across paths");
    let from_bound = bound.rasterize(bound_gid).expect("rasterize 'A' (bound)");

    assert_eq!(from_bound.width, oneshot.width);
    assert_eq!(from_bound.height, oneshot.height);
    assert_eq!(from_bound.bearing_x, oneshot.bearing_x);
    assert_eq!(from_bound.bearing_y, oneshot.bearing_y);
    assert_eq!(from_bound.pixels, oneshot.pixels);
}

/// Golden: render a two-glyph 'AB' run rasterized through one
/// `BoundRaster` (single font parse, single Scaler build, two
/// glyphs). Exercises the shaped-run shape consumers will use for
/// real text — a vec of `GlyphInstance` keyed off the bound
/// raster's `key_for_glyph` helper.
#[test]
fn p10a3_run_layout() {
    let handle = FontHandle::new(Arc::from(PROGGY_TTF), 0, 3);
    let mut ctx = RasterContext::new();
    let mut bound = ctx.bind(&handle, 13.0, false).expect("bind Proggy");

    let (key_a, raster_a) = bound
        .rasterize_char('A')
        .expect("rasterize 'A'");
    let (key_b, raster_b) = bound
        .rasterize_char('B')
        .expect("rasterize 'B'");

    // Different glyphs in the same font + size share font_id and
    // size_x64, differ on glyph_id.
    assert_eq!(key_a.font_id, key_b.font_id);
    assert_eq!(key_a.size_x64, key_b.size_x64);
    assert_ne!(key_a.glyph_id, key_b.glyph_id, "A and B are different glyphs");

    // Drop the bound raster early so we can re-borrow ctx if the
    // test grows; not strictly necessary here.
    drop(bound);

    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    scene.set_glyph_raster(key_a, raster_a);
    scene.set_glyph_raster(key_b, raster_b);
    // Pen positions: 'A' at (12, 32), 'B' at (22, 32). Loose 10-px
    // advance — 10a.3 doesn't ship horizontal-metrics support, so
    // the test just hand-spaces the pen. Real consumers compute
    // advance from font metrics + shaping.
    scene.push_text_run(
        vec![
            GlyphInstance { key: key_a, x: 12.0, y: 32.0 },
            GlyphInstance { key: key_b, x: 22.0, y: 32.0 },
        ],
        [1.0, 1.0, 1.0, 1.0],
    );
    run_scene_golden("p10a3_run_layout", scene);
}

// ── 10a.5 — text in tiled scenes + atlas size option ──────────────

/// Render `scene` with arbitrary [`NetrenderOptions`]. Returns
/// framebuffer bytes. Generalisation of [`render_with_subpixel_aa`]
/// for tests that vary more than one option (10a.5: tile_cache_size,
/// glyph_atlas_size).
fn render_with_options(scene: &Scene, options: NetrenderOptions) -> Vec<u8> {
    let [vw, vh] = [scene.viewport_width, scene.viewport_height];
    let handles = boot().expect("wgpu boot");
    let device = handles.device.clone();
    let renderer = create_netrender_instance(handles, options)
        .expect("create_netrender_instance");

    let target_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p10a5 target"),
        size: wgpu::Extent3d { width: vw, height: vh, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target_tex.create_view(&wgpu::TextureViewDescriptor::default());

    let prepared = renderer.prepare(scene);
    renderer.render(
        &prepared,
        FrameTarget { view: &target_view, format: TARGET_FORMAT, width: vw, height: vh },
        ColorLoad::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
    );

    renderer.wgpu_device.read_rgba8_texture(&target_tex, vw, vh)
}

/// Receipt: a scene with text glyphs spread across multiple tiles
/// renders pixel-equivalent (within ±2/255 per Phase 7C convention)
/// through the direct path and the tile-cache path. Until 10a.5
/// wired text into `render_dirty_tiles_with_transforms`, the tile
/// path silently dropped text — so this test would have failed with
/// "everything but the background is missing" against a 10a.4 build.
///
/// Glyph layout — three Proggy 'A's at x = 5 / 25 / 45 with a 64-px
/// viewport and a 32-px tile size. The first two land in tile (0,0),
/// the third spans tile (0,0) and (1,0) along its left edge. Cross-
/// tile compositing is the whole point of the receipt.
#[test]
fn p10a5_text_tiled_matches_direct() {
    let mut scene_direct = Scene::new(VIEWPORT, VIEWPORT);
    scene_direct.set_glyph_raster(KEY_A, glyph_a_5x7());
    scene_direct.push_text_run(
        vec![
            GlyphInstance { key: KEY_A, x: 5.0, y: 30.0 },
            GlyphInstance { key: KEY_A, x: 25.0, y: 30.0 },
            GlyphInstance { key: KEY_A, x: 45.0, y: 30.0 },
        ],
        [1.0, 1.0, 1.0, 1.0],
    );
    let scene_tiled = scene_direct.clone();

    let direct = render_with_options(&scene_direct, NetrenderOptions::default());
    let tiled = render_with_options(
        &scene_tiled,
        NetrenderOptions {
            tile_cache_size: Some(32),
            ..NetrenderOptions::default()
        },
    );

    assert_eq!(direct.len(), tiled.len(), "framebuffer dimensions match");
    let differing: Vec<(usize, [u8; 4], [u8; 4])> = direct
        .chunks_exact(4)
        .zip(tiled.chunks_exact(4))
        .enumerate()
        .filter_map(|(i, (a, b))| {
            let exceeds = a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 2);
            if exceeds {
                Some((
                    i,
                    [a[0], a[1], a[2], a[3]],
                    [b[0], b[1], b[2], b[3]],
                ))
            } else {
                None
            }
        })
        .collect();
    assert!(
        differing.is_empty(),
        "{} pixels differ by more than ±2/255 between direct and tiled paths; \
         first divergence at index {}: direct={:?} tiled={:?}",
        differing.len(),
        differing.first().map(|(i, _, _)| *i).unwrap_or(0),
        differing.first().map(|(_, a, _)| *a).unwrap_or([0; 4]),
        differing.first().map(|(_, _, b)| *b).unwrap_or([0; 4]),
    );
}

/// Smoke: `NetrenderOptions::glyph_atlas_size = Some(N)` flows
/// through `init.rs` to `GlyphAtlas::new(N, N)`. Constructs a
/// renderer with a deliberately small atlas and renders a scene
/// containing the test glyph (5×7), well inside `64 × 64`. Glyph
/// upload + render path exercises the non-default size end-to-end.
#[test]
fn p10a5_glyph_atlas_size_option_honored() {
    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    scene.set_glyph_raster(KEY_A, glyph_a_5x7());
    scene.push_text_run(
        vec![GlyphInstance { key: KEY_A, x: 10.0, y: 30.0 }],
        [1.0, 1.0, 1.0, 1.0],
    );
    // 64-px atlas easily fits the 5×7 glyph; this confirms the
    // option is plumbed through without breaking smaller-than-default
    // configurations.
    let actual = render_with_options(
        &scene,
        NetrenderOptions {
            glyph_atlas_size: Some(64),
            ..NetrenderOptions::default()
        },
    );
    // Verify against the same scene through the default-size path.
    let expected = render_with_options(&scene, NetrenderOptions::default());
    assert_eq!(
        actual, expected,
        "atlas size {} produces same output as default {} for a glyph that fits both",
        64, "1024",
    );
}

// ── 10a.4 — subpixel-AA dual-source pipeline ───────────────────────

/// Render `scene` with the explicit `text_subpixel_aa` toggle and
/// return the framebuffer bytes. Bypasses [`run_scene_golden`]
/// because 10a.4's tests cross-compare two framebuffers rather than
/// matching a single golden PNG.
fn render_with_subpixel_aa(scene: &Scene, text_subpixel_aa: bool) -> Vec<u8> {
    let [vw, vh] = [scene.viewport_width, scene.viewport_height];
    let handles = boot().expect("wgpu boot");
    let device = handles.device.clone();
    let renderer = create_netrender_instance(
        handles,
        NetrenderOptions {
            text_subpixel_aa,
            ..NetrenderOptions::default()
        },
    )
    .expect("create_netrender_instance");

    let target_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p10a4 target"),
        size: wgpu::Extent3d { width: vw, height: vh, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target_tex.create_view(&wgpu::TextureViewDescriptor::default());

    let prepared = renderer.prepare(scene);
    renderer.render(
        &prepared,
        FrameTarget { view: &target_view, format: TARGET_FORMAT, width: vw, height: vh },
        ColorLoad::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
    );

    renderer.wgpu_device.read_rgba8_texture(&target_tex, vw, vh)
}

/// Did the booted device pick up `Features::DUAL_SOURCE_BLENDING`?
/// `core::boot` opportunistically requests every adapter-supported
/// feature in `OPTIONAL_FEATURES`; the test queries the post-boot
/// device to know which conditional path runs on this machine.
fn dual_source_supported() -> bool {
    let handles = boot().expect("wgpu boot");
    handles.device.features().contains(wgpu::Features::DUAL_SOURCE_BLENDING)
}

/// Sanity: `WgpuDevice::ensure_brush_text_dual_source` returns
/// `Some(_)` on adapters that expose `Features::DUAL_SOURCE_BLENDING`,
/// and `None` on adapters that don't. The cache holds the negative
/// result, so a second call returns the same `None` without
/// rebuilding.
#[test]
fn p10a4_dual_source_pipeline_built_when_supported() {
    let handles = boot().expect("wgpu boot");
    let supported = handles.device.features().contains(wgpu::Features::DUAL_SOURCE_BLENDING);
    let renderer =
        create_netrender_instance(handles, NetrenderOptions::default())
            .expect("create_netrender_instance");

    let first = renderer.wgpu_device.ensure_brush_text_dual_source(
        TARGET_FORMAT,
        wgpu::TextureFormat::Depth32Float,
    );
    let second = renderer.wgpu_device.ensure_brush_text_dual_source(
        TARGET_FORMAT,
        wgpu::TextureFormat::Depth32Float,
    );

    assert_eq!(
        first.is_some(),
        supported,
        "dual-source pipeline availability matches adapter feature: \
         supported={supported}, first.is_some={}",
        first.is_some(),
    );
    assert_eq!(
        first.is_some(),
        second.is_some(),
        "cache returns the same Option on repeat calls",
    );

    if !supported {
        println!(
            "  note: this adapter does not expose DUAL_SOURCE_BLENDING; \
             grayscale fallback is the only path exercised at runtime",
        );
    }
}

/// Equivalence: with the R8 atlas (10a.1) feeding both pipelines,
/// the dual-source path's per-channel coverage broadcast collapses
/// to the same blend equation as the grayscale path's
/// `PREMULTIPLIED_ALPHA_BLENDING`. Output should be byte-identical.
///
/// On adapters lacking `DUAL_SOURCE_BLENDING`, the renderer falls
/// back to grayscale silently (because the dual-source factory
/// returns `None`), so the two renders are trivially equal — the
/// test logs the skip but the equality assertion still holds.
#[test]
fn p10a4_grayscale_equivalence() {
    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    scene.set_glyph_raster(KEY_A, glyph_a_5x7());
    scene.push_text_run(
        vec![GlyphInstance { key: KEY_A, x: 10.0, y: 30.0 }],
        [1.0, 1.0, 1.0, 1.0],
    );

    let grayscale = render_with_subpixel_aa(&scene, false);
    let subpixel = render_with_subpixel_aa(&scene, true);

    if !dual_source_supported() {
        println!(
            "  note: adapter lacks DUAL_SOURCE_BLENDING; both renders \
             went through the grayscale path (trivial equality)",
        );
    }

    assert_eq!(
        grayscale.len(),
        subpixel.len(),
        "framebuffer dimensions match",
    );
    let differing = grayscale
        .chunks_exact(4)
        .zip(subpixel.chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        differing, 0,
        "grayscale-broadcast input must produce bit-identical output \
         under both pipelines (differing pixels: {differing})",
    );
}

/// `FontHandle` is `Clone`-cheap (Arc-backed): a clone shares the
/// underlying bytes. Test that two clones rasterize identically.
#[test]
fn p10a3_font_handle_is_arc_cheap() {
    let h1 = FontHandle::from_static(PROGGY_TTF, 0, 4);
    let h2 = h1.clone();

    // Same Arc-backed bytes — pointer equality on the slice
    // confirms no copy-on-clone.
    assert!(
        std::ptr::eq(h1.bytes().as_ptr(), h2.bytes().as_ptr()),
        "FontHandle::clone must share Arc-backed bytes (no copy)",
    );

    let mut ctx = RasterContext::new();
    let r1 = {
        let mut b = ctx.bind(&h1, 13.0, false).unwrap();
        b.rasterize_char('A').unwrap().1
    };
    let r2 = {
        let mut b = ctx.bind(&h2, 13.0, false).unwrap();
        b.rasterize_char('A').unwrap().1
    };
    assert_eq!(r1.pixels, r2.pixels);
}
