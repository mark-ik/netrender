/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Layer composition: merge several paint outputs into one `Scene` with
//! per-layer id-namespace remapping.

use std::collections::HashMap;
use std::sync::Arc;

use netrender::{
    FontBlob, FontId, ImageData, ImageKey as NrImageKey, Scene, peniko,
};
use paint_list_api::{
    FontInstanceKey, FontResource, IdNamespace, ImageKey, ImageResource,
    PaintCmd,
};
use crate::translate_paint_cmd_stream;
use crate::TranslatedDisplayList;

/// First `IdNamespace` the compositor remaps layers into. Far above the small
/// per-producer namespaces (genet fonts 0, images 1; nematic/inker likewise low)
/// so a remapped key never aliases a value a producer might also be using in the
/// same merged stream. Layer `i` is remapped into `IdNamespace(BASE + i)`.
const COMPOSITE_NAMESPACE_BASE: u32 = 0xC000_0000;

/// One layer to composite, in back-to-front order (earlier layers paint behind
/// later ones). Borrows a producer's paint output — the producer (a [`PaintList`]
/// impl, or any source) stays the owner and exposes these via
/// `commands()`/`fonts()`/`images()`.
#[derive(Clone, Copy)]
pub struct CompositeLayer<'a> {
    pub commands: &'a [PaintCmd],
    pub fonts: &'a [FontResource],
    pub images: &'a [ImageResource],
}

impl<'a> CompositeLayer<'a> {
    /// A layer from an already-extracted command slice with no font/image
    /// side-tables (e.g. the orrery's scene-paint underlay, which emits only
    /// strokes + rects today).
    pub fn commands_only(commands: &'a [PaintCmd]) -> Self {
        Self { commands, fonts: &[], images: &[] }
    }
}

/// Composite several paint layers into one [`netrender::Scene`], back-to-front.
///
/// Each producer mints font/image keys in its own [`IdNamespace`], and two
/// producers can independently pick the same namespace (both default to 0), so
/// naively concatenating their side-tables would collide — the translator's
/// `FontInstanceKey -> FontId` / `ImageKey -> u64` maps would clobber one
/// producer's entry with another's. This remaps each layer's keys into a
/// composite-unique namespace and rewrites the matching references in that
/// layer's `DrawText` / `DrawImage` / `DrawRepeatingImage` commands, then
/// translates the merged stream against `viewport` (the shared final target).
/// Layers with no fonts/images pass through with their commands untouched.
pub fn composite_paint_layers(
    viewport: paint_list_api::DeviceIntSize,
    layers: &[CompositeLayer<'_>],
) -> TranslatedDisplayList {
    let (commands, fonts, images) = merge_layers(layers);
    translate_paint_cmd_stream(viewport, &commands, &fonts, &images)
}

/// Merge layers' commands + side-tables into one stream with collision-free keys.
/// Split out from [`composite_paint_layers`] so the remapping is unit-testable
/// without going through `Scene` lowering (and its font parsing).
pub(crate) fn merge_layers(
    layers: &[CompositeLayer<'_>],
) -> (Vec<PaintCmd>, Vec<FontResource>, Vec<ImageResource>) {
    let mut commands: Vec<PaintCmd> = Vec::new();
    let mut fonts: Vec<FontResource> = Vec::new();
    let mut images: Vec<ImageResource> = Vec::new();

    for (i, layer) in layers.iter().enumerate() {
        let ns = IdNamespace(COMPOSITE_NAMESPACE_BASE.wrapping_add(i as u32));

        // Remap this layer's font keys into `ns`, preserving the within-layer
        // index, and record old -> new for rewriting the command references.
        let mut font_remap: HashMap<FontInstanceKey, FontInstanceKey> = HashMap::new();
        for (j, fr) in layer.fonts.iter().enumerate() {
            let new_key = FontInstanceKey(ns, j as u32);
            font_remap.insert(fr.key, new_key);
            fonts.push(FontResource { key: new_key, data: fr.data.clone(), index: fr.index });
        }
        let mut image_remap: HashMap<ImageKey, ImageKey> = HashMap::new();
        for (j, ir) in layer.images.iter().enumerate() {
            let new_key = ImageKey(ns, j as u32);
            image_remap.insert(ir.key, new_key);
            images.push(ImageResource {
                key: new_key,
                width: ir.width,
                height: ir.height,
                data: ir.data.clone(),
            });
        }

        // Append commands. The common case (no fonts/images, e.g. the underlay)
        // needs no rewrite, so pass the slice through verbatim.
        if font_remap.is_empty() && image_remap.is_empty() {
            commands.extend_from_slice(layer.commands);
        } else {
            commands.extend(
                layer
                    .commands
                    .iter()
                    .map(|cmd| remap_cmd_keys(cmd, &font_remap, &image_remap)),
            );
        }
    }

    (commands, fonts, images)
}

/// Clone a command, rewriting any font/image key reference through the per-layer
/// remap. Only the three key-bearing variants carry a reference; everything else
/// is a verbatim clone.
fn remap_cmd_keys(
    cmd: &PaintCmd,
    fonts: &HashMap<FontInstanceKey, FontInstanceKey>,
    images: &HashMap<ImageKey, ImageKey>,
) -> PaintCmd {
    let mut c = cmd.clone();
    match &mut c {
        PaintCmd::DrawText(t) => {
            if let Some(&new_key) = fonts.get(&t.font_instance) {
                t.font_instance = new_key;
            }
        },
        PaintCmd::DrawImage(it) => {
            if let Some(&new_key) = images.get(&it.image_key) {
                it.image_key = new_key;
            }
        },
        PaintCmd::DrawRepeatingImage(it) => {
            if let Some(&new_key) = images.get(&it.image_key) {
                it.image_key = new_key;
            }
        },
        _ => {},
    }
    c
}

/// Register a paint list's font side-table into the scene's font
/// palette, returning the `FontInstanceKey → FontId` map that
/// `DrawText` lowering resolves through.
///
/// The `peniko::Blob` per font is cached process-wide, keyed by the byte
/// identity of the producer's shared `Arc` (the entry holds the `Arc`, so the
/// allocation is pinned and the pointer can't be reused while the entry
/// lives). Rewrapping in a fresh `Blob` per translate minted a fresh
/// vello-dedup id per FRAME, so every downstream font/glyph cache missed on
/// every frame; a stable `Blob` (its id survives clones) lets them hit.
/// Keying on byte identity rather than `FontInstanceKey` keeps the composite
/// path sound — it re-mints keys per merge, but reuses the producers' `Arc`s.
pub(crate) fn register_fonts(scene: &mut Scene, fonts: &[FontResource]) -> HashMap<FontInstanceKey, FontId> {
    static BLOB_CACHE: std::sync::Mutex<
        Option<HashMap<(usize, usize, u32), (FontBlob, Arc<Vec<u8>>)>>,
    > = std::sync::Mutex::new(None);
    let mut map = HashMap::new();
    for fr in fonts {
        let identity = (
            Arc::as_ptr(&fr.data) as *const u8 as usize,
            fr.data.len(),
            fr.index,
        );
        let blob = {
            let mut cache = BLOB_CACHE.lock().expect("font blob cache poisoned");
            let cache = cache.get_or_insert_with(HashMap::new);
            // A producer that re-allocates its font bytes per emit would grow a
            // pointer-keyed cache without bound; real faces number in the dozens,
            // so past this bound just reset (costs dedup quality, never bytes).
            if cache.len() > 256 {
                cache.clear();
            }
            let (blob, _pin) = cache
                .entry(identity)
                .or_insert_with(|| {
                    (
                        FontBlob {
                            data: peniko::Blob::new(fr.data.clone()),
                            index: fr.index,
                        },
                        fr.data.clone(),
                    )
                });
            blob.clone()
        };
        let font_id = scene.push_font(blob);
        map.insert(fr.key, font_id);
    }
    map
}

/// Register a paint list's image side-table into the scene's image
/// sources, returning the `ImageKey → netrender ImageKey` map that
/// `DrawImage` lowering resolves through. netrender's `ImageKey` is a
/// flat `u64`; paint-list's is `(IdNamespace, u32)`, so we fold the
/// namespace + index into the `u64` directly.
///
/// The scene key MUST be derived from the consumer's `ImageResource.key`,
/// not a per-render index: the rasterizer caches `scene_key → bytes` for
/// its whole lifetime and `debug_assert`s that a re-encountered key
/// carries identical bytes. A per-render `i + 1` made every render's
/// first image collide on scene key `1` with different bytes, poisoning
/// the rasterizer lock across a multi-render session (e.g. the WPT
/// reftest runner). Producers mint `ImageResource.key` unique per image,
/// so folding it in gives a stable, collision-free scene key.
pub(crate) fn register_images(scene: &mut Scene, images: &[ImageResource]) -> HashMap<ImageKey, NrImageKey> {
    let mut map = HashMap::new();
    for ir in images {
        // Fold (namespace, index) into the flat u64. `| 1 << 63` keeps
        // it clear of the reserved low keys (0, demo constants) and of a
        // pure-zero namespace+index.
        let nr_key = ((ir.key.0 .0 as u64) << 32) | (ir.key.1 as u64) | (1 << 63);
        scene.set_image_source(
            nr_key,
            ImageData::from_bytes(ir.width, ir.height, ir.data.clone()),
        );
        map.insert(ir.key, nr_key);
    }
    map
}
