/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 3 scene representation — adds per-primitive transforms and
//! axis-aligned clip rectangles to the Phase 2 solid-rect baseline.
//! Phase 5 adds image primitives (textured rects).
//!
//! Design plan §5 Phase 3: "Lift `space.rs`, `spatial_tree.rs`,
//! `transform.rs` math from old webrender." Phase 3 uses 4×4
//! column-major matrices for generality; the 2D affine subset is the
//! initial surface (translate / rotate / scale helpers). Full spatial
//! tree hierarchy (parent → child reference chains) is deferred to the
//! later phase that ingests `BuiltDisplayList` spatial nodes.
//!
//! Backward compat: `Scene::push_rect` still works unchanged. The
//! transforms array always has index 0 = identity, so existing callers
//! that do not pass a `transform_id` render exactly as in Phase 2.

use std::collections::HashMap;

pub use netrender_device::GradientKind;
pub use netrender_device::SurfaceKey;

mod build;
mod debug;
mod elements;
mod fragment;
mod geometry;

pub use elements::*;
pub use fragment::*;
pub use geometry::*;

/// A flat list of primitives to be rendered into one frame.
///
/// Phase 3 adds `transforms` (a palette of 4×4 matrices) and per-rect
/// `transform_id` / `clip_rect`. Phase 4 sorts for correct depth order.
/// Phase 5 adds `images` (textured rects) and `image_sources` (pixel data).
///
/// **Painter order** (post-2026-05-04 op-list refactor): consumer
/// push order is the painter order. Every `push_*` helper appends a
/// `SceneOp` variant to `self.ops`; the rasterizer iterates `ops` in
/// sequence and dispatches per-variant. This replaces the previous
/// per-type `Vec<SceneRect>`, `Vec<SceneImage>`, … design where
/// painter order was fixed by type (rects → strokes → gradients →
/// images → shapes → glyph runs) regardless of push order. The old
/// design surfaced its limit in the `demo_card_grid` Card 6 probe:
/// a "badge" rect pushed after an image still painted under the
/// image. Op-list painter order makes consumer intent the source
/// of truth.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Scene {
    /// Viewport size in device pixels.
    pub viewport_width: u32,
    pub viewport_height: u32,
    /// Draw operations in painter order (back-to-front, push order).
    /// One entry per primitive; the rasterizer dispatches per
    /// variant. See [`SceneOp`].
    pub ops: Vec<SceneOp>,
    /// Phase 10a' font palette. Index `0` is reserved (panic on
    /// push_glyph_run with `font_id = 0`); real fonts start at
    /// index 1.
    pub fonts: Vec<FontBlob>,
    /// Phase 12a' scene-level alpha multiplier (`1.0` = unchanged,
    /// `0.0` = fully transparent). Implemented by wrapping the
    /// entire master scene in a `push_layer(blend, alpha, ...)`.
    /// Useful for whole-canvas fade transitions.
    pub root_alpha: f32,
    /// Phase 12a' scene-level blend mode. Default is
    /// [`SceneBlendMode::Normal`] (plain `source-over`); other
    /// values apply a `mix-blend-mode`-style composite over the
    /// `base_color` / target.
    pub root_blend_mode: SceneBlendMode,
    /// Transform palette. Index 0 is always identity.
    pub transforms: Vec<Transform>,
    /// CPU-side pixel data keyed by `ImageKey`. On first `prepare()`,
    /// each entry is uploaded to the GPU and cached there. Subsequent
    /// frames may omit data for already-cached keys.
    #[cfg_attr(feature = "serde", serde(with = "image_sources_serde"))]
    pub image_sources: HashMap<ImageKey, ImageData>,
    /// Native-compositor surfaces declared by the consumer. Order is
    /// z-order (first declared is bottom-most), matching the same
    /// "vec position = ordering" convention as `ops`. Read by
    /// `Renderer::render_with_compositor`; ignored by other render
    /// entry points.
    ///
    /// See
    /// [`netrender-notes/2026-05-05_compositor_handoff_path_b_prime.md`](../../netrender-notes/2026-05-05_compositor_handoff_path_b_prime.md)
    /// for the design.
    pub compositor_surfaces: Vec<CompositorSurface>,
}

/// Reserved id for the no-font sentinel `fonts[0]`. Picked at
/// `u64::MAX` so it never collides with peniko's
/// monotonically-incrementing counter (started at 0). See
/// `Scene::new` for the rationale.
const SENTINEL_FONT_BLOB_ID: u64 = u64::MAX;

fn sentinel_blob() -> vello::peniko::Blob<u8> {
    use std::sync::Arc;
    let arc: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(Vec::<u8>::new());
    vello::peniko::Blob::from_raw_parts(arc, SENTINEL_FONT_BLOB_ID)
}

/// Custom (de)serialization for `clip_rect: [f32; 4]` fields. The
/// in-memory `NO_CLIP` sentinel uses `±f32::INFINITY`, which JSON
/// represents as `null` (and refuses to deserialize back as `f32`).
/// Map `NO_CLIP` to `None` on the wire and any finite rect to
/// `Some([..])` so JSON round-trip is lossless. Postcard handles
/// infinities natively but uses the same encoding for symmetry.
#[cfg(feature = "serde")]
mod clip_rect_serde {
    use super::NO_CLIP;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(rect: &[f32; 4], ser: S) -> Result<S::Ok, S::Error> {
        if *rect == NO_CLIP {
            None::<[f32; 4]>.serialize(ser)
        } else {
            Some(*rect).serialize(ser)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<[f32; 4], D::Error> {
        let opt: Option<[f32; 4]> = Deserialize::deserialize(de)?;
        Ok(opt.unwrap_or(NO_CLIP))
    }
}

/// Custom (de)serialization for `peniko::Blob<u8>` fields that
/// **preserves the blob id across round-trip**. Peniko's built-in
/// serde impl serializes only the bytes and mints a fresh id on
/// deserialize via the global `ID_COUNTER`; we bypass that and emit
/// `(bytes, id)` so a captured Scene's atlas-dedup identity survives
/// a snapshot/replay cycle.
///
/// Note that `Blob::PartialEq` is id-based (peniko/blob.rs:54-58),
/// so preserving the id is also what makes
/// `assert_eq!(original, replayed)` work in receipt tests.
///
/// Caveat: an unrelated `Blob::new(same_bytes)` call elsewhere in
/// the process still mints a fresh counter id and will not equal a
/// captured Blob with the same bytes — that's by peniko design.
#[cfg(feature = "serde")]
mod blob_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::Arc;
    use vello::peniko::Blob;

    pub fn serialize<S: Serializer>(blob: &Blob<u8>, ser: S) -> Result<S::Ok, S::Error> {
        let bytes: &[u8] = blob.data();
        let id: u64 = blob.id();
        (bytes, id).serialize(ser)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Blob<u8>, D::Error> {
        let (bytes, id): (Vec<u8>, u64) = Deserialize::deserialize(de)?;
        let arc: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(bytes);
        Ok(Blob::from_raw_parts(arc, id))
    }
}

/// Custom (de)serialization for `Scene::image_sources` that emits a
/// **sorted Vec** of entries instead of relying on `HashMap`'s
/// non-deterministic iteration order. Without this, two snapshots of
/// the same Scene could produce different byte sequences, which
/// breaks the `snapshot → replay → snapshot` byte-equality determinism
/// receipt the roadmap calls for.
#[cfg(feature = "serde")]
mod image_sources_serde {
    use super::{ImageData, ImageKey};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;

    pub fn serialize<S: Serializer>(
        map: &HashMap<ImageKey, ImageData>,
        ser: S,
    ) -> Result<S::Ok, S::Error> {
        let mut entries: Vec<(&ImageKey, &ImageData)> = map.iter().collect();
        entries.sort_by_key(|(k, _)| **k);
        entries.serialize(ser)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        de: D,
    ) -> Result<HashMap<ImageKey, ImageData>, D::Error> {
        let entries: Vec<(ImageKey, ImageData)> = Deserialize::deserialize(de)?;
        Ok(entries.into_iter().collect())
    }
}

#[cfg(feature = "serde")]
impl Scene {
    /// Roadmap A2 — postcard binary snapshot. Small (no-overhead
    /// varint encoding) and fast; the right pick for production
    /// fixtures and high-volume replay. Blob ids are preserved (see
    /// `blob_serde`).
    ///
    /// Round-trip determinism: `replay_postcard(scene.snapshot_postcard())`
    /// produces a Scene whose own `snapshot_postcard()` is byte-equal
    /// to the original (modulo the iteration-order normalisation
    /// applied in `image_sources_serde`).
    pub fn snapshot_postcard(&self) -> Vec<u8> {
        postcard::to_allocvec(self)
            .expect("Scene::snapshot_postcard: serialization should not fail on owned data")
    }

    /// Roadmap A2 — postcard binary replay. Returns the deserialised
    /// Scene or a `postcard::Error` if the bytes are malformed /
    /// version-mismatched.
    pub fn replay_postcard(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }

    /// Roadmap A2 — JSON text snapshot. Roughly 3–5× larger than
    /// postcard but human-readable, cross-tool inspectable, and
    /// diff-friendly in git. Use for fixtures that benefit from being
    /// readable in code review or for cross-language consumers.
    pub fn snapshot_json(&self) -> String {
        serde_json::to_string(self)
            .expect("Scene::snapshot_json: serialization should not fail on owned data")
    }

    /// Roadmap A2 — JSON text replay. Returns the deserialised Scene
    /// or a `serde_json::Error` if the JSON is malformed.
    pub fn replay_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}
