/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 7A — picture-cache invalidation algorithm.
//!
//! This module implements the *algorithm* portion of Phase 7: hashing
//! per-tile primitive dependencies, frame-stamping seen tiles, and
//! reporting which tiles changed between two `invalidate(scene)` calls.
//! Tile texture allocation + per-tile rendering land in Phase 7B; the
//! `Tile` struct intentionally has no `texture` field yet.
//!
//! Algorithm:
//!   1. Tick `current_frame`.
//!   2. For each tile in the viewport grid:
//!      a. Hash the per-prim state of every primitive whose world AABB
//!         intersects the tile, in painter order.
//!      b. Compare against the tile's `last_hash`. New or changed →
//!         report as dirty and update the cached hash.
//!      c. Mark the tile as seen this frame.
//!   3. Evict tiles whose `last_seen_frame` is more than `RETAIN_FRAMES`
//!      stale (Arc drop, wgpu reclaims memory in Phase 7B+).
//!
//! The hash uses `DefaultHasher` (SipHash-1-3) — fast, deterministic
//! within a process, and good enough to make collisions astronomically
//! unlikely for the kind of state that fits in a primitive struct.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::sync::Arc;

use crate::scene::{
    PathOp, Scene, SceneGlyphRun, SceneGradient, SceneImage, SceneRect, SceneShape, SceneStroke,
    Transform,
};

/// Integer (col, row) coordinate of a tile within the cache grid.
/// Tile (cx, cy) covers world rect `(cx*T, cy*T, (cx+1)*T, (cy+1)*T)`.
pub type TileCoord = (i32, i32);

/// Number of frames a tile may go un-touched before eviction.
const RETAIN_FRAMES: u64 = 4;

/// Sentinel value for a freshly-inserted tile's `last_hash`. Collision
/// with a real hash is harmless because new tiles are also detected via
/// `last_seen_frame == 0`.
const FRESH_HASH_SENTINEL: u64 = 0xDEAD_BEEF_DEAD_BEEF;

/// One tile's bookkeeping. Phase 7A stores the world rect, the last
/// computed dependency hash, and the frame stamp. Phase 7B adds an
/// `Arc<wgpu::Texture>` for the cached render output.
#[derive(Clone)]
pub(crate) struct Tile {
    /// World-space rect this tile covers. Read by 7B's per-tile
    /// projection builder.
    pub world_rect: [f32; 4],
    pub last_hash: u64,
    pub last_seen_frame: u64,
    /// Cached render output. `None` until the tile is first rendered;
    /// re-allocated on each dirty re-render (Arc drop releases the old
    /// texture's GPU memory).
    pub texture: Option<Arc<wgpu::Texture>>,
}

/// Picture-cache state. One instance per cached picture (Phase 7A ships
/// with one per `Renderer`; multi-picture caching lands when a concrete
/// consumer surfaces the need).
pub struct TileCache {
    tile_size: u32,
    pub(crate) tiles: HashMap<TileCoord, Tile>,
    current_frame: u64,
    dirty_count_last_invalidate: usize,
}

impl TileCache {
    /// Construct a new tile cache with the given square tile size in
    /// device pixels. `tile_size` must be > 0.
    pub fn new(tile_size: u32) -> Self {
        assert!(tile_size > 0, "tile_size must be > 0");
        Self {
            tile_size,
            tiles: HashMap::new(),
            current_frame: 0,
            dirty_count_last_invalidate: 0,
        }
    }

    pub fn tile_size(&self) -> u32 {
        self.tile_size
    }

    pub fn current_frame(&self) -> u64 {
        self.current_frame
    }

    /// Number of tiles reported dirty by the most recent `invalidate`
    /// call, or 0 if `invalidate` has never been called.
    pub fn dirty_count_last_invalidate(&self) -> usize {
        self.dirty_count_last_invalidate
    }

    /// Number of tiles currently cached (visited within `RETAIN_FRAMES`).
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// World-space rect for the named tile, or `None` if the tile is
    /// not in the cache.
    pub fn tile_world_rect(&self, coord: TileCoord) -> Option<[f32; 4]> {
        self.tiles.get(&coord).map(|t| t.world_rect)
    }

    /// Cached render texture for the named tile, or `None` if the tile
    /// has never been rendered (or has been evicted).
    pub fn tile_texture(&self, coord: TileCoord) -> Option<Arc<wgpu::Texture>> {
        self.tiles.get(&coord).and_then(|t| t.texture.clone())
    }

    /// Tick the frame counter, recompute each tile's dependency hash
    /// against `scene`, and return the list of tile coords whose
    /// dependencies changed (= need re-rendering in Phase 7B).
    ///
    /// Tiles outside the new viewport grid are evicted after
    /// `RETAIN_FRAMES` frames of absence.
    pub fn invalidate(&mut self, scene: &Scene) -> Vec<TileCoord> {
        self.current_frame += 1;
        let frame = self.current_frame;
        let tile_size = self.tile_size;

        let n_cols = (scene.viewport_width + tile_size - 1) / tile_size;
        let n_rows = (scene.viewport_height + tile_size - 1) / tile_size;

        let mut dirty = Vec::new();

        for row in 0..n_rows as i32 {
            for col in 0..n_cols as i32 {
                let coord = (col, row);
                let world_rect = [
                    (col * tile_size as i32) as f32,
                    (row * tile_size as i32) as f32,
                    ((col + 1) * tile_size as i32) as f32,
                    ((row + 1) * tile_size as i32) as f32,
                ];

                let new_hash = hash_tile_deps(scene, world_rect);

                let tile = self.tiles.entry(coord).or_insert(Tile {
                    world_rect,
                    last_hash: FRESH_HASH_SENTINEL,
                    last_seen_frame: 0,
                    texture: None,
                });

                let is_new = tile.last_seen_frame == 0;
                let changed = tile.last_hash != new_hash;

                if is_new || changed {
                    tile.last_hash = new_hash;
                    dirty.push(coord);
                }
                tile.last_seen_frame = frame;
            }
        }

        // Retain heuristic: evict tiles not seen recently.
        let cutoff = frame.saturating_sub(RETAIN_FRAMES);
        self.tiles.retain(|_, t| t.last_seen_frame > cutoff);

        self.dirty_count_last_invalidate = dirty.len();
        dirty
    }
}

// ── Hashing ────────────────────────────────────────────────────────────────

/// Hash the dependency state of every primitive intersecting `tile_rect`,
/// in painter order. Empty tiles get a deterministic empty-hasher value;
/// two empty tiles hash identically, so they're never spuriously dirty.
fn hash_tile_deps(scene: &Scene, tile_rect: [f32; 4]) -> u64 {
    let mut hasher = DefaultHasher::new();

    for rect in &scene.rects {
        let aabb = world_aabb_rect(rect, scene);
        if aabb_intersects(aabb, tile_rect) {
            hash_rect(&mut hasher, rect);
        }
    }
    for image in &scene.images {
        let aabb = world_aabb_image(image, scene);
        if aabb_intersects(aabb, tile_rect) {
            hash_image(&mut hasher, image);
        }
    }
    for grad in &scene.gradients {
        let aabb = world_aabb_gradient(grad, scene);
        if aabb_intersects(aabb, tile_rect) {
            hash_gradient(&mut hasher, grad);
        }
    }
    for stroke in &scene.strokes {
        let aabb = world_aabb_stroke(stroke, scene);
        if aabb_intersects(aabb, tile_rect) {
            hash_stroke(&mut hasher, stroke);
        }
    }
    for shape in &scene.shapes {
        if let Some(aabb) = world_aabb_shape(shape, scene) {
            if aabb_intersects(aabb, tile_rect) {
                hash_shape(&mut hasher, shape);
            }
        }
    }
    for run in &scene.glyph_runs {
        if let Some(aabb) = world_aabb_glyph_run(run, scene) {
            if aabb_intersects(aabb, tile_rect) {
                hash_glyph_run(&mut hasher, run);
            }
        }
    }

    hasher.finish()
}

fn hash_rect(h: &mut DefaultHasher, r: &SceneRect) {
    h.write_u32(r.x0.to_bits());
    h.write_u32(r.y0.to_bits());
    h.write_u32(r.x1.to_bits());
    h.write_u32(r.y1.to_bits());
    for c in r.color {
        h.write_u32(c.to_bits());
    }
    h.write_u32(r.transform_id);
    for c in r.clip_rect {
        h.write_u32(c.to_bits());
    }
    for c in r.clip_corner_radii {
        h.write_u32(c.to_bits());
    }
}

fn hash_image(h: &mut DefaultHasher, i: &SceneImage) {
    h.write_u32(i.x0.to_bits());
    h.write_u32(i.y0.to_bits());
    h.write_u32(i.x1.to_bits());
    h.write_u32(i.y1.to_bits());
    for c in i.uv {
        h.write_u32(c.to_bits());
    }
    for c in i.color {
        h.write_u32(c.to_bits());
    }
    h.write_u64(i.key);
    h.write_u32(i.transform_id);
    for c in i.clip_rect {
        h.write_u32(c.to_bits());
    }
    for c in i.clip_corner_radii {
        h.write_u32(c.to_bits());
    }
}

fn hash_glyph_run(h: &mut DefaultHasher, r: &SceneGlyphRun) {
    h.write_u32(r.font_id);
    h.write_u32(r.font_size.to_bits());
    h.write_usize(r.glyphs.len());
    for g in &r.glyphs {
        h.write_u32(g.id);
        h.write_u32(g.x.to_bits());
        h.write_u32(g.y.to_bits());
    }
    for c in r.color {
        h.write_u32(c.to_bits());
    }
    h.write_u32(r.transform_id);
    for c in r.clip_rect {
        h.write_u32(c.to_bits());
    }
    for c in r.clip_corner_radii {
        h.write_u32(c.to_bits());
    }
}

fn hash_shape(h: &mut DefaultHasher, s: &SceneShape) {
    h.write_usize(s.path.ops.len());
    for op in &s.path.ops {
        match *op {
            PathOp::MoveTo(x, y) => {
                h.write_u8(0);
                h.write_u32(x.to_bits()); h.write_u32(y.to_bits());
            }
            PathOp::LineTo(x, y) => {
                h.write_u8(1);
                h.write_u32(x.to_bits()); h.write_u32(y.to_bits());
            }
            PathOp::QuadTo(cx, cy, x, y) => {
                h.write_u8(2);
                h.write_u32(cx.to_bits()); h.write_u32(cy.to_bits());
                h.write_u32(x.to_bits()); h.write_u32(y.to_bits());
            }
            PathOp::CubicTo(c1x, c1y, c2x, c2y, x, y) => {
                h.write_u8(3);
                h.write_u32(c1x.to_bits()); h.write_u32(c1y.to_bits());
                h.write_u32(c2x.to_bits()); h.write_u32(c2y.to_bits());
                h.write_u32(x.to_bits()); h.write_u32(y.to_bits());
            }
            PathOp::Close => h.write_u8(4),
        }
    }
    if let Some(c) = s.fill_color {
        h.write_u8(1);
        for v in c { h.write_u32(v.to_bits()); }
    } else {
        h.write_u8(0);
    }
    if let Some(stroke) = s.stroke {
        h.write_u8(1);
        for v in stroke.color { h.write_u32(v.to_bits()); }
        h.write_u32(stroke.width.to_bits());
    } else {
        h.write_u8(0);
    }
    h.write_u32(s.transform_id);
    for c in s.clip_rect { h.write_u32(c.to_bits()); }
    for c in s.clip_corner_radii { h.write_u32(c.to_bits()); }
}

fn hash_stroke(h: &mut DefaultHasher, s: &SceneStroke) {
    h.write_u32(s.x0.to_bits());
    h.write_u32(s.y0.to_bits());
    h.write_u32(s.x1.to_bits());
    h.write_u32(s.y1.to_bits());
    for c in s.color {
        h.write_u32(c.to_bits());
    }
    h.write_u32(s.stroke_width.to_bits());
    for c in s.stroke_corner_radii {
        h.write_u32(c.to_bits());
    }
    h.write_u32(s.transform_id);
    for c in s.clip_rect {
        h.write_u32(c.to_bits());
    }
    for c in s.clip_corner_radii {
        h.write_u32(c.to_bits());
    }
}

fn hash_gradient(h: &mut DefaultHasher, g: &SceneGradient) {
    h.write_u32(g.x0.to_bits());
    h.write_u32(g.y0.to_bits());
    h.write_u32(g.x1.to_bits());
    h.write_u32(g.y1.to_bits());
    h.write_u32(g.kind.as_u32());
    for f in g.params {
        h.write_u32(f.to_bits());
    }
    // Stops contribute their offset + color in painter order.
    h.write_usize(g.stops.len());
    for stop in &g.stops {
        h.write_u32(stop.offset.to_bits());
        for c in stop.color {
            h.write_u32(c.to_bits());
        }
    }
    h.write_u32(g.transform_id);
    for c in g.clip_rect {
        h.write_u32(c.to_bits());
    }
    for c in g.clip_corner_radii {
        h.write_u32(c.to_bits());
    }
}

// ── Geometry ───────────────────────────────────────────────────────────────

/// World-space AABB of a primitive's `[x0, y0, x1, y1]` local rect
/// after applying the 2-D affine portion of `transforms[transform_id]`.
/// Identity transform (id 0) is a fast path that returns `local`
/// unchanged.
///
/// Used by the tile cache's per-tile dependency hash and by the
/// renderer's per-tile primitive filter (Phase 7B+). Conservative on
/// rotated rects (the AABB is larger than the rotated rect's true
/// bounds) — correct in both directions: over-marking dirty is safe,
/// over-including in a tile is safe (NDC clipping crops the extras).
pub(crate) fn world_aabb(local: [f32; 4], transform_id: u32, scene: &Scene) -> [f32; 4] {
    if transform_id == 0 {
        local
    } else {
        let t = &scene.transforms[transform_id as usize];
        transformed_aabb(local, t)
    }
}

fn world_aabb_rect(rect: &SceneRect, scene: &Scene) -> [f32; 4] {
    world_aabb([rect.x0, rect.y0, rect.x1, rect.y1], rect.transform_id, scene)
}

fn world_aabb_image(image: &SceneImage, scene: &Scene) -> [f32; 4] {
    world_aabb([image.x0, image.y0, image.x1, image.y1], image.transform_id, scene)
}

fn world_aabb_gradient(g: &SceneGradient, scene: &Scene) -> [f32; 4] {
    world_aabb([g.x0, g.y0, g.x1, g.y1], g.transform_id, scene)
}

pub(crate) fn world_aabb_glyph_run(r: &SceneGlyphRun, scene: &Scene) -> Option<[f32; 4]> {
    if r.glyphs.is_empty() {
        return None;
    }
    // Conservative AABB: bounding box of glyph origins inflated by
    // ±font_size on each axis to cover ascenders / descenders /
    // wide glyph extents. Real per-glyph bounds need font metrics
    // we don't load here; this overestimates safely.
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for g in &r.glyphs {
        if g.x < min_x { min_x = g.x; }
        if g.y < min_y { min_y = g.y; }
        if g.x > max_x { max_x = g.x; }
        if g.y > max_y { max_y = g.y; }
    }
    let pad = r.font_size;
    let inflated = [min_x - pad, min_y - pad, max_x + pad, max_y + pad];
    Some(world_aabb(inflated, r.transform_id, scene))
}

pub(crate) fn world_aabb_shape(s: &SceneShape, scene: &Scene) -> Option<[f32; 4]> {
    let local = s.path.local_aabb()?;
    // Inflate by half stroke width if a stroke is present, same
    // reasoning as world_aabb_stroke.
    let half = s.stroke.map_or(0.0, |st| st.width * 0.5);
    let inflated = [local[0] - half, local[1] - half, local[2] + half, local[3] + half];
    Some(world_aabb(inflated, s.transform_id, scene))
}

fn world_aabb_stroke(s: &SceneStroke, scene: &Scene) -> [f32; 4] {
    // Stroke extends `width / 2` outward from the path bounds.
    // Inflate the AABB by half the stroke width before transforming
    // so the tile filter doesn't miss tiles the stroke pen reaches
    // into.
    let half = s.stroke_width * 0.5;
    let inflated = [s.x0 - half, s.y0 - half, s.x1 + half, s.y1 + half];
    world_aabb(inflated, s.transform_id, scene)
}

/// AABB of `[x0, y0, x1, y1]` after applying the 2-D affine portion of
/// `t` to each corner. Conservative: rotated rects produce a larger AABB
/// than the rotated rect's true bounds.
fn transformed_aabb(rect: [f32; 4], t: &Transform) -> [f32; 4] {
    let corners = [
        (rect[0], rect[1]),
        (rect[2], rect[1]),
        (rect[0], rect[3]),
        (rect[2], rect[3]),
    ];
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for (x, y) in corners {
        // Column-major mat4 × (x, y, 0, 1):
        //   x' = m[0]*x + m[4]*y + m[12]
        //   y' = m[1]*x + m[5]*y + m[13]
        let tx = t.m[0] * x + t.m[4] * y + t.m[12];
        let ty = t.m[1] * x + t.m[5] * y + t.m[13];
        min_x = min_x.min(tx);
        min_y = min_y.min(ty);
        max_x = max_x.max(tx);
        max_y = max_y.max(ty);
    }
    [min_x, min_y, max_x, max_y]
}

/// Half-open AABB intersection: `a` and `b` overlap iff their
/// `[x0, x1) × [y0, y1)` regions share at least one pixel. Touching
/// edges (a.x1 == b.x0) do NOT count as intersection — matches the
/// half-open rasterization semantics of the brush pipelines.
pub(crate) fn aabb_intersects(a: [f32; 4], b: [f32; 4]) -> bool {
    !(a[2] <= b[0] || a[0] >= b[2] || a[3] <= b[1] || a[1] >= b[3])
}
