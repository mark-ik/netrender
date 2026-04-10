/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Glyph rasterizer for wasm32 targets using fontdue.
//!
//! fontdue is a pure-Rust font parser and rasterizer that compiles to
//! wasm32-unknown-unknown without any native dependencies.  It handles
//! TrueType and OpenType fonts with reasonable quality.

use api::{FontKey, GlyphDimensions, NativeFontHandle};
use crate::rasterizer::{FontInstance, GlyphKey};
use crate::rasterizer::{GlyphFormat, GlyphRasterError, GlyphRasterResult, RasterizedGlyph};
use crate::types::FastHashMap;
use std::sync::Arc;

struct LoadedFont {
    font: fontdue::Font,
    _data: Arc<Vec<u8>>,
}

pub struct FontContext {
    fonts: FastHashMap<FontKey, LoadedFont>,
}

// SAFETY: FontContext is only accessed from the rasterizer thread / rayon pool.
unsafe impl Send for FontContext {}

impl FontContext {
    pub fn distribute_across_threads() -> bool {
        true
    }

    pub fn new() -> FontContext {
        FontContext {
            fonts: FastHashMap::default(),
        }
    }

    pub fn add_raw_font(&mut self, font_key: &FontKey, data: Arc<Vec<u8>>, index: u32) {
        if self.fonts.contains_key(font_key) {
            return;
        }

        let settings = fontdue::FontSettings {
            collection_index: index,
            ..Default::default()
        };
        match fontdue::Font::from_bytes(data.as_slice(), settings) {
            Ok(font) => {
                self.fonts.insert(*font_key, LoadedFont { font, _data: data });
            }
            Err(e) => {
                log::warn!("fontdue: failed to load font {:?}: {}", font_key, e);
            }
        }
    }

    pub fn add_native_font(&mut self, font_key: &FontKey, _font_handle: NativeFontHandle) {
        // Native font handles are not meaningful on wasm.
        // The caller should use add_raw_font with the font bytes instead.
        if !self.fonts.contains_key(font_key) {
            log::warn!("fontdue: add_native_font is a no-op on wasm, use add_raw_font instead");
        }
    }

    pub fn delete_font(&mut self, font_key: &FontKey) {
        self.fonts.remove(font_key);
    }

    pub fn delete_font_instance(&mut self, _instance: &FontInstance) {
        // No per-instance resources to clean up.
    }

    pub fn get_glyph_index(&mut self, font_key: FontKey, ch: char) -> Option<u32> {
        let loaded = self.fonts.get(&font_key)?;
        let index = loaded.font.lookup_glyph_index(ch);
        if index == 0 { None } else { Some(index as u32) }
    }

    pub fn get_glyph_dimensions(
        &mut self,
        font: &FontInstance,
        key: &GlyphKey,
    ) -> Option<GlyphDimensions> {
        let loaded = self.fonts.get(&font.base.font_key)?;
        let size_px = font.size.to_f32_px();
        let glyph_index = key.index() as u16;

        let metrics = loaded.font.metrics_indexed(glyph_index, size_px);
        if metrics.width == 0 || metrics.height == 0 {
            return None;
        }

        Some(GlyphDimensions {
            left: metrics.xmin,
            top: -metrics.ymin,
            width: metrics.width as i32,
            height: metrics.height as i32,
            advance: metrics.advance_width,
        })
    }

    pub fn prepare_font(_font: &mut FontInstance) {
        // No platform-specific preparation needed.
    }

    pub fn begin_rasterize(_font: &FontInstance) {
    }

    pub fn end_rasterize(_font: &FontInstance) {
    }

    pub fn rasterize_glyph(
        &mut self,
        font: &FontInstance,
        key: &GlyphKey,
    ) -> GlyphRasterResult {
        let loaded = match self.fonts.get(&font.base.font_key) {
            Some(f) => f,
            None => return Err(GlyphRasterError::LoadFailed),
        };

        let size_px = font.size.to_f32_px();
        let glyph_index = key.index() as u16;

        let (metrics, alpha_bitmap) = loaded.font.rasterize_indexed(glyph_index, size_px);
        if metrics.width == 0 || metrics.height == 0 {
            return Err(GlyphRasterError::LoadFailed);
        }

        // Convert alpha coverage to BGRA.  WebRender expects pre-multiplied
        // BGRA pixels for Alpha-format glyphs: B=a, G=a, R=a, A=a where a
        // is the coverage value.
        let pixel_count = metrics.width * metrics.height;
        let mut bgra = Vec::with_capacity(pixel_count * 4);
        for &a in &alpha_bitmap {
            bgra.push(a); // B
            bgra.push(a); // G
            bgra.push(a); // R
            bgra.push(a); // A
        }

        let padding = if font.use_texture_padding() { 1i32 } else { 0 };
        let padded_width = metrics.width as i32 + padding * 2;
        let padded_height = metrics.height as i32 + padding * 2;

        // Add zero-filled padding border around the glyph if requested.
        let bytes = if padding > 0 {
            let mut padded = vec![0u8; (padded_width * padded_height * 4) as usize];
            for y in 0..metrics.height {
                let src_row = y * metrics.width;
                let dst_row = ((y as i32 + padding) * padded_width + padding) as usize;
                for x in 0..metrics.width {
                    let a = alpha_bitmap[src_row + x];
                    let dst = (dst_row + x) * 4;
                    padded[dst] = a;     // B
                    padded[dst + 1] = a; // G
                    padded[dst + 2] = a; // R
                    padded[dst + 3] = a; // A
                }
            }
            padded
        } else {
            bgra
        };

        Ok(RasterizedGlyph {
            left: (metrics.xmin - padding) as f32,
            top: (-metrics.ymin + padding) as f32,
            width: padded_width,
            height: padded_height,
            scale: 1.0,
            format: font.get_alpha_glyph_format(),
            bytes,
        })
    }
}
