/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 10a.2 / 10a.3 — pure-Rust glyph rasterization wrapper around
//! [`swash::scale::ScaleContext`].
//!
//! Two API shapes:
//!
//! - **One-shot** ([`RasterContext::rasterize`] /
//!   [`RasterContext::glyph_id_for_char`]). Takes raw `&[u8]` font
//!   bytes per call; re-parses the font header on every invocation.
//!   Right for tests and for consumers that hand in a different
//!   font on every call.
//! - **Bound** ([`RasterContext::bind`] → [`BoundRaster`]). Takes
//!   a [`FontHandle`] + a `(px_size, hint)` pair, parses the font
//!   header and builds the swash `Scaler` once, then reuses both
//!   across many [`BoundRaster::rasterize`] calls. Right for shaped
//!   text runs where many glyphs come from the same font at the
//!   same size.
//!
//! Per axiom 16 (external resources are local by the time they hit
//! the renderer), rasterization is fundamentally a consumer concern.
//! This module ships as a public convenience for the common-case
//! consumer that wants the canonical Rust outline + bitmap
//! rasterizer without having to wire it themselves; consumers with
//! their own rasterization stack (Parley, vello scene-as-atlas,
//! swash with custom hinting policy) just don't use it.
//!
//! Note (2026-05-02): swash 0.2.7 internally pulls
//! [`skrifa`](https://docs.rs/skrifa) — the Linebender font crate
//! that the design plan flagged as a future migration target. The
//! plan's "swap rasterizer behind a stable interface when Linebender
//! ships a skrifa-native rasterizer" is partly already resolved
//! upstream.

use std::sync::Arc;

use crate::scene::{GlyphKey, GlyphRaster};

/// Source priority used by [`RasterContext::rasterize`]. Outline first
/// (most TTF / OTF fonts ship vector glyphs); monochrome bitmap
/// strikes second (Proggy and other EBDT fonts); color bitmap strikes
/// third (Phase 10b will introduce a parallel color-aware path —
/// today the color bitmap is squashed into its alpha plane).
const SOURCE_PRIORITY: [swash::scale::Source; 3] = [
    swash::scale::Source::Outline,
    swash::scale::Source::Bitmap(swash::scale::StrikeWith::BestFit),
    swash::scale::Source::ColorBitmap(swash::scale::StrikeWith::BestFit),
];

/// Reusable rasterizer state. One per consumer thread; the `swash`
/// internals cache scaled outlines and other shape data inside the
/// scale context, so reusing one [`RasterContext`] across many
/// [`rasterize`](Self::rasterize) calls is faster than building a
/// fresh one per glyph.
pub struct RasterContext {
    inner: swash::scale::ScaleContext,
}

impl RasterContext {
    pub fn new() -> Self {
        Self { inner: swash::scale::ScaleContext::new() }
    }

    /// Rasterize one glyph at `px_size` from `font_bytes` (TTF / OTF /
    /// collection) at the given `font_index` (`0` for single-font
    /// files; per-face for `.ttc` / `.otc` collections). Returns
    /// `None` if the font fails to parse, the glyph is missing, or
    /// rendering fails.
    ///
    /// Sources are tried in order:
    ///
    /// 1. `Source::Outline` — vector glyphs (most TTF / OTF fonts).
    ///    `hint` enables TrueType hinting against the requested
    ///    pixel grid; recommended for small sizes.
    /// 2. `Source::Bitmap(BestFit)` — monochrome embedded bitmap
    ///    strikes (EBDT). Picks the closest-fit strike size.
    ///    Bitmap-only fonts (Proggy) hit this path.
    /// 3. `Source::ColorBitmap(BestFit)` — color emoji bitmap
    ///    strikes (CBDT). Forced into the alpha format below;
    ///    Phase 10b will introduce a parallel color-aware path.
    ///
    /// The output is always single-channel `R8` coverage (`zeno`
    /// `Format::Alpha`). Color emoji currently degrades to its
    /// alpha plane; preserving color requires the dedicated atlas
    /// + shader sub-task in Phase 10b.
    pub fn rasterize(
        &mut self,
        font_bytes: &[u8],
        font_index: u32,
        glyph_id: u16,
        px_size: f32,
        hint: bool,
    ) -> Option<GlyphRaster> {
        let font = swash::FontRef::from_index(font_bytes, font_index as usize)?;
        let mut scaler = self.inner
            .builder(font)
            .size(px_size)
            .hint(hint)
            .build();

        // Per-source iteration is the policy, not a workaround for it:
        // a `Render::new(&SOURCE_PRIORITY)` slice form short-circuits
        // at the first source whose `has_X()` table-presence gate
        // passes, but the gate doesn't check whether any glyph data
        // actually lives in that table. Proggy has empty outline
        // tables (gate passes) and a populated EBDT bitmap strike
        // (which the slice form never reaches). Iterating per source
        // and treating "succeeded with `(0, 0)` placement" as "this
        // source has no data for this glyph" routes correctly on
        // such fonts. Empty placement on every source is also the
        // correct return for legitimately-empty glyphs (space,
        // zero-width joiner, format-only chars) — the consumer
        // advances the pen via glyph metrics regardless.
        for source in &SOURCE_PRIORITY {
            let image = swash::scale::Render::new(std::slice::from_ref(source))
                .format(zeno::Format::Alpha)
                .render(&mut scaler, glyph_id);
            if let Some(image) = image {
                if image.placement.width > 0 && image.placement.height > 0 {
                    // swash's `placement.left` / `placement.top`
                    // follow the FreeType convention: `left` =
                    // pen-relative x of the bitmap's left edge;
                    // `top` = baseline-relative y of the bitmap's
                    // top edge (positive = up). These map straight
                    // into our [`GlyphRaster::bearing_x`] /
                    // `bearing_y`.
                    return Some(GlyphRaster {
                        width: image.placement.width,
                        height: image.placement.height,
                        bearing_x: image.placement.left,
                        bearing_y: image.placement.top,
                        pixels: image.data,
                    });
                }
            }
        }
        None
    }

    /// Look up the glyph id for `c` in the font's character map.
    /// Returns `None` if the font fails to parse; returns the font's
    /// `.notdef` glyph (typically id 0) when `c` is not mapped.
    pub fn glyph_id_for_char(
        &self,
        font_bytes: &[u8],
        font_index: u32,
        c: char,
    ) -> Option<u16> {
        let font = swash::FontRef::from_index(font_bytes, font_index as usize)?;
        Some(font.charmap().map(c))
    }

    /// Bind a [`FontHandle`] + size + hinting policy into a
    /// [`BoundRaster`] that can rasterize many glyphs without
    /// re-parsing the font header or rebuilding the swash `Scaler`.
    /// Right for shaped runs where many glyphs come from one font
    /// at one size; falls back to [`RasterContext::rasterize`] for
    /// one-shot calls.
    ///
    /// Returns `None` if the font fails to parse at the handle's
    /// `font_index`. The returned [`BoundRaster`] borrows both the
    /// `RasterContext` (mutably, for the cached scratch buffers) and
    /// the [`FontHandle`] (immutably, for the parsed bytes); both
    /// must outlive the bound raster.
    pub fn bind<'a>(
        &'a mut self,
        handle: &'a FontHandle,
        px_size: f32,
        hint: bool,
    ) -> Option<BoundRaster<'a>> {
        let font = swash::FontRef::from_index(
            handle.bytes(),
            handle.font_index() as usize,
        )?;
        let scaler = self.inner
            .builder(font)
            .size(px_size)
            .hint(hint)
            .build();
        Some(BoundRaster {
            scaler,
            font,
            font_id: handle.font_id(),
            px_size,
        })
    }
}

impl Default for RasterContext {
    fn default() -> Self {
        Self::new()
    }
}

/// Owned, cheap-to-clone reference to a font's parsed bytes.
///
/// `FontHandle` is the unit of font identity netrender consumes:
/// consumers load font bytes (mmap, asset bundle, network), wrap
/// them in an `Arc<[u8]>`, and tag the handle with a
/// caller-assigned `font_id`. The `font_id` is the same value used
/// in [`crate::GlyphKey::font_id`] — `BoundRaster` will produce
/// keys with this id automatically via
/// [`BoundRaster::key_for_glyph`].
///
/// Cloning is `Arc::clone` cheap; share one handle across many
/// runs / sizes / threads.
///
/// **Caller-assigned `font_id` invariant**: two distinct fonts
/// must never share a `font_id`, or atlas slots will collide
/// (glyph 'A' from font A might render as 'A' from font B). Today
/// netrender does not deduplicate or hash font bytes — that's a
/// 10b atlas-eviction-era concern. Until then, the consumer must
/// hand out unique ids per font (a monotonic counter is the
/// simplest correct policy).
#[derive(Clone)]
pub struct FontHandle {
    bytes: Arc<[u8]>,
    font_index: u32,
    font_id: u32,
}

impl FontHandle {
    /// Wrap an `Arc<[u8]>` of pre-loaded font bytes. `font_index` is
    /// `0` for single-font files; per-face for `.ttc` / `.otc`
    /// collections. `font_id` is caller-assigned and used as the
    /// `font_id` field of every [`crate::GlyphKey`] produced by a
    /// [`BoundRaster`] bound from this handle.
    pub fn new(bytes: Arc<[u8]>, font_index: u32, font_id: u32) -> Self {
        Self { bytes, font_index, font_id }
    }

    /// Convenience constructor for `&'static [u8]` (typical
    /// `include_bytes!` callers); copies once into an `Arc<[u8]>`.
    pub fn from_static(bytes: &'static [u8], font_index: u32, font_id: u32) -> Self {
        Self::new(Arc::from(bytes), font_index, font_id)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn font_index(&self) -> u32 {
        self.font_index
    }

    pub fn font_id(&self) -> u32 {
        self.font_id
    }
}

/// A swash `Scaler` bound to one font + size + hinting policy.
/// Returned by [`RasterContext::bind`]; rasterizes many glyphs
/// from the bound font without re-parsing the font header on each
/// call. Drops back to the [`RasterContext`] (and frees its mutable
/// borrow) when the bound raster goes out of scope.
pub struct BoundRaster<'a> {
    scaler: swash::scale::Scaler<'a>,
    /// Kept separately from the scaler for charmap lookups
    /// ([`Self::glyph_id_for_char`]). `FontRef<'_>` is `Copy`, so
    /// holding both does not duplicate state.
    font: swash::FontRef<'a>,
    font_id: u32,
    px_size: f32,
}

impl<'a> BoundRaster<'a> {
    /// Rasterize the glyph at `glyph_id` in the bound font. Uses
    /// the same source-priority + empty-placement-skip policy as
    /// the one-shot [`RasterContext::rasterize`].
    pub fn rasterize(&mut self, glyph_id: u16) -> Option<GlyphRaster> {
        for source in &SOURCE_PRIORITY {
            let image = swash::scale::Render::new(std::slice::from_ref(source))
                .format(zeno::Format::Alpha)
                .render(&mut self.scaler, glyph_id);
            if let Some(image) = image {
                if image.placement.width > 0 && image.placement.height > 0 {
                    return Some(GlyphRaster {
                        width: image.placement.width,
                        height: image.placement.height,
                        bearing_x: image.placement.left,
                        bearing_y: image.placement.top,
                        pixels: image.data,
                    });
                }
            }
        }
        None
    }

    /// Look up the glyph id for `c` in the bound font's character
    /// map. Returns the font's `.notdef` glyph (typically id 0)
    /// when `c` is not mapped.
    pub fn glyph_id_for_char(&self, c: char) -> u16 {
        self.font.charmap().map(c)
    }

    /// `font_id` of the [`FontHandle`] this raster was bound from.
    pub fn font_id(&self) -> u32 {
        self.font_id
    }

    /// Size this raster was bound at, in pixels.
    pub fn px_size(&self) -> f32 {
        self.px_size
    }

    /// Build a [`GlyphKey`] for the given glyph id, scoped to this
    /// raster's font + size. The `size_x64` field encodes
    /// `px_size * 64` (1/64th-pixel resolution, matching the
    /// FreeType / swash convention used elsewhere).
    pub fn key_for_glyph(&self, glyph_id: u16) -> GlyphKey {
        GlyphKey {
            font_id: self.font_id,
            glyph_id: glyph_id as u32,
            size_x64: (self.px_size * 64.0) as u32,
        }
    }

    /// Convenience: combine [`Self::glyph_id_for_char`] +
    /// [`Self::rasterize`] + [`Self::key_for_glyph`] into a single
    /// `(key, raster)` pair ready to feed [`crate::Scene::set_glyph_raster`].
    /// Returns `None` if the glyph rasterizes to an empty image
    /// (legitimately blank glyph, or no source data).
    pub fn rasterize_char(&mut self, c: char) -> Option<(GlyphKey, GlyphRaster)> {
        let gid = self.glyph_id_for_char(c);
        let raster = self.rasterize(gid)?;
        Some((self.key_for_glyph(gid), raster))
    }
}
