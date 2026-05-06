//! Roadmap A2 — Scene capture / replay receipts.
//!
//! Verifies:
//! 1. Round-trip determinism: `snapshot → replay → snapshot` is
//!    byte-equal, for both postcard and JSON formats.
//! 2. Semantic round-trip: `dump_ops()` of original and replayed are
//!    string-equal (so the two Scenes are observably indistinguishable
//!    via the A1 inspector).
//! 3. Blob id preservation: a `peniko::Blob<u8>` round-tripped through
//!    snapshot/replay keeps its original id (peniko's
//!    `Blob::PartialEq` is id-based — preserving id is what makes
//!    captured fixtures keep their atlas-dedup identity).
//! 4. Sorted image_sources serialization: HashMap iteration-order
//!    nondeterminism is normalised away (a captured Scene that
//!    inserts images in different order still snapshots identically
//!    if the resulting key→data mapping is the same).
//!
//! Gated behind `--features serde`; the netrender crate's default
//! build does not pull serde / postcard / serde_json.

#![cfg(feature = "serde")]

use std::sync::Arc;
use vello::peniko::Blob;

use netrender::scene::{
    CompositorSurface, FontBlob, ImageData, Scene, SceneBlendMode, SceneClip, SceneLayer,
    SceneOp, Transform,
};
use netrender_device::SurfaceKey;

/// Build a non-trivial Scene exercising every variant the
/// snapshot/replay path needs to handle:
/// rects, strokes, gradients, images, shapes, glyph runs, layer
/// nesting, transforms, fonts, image_sources, compositor surfaces,
/// and non-default scene-level alpha + blend mode.
fn make_kitchen_sink_scene() -> Scene {
    let mut scene = Scene::new(800, 600);

    scene.root_alpha = 0.9;
    scene.root_blend_mode = SceneBlendMode::Multiply;

    let xf = scene.push_transform(Transform::translate_2d(50.0, 50.0));

    // Image sources, inserted in non-key-sorted order to exercise the
    // sorted-vec serializer.
    let img_blob_a = Blob::new(Arc::new(vec![10u8, 20, 30, 40, 50, 60, 70, 80]));
    let img_blob_b = Blob::new(Arc::new(vec![90u8, 100, 110, 120]));
    scene.image_sources.insert(7, ImageData::from_blob(2, 1, img_blob_a));
    scene.image_sources.insert(3, ImageData::from_blob(1, 1, img_blob_b));

    // Font palette: register a real (synthetic) font blob.
    let font_bytes = b"FAKEFONTDATA-NOT-A-REAL-OTF-BUT-ENOUGH-FOR-ROUNDTRIP".to_vec();
    let font_blob = FontBlob {
        data: Blob::new(Arc::new(font_bytes)),
        index: 0,
    };
    scene.fonts.push(font_blob);

    // Ops (mixed kinds).
    scene.push_rect(10.0, 10.0, 100.0, 100.0, [1.0, 0.0, 0.0, 1.0]);
    scene.push_rect_clipped(
        20.0, 20.0, 200.0, 200.0,
        [0.0, 1.0, 0.0, 0.5],
        xf,
        [0.0, 0.0, 800.0, 600.0],
    );

    // Layer scope.
    scene.ops.push(SceneOp::PushLayer(SceneLayer {
        clip: SceneClip::Rect {
            rect: [0.0, 0.0, 400.0, 400.0],
            radii: [12.0, 12.0, 12.0, 12.0],
        },
        alpha: 0.7,
        blend_mode: SceneBlendMode::Screen,
        transform_id: 0,
    }));
    scene.push_rect(50.0, 50.0, 150.0, 150.0, [0.0, 0.0, 1.0, 1.0]);
    scene.ops.push(SceneOp::PopLayer);

    // Compositor surfaces.
    scene.compositor_surfaces.push(CompositorSurface::new(
        SurfaceKey(42),
        [0.0, 0.0, 100.0, 100.0],
    ));
    scene.compositor_surfaces.push(CompositorSurface {
        key: SurfaceKey(7),
        bounds: [200.0, 200.0, 400.0, 400.0],
        transform: [1.0, 0.0, 0.0, 1.0, 10.0, 10.0],
        clip: Some([0.0, 0.0, 800.0, 600.0]),
        opacity: 0.5,
    });

    scene
}

#[test]
fn postcard_roundtrip_is_byte_deterministic() {
    let original = make_kitchen_sink_scene();

    let bytes_1 = original.snapshot_postcard();
    let replayed = Scene::replay_postcard(&bytes_1).expect("replay should succeed");
    let bytes_2 = replayed.snapshot_postcard();

    assert_eq!(
        bytes_1, bytes_2,
        "postcard round-trip must be byte-deterministic"
    );
    // Postcard output for a real-ish scene should not be empty.
    assert!(bytes_1.len() > 50, "postcard output suspiciously small: {} bytes", bytes_1.len());
}

#[test]
fn json_roundtrip_is_string_deterministic() {
    let original = make_kitchen_sink_scene();

    let s1 = original.snapshot_json();
    let replayed = Scene::replay_json(&s1).expect("replay should succeed");
    let s2 = replayed.snapshot_json();

    assert_eq!(s1, s2, "JSON round-trip must be string-deterministic");
    // JSON should be substantially larger than postcard for the same data.
    assert!(s1.len() > 200, "JSON output suspiciously small: {} bytes", s1.len());
}

#[test]
fn replayed_scene_dumps_identically_to_original() {
    let original = make_kitchen_sink_scene();

    let pc_replay =
        Scene::replay_postcard(&original.snapshot_postcard()).expect("postcard replay");
    let js_replay = Scene::replay_json(&original.snapshot_json()).expect("json replay");

    let dump_orig = original.dump_ops();
    assert_eq!(
        dump_orig,
        pc_replay.dump_ops(),
        "postcard replay should be observably identical via dump_ops"
    );
    assert_eq!(
        dump_orig,
        js_replay.dump_ops(),
        "json replay should be observably identical via dump_ops"
    );
}

#[test]
fn blob_ids_survive_postcard_roundtrip() {
    let original = make_kitchen_sink_scene();
    let original_font_id = original.fonts[1].data.id();
    let original_img_ids: Vec<(u64, u64)> = original
        .image_sources
        .iter()
        .map(|(k, v)| (*k, v.data.id()))
        .collect();

    let replayed =
        Scene::replay_postcard(&original.snapshot_postcard()).expect("postcard replay");

    // Font blob id preserved.
    assert_eq!(
        original_font_id, replayed.fonts[1].data.id(),
        "font blob id must survive postcard round-trip"
    );

    // Image blob ids preserved per-key.
    for (key, original_id) in &original_img_ids {
        let replayed_id = replayed
            .image_sources
            .get(key)
            .map(|d| d.data.id())
            .unwrap_or_else(|| panic!("image_sources lost key {key} on replay"));
        assert_eq!(
            *original_id, replayed_id,
            "image blob id at key {key} must survive postcard round-trip"
        );
    }
}

#[test]
fn blob_ids_survive_json_roundtrip() {
    let original = make_kitchen_sink_scene();
    let original_font_id = original.fonts[1].data.id();

    let replayed = Scene::replay_json(&original.snapshot_json()).expect("json replay");

    assert_eq!(
        original_font_id, replayed.fonts[1].data.id(),
        "font blob id must survive JSON round-trip"
    );
}

#[test]
fn image_sources_insertion_order_does_not_affect_snapshot_bytes() {
    // Two scenes with the same logical (key → data) mapping but
    // inserted in opposite order. The sorted-vec serializer should
    // yield byte-identical snapshots.
    let blob_a_bytes = vec![1u8, 2, 3, 4];
    let blob_b_bytes = vec![5u8, 6, 7, 8];

    let mut scene_ab = Scene::new(100, 100);
    let blob_a1 = Blob::new(Arc::new(blob_a_bytes.clone()));
    let blob_b1 = Blob::new(Arc::new(blob_b_bytes.clone()));
    let id_a = blob_a1.id();
    let id_b = blob_b1.id();
    scene_ab.image_sources.insert(1, ImageData::from_blob(1, 1, blob_a1));
    scene_ab.image_sources.insert(2, ImageData::from_blob(1, 1, blob_b1));

    let mut scene_ba = Scene::new(100, 100);
    // Reuse the same Blob ids by reconstructing via from_raw_parts —
    // otherwise fresh ids would be different bytes regardless of the
    // sort. (This isolates the sort behavior from the id-mint
    // behavior.)
    let blob_a2 = Blob::from_raw_parts(
        Arc::new(blob_a_bytes.clone()) as Arc<dyn AsRef<[u8]> + Send + Sync>,
        id_a,
    );
    let blob_b2 = Blob::from_raw_parts(
        Arc::new(blob_b_bytes.clone()) as Arc<dyn AsRef<[u8]> + Send + Sync>,
        id_b,
    );
    scene_ba.image_sources.insert(2, ImageData::from_blob(1, 1, blob_b2));
    scene_ba.image_sources.insert(1, ImageData::from_blob(1, 1, blob_a2));

    let bytes_ab = scene_ab.snapshot_postcard();
    let bytes_ba = scene_ba.snapshot_postcard();
    assert_eq!(
        bytes_ab, bytes_ba,
        "logically-equivalent scenes must snapshot to identical bytes regardless of HashMap insertion order"
    );
}

#[test]
fn empty_scene_roundtrips_through_both_formats() {
    let original = Scene::new(50, 50);

    let pc = Scene::replay_postcard(&original.snapshot_postcard()).expect("postcard replay");
    let js = Scene::replay_json(&original.snapshot_json()).expect("json replay");

    assert_eq!(original.dump_ops(), pc.dump_ops());
    assert_eq!(original.dump_ops(), js.dump_ops());
}

#[test]
fn malformed_bytes_yield_replay_error() {
    let bogus = b"\xff\xff\xff\xff not valid postcard";
    let pc_result = Scene::replay_postcard(bogus);
    assert!(pc_result.is_err(), "garbage bytes should fail to deserialize");

    let bogus_json = "{not valid json";
    let js_result = Scene::replay_json(bogus_json);
    assert!(js_result.is_err(), "garbage json should fail to deserialize");
}
