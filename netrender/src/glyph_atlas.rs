/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 10a.1 glyph atlas — single R8Unorm `wgpu::Texture` with a
//! bump-row packer.
//!
//! Glyphs are uploaded once on first observation, keyed by [`GlyphKey`].
//! Subsequent frames reuse the existing slot. The atlas is held behind
//! a `Mutex` in `Renderer` so `prepare()` can mutate it from a `&self`
//! context, mirroring [`crate::image_cache::ImageCache`].
//!
//! Allocation strategy: bump-allocate row by row. `next_x` advances
//! horizontally; on horizontal overflow, wrap to a new row at
//! `next_y += current_row_height`. Vertical overflow panics — eviction
//! is a 10b sub-task. For the 10a.1 single-glyph receipt the default
//! 1024×1024 atlas is many orders of magnitude oversized.

use std::collections::HashMap;
use std::sync::Arc;

use crate::scene::{GlyphKey, GlyphRaster};

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

pub(crate) struct GlyphAtlas {
    width: u32,
    height: u32,
    texture: Arc<wgpu::Texture>,
    slots: HashMap<GlyphKey, GlyphSlot>,
    next_x: u32,
    next_y: u32,
    current_row_height: u32,
}

impl GlyphAtlas {
    /// Allocate an empty `width × height` R8Unorm atlas. Default
    /// dimensions are picked at construction so the atlas-size knob
    /// can land in `NetrenderOptions` later (10a.5) without touching
    /// the atlas API.
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = Arc::new(device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas (R8Unorm)"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
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
        }
    }

    pub fn texture(&self) -> Arc<wgpu::Texture> {
        self.texture.clone()
    }

    /// Return the slot for `key`, uploading `raster` on first
    /// observation. Subsequent calls with the same key ignore
    /// `raster`. Panics if `raster` doesn't fit in the atlas.
    pub fn get_or_upload(
        &mut self,
        key: GlyphKey,
        raster: &GlyphRaster,
        queue: &wgpu::Queue,
    ) -> GlyphSlot {
        if let Some(&slot) = self.slots.get(&key) {
            return slot;
        }
        let slot = self.allocate(raster);
        self.upload(slot, raster, queue);
        self.slots.insert(key, slot);
        slot
    }

    pub fn get(&self, key: GlyphKey) -> Option<GlyphSlot> {
        self.slots.get(&key).copied()
    }

    fn allocate(&mut self, raster: &GlyphRaster) -> GlyphSlot {
        let (w, h) = (raster.width, raster.height);
        // Wrap to a new row if this glyph won't fit horizontally.
        if self.next_x + w > self.width {
            self.next_y += self.current_row_height;
            self.next_x = 0;
            self.current_row_height = 0;
        }
        // Vertical overflow: fail loud. Eviction is 10b.
        assert!(
            self.next_y + h <= self.height,
            "glyph atlas full: needed ({w}, {h}) at ({}, {}) in {}×{}",
            self.next_x, self.next_y, self.width, self.height,
        );
        let x = self.next_x;
        let y = self.next_y;
        self.next_x += w;
        if h > self.current_row_height {
            self.current_row_height = h;
        }

        let aw = self.width as f32;
        let ah = self.height as f32;
        GlyphSlot {
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
        }
    }

    fn upload(&self, slot: GlyphSlot, raster: &GlyphRaster, queue: &wgpu::Queue) {
        // Recover the atlas-pixel origin from the slot's UV rect.
        let origin_x = (slot.uv_rect[0] * self.width as f32).round() as u32;
        let origin_y = (slot.uv_rect[1] * self.height as f32).round() as u32;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: origin_x, y: origin_y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &raster.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(slot.width),
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
