/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! `SceneFragment` — isolated sub-scenes merged into a parent via
//! `Scene::append_fragment`, with id-remap helpers.

use super::*;

// ── E2 SceneFragment + Scene::append_fragment ───────────────────────

/// Roadmap E2 — a sub-scene that one thread (or one work unit)
/// builds in isolation, to be merged into a parent [`Scene`] via
/// [`Scene::append_fragment`].
///
/// Fragments mirror Scene's op-list shape (ops, transforms, fonts,
/// image_sources) but omit scene-level state (viewport size, root
/// alpha / blend, compositor surfaces) — those belong to the
/// owning Scene.
///
/// On append, the parent Scene rewrites the fragment's transform
/// and font ids to the parent's index space; identity (id 0) and
/// the sentinel font (id 0) stay at id 0. Image keys are
/// caller-assigned `u64`s and aren't remapped — the consumer is
/// responsible for picking non-colliding keys across fragments
/// (typical pattern: hash of source URL, monotonic counter shared
/// across threads via atomics, or partition the keyspace by
/// thread id).
///
/// **Determinism**: append is order-preserving — fragment ops
/// arrive at the end of `parent.ops` in the order the consumer
/// calls `append_fragment`. Parallel scene-building consumers must
/// decide their painter-order policy explicitly (typically: each
/// fragment owns a Z-band or a workspace pane).
#[derive(Debug, Clone)]
pub struct SceneFragment {
    /// Draw operations in painter order (fragment-local).
    pub ops: Vec<SceneOp>,
    /// Local transform palette. Index 0 is identity, matching the
    /// `Scene` convention; non-identity entries get rewritten to
    /// the parent's index space on append.
    pub transforms: Vec<Transform>,
    /// Local font palette. Index 0 is the sentinel; real fonts at
    /// index 1+. Rewritten on append.
    pub fonts: Vec<FontBlob>,
    /// Image sources. Caller-assigned `ImageKey`s; not remapped on
    /// append.
    pub image_sources: HashMap<ImageKey, ImageData>,
}

impl Default for SceneFragment {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneFragment {
    /// Construct an empty fragment with the identity transform at
    /// index 0 and the sentinel font at index 0 (matching
    /// [`Scene::new`]'s palette invariants).
    pub fn new() -> Self {
        Self {
            ops: Vec::new(),
            transforms: vec![Transform::IDENTITY],
            fonts: vec![FontBlob {
                data: sentinel_blob(),
                index: 0,
            }],
            image_sources: HashMap::new(),
        }
    }

    /// Register a transform; returns its fragment-local index.
    pub fn push_transform(&mut self, t: Transform) -> u32 {
        let id = self.transforms.len() as u32;
        self.transforms.push(t);
        id
    }

    /// Register a font; returns its fragment-local index.
    pub fn push_font(&mut self, blob: FontBlob) -> FontId {
        let id = self.fonts.len() as u32;
        self.fonts.push(blob);
        id
    }

    /// Register an image source under `key`. Caller is responsible
    /// for non-colliding keys across fragments (see type docs).
    pub fn set_image_source(&mut self, key: ImageKey, data: ImageData) {
        self.image_sources.insert(key, data);
    }

    /// Append a [`SceneRect`] op directly. For convenience helpers
    /// matching `Scene::push_rect*`, build the rect inline and push
    /// — fragments expose `ops` directly so the consumer can stage
    /// any op variant without a dedicated wrapper for each.
    pub fn push_op(&mut self, op: SceneOp) {
        self.ops.push(op);
    }

    /// Convenience: append a solid-color rect. Identity transform,
    /// no clip. Mirrors [`Scene::push_rect`].
    pub fn push_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: [f32; 4]) {
        self.ops.push(SceneOp::Rect(SceneRect {
            x0,
            y0,
            x1,
            y1,
            color,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
        }));
    }
}

impl Scene {
    /// Roadmap E2 — merge a fragment into this Scene. Ops, fonts,
    /// and transforms get appended; ids in fragment ops are
    /// rewritten to the Scene's index space (identity / sentinel
    /// stay at 0).
    ///
    /// Image sources merge by `ImageKey`: collisions overwrite
    /// existing entries. Callers running fragments in parallel are
    /// expected to partition the keyspace.
    ///
    /// Order-preserving: fragment ops land at the end of `self.ops`
    /// in iteration order.
    pub fn append_fragment(&mut self, fragment: SceneFragment) {
        // Local index 0 (identity / sentinel) stays at 0 in scene.
        // Local index k >= 1 maps to scene index (scene_old_len + k - 1)
        // because we extend scene.transforms with fragment[1..] (skipping
        // the local identity).
        let xf_base = self.transforms.len() as u32 - 1;
        let font_base = self.fonts.len() as u32 - 1;

        let SceneFragment {
            ops,
            transforms,
            fonts,
            image_sources,
        } = fragment;

        self.transforms.extend(transforms.into_iter().skip(1));
        self.fonts.extend(fonts.into_iter().skip(1));
        for (k, v) in image_sources {
            self.image_sources.insert(k, v);
        }

        for mut op in ops {
            remap_op_ids(&mut op, xf_base, font_base);
            self.ops.push(op);
        }
    }
}

/// Rewrite a fragment-local op's transform_id and font_id (where
/// applicable) into the parent Scene's index space. Local id 0
/// (identity / sentinel) is preserved at 0.
fn remap_op_ids(op: &mut SceneOp, xf_base: u32, font_base: u32) {
    if let Some(xf) = op_transform_id_mut(op) {
        if *xf != 0 {
            *xf += xf_base;
        }
    }
    if let SceneOp::GlyphRun(r) = op {
        if r.font_id != 0 {
            r.font_id += font_base;
        }
    }
}

fn op_transform_id_mut(op: &mut SceneOp) -> Option<&mut u32> {
    match op {
        SceneOp::Rect(r) => Some(&mut r.transform_id),
        SceneOp::Stroke(s) => Some(&mut s.transform_id),
        SceneOp::Gradient(g) => Some(&mut g.transform_id),
        SceneOp::Image(i) => Some(&mut i.transform_id),
        SceneOp::Pattern(p) => Some(&mut p.transform_id),
        SceneOp::Shape(s) => Some(&mut s.transform_id),
        SceneOp::GlyphRun(r) => Some(&mut r.transform_id),
        SceneOp::PushLayer(l) => Some(&mut l.transform_id),
        SceneOp::PopLayer => None,
    }
}
