/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 10a.1 / 10b.1 / 10b.2 glyph atlas — single `Rgba8Unorm`
//! `wgpu::Texture` with a bump-row packer + LRU eviction.
//!
//! Glyphs are uploaded once on first observation, keyed by [`GlyphKey`].
//! Subsequent frames reuse the existing slot. The atlas is held behind
//! a `Mutex` in `Renderer` so `prepare()` can mutate it from a `&self`
//! context, mirroring [`crate::image_cache::ImageCache`].
//!
//! Storage format (10b.1): `Rgba8Unorm`. The atlas accepts both
//! [`GlyphFormat::Alpha`] (1 byte/pixel) and [`GlyphFormat::Subpixel`]
//! (3 bytes/pixel) rasters and expands them to RGBA8 on upload:
//! `Alpha` → `(c, c, c, 255)`, `Subpixel` → `(r, g, b, 255)`. The
//! grayscale `ps_text_run` shader still samples `.r` (correct for both
//! layouts because the broadcast preserves it); the dual-source
//! `ps_text_run_dual_source` shader samples `.rgb` and gets per-channel
//! LCD coverage on `Subpixel` glyphs and a bit-equivalent broadcast on
//! `Alpha` glyphs.
//!
//! Allocation strategy (10b.2): bump-allocate row by row. `next_x`
//! advances horizontally; on horizontal overflow, wrap to a new row at
//! `next_y += current_row_height`. On vertical overflow, evict the
//! least-recently-used slot (per-slot `last_used` frame stamp,
//! advanced by [`GlyphAtlas::begin_frame`] each time the renderer
//! starts a new frame), repack the survivors into a fresh bump-packer
//! pass on the same texture, and retry. Eviction loops until either
//! allocation succeeds or every surviving slot was touched in the
//! current frame — at that point the working set genuinely exceeds
//! the atlas size and we panic with a message pointing the consumer
//! at [`crate::NetrenderOptions::glyph_atlas_size`].
//!
//! Repack maintains atlas correctness: every glyph drawn this frame
//! keeps a valid (possibly relocated) [`GlyphSlot`] before the batch
//! builder reads slots in [`crate::renderer::Renderer::prepare_direct`].
//! Survivors get new UV rects on repack; consumers re-read via
//! [`GlyphAtlas::get`] every frame, so the relocation is invisible.
//!
//! Per-slot CPU-side raster cache: the atlas stores an
//! `Arc<GlyphRaster>` per slot so repack can re-upload without asking
//! the consumer to re-supply pixels. This roughly doubles the
//! atlas's memory footprint (CPU side mirrors GPU side) but matches
//! the design plan's "atlas eviction" carry-forward — consumers are
//! free to omit `Scene::set_glyph_raster` for already-cached keys
//! across frames, and the atlas's repack still succeeds.

use std::collections::HashMap;
use std::sync::Arc;

use crate::scene::{GlyphFormat, GlyphKey, GlyphRaster};

/// Where in the atlas one glyph lives. UV coordinates are pre-divided
/// by atlas extent so the batch builder can write them straight into
/// the per-instance `uv_rect` slot without re-knowing the atlas size.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GlyphSlot {
    /// Rasterized bitmap dimensions, in atlas pixels.
    pub width: u32,
    pub height: u32,
    /// Glyph metric: distance from the pen's x to the bitmap's left
    /// edge. Forwarded from [`GlyphRaster::bearing_x`] at upload time
    /// so the batch builder doesn't need to keep the raster around.
    pub bearing_x: i32,
    /// Glyph metric: distance from the pen's y (baseline) to the
    /// bitmap's top edge. Forwarded from [`GlyphRaster::bearing_y`].
    pub bearing_y: i32,
    /// Atlas UV [u0, v0, u1, v1] in normalized [0, 1] space.
    pub uv_rect: [f32; 4],
}

/// Per-slot internal record. Public callers receive [`GlyphSlot`]
/// (the renderer-facing subset, `Copy`); the atlas keeps the LRU
/// stamp and a CPU-side raster cache for repack.
struct SlotEntry {
    slot: GlyphSlot,
    last_used: u64,
    raster: Arc<GlyphRaster>,
}

pub(crate) struct GlyphAtlas {
    width: u32,
    height: u32,
    texture: Arc<wgpu::Texture>,
    slots: HashMap<GlyphKey, SlotEntry>,
    next_x: u32,
    next_y: u32,
    current_row_height: u32,
    /// Monotonic frame counter. Bumped by [`GlyphAtlas::begin_frame`]
    /// once per [`crate::renderer::Renderer::prepare`] call. Slot
    /// `last_used` values are compared against this to find the LRU
    /// candidate on overflow.
    current_frame: u64,
}

impl GlyphAtlas {
    /// Allocate an empty `width × height` `Rgba8Unorm` atlas. Default
    /// dimensions are picked at construction so the atlas-size knob in
    /// `NetrenderOptions` (10a.5) governs the texture extent without
    /// touching the atlas API.
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = Arc::new(device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas (Rgba8Unorm)"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        }));
        Self {
            width,
            height,
            texture,
            slots: HashMap::new(),
            next_x: 0,
            next_y: 0,
            current_row_height: 0,
            current_frame: 0,
        }
    }

    pub fn texture(&self) -> Arc<wgpu::Texture> {
        self.texture.clone()
    }

    /// Advance the LRU frame counter. The renderer calls this once
    /// at the top of [`crate::renderer::Renderer::prepare`] (both
    /// the direct and tiled paths) so every slot uploaded or re-hit
    /// inside that prepare bumps to the same `last_used` value.
    pub fn begin_frame(&mut self) {
        self.current_frame = self.current_frame.wrapping_add(1);
    }

    /// Return the slot for `key`, uploading `raster` on first
    /// observation. Subsequent calls with the same key ignore
    /// `raster` (the atlas's CPU-side cache is authoritative for
    /// repack) and bump the slot's `last_used` to the current frame.
    /// Triggers LRU eviction + repack on atlas overflow; panics only
    /// if the working set genuinely exceeds atlas size or `raster`
    /// is bigger than the whole atlas.
    pub fn get_or_upload(
        &mut self,
        key: GlyphKey,
        raster: &GlyphRaster,
        queue: &wgpu::Queue,
    ) -> GlyphSlot {
        if let Some(entry) = self.slots.get_mut(&key) {
            entry.last_used = self.current_frame;
            return entry.slot;
        }
        // Glyph genuinely too big for the atlas — eviction can't help.
        assert!(
            raster.width <= self.width && raster.height <= self.height,
            "glyph raster {}×{} larger than atlas {}×{}; raise \
             NetrenderOptions::glyph_atlas_size",
            raster.width, raster.height, self.width, self.height,
        );

        let raster_arc = Arc::new(raster.clone());
        let slot = self.allocate_or_evict(&raster_arc, queue);
        self.upload(slot, &raster_arc, queue);
        self.slots.insert(
            key,
            SlotEntry { slot, last_used: self.current_frame, raster: raster_arc },
        );
        slot
    }

    pub fn get(&self, key: GlyphKey) -> Option<GlyphSlot> {
        self.slots.get(&key).map(|e| e.slot)
    }

    /// Allocate a slot for `raster`, evicting LRU survivors until it
    /// fits. Panics if every surviving slot was used in the current
    /// frame and `raster` still doesn't fit (working-set-too-large).
    ///
    /// Worst-case cost: O(K × N) where N is the surviving slot count
    /// and K is the number of evictions needed to make room. Each
    /// iteration evicts one slot and repacks the remaining survivors,
    /// because bump-row packing can leave dead vertical space at the
    /// bottom row that one evicted slot doesn't always clear up.
    /// Acceptable while atlases are small and overflow is rare; a
    /// future skyline packer would amortise this.
    fn allocate_or_evict(
        &mut self,
        raster: &GlyphRaster,
        queue: &wgpu::Queue,
    ) -> GlyphSlot {
        loop {
            if let Some(slot) = self.try_allocate(raster) {
                return slot;
            }
            // Atlas is full. Find the LRU slot whose `last_used` is
            // strictly older than the current frame; evicting a
            // current-frame slot would leave a glyph drawn this frame
            // un-rasterized, which is a correctness bug.
            let evict_key = self
                .slots
                .iter()
                .filter(|(_, e)| e.last_used < self.current_frame)
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| *k);
            let evict_key = match evict_key {
                Some(k) => k,
                None => panic!(
                    "glyph atlas working set exceeds atlas size: every slot \
                     was used in the current frame ({} slots in {}×{}); raise \
                     NetrenderOptions::glyph_atlas_size",
                    self.slots.len(), self.width, self.height,
                ),
            };
            self.slots.remove(&evict_key);
            self.repack_survivors(queue);
        }
    }

    /// Attempt one bump-packer allocation. Mutates the cursor only
    /// on success; returns `None` if the requested rect doesn't fit
    /// in the remaining atlas space (caller decides whether to
    /// evict + retry).
    fn try_allocate(&mut self, raster: &GlyphRaster) -> Option<GlyphSlot> {
        let (w, h) = (raster.width, raster.height);
        let x;
        let y;
        if self.next_x + w > self.width {
            // Wrap to a new row.
            let new_y = self.next_y + self.current_row_height;
            if new_y + h > self.height {
                return None;
            }
            x = 0;
            y = new_y;
            self.next_x = w;
            self.next_y = new_y;
            self.current_row_height = h;
        } else {
            if self.next_y + h > self.height {
                return None;
            }
            x = self.next_x;
            y = self.next_y;
            self.next_x += w;
            if h > self.current_row_height {
                self.current_row_height = h;
            }
        }

        let aw = self.width as f32;
        let ah = self.height as f32;
        Some(GlyphSlot {
            width: w,
            height: h,
            bearing_x: raster.bearing_x,
            bearing_y: raster.bearing_y,
            uv_rect: [
                x as f32 / aw,
                y as f32 / ah,
                (x + w) as f32 / aw,
                (y + h) as f32 / ah,
            ],
        })
    }

    /// After dropping the LRU slot, rebuild the bump packer from
    /// scratch. Walk surviving entries in tallest-first order
    /// (lowest waste in the bump-row scheme), assign each a fresh
    /// slot, re-upload from the cached raster. Updates each
    /// `SlotEntry.slot` so subsequent `get` calls see the new UVs.
    fn repack_survivors(&mut self, queue: &wgpu::Queue) {
        self.next_x = 0;
        self.next_y = 0;
        self.current_row_height = 0;

        // `drain` empties the map; we re-insert as we go.
        let mut entries: Vec<(GlyphKey, SlotEntry)> = self.slots.drain().collect();
        // Tallest first packs the bump rows tightest — every
        // glyph in a row contributes max(row_height, glyph_height),
        // so equalising heights within a row minimises wasted vertical
        // space.
        entries.sort_by(|a, b| b.1.slot.height.cmp(&a.1.slot.height));

        for (key, mut entry) in entries {
            // After eviction the survivors must fit; if try_allocate
            // returns None here, the atlas configuration is genuinely
            // infeasible (e.g. one survivor is taller than the atlas)
            // and the assert at `get_or_upload` entry should have
            // caught it.
            let new_slot = self
                .try_allocate(&entry.raster)
                .expect("repacked survivor must fit after eviction");
            entry.slot = new_slot;
            self.upload(new_slot, &entry.raster, queue);
            self.slots.insert(key, entry);
        }
    }

    fn upload(&self, slot: GlyphSlot, raster: &GlyphRaster, queue: &wgpu::Queue) {
        // Recover the atlas-pixel origin from the slot's UV rect.
        let origin_x = (slot.uv_rect[0] * self.width as f32).round() as u32;
        let origin_y = (slot.uv_rect[1] * self.height as f32).round() as u32;

        let pixel_count = (slot.width as usize) * (slot.height as usize);
        let expected_bytes = pixel_count * raster.format.bytes_per_pixel() as usize;
        assert_eq!(
            raster.pixels.len(),
            expected_bytes,
            "glyph raster pixel count mismatch: format={:?} width={} height={} \
             expected {} bytes, got {}",
            raster.format, slot.width, slot.height, expected_bytes, raster.pixels.len(),
        );

        let mut rgba = vec![0u8; pixel_count * 4];
        match raster.format {
            GlyphFormat::Alpha => {
                // (c, c, c, 255) so both the grayscale shader (samples
                // `.r`) and the dual-source shader (samples `.rgb` for
                // per-channel coverage) see a coverage triple. The
                // alpha byte is decorative for both shaders — they
                // multiply by a premultiplied tint and ignore atlas
                // alpha — but uploading 255 keeps the texture viewable
                // in debug tools.
                for (dst, &c) in rgba.chunks_exact_mut(4).zip(raster.pixels.iter()) {
                    dst[0] = c;
                    dst[1] = c;
                    dst[2] = c;
                    dst[3] = 255;
                }
            }
            GlyphFormat::Subpixel => {
                // (r, g, b, 255) — the LCD coverage triple that
                // `swash::Format::Subpixel` produces. Three input
                // bytes per pixel, four output bytes per pixel.
                for (dst, src) in rgba.chunks_exact_mut(4).zip(raster.pixels.chunks_exact(3)) {
                    dst[0] = src[0];
                    dst[1] = src[1];
                    dst[2] = src[2];
                    dst[3] = 255;
                }
            }
        }

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: origin_x, y: origin_y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(slot.width * 4),
                rows_per_image: Some(slot.height),
            },
            wgpu::Extent3d {
                width: slot.width,
                height: slot.height,
                depth_or_array_layers: 1,
            },
        );
    }
}
