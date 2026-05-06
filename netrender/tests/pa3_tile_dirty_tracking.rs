//! Roadmap A3 — tile-dirty visualizer receipts (CPU-side tracking).
//!
//! GPU-readback verification of the painted red wash is deferred (it
//! would need a full vello render + texture readback alongside other
//! GPU smoke tests). This file covers the algorithm-level pieces:
//!
//! 1. `Tile::last_dirty_frame` is set when a tile is reported dirty
//!    by `invalidate`, and stays put across frames where the tile is
//!    seen-but-unchanged.
//! 2. `TileCache::recent_dirty_tiles(window)` returns tiles dirtied
//!    within the last `window` invalidate calls, with age fractions
//!    that map linearly onto `[0.0, 1.0)`.
//! 3. Tiles never dirtied / aged-out / outside the viewport are
//!    excluded from the recent-dirty list.
//! 4. `window = 0` returns an empty Vec (escape valve for "overlay
//!    off" without re-checking the flag elsewhere).

use netrender::scene::Scene;
use netrender::tile_cache::TileCache;

const TILE_SIZE: u32 = 32;

/// Build a scene with one rect that's positioned to land in tile
/// `(col, row)` (using `TILE_SIZE`).
fn scene_with_rect_in_tile(col: i32, row: i32, color_seed: f32) -> Scene {
    let mut scene = Scene::new(128, 128);
    let x0 = (col * TILE_SIZE as i32) as f32 + 4.0;
    let y0 = (row * TILE_SIZE as i32) as f32 + 4.0;
    scene.push_rect(x0, y0, x0 + 8.0, y0 + 8.0, [color_seed, 0.0, 0.0, 1.0]);
    scene
}

#[test]
fn fresh_invalidate_dirties_every_visible_tile() {
    let mut cache = TileCache::new(TILE_SIZE);
    let scene = Scene::new(128, 128); // empty 4×4 grid of tiles
    let dirty = cache.invalidate(&scene);

    // 128/32 = 4, so 16 tiles all reported new.
    assert_eq!(dirty.len(), 16);

    // Every tile should have its last_dirty_frame == current_frame (1).
    let recent = cache.recent_dirty_tiles(8);
    assert_eq!(recent.len(), 16, "all 16 tiles dirtied this frame");
    for (_, age) in &recent {
        assert!(*age < f32::EPSILON, "age 0 expected for just-dirtied tiles, got {age}");
    }
}

#[test]
fn unchanged_tile_keeps_old_last_dirty_frame() {
    let mut cache = TileCache::new(TILE_SIZE);
    let scene = scene_with_rect_in_tile(1, 1, 1.0);

    // Frame 1: every tile is new, so all dirty.
    let _ = cache.invalidate(&scene);
    // Frame 2: same scene, no tiles change → all clean.
    let dirty = cache.invalidate(&scene);
    assert!(dirty.is_empty(), "unchanged scene should report no dirty tiles");

    // last_dirty_frame for every tile is still 1; current_frame is 2.
    // age = 1, so age_frac = 1/window.
    let recent = cache.recent_dirty_tiles(8);
    assert_eq!(recent.len(), 16, "all tiles still within 8-frame window");
    for (_, age) in &recent {
        let expected = 1.0 / 8.0;
        assert!(
            (age - expected).abs() < 1e-6,
            "expected age_frac {expected}, got {age}"
        );
    }
}

#[test]
fn dirty_tile_has_age_zero_in_recent_list() {
    let mut cache = TileCache::new(TILE_SIZE);
    let scene_a = scene_with_rect_in_tile(1, 1, 1.0);
    let scene_b = scene_with_rect_in_tile(1, 1, 0.5); // different color → different hash

    // Frame 1: bring everything online.
    let _ = cache.invalidate(&scene_a);
    // Frame 2: change tile (1, 1)'s content. The rect's AABB is
    // entirely inside that one tile, so only (1, 1) re-hashes.
    let dirty = cache.invalidate(&scene_b);
    assert_eq!(dirty, vec![(1, 1)], "only the modified tile reported");

    let recent = cache.recent_dirty_tiles(8);
    let recent_at_1_1: Vec<f32> = recent
        .iter()
        .filter(|(rect, _)| {
            let cx = (rect[0] + rect[2]) / 2.0;
            let cy = (rect[1] + rect[3]) / 2.0;
            // Tile (1, 1) covers world rect [32..64, 32..64]; its
            // center is (48, 48).
            (cx - 48.0).abs() < 1.0 && (cy - 48.0).abs() < 1.0
        })
        .map(|(_, age)| *age)
        .collect();
    assert_eq!(recent_at_1_1.len(), 1, "tile (1,1) found exactly once");
    assert!(recent_at_1_1[0] < f32::EPSILON, "tile (1,1) is freshly dirty: {:?}", recent_at_1_1[0]);
}

#[test]
fn aged_out_tiles_excluded_from_recent_list() {
    let mut cache = TileCache::new(TILE_SIZE);
    let scene = scene_with_rect_in_tile(0, 0, 1.0);

    // Frame 1 dirties everything. Then 4 calls with the same scene
    // keep them in the cache (RETAIN_FRAMES window) but they go clean.
    let _ = cache.invalidate(&scene);
    for _ in 0..3 {
        let _ = cache.invalidate(&scene);
    }
    // Now current_frame = 4 and last_dirty_frame = 1 for all tiles
    // (age = 3). A window of 3 should *exclude* them (3 >= 3).
    let recent = cache.recent_dirty_tiles(3);
    assert!(recent.is_empty(), "tiles dirtied 3 frames ago must be excluded by a 3-frame window");

    // A window of 4 should include them at age_frac = 3/4.
    let recent = cache.recent_dirty_tiles(4);
    assert_eq!(recent.len(), 16);
    for (_, age) in &recent {
        let expected = 3.0 / 4.0;
        assert!((age - expected).abs() < 1e-6, "expected {expected}, got {age}");
    }
}

#[test]
fn window_zero_returns_empty() {
    let mut cache = TileCache::new(TILE_SIZE);
    let scene = Scene::new(128, 128);
    let _ = cache.invalidate(&scene);
    assert!(
        cache.recent_dirty_tiles(0).is_empty(),
        "window 0 short-circuits to empty"
    );
}

#[test]
fn never_dirtied_tile_excluded() {
    // After only one invalidate, every tile in the cache has
    // last_dirty_frame == 1. There's no path where a tile lands in
    // tiles{} without being dirtied at insertion (the algorithm
    // marks "is_new" as dirty on first sight). So this test verifies
    // the *negation*: a freshly-constructed cache with NO invalidate
    // calls returns an empty recent_dirty_tiles regardless of window.
    let cache = TileCache::new(TILE_SIZE);
    assert!(cache.recent_dirty_tiles(100).is_empty());
}

#[test]
fn age_fraction_grows_linearly_with_frame_distance() {
    let mut cache = TileCache::new(TILE_SIZE);
    let scene = Scene::new(128, 128);
    // Frame 1: dirty all.
    let _ = cache.invalidate(&scene);

    // Step forward N frames with no changes; verify age = N/window
    // for every step within the window.
    let window = 10;
    for steps_after_dirty in 1..window {
        let _ = cache.invalidate(&scene);
        let recent = cache.recent_dirty_tiles(window as u64);
        let expected = steps_after_dirty as f32 / window as f32;
        for (_, age) in &recent {
            assert!(
                (age - expected).abs() < 1e-6,
                "step {steps_after_dirty}: expected {expected}, got {age}"
            );
        }
    }
}
