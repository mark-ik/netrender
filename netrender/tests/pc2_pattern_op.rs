//! Roadmap C2 — `SceneOp::Pattern` (repeated-tile fill) receipts.
//!
//! Pure CPU. Verifies:
//!
//! 1. `Scene::push_pattern` appends a `SceneOp::Pattern` op with
//!    sane defaults (identity transform, no clip).
//! 2. The op carries the supplied `tile`, `extent`, and `scale`.
//! 3. `iter_*` filter helpers don't see Pattern as a Rect / Image
//!    (defensive sanity).
//! 4. Tile-cache hashing: changing `tile`, `extent`, or `scale`
//!    invalidates the tile.
//! 5. Hit testing: a point inside the pattern's `extent` rect
//!    registers a hit with `HitOpKind::Pattern`; outside misses.
//! 6. `dump_ops()` (A1 inspector) labels the new op as `Pattern`.

use std::sync::Arc;

use netrender::peniko::Blob;
use netrender::scene::{ImageData as NetImageData, Scene, SceneOp};
use netrender::tile_cache::TileCache;
use netrender::{hit_test_topmost, HitOpKind};

const TILE: u32 = 32;

fn img(seed: u8) -> NetImageData {
    NetImageData::from_blob(2, 2, Blob::new(Arc::new(vec![seed; 16])))
}

#[test]
fn pc2_push_pattern_appends_op() {
    let mut scene = Scene::new(256, 256);
    scene.image_sources.insert(7, img(10));
    scene.push_pattern(7, [0.0, 0.0, 256.0, 256.0], 1.0);

    match scene.ops.last().unwrap() {
        SceneOp::Pattern(p) => {
            assert_eq!(p.tile, 7);
            assert_eq!(p.extent, [0.0, 0.0, 256.0, 256.0]);
            assert_eq!(p.scale, 1.0);
            assert_eq!(p.transform_id, 0);
        }
        other => panic!("expected SceneOp::Pattern, got {other:?}"),
    }
}

#[test]
fn pc2_pattern_not_iterated_as_image_or_rect() {
    let mut scene = Scene::new(256, 256);
    scene.image_sources.insert(7, img(10));
    scene.push_pattern(7, [0.0, 0.0, 256.0, 256.0], 1.0);

    assert_eq!(scene.iter_images().count(), 0, "Pattern is not an Image");
    assert_eq!(scene.iter_rects().count(), 0, "Pattern is not a Rect");
}

fn dirty_count_after_change(decorate: impl FnOnce(&mut netrender::scene::ScenePattern)) -> usize {
    let mut scene = Scene::new(64, 64);
    scene.image_sources.insert(1, img(20));
    scene.push_pattern(1, [0.0, 0.0, 64.0, 64.0], 1.0);

    let mut cache = TileCache::new(TILE);
    let _ = cache.invalidate(&scene);
    let _ = cache.invalidate(&scene);

    if let SceneOp::Pattern(p) = scene.ops.last_mut().unwrap() {
        decorate(p);
    }
    cache.invalidate(&scene).len()
}

#[test]
fn pc2_changing_tile_key_invalidates_tile() {
    let dirty = dirty_count_after_change(|p| p.tile = 999);
    assert!(dirty > 0, "tile key change invalidates: {dirty}");
}

#[test]
fn pc2_changing_scale_invalidates_tile() {
    let dirty = dirty_count_after_change(|p| p.scale = 2.0);
    assert!(dirty > 0, "scale change invalidates: {dirty}");
}

#[test]
fn pc2_changing_extent_invalidates_tile() {
    let dirty = dirty_count_after_change(|p| p.extent = [10.0, 10.0, 50.0, 50.0]);
    assert!(dirty > 0, "extent change invalidates: {dirty}");
}

#[test]
fn pc2_unchanged_pattern_keeps_tiles_clean() {
    let mut scene = Scene::new(64, 64);
    scene.image_sources.insert(1, img(20));
    scene.push_pattern(1, [0.0, 0.0, 64.0, 64.0], 1.0);
    let mut cache = TileCache::new(TILE);
    let _ = cache.invalidate(&scene);
    assert_eq!(cache.invalidate(&scene).len(), 0);
}

#[test]
fn pc2_hit_inside_extent_registers_pattern_kind() {
    let mut scene = Scene::new(256, 256);
    scene.image_sources.insert(7, img(10));
    scene.push_pattern(7, [10.0, 10.0, 200.0, 200.0], 1.0);

    let inside = hit_test_topmost(&scene, [100.0, 100.0]);
    assert!(
        inside.is_some_and(|h| h.kind == HitOpKind::Pattern),
        "interior hit reports Pattern: {inside:?}"
    );
    let outside = hit_test_topmost(&scene, [5.0, 5.0]);
    assert!(outside.is_none(), "outside extent misses: {outside:?}");
}

#[test]
fn pc2_dump_ops_labels_pattern() {
    let mut scene = Scene::new(256, 256);
    scene.image_sources.insert(7, img(10));
    scene.push_pattern(7, [0.0, 0.0, 256.0, 256.0], 2.0);

    let dump = scene.dump_ops();
    assert!(dump.contains("Pattern"), "dump labels Pattern: {dump}");
    assert!(dump.contains("tile=7"), "dump shows tile id");
    assert!(dump.contains("scale=2"), "dump shows scale");
}
