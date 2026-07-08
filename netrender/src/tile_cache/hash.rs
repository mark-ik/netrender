/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Per-tile primitive-dependency hashing for the picture cache (see [`super`]).

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;

use crate::scene::{
    PathOp, Scene, SceneGlyphRun, SceneGradient, SceneImage, SceneOp, ScenePattern, SceneRect,
    SceneShape, SceneStroke,
};

use super::{
    aabb_intersects, world_aabb, world_aabb_glyph_run, world_aabb_gradient, world_aabb_image,
    world_aabb_rect, world_aabb_shape, world_aabb_stroke,
};

/// Hash the dependency state of every primitive intersecting `tile_rect`,
/// in painter order. Empty tiles get a deterministic empty-hasher value;
/// two empty tiles hash identically, so they're never spuriously dirty.
pub(super) fn hash_tile_deps(scene: &Scene, tile_rect: [f32; 4]) -> u64 {
    let mut hasher = DefaultHasher::new();

    // Phase 12a' scene-level alpha + blend mode are global — every
    // tile's hash includes them so a change invalidates everything
    // (which is correct: the alpha/blend wrap affects every pixel
    // of the master scene composite).
    hasher.write_u32(scene.root_alpha.to_bits());
    hasher.write_u8(scene.root_blend_mode as u8);

    // Walk ops in painter order; hash anything whose AABB intersects
    // the tile. The ordering is consumer push order, so within-tile
    // hash bytes change if a primitive is reordered relative to its
    // siblings (which is correct: reordering changes the rendered
    // result).
    for op in &scene.ops {
        match op {
            SceneOp::Rect(rect) => {
                let aabb = world_aabb_rect(rect, scene);
                if aabb_intersects(aabb, tile_rect) {
                    hash_aabb(&mut hasher, aabb);
                    hash_rect(&mut hasher, rect);
                }
            }
            SceneOp::Image(image) => {
                let aabb = world_aabb_image(image, scene);
                if aabb_intersects(aabb, tile_rect) {
                    hash_aabb(&mut hasher, aabb);
                    hash_image(&mut hasher, image);
                }
            }
            SceneOp::Pattern(pattern) => {
                let aabb = world_aabb(pattern.extent, pattern.transform_id, scene);
                if aabb_intersects(aabb, tile_rect) {
                    hash_aabb(&mut hasher, aabb);
                    hash_pattern(&mut hasher, pattern);
                }
            }
            SceneOp::Gradient(grad) => {
                let aabb = world_aabb_gradient(grad, scene);
                if aabb_intersects(aabb, tile_rect) {
                    hash_aabb(&mut hasher, aabb);
                    hash_gradient(&mut hasher, grad);
                }
            }
            SceneOp::Stroke(stroke) => {
                let aabb = world_aabb_stroke(stroke, scene);
                if aabb_intersects(aabb, tile_rect) {
                    hash_aabb(&mut hasher, aabb);
                    hash_stroke(&mut hasher, stroke);
                }
            }
            SceneOp::Shape(shape) => {
                if let Some(aabb) = world_aabb_shape(shape, scene) {
                    if aabb_intersects(aabb, tile_rect) {
                        hash_aabb(&mut hasher, aabb);
                        hash_shape(&mut hasher, shape);
                    }
                }
            }
            SceneOp::GlyphRun(run) => {
                if let Some(aabb) = world_aabb_glyph_run(run, scene) {
                    if aabb_intersects(aabb, tile_rect) {
                        hash_aabb(&mut hasher, aabb);
                        hash_glyph_run(&mut hasher, run);
                    }
                }
            }
            // Layer push/pop ops are global to a tile's content
            // structure: the layer's visual effect modifies every
            // inner op's pixels. Hash the layer fields for every
            // tile so changes invalidate all affected tiles.
            // (Conservative — could be tightened by walking the
            // layer's clip-AABB later if profiles surface it.)
            SceneOp::PushLayer(layer) => hash_push_layer(&mut hasher, layer),
            SceneOp::PopLayer => hasher.write_u8(0xFF),
        }
    }

    hasher.finish()
}

/// Hash a primitive's world-space AABB into the tile dependency hash.
///
/// The per-primitive `hash_*` functions hash `transform_id` (the *index* into
/// `scene.transforms`), not the transform's matrix values. When a primitive
/// moves under a stable id — e.g. a dragged element re-emitted in the same scene
/// position each frame, so it keeps the same transform index while its translate
/// changes — its local hash is unchanged, so a tile that still contains it is not
/// marked dirty and keeps a stale cached render (the moved primitive ghosts at
/// its old position). Folding the world AABB (which `world_aabb_*` derives by
/// applying the transform) into the hash makes any positional change invalidate
/// the tiles the primitive occupies.
fn hash_aabb(h: &mut DefaultHasher, aabb: [f32; 4]) {
    for v in aabb {
        h.write_u32(v.to_bits());
    }
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
    // Sampling state changes the rendered pixels; both flags must
    // dirty the tile.
    h.write_u8(i.clamp_to_uv as u8);
    h.write_u8(i.nearest as u8);
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
    // Roadmap C4 — variable-font axis values change the rendered
    // glyph shape; include in the tile hash.
    h.write_usize(r.font_axis_values.len());
    for (tag, value) in &r.font_axis_values {
        h.write(tag);
        h.write_u32(value.to_bits());
    }
}

fn hash_pattern(h: &mut DefaultHasher, p: &ScenePattern) {
    h.write_u64(p.tile);
    for c in p.extent {
        h.write_u32(c.to_bits());
    }
    for s in p.scale {
        h.write_u32(s.to_bits());
    }
    h.write_u32(p.transform_id);
    for c in p.clip_rect {
        h.write_u32(c.to_bits());
    }
    for c in p.clip_corner_radii {
        h.write_u32(c.to_bits());
    }
    h.write_u8(p.nearest as u8);
}

fn hash_shape(h: &mut DefaultHasher, s: &SceneShape) {
    h.write_usize(s.path.ops.len());
    for op in &s.path.ops {
        match *op {
            PathOp::MoveTo(x, y) => {
                h.write_u8(0);
                h.write_u32(x.to_bits());
                h.write_u32(y.to_bits());
            }
            PathOp::LineTo(x, y) => {
                h.write_u8(1);
                h.write_u32(x.to_bits());
                h.write_u32(y.to_bits());
            }
            PathOp::QuadTo(cx, cy, x, y) => {
                h.write_u8(2);
                h.write_u32(cx.to_bits());
                h.write_u32(cy.to_bits());
                h.write_u32(x.to_bits());
                h.write_u32(y.to_bits());
            }
            PathOp::CubicTo(c1x, c1y, c2x, c2y, x, y) => {
                h.write_u8(3);
                h.write_u32(c1x.to_bits());
                h.write_u32(c1y.to_bits());
                h.write_u32(c2x.to_bits());
                h.write_u32(c2y.to_bits());
                h.write_u32(x.to_bits());
                h.write_u32(y.to_bits());
            }
            PathOp::Close => h.write_u8(4),
        }
    }
    if let Some(c) = s.fill_color {
        h.write_u8(1);
        for v in c {
            h.write_u32(v.to_bits());
        }
    } else {
        h.write_u8(0);
    }
    if let Some(stroke) = &s.stroke {
        h.write_u8(1);
        for v in stroke.color {
            h.write_u32(v.to_bits());
        }
        h.write_u32(stroke.width.to_bits());
        h.write_u8(stroke.cap as u8);
        h.write_u8(stroke.join as u8);
        h.write_usize(stroke.dash_pattern.len());
        for v in &stroke.dash_pattern {
            h.write_u32(v.to_bits());
        }
        h.write_u32(stroke.dash_offset.to_bits());
    } else {
        h.write_u8(0);
    }
    h.write_u32(s.transform_id);
    for c in s.clip_rect {
        h.write_u32(c.to_bits());
    }
    for c in s.clip_corner_radii {
        h.write_u32(c.to_bits());
    }
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
    // Roadmap C1 — cap / join / dash are part of the stroke's
    // visible geometry; changing them must invalidate the tile.
    h.write_u8(s.cap as u8);
    h.write_u8(s.join as u8);
    h.write_usize(s.dash_pattern.len());
    for v in &s.dash_pattern {
        h.write_u32(v.to_bits());
    }
    h.write_u32(s.dash_offset.to_bits());
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

/// Hash a single filter function (discriminant + its `f32` amount) into the
/// layer key, so a filter change invalidates the cached tile.
fn hash_scene_filter(h: &mut DefaultHasher, f: crate::scene::SceneFilter) {
    use crate::scene::SceneFilter as F;
    let (tag, v) = match f {
        F::Blur(v) => (0u8, v),
        F::Brightness(v) => (1, v),
        F::Contrast(v) => (2, v),
        F::Grayscale(v) => (3, v),
        F::HueRotate(v) => (4, v),
        F::Invert(v) => (5, v),
        F::Saturate(v) => (6, v),
        F::Sepia(v) => (7, v),
    };
    h.write_u8(tag);
    h.write_u32(v.to_bits());
}

fn hash_push_layer(h: &mut DefaultHasher, layer: &crate::scene::SceneLayer) {
    use crate::scene::SceneClip;
    h.write_u32(layer.alpha.to_bits());
    h.write_u8(layer.blend_mode as u8);
    // Roadmap C3 — compose mode is part of the layer's visible
    // identity (SrcOver vs DestIn changes everything).
    h.write_u8(layer.compose as u8);
    h.write_u32(layer.transform_id);
    // Roadmap D1 — backdrop_filter changes what the layer paints
    // over (pre-rendered blurred prefix); include in the hash.
    match layer.backdrop_filter {
        None => h.write_u8(0),
        Some(f) => {
            h.write_u8(1);
            hash_scene_filter(h, f);
        }
    }
    // CSS `filter` chain on the layer's own output — part of its visible identity.
    h.write_usize(layer.filters.len());
    for f in &layer.filters {
        hash_scene_filter(h, *f);
    }
    match &layer.clip {
        SceneClip::None => h.write_u8(0),
        SceneClip::Rect { rect, radii } => {
            h.write_u8(1);
            for f in rect {
                h.write_u32(f.to_bits());
            }
            for f in radii {
                h.write_u32(f.to_bits());
            }
        }
        SceneClip::Path(path) => {
            h.write_u8(2);
            h.write_usize(path.ops.len());
            for op in &path.ops {
                match *op {
                    crate::scene::PathOp::MoveTo(x, y) => {
                        h.write_u8(b'M');
                        h.write_u32(x.to_bits());
                        h.write_u32(y.to_bits());
                    }
                    crate::scene::PathOp::LineTo(x, y) => {
                        h.write_u8(b'L');
                        h.write_u32(x.to_bits());
                        h.write_u32(y.to_bits());
                    }
                    crate::scene::PathOp::QuadTo(cx, cy, x, y) => {
                        h.write_u8(b'Q');
                        h.write_u32(cx.to_bits());
                        h.write_u32(cy.to_bits());
                        h.write_u32(x.to_bits());
                        h.write_u32(y.to_bits());
                    }
                    crate::scene::PathOp::CubicTo(c1x, c1y, c2x, c2y, x, y) => {
                        h.write_u8(b'C');
                        h.write_u32(c1x.to_bits());
                        h.write_u32(c1y.to_bits());
                        h.write_u32(c2x.to_bits());
                        h.write_u32(c2y.to_bits());
                        h.write_u32(x.to_bits());
                        h.write_u32(y.to_bits());
                    }
                    crate::scene::PathOp::Close => h.write_u8(b'Z'),
                }
            }
        }
    }
}
