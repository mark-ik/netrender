//! Roadmap C1 — stroke decorations (cap / join / dash) receipts.
//!
//! Pure CPU. Verifies the API surface and tile-cache integration:
//!
//! 1. New SceneStroke fields default sensibly when constructed via
//!    the existing `push_stroke*` helpers (Cap=Butt, Join=Miter,
//!    no dashes).
//! 2. `push_stroke_decorated` applies cap / join / dash from
//!    arguments.
//! 3. Changing `cap`, `join`, `dash_pattern`, or `dash_offset`
//!    invalidates the tile cache (cap and dash are visible
//!    geometry; the hash must include them).
//! 4. `SceneStrokeCap` / `SceneStrokeJoin` defaults match the
//!    documented values.
//!
//! Visual verification (the actual cap/join/dash rendering through
//! kurbo + vello) is covered by p11prime_a's existing receipts —
//! the netrender side is a thin pass-through.

use netrender::scene::{Scene, SceneOp, SceneStroke, SceneStrokeCap, SceneStrokeJoin};
use netrender::tile_cache::TileCache;

const TILE: u32 = 32;

#[test]
fn pc1_default_cap_is_butt_join_is_miter() {
    assert_eq!(SceneStrokeCap::default(), SceneStrokeCap::Butt);
    assert_eq!(SceneStrokeJoin::default(), SceneStrokeJoin::Miter);
}

#[test]
fn pc1_push_stroke_defaults_to_butt_miter_solid() {
    let mut scene = Scene::new(64, 64);
    scene.push_stroke(0.0, 0.0, 32.0, 32.0, [1.0, 0.0, 0.0, 1.0], 2.0);

    match scene.ops.last().unwrap() {
        SceneOp::Stroke(s) => {
            assert_eq!(s.cap, SceneStrokeCap::Butt);
            assert_eq!(s.join, SceneStrokeJoin::Miter);
            assert!(s.dash_pattern.is_empty());
            assert_eq!(s.dash_offset, 0.0);
        }
        other => panic!("expected SceneOp::Stroke, got {other:?}"),
    }
}

#[test]
fn pc1_push_stroke_decorated_applies_args() {
    let mut scene = Scene::new(64, 64);
    scene.push_stroke_decorated(
        0.0, 0.0, 32.0, 32.0,
        [1.0, 0.0, 0.0, 1.0],
        2.0,
        SceneStrokeCap::Round,
        SceneStrokeJoin::Bevel,
        vec![4.0, 2.0],
    );

    match scene.ops.last().unwrap() {
        SceneOp::Stroke(s) => {
            assert_eq!(s.cap, SceneStrokeCap::Round);
            assert_eq!(s.join, SceneStrokeJoin::Bevel);
            assert_eq!(s.dash_pattern, vec![4.0, 2.0]);
        }
        other => panic!("expected SceneOp::Stroke, got {other:?}"),
    }
}

/// Helper: build a 1-stroke scene with given decorations and run
/// `tile_cache.invalidate(scene)` twice. The first call dirties
/// every tile (initial); the second is the test signal — tiles are
/// dirty iff the stroke's hash changed across calls.
fn dirty_count_after_change(decorate: impl FnOnce(&mut SceneStroke)) -> usize {
    let mut scene = Scene::new(64, 64);
    scene.push_stroke(8.0, 8.0, 56.0, 56.0, [1.0, 0.0, 0.0, 1.0], 2.0);

    let mut cache = TileCache::new(TILE);
    let _ = cache.invalidate(&scene); // initial: all tiles dirty
    let _ = cache.invalidate(&scene); // unchanged: should report 0 dirty

    // Apply the change.
    if let SceneOp::Stroke(s) = scene.ops.last_mut().unwrap() {
        decorate(s);
    }
    cache.invalidate(&scene).len()
}

#[test]
fn pc1_changing_cap_invalidates_tile() {
    let dirty = dirty_count_after_change(|s| s.cap = SceneStrokeCap::Round);
    assert!(dirty > 0, "cap change should invalidate tiles, got {dirty}");
}

#[test]
fn pc1_changing_join_invalidates_tile() {
    let dirty = dirty_count_after_change(|s| s.join = SceneStrokeJoin::Bevel);
    assert!(dirty > 0, "join change should invalidate tiles, got {dirty}");
}

#[test]
fn pc1_adding_dash_pattern_invalidates_tile() {
    let dirty = dirty_count_after_change(|s| s.dash_pattern = vec![4.0, 2.0]);
    assert!(dirty > 0, "dash pattern change invalidates: {dirty}");
}

#[test]
fn pc1_changing_dash_offset_invalidates_tile() {
    // Set up with a dash pattern, then change just the offset.
    let mut scene = Scene::new(64, 64);
    scene.push_stroke_decorated(
        8.0, 8.0, 56.0, 56.0,
        [1.0, 0.0, 0.0, 1.0],
        2.0,
        SceneStrokeCap::Butt,
        SceneStrokeJoin::Miter,
        vec![4.0, 2.0],
    );
    let mut cache = TileCache::new(TILE);
    let _ = cache.invalidate(&scene);
    let _ = cache.invalidate(&scene);

    if let SceneOp::Stroke(s) = scene.ops.last_mut().unwrap() {
        s.dash_offset = 3.0;
    }
    let dirty = cache.invalidate(&scene);
    assert!(!dirty.is_empty(), "dash_offset change invalidates: {}", dirty.len());
}

#[test]
fn pc1_unchanged_decorations_keep_tiles_clean() {
    // Sanity check: pushing the same decorated stroke twice in a
    // row should report 0 dirty tiles on the second invalidate.
    let mut scene = Scene::new(64, 64);
    scene.push_stroke_decorated(
        8.0, 8.0, 56.0, 56.0,
        [1.0, 0.0, 0.0, 1.0],
        2.0,
        SceneStrokeCap::Round,
        SceneStrokeJoin::Bevel,
        vec![4.0, 2.0],
    );
    let mut cache = TileCache::new(TILE);
    let _ = cache.invalidate(&scene);
    let dirty = cache.invalidate(&scene);
    assert_eq!(dirty.len(), 0, "unchanged scene reports no dirty tiles");
}
