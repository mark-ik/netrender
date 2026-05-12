/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Asset palette coordination for the C-architecture pattern: one
//! shared `vello::Scene` per frame composed from multiple consumers.
//!
//! [`FontRegistry`] and [`ImageRegistry`] let multiple consumers (or
//! multiple `push_layout` / `set_image_source` calls within one
//! consumer) share the asset slots in a target [`Scene`]. Without
//! them, every call would register a fresh `FontBlob` / `ImageData`
//! entry, bloating `scene.fonts` and `scene.image_sources` and
//! defeating vello's `Blob::id()`-keyed atlas dedup at the moment
//! tile-Scenes get appended into a master.
//!
//! Both registries are **consumer-side state**: pure HashMaps with
//! no GPU resources. Held across frames in the typical case (the
//! consumer keeps one of each on its renderer struct), or freshly
//! constructed per frame in the simpler case.
//!
//! ## When to use each
//!
//! - **`FontRegistry`** — pass it to
//!   [`netrender_text::push_layout`][nt-pl]. Same `parley::FontData`
//!   referenced by N glyph runs (across calls or layouts) registers
//!   exactly one [`FontId`] in the target Scene.
//! - **`ImageRegistry<K>`** — call [`ImageRegistry::intern`] when
//!   adding image data to a scene. Same `consumer_key` (whatever
//!   identity the consumer uses for the logical image — a path, a
//!   URL, an opaque token) returns the same [`ImageKey`] and
//!   inserts the data into the scene exactly once.
//!
//! ## Cross-consumer dedup
//!
//! Within one frame, two consumers building scenes that get
//! [composed into a master][compose-into] dedup at the vello atlas
//! level via `peniko::Blob::id()` *if and only if* they hand the
//! same `Arc`-shared bytes to vello. The registries make that easy:
//! share one `FontRegistry` / `ImageRegistry` between the two
//! consumers (e.g., own them on the workbench / app-level shared
//! state) and identical assets register exactly once across both.
//!
//! [nt-pl]: ../../netrender_text/fn.push_layout.html
//! [compose-into]: crate::vello_tile_rasterizer::VelloTileRasterizer::compose_into

use std::collections::HashMap;
use std::hash::Hash;

use crate::scene::{FontBlob, FontId, ImageData, ImageKey, Scene};

/// Cross-call font dedup. Threaded through
/// [`netrender_text::push_layout`][nt-pl]; identical
/// `(peniko::Blob::id(), font-collection index)` pairs register
/// exactly one [`FontId`] in the target [`Scene`] no matter how
/// many times the underlying font is referenced.
///
/// A consumer typically keeps one `FontRegistry` per long-lived
/// render context (per app, per workbench, per pane). Across
/// frames, calling [`Self::clear`] when the target Scene gets reset
/// ensures registry IDs stay valid.
///
/// [nt-pl]: ../../netrender_text/fn.push_layout.html
#[derive(Debug, Default)]
pub struct FontRegistry {
    by_id: HashMap<(u64, u32), FontId>,
}

impl FontRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get-or-insert. If the `(blob_id, index)` pair is new in this
    /// registry, registers `font` with `scene` and remembers the
    /// returned [`FontId`]; otherwise returns the cached `FontId`
    /// without touching `scene.fonts`.
    pub fn intern(&mut self, scene: &mut Scene, font: FontBlob) -> FontId {
        let key = (font.data.id(), font.index);
        *self
            .by_id
            .entry(key)
            .or_insert_with(|| scene.push_font(font))
    }

    /// Look up a previously-interned font without inserting. Returns
    /// `None` if the `(blob_id, index)` pair has not been seen.
    /// Useful when the consumer already has a `FontBlob` and wants
    /// to know whether interning would be a fresh insert.
    pub fn get(&self, blob_id: u64, index: u32) -> Option<FontId> {
        self.by_id.get(&(blob_id, index)).copied()
    }

    /// Number of distinct fonts currently tracked.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Drop all cached entries. Call when the target [`Scene`]'s
    /// `fonts` palette is reset (e.g., consumer rebuilds Scene from
    /// scratch); otherwise the registry's [`FontId`]s would point
    /// at stale slots.
    pub fn clear(&mut self) {
        self.by_id.clear();
    }
}

/// Cross-consumer image-key coordination. The consumer supplies a
/// key type `K` (a path string, a URL, a hash, an opaque token)
/// that uniquely identifies a logical image across the whole
/// registry's lifetime. Calling [`Self::intern`] with the same
/// `consumer_key` returns the same [`ImageKey`] every time, and
/// inserts the bytes into the target scene exactly once.
///
/// **Why a consumer-supplied key:** [`ImageData`] holds
/// `peniko::Blob<u8>`, which is `Arc<Vec<u8>>` plus an id. Two
/// `ImageData`s with identical content but built from separate
/// `Vec`s have distinct Blob ids — vello can't dedup them at the
/// atlas. The registry shifts dedup decisions to the consumer:
/// "these two scene-level `ImageKey`s should refer to the same
/// atlas slot" is a consumer-domain question (same URL? same
/// content hash?) we don't try to answer here.
///
/// `K = String` works for the common path-shaped case;
/// `K = u64` for hash-shaped; consumers can also use opaque
/// newtypes.
///
/// `ImageRegistry` only allocates fresh [`ImageKey`]s; it never
/// reuses across distinct `consumer_key`s. To recycle keys (e.g.,
/// after an image is fully evicted), reconstruct the registry.
#[derive(Debug)]
pub struct ImageRegistry<K> {
    by_consumer_key: HashMap<K, ImageKey>,
    next_key: ImageKey,
}

impl<K: Eq + Hash> Default for ImageRegistry<K> {
    fn default() -> Self {
        Self {
            by_consumer_key: HashMap::new(),
            // Start at 1; some consumer code uses 0 as a sentinel.
            next_key: 1,
        }
    }
}

impl<K: Eq + Hash> ImageRegistry<K> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get-or-insert. If `consumer_key` is new in this registry,
    /// allocates a fresh [`ImageKey`], inserts `data` into
    /// `scene.image_sources` under that key, and returns the new
    /// key. If `consumer_key` has been seen before, returns the
    /// cached [`ImageKey`] without touching `scene` (the data is
    /// already there from the first insert; subsequent calls don't
    /// need to re-supply it). The `data` argument on a hit is
    /// ignored — consumers can pass a placeholder if it's costly
    /// to construct, or use [`Self::get`] first.
    pub fn intern(&mut self, scene: &mut Scene, consumer_key: K, data: ImageData) -> ImageKey {
        if let Some(&ik) = self.by_consumer_key.get(&consumer_key) {
            return ik;
        }
        let ik = self.next_key;
        self.next_key = self
            .next_key
            .checked_add(1)
            .expect("ImageRegistry key counter overflow");
        scene.set_image_source(ik, data);
        self.by_consumer_key.insert(consumer_key, ik);
        ik
    }

    /// Look up a `consumer_key` without inserting. Returns the
    /// cached [`ImageKey`] if present, `None` otherwise.
    pub fn get(&self, consumer_key: &K) -> Option<ImageKey> {
        self.by_consumer_key.get(consumer_key).copied()
    }

    /// Number of distinct images currently tracked.
    pub fn len(&self) -> usize {
        self.by_consumer_key.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_consumer_key.is_empty()
    }

    /// Drop all cached entries. Call when the target [`Scene`]'s
    /// `image_sources` are reset; otherwise the registry's
    /// [`ImageKey`]s would point at slots that have been removed.
    pub fn clear(&mut self) {
        self.by_consumer_key.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{ImageData, Scene};
    use std::sync::Arc;
    use vello::peniko::Blob;

    fn dummy_font(payload: u8) -> FontBlob {
        FontBlob {
            data: Blob::new(Arc::new(vec![payload; 4])),
            index: 0,
        }
    }

    #[test]
    fn font_registry_dedups_within_call() {
        let mut scene = Scene::new(64, 64);
        let mut reg = FontRegistry::new();

        let font = dummy_font(1);
        let id1 = reg.intern(&mut scene, font.clone());
        let id2 = reg.intern(&mut scene, font.clone());
        assert_eq!(id1, id2);
        // Sentinel at slot 0 + one real font = 2 entries total.
        assert_eq!(scene.fonts.len(), 2);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn font_registry_separates_distinct_blobs() {
        let mut scene = Scene::new(64, 64);
        let mut reg = FontRegistry::new();

        let id1 = reg.intern(&mut scene, dummy_font(1));
        let id2 = reg.intern(&mut scene, dummy_font(2));
        assert_ne!(id1, id2, "different blob ids should get different FontIds");
        assert_eq!(scene.fonts.len(), 3);
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn font_registry_separates_distinct_indices() {
        let mut scene = Scene::new(64, 64);
        let mut reg = FontRegistry::new();

        // Same blob bytes, different collection indices (e.g., a
        // TTC carrying multiple faces).
        let blob = Blob::new(Arc::new(vec![1u8, 2, 3]));
        let id1 = reg.intern(
            &mut scene,
            FontBlob {
                data: blob.clone(),
                index: 0,
            },
        );
        let id2 = reg.intern(
            &mut scene,
            FontBlob {
                data: blob.clone(),
                index: 1,
            },
        );
        assert_ne!(id1, id2);
    }

    #[test]
    fn image_registry_dedups_by_consumer_key() {
        let mut scene = Scene::new(64, 64);
        let mut reg: ImageRegistry<String> = ImageRegistry::new();

        let img = ImageData::from_bytes(1, 1, vec![0xff, 0, 0, 0xff]);
        let k1 = reg.intern(&mut scene, "favicon.png".into(), img.clone());
        let k2 = reg.intern(&mut scene, "favicon.png".into(), img.clone());
        assert_eq!(k1, k2);
        assert_eq!(scene.image_sources.len(), 1);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn image_registry_allocates_distinct_keys_for_distinct_consumer_keys() {
        let mut scene = Scene::new(64, 64);
        let mut reg: ImageRegistry<&'static str> = ImageRegistry::new();

        let red = ImageData::from_bytes(1, 1, vec![0xff, 0, 0, 0xff]);
        let blue = ImageData::from_bytes(1, 1, vec![0, 0, 0xff, 0xff]);
        let k_red = reg.intern(&mut scene, "red", red);
        let k_blue = reg.intern(&mut scene, "blue", blue);
        assert_ne!(k_red, k_blue);
        assert_eq!(scene.image_sources.len(), 2);
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn image_registry_get_does_not_insert() {
        let reg: ImageRegistry<u64> = ImageRegistry::new();
        assert!(reg.get(&7).is_none());
        assert!(reg.is_empty());
    }
}
