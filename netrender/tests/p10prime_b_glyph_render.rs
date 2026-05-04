/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 10b' — real-font GPU smoke test.
//!
//! Loads a system font (Arial on Windows, DejaVu / Liberation on
//! Linux), constructs a glyph run, runs it through render_vello,
//! and verifies pixels are painted.
//!
//! The test is **skipped vacuously** when no known font path exists
//! on the host (for CI hosts without bundled system fonts). Manual
//! runs on dev boxes verify the GPU rasterization end-to-end.
//!
//! Net netrender doesn't bundle a font (license / repo-size
//! tradeoff). When a consumer needs deterministic CI text
//! rendering, the right move is to bundle a tiny permissive font
//! (Roboto, Inter, etc.) under `tests/data/` per consumer
//! discretion.

use std::sync::Arc;

use netrender::{
    ColorLoad, FontBlob, Glyph, NetrenderOptions, Scene, boot, create_netrender_instance,
};

const DIM: u32 = 128;
const TILE_SIZE: u32 = 64;

/// Try a known list of system font paths. Returns the bytes of the
/// first one that exists, or `None` if nothing matched.
fn try_load_system_font() -> Option<Vec<u8>> {
    let candidates = [
        // Windows
        r"C:\Windows\Fonts\arial.ttf",
        r"C:\Windows\Fonts\segoeui.ttf",
        // macOS (won't exist on Windows but covers cross-host runs)
        "/System/Library/Fonts/Helvetica.ttc",
        "/Library/Fonts/Arial.ttf",
        // Linux (typical distros)
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            eprintln!("p10b: loaded {} ({} bytes)", path, bytes.len());
            return Some(bytes);
        }
    }
    None
}

fn make_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p10b target"),
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
        label: Some("p10b view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, view)
}

/// End-to-end smoke: load a font, push a glyph run, render, verify
/// at least *some* pixels were painted. We don't check specific
/// shapes (glyph indices are font-dependent), only that the
/// rasterization pipeline produced visible output.
#[test]
fn p10b_01_render_real_font_glyph() {
    let Some(font_bytes) = try_load_system_font() else {
        eprintln!("p10b_01: no system font found; skipping");
        return;
    };

    let handles = boot().expect("wgpu boot");
    let renderer = create_netrender_instance(
        handles.clone(),
        NetrenderOptions { tile_cache_size: Some(TILE_SIZE), enable_vello: true },
    )
    .expect("create_netrender_instance");

    let mut scene = Scene::new(DIM, DIM);
    let font_id = scene.push_font(FontBlob {
        data: Arc::new(font_bytes),
        index: 0,
    });

    // Glyph IDs are font-internal; for Arial / DejaVu / common
    // Latin fonts, IDs in the 30-50 range correspond to uppercase
    // letters. Push several at different positions; we only need
    // *any* of them to produce visible pixels for the smoke to
    // pass.
    let glyphs = vec![
        Glyph { id: 36, x: 16.0,  y: 64.0 },  // commonly 'A'
        Glyph { id: 37, x: 32.0,  y: 64.0 },  // commonly 'B'
        Glyph { id: 38, x: 48.0,  y: 64.0 },  // commonly 'C'
        Glyph { id: 36, x: 64.0,  y: 64.0 },
        Glyph { id: 36, x: 80.0,  y: 64.0 },
    ];

    scene.push_glyph_run(font_id, 32.0, glyphs, [1.0, 1.0, 1.0, 1.0]);

    let (target, view) = make_target(&handles.device);
    renderer.render_vello(&scene, &view, ColorLoad::Clear(wgpu::Color::BLACK));
    let bytes = renderer.wgpu_device.read_rgba8_texture(&target, DIM, DIM);

    // Count non-black pixels (background was cleared to black).
    // White text-on-black should produce thousands of paint pixels.
    let mut painted = 0usize;
    for chunk in bytes.chunks_exact(4) {
        // "Painted" = at least one of RGB above background black.
        if chunk[0] > 16 || chunk[1] > 16 || chunk[2] > 16 {
            painted += 1;
        }
    }

    eprintln!("p10b_01: painted {} non-background pixels (of {})", painted, DIM * DIM);
    assert!(
        painted > 100,
        "render_vello produced only {} painted pixels — expected hundreds for 5 glyphs at size 32",
        painted
    );
}
