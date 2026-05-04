/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 10a' — text Scene API plumbing (no real font / GPU
//! rasterization yet; that's 10b').
//!
//! Receipts at the data-structure level: the API compiles, the
//! font palette indexes correctly, the tile cache hashes glyph
//! runs without panicking, and the dirty-tile-detection algorithm
//! reacts to glyph-run mutations.

use std::sync::Arc;

use netrender::{FontBlob, Glyph, Scene, TileCache};

#[test]
fn p10a_01_font_palette_starts_at_one() {
    let scene = Scene::new(64, 64);
    // Index 0 is the reserved no-font sentinel.
    assert_eq!(scene.fonts.len(), 1, "scene starts with the no-font sentinel");
    assert_eq!(scene.fonts[0].index, 0);
    assert!(scene.fonts[0].data.is_empty());
}

#[test]
fn p10a_02_push_font_returns_nonzero_id() {
    let mut scene = Scene::new(64, 64);
    let id_a = scene.push_font(FontBlob { data: Arc::new(vec![1, 2, 3]), index: 0 });
    let id_b = scene.push_font(FontBlob { data: Arc::new(vec![4, 5, 6]), index: 1 });
    assert_eq!(id_a, 1);
    assert_eq!(id_b, 2);
    assert_eq!(scene.fonts.len(), 3);
}

#[test]
fn p10a_03_push_glyph_run_storage() {
    let mut scene = Scene::new(64, 64);
    let id = scene.push_font(FontBlob { data: Arc::new(vec![0u8; 100]), index: 0 });
    scene.push_glyph_run(
        id,
        16.0,
        vec![
            Glyph { id: 10, x: 0.0, y: 16.0 },
            Glyph { id: 11, x: 8.0, y: 16.0 },
            Glyph { id: 12, x: 16.0, y: 16.0 },
        ],
        [0.0, 0.0, 0.0, 1.0],
    );
    assert_eq!(scene.glyph_runs.len(), 1);
    let run = &scene.glyph_runs[0];
    assert_eq!(run.font_id, 1);
    assert_eq!(run.font_size, 16.0);
    assert_eq!(run.glyphs.len(), 3);
    assert_eq!(run.glyphs[1].id, 11);
    assert_eq!(run.glyphs[1].x, 8.0);
}

#[test]
fn p10a_04_tile_cache_hashes_glyph_runs() {
    // Build a scene with one glyph run; verify TileCache::invalidate
    // doesn't panic when hashing the run (the hash function reads
    // font_id + glyph positions, no font data needed).
    let mut scene = Scene::new(64, 64);
    let id = scene.push_font(FontBlob { data: Arc::new(vec![0u8; 100]), index: 0 });
    scene.push_glyph_run(
        id,
        16.0,
        vec![Glyph { id: 1, x: 16.0, y: 16.0 }],
        [1.0, 0.0, 0.0, 1.0],
    );

    let mut tc = TileCache::new(32);
    let dirty = tc.invalidate(&scene);
    // Glyph at (16, 16) inflated by font_size = 16 → AABB
    // (0, 0)–(32, 32). With 32-pixel tiles, that's tile (0, 0).
    assert!(
        !dirty.is_empty(),
        "first frame with one glyph run should report dirty tiles"
    );
}

#[test]
fn p10a_05_changing_glyph_invalidates_tile() {
    // Render twice with the same glyph; second frame reports zero
    // dirty. Then change one glyph's position; the next frame
    // reports a non-empty dirty list.
    let id_font = 1u32;
    let mk_scene = |x: f32| {
        let mut s = Scene::new(64, 64);
        s.push_font(FontBlob { data: Arc::new(vec![0u8; 100]), index: 0 });
        s.push_glyph_run(
            id_font,
            16.0,
            vec![Glyph { id: 5, x, y: 16.0 }],
            [1.0, 1.0, 1.0, 1.0],
        );
        s
    };

    let mut tc = TileCache::new(32);
    let scene_a = mk_scene(16.0);
    let _ = tc.invalidate(&scene_a);
    let dirty_unchanged = tc.invalidate(&scene_a);
    assert_eq!(
        dirty_unchanged.len(),
        0,
        "re-invalidate with unchanged scene: zero dirty tiles"
    );

    let scene_b = mk_scene(20.0);  // moved 4px right
    let dirty_changed = tc.invalidate(&scene_b);
    assert!(
        !dirty_changed.is_empty(),
        "moved-glyph scene should report dirty tiles"
    );
}
