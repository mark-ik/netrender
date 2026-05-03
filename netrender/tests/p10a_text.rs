/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 10a.1 receipt — grayscale text via the renderer-owned
//! glyph atlas + `ps_text_run` pipeline.
//!
//! The test fixture is a hand-authored 5×7 'A' bitmap (no rasterizer
//! dependency). 10a.2 will replace it with `swash::Scaler`.
//!
//! Tests:
//!   p10a1_hand_authored_glyph     — golden: 'A' on transparent
//!   p10a1_pen_position_math       — assert the bitmap lands at the
//!                                   expected pen + bearing position
//!   p10a1_run_groups_glyphs       — two-glyph run shares z + color

use std::path::{Path, PathBuf};

use netrender::{
    ColorLoad, FrameTarget, GlyphInstance, GlyphKey, GlyphRaster, NetrenderOptions, Scene,
    boot, create_netrender_instance,
};

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
