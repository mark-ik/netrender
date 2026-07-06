/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! `Scene` builder methods: the `push_*` op constructors, scene constructors,
//! op iterators, debug dump, and compositor-surface declares. One cohesive
//! builder API over `Scene` (see [`super`]).

use super::*;
use super::debug::dump_op;

impl Scene {
    pub fn new(viewport_width: u32, viewport_height: u32) -> Self {
        Self {
            viewport_width,
            viewport_height,
            ops: Vec::new(),
            // Index 0 reserved as a no-font sentinel; real fonts
            // start at index 1. Sentinel uses an empty Blob with a
            // **fixed id (`u64::MAX`)** rather than peniko's mint —
            // emit_glyph_run skips runs with font_id == 0 so the id
            // is functionally irrelevant, but keeping it deterministic
            // means two `Scene::new()` calls produce byte-identical
            // snapshots (A2 round-trip determinism).
            fonts: vec![FontBlob {
                data: sentinel_blob(),
                index: 0,
            }],
            root_alpha: 1.0,
            root_blend_mode: SceneBlendMode::Normal,
            transforms: vec![Transform::IDENTITY], // index 0 = identity
            image_sources: HashMap::new(),
            compositor_surfaces: Vec::new(),
        }
    }

    /// Register a transform and return its index into the palette.
    pub fn push_transform(&mut self, t: Transform) -> u32 {
        let id = self.transforms.len() as u32;
        self.transforms.push(t);
        id
    }

    /// Append a rect at device-pixel coordinates with no transform and
    /// no clip (backward-compatible Phase 2 API).
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

    /// Append a rect with an explicit transform id.
    pub fn push_rect_transformed(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        color: [f32; 4],
        transform_id: u32,
    ) {
        self.ops.push(SceneOp::Rect(SceneRect {
            x0,
            y0,
            x1,
            y1,
            color,
            transform_id,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
        }));
    }

    /// Append a rect with an explicit transform and a device-space
    /// axis-aligned clip.
    pub fn push_rect_clipped(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        color: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
    ) {
        self.ops.push(SceneOp::Rect(SceneRect {
            x0,
            y0,
            x1,
            y1,
            color,
            transform_id,
            clip_rect,
            clip_corner_radii: SHARP_CLIP,
        }));
    }

    /// Append a rect with a rounded-rect clip (Phase 9'). `clip_corner_radii`
    /// is `[top_left, top_right, bottom_right, bottom_left]` in device
    /// pixels. All-zero radii degenerate to the same result as
    /// `push_rect_clipped` (a sharp axis-aligned clip).
    pub fn push_rect_clipped_rounded(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        color: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
        clip_corner_radii: [f32; 4],
    ) {
        self.ops.push(SceneOp::Rect(SceneRect {
            x0,
            y0,
            x1,
            y1,
            color,
            transform_id,
            clip_rect,
            clip_corner_radii,
        }));
    }

    /// Register pixel data for `key` without adding a draw primitive.
    /// Call this before `push_image_ref` if you want to separate data
    /// registration from draw-list building.
    pub fn set_image_source(&mut self, key: ImageKey, data: ImageData) {
        self.image_sources.entry(key).or_insert(data);
    }

    /// Append an image rect at device-pixel coordinates.
    ///
    /// `data` is uploaded once on first `prepare()` and cached by `key`.
    /// Subsequent calls with the same `key` ignore `data`.
    /// UV defaults to `[0, 0, 1, 1]` (full image); tint to white `[1,1,1,1]`.
    pub fn push_image(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        key: ImageKey,
        data: ImageData,
    ) {
        self.image_sources.entry(key).or_insert(data);
        self.ops.push(SceneOp::Image(SceneImage {
            x0,
            y0,
            x1,
            y1,
            uv: [0.0, 0.0, 1.0, 1.0],
            color: [1.0, 1.0, 1.0, 1.0],
            key,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
            clamp_to_uv: false,
            nearest: false,
        }));
    }

    /// Phase 8D general API: push an arbitrary-kind, arbitrary-stops
    /// gradient. The 2-stop convenience methods below build a
    /// `SceneGradient` and forward to this.
    pub fn push_gradient(&mut self, gradient: SceneGradient) {
        self.ops.push(SceneOp::Gradient(gradient));
    }

    /// 2-stop linear gradient (Phase 8A convenience; preserved post-8D).
    pub fn push_linear_gradient(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        start: [f32; 2],
        end: [f32; 2],
        color0: [f32; 4],
        color1: [f32; 4],
    ) {
        self.ops.push(SceneOp::Gradient(two_stop_gradient(
            GradientKind::Linear,
            x0,
            y0,
            x1,
            y1,
            [start[0], start[1], end[0], end[1]],
            color0,
            color1,
            0,
            NO_CLIP,
        )));
    }

    /// 2-stop linear gradient with full control over transform and clip.
    pub fn push_linear_gradient_full(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        start: [f32; 2],
        end: [f32; 2],
        color0: [f32; 4],
        color1: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
    ) {
        self.ops.push(SceneOp::Gradient(two_stop_gradient(
            GradientKind::Linear,
            x0,
            y0,
            x1,
            y1,
            [start[0], start[1], end[0], end[1]],
            color0,
            color1,
            transform_id,
            clip_rect,
        )));
    }

    /// 2-stop radial gradient (Phase 8B convenience). For circular,
    /// pass `radii = [r, r]`. Color0 at center, color1 at the
    /// elliptical boundary (clamps beyond).
    pub fn push_radial_gradient(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        center: [f32; 2],
        radii: [f32; 2],
        color0: [f32; 4],
        color1: [f32; 4],
    ) {
        self.ops.push(SceneOp::Gradient(two_stop_gradient(
            GradientKind::Radial,
            x0,
            y0,
            x1,
            y1,
            [center[0], center[1], radii[0], radii[1]],
            color0,
            color1,
            0,
            NO_CLIP,
        )));
    }

    /// 2-stop conic gradient (Phase 8C convenience). `t = 0` at
    /// `start_angle`, sweeping clockwise (with y-down screen coords)
    /// back to the seam at `t = 1`.
    pub fn push_conic_gradient(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        center: [f32; 2],
        start_angle: f32,
        color0: [f32; 4],
        color1: [f32; 4],
    ) {
        self.ops.push(SceneOp::Gradient(two_stop_gradient(
            GradientKind::Conic,
            x0,
            y0,
            x1,
            y1,
            [center[0], center[1], start_angle, 0.0],
            color0,
            color1,
            0,
            NO_CLIP,
        )));
    }

    /// 2-stop conic gradient with full control over transform and clip.
    pub fn push_conic_gradient_full(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        center: [f32; 2],
        start_angle: f32,
        color0: [f32; 4],
        color1: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
    ) {
        self.ops.push(SceneOp::Gradient(two_stop_gradient(
            GradientKind::Conic,
            x0,
            y0,
            x1,
            y1,
            [center[0], center[1], start_angle, 0.0],
            color0,
            color1,
            transform_id,
            clip_rect,
        )));
    }

    /// 2-stop radial gradient with full control over transform and clip.
    pub fn push_radial_gradient_full(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        center: [f32; 2],
        radii: [f32; 2],
        color0: [f32; 4],
        color1: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
    ) {
        self.ops.push(SceneOp::Gradient(two_stop_gradient(
            GradientKind::Radial,
            x0,
            y0,
            x1,
            y1,
            [center[0], center[1], radii[0], radii[1]],
            color0,
            color1,
            transform_id,
            clip_rect,
        )));
    }

    /// Roadmap C2 — append a repeated-tile pattern fill. The image
    /// at `tile` repeats at `image_size * scale` to cover `extent`.
    /// Identity transform, no clip; for richer construction build
    /// the [`ScenePattern`] struct directly and push it.
    pub fn push_pattern(&mut self, tile: ImageKey, extent: [f32; 4], scale: [f32; 2]) {
        self.ops.push(SceneOp::Pattern(ScenePattern {
            tile,
            extent,
            scale,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
            nearest: false,
        }));
    }

    /// Append an image rect with full control over UV, tint, transform,
    /// and clip.
    pub fn push_image_full(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        uv: [f32; 4],
        color: [f32; 4],
        key: ImageKey,
        transform_id: u32,
        clip_rect: [f32; 4],
    ) {
        self.ops.push(SceneOp::Image(SceneImage {
            x0,
            y0,
            x1,
            y1,
            uv,
            color,
            key,
            transform_id,
            clip_rect,
            clip_corner_radii: SHARP_CLIP,
            clamp_to_uv: false,
            nearest: false,
        }));
    }

    /// Like [`Self::push_image_full`], but **clamps the sampler to the `uv`
    /// sub-rect**: the source is cropped to that region before drawing, so
    /// bilinear filtering at the sub-rect edges cannot bleed in adjacent source
    /// pixels (nine-patch slice seams, sprite-sheet cells).
    #[allow(clippy::too_many_arguments)]
    pub fn push_image_clamped(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        uv: [f32; 4],
        color: [f32; 4],
        key: ImageKey,
        transform_id: u32,
        clip_rect: [f32; 4],
    ) {
        self.ops.push(SceneOp::Image(SceneImage {
            x0,
            y0,
            x1,
            y1,
            uv,
            color,
            key,
            transform_id,
            clip_rect,
            clip_corner_radii: SHARP_CLIP,
            clamp_to_uv: true,
            nearest: false,
        }));
    }

    /// Phase 10a': register a font with the scene. Returns a
    /// non-zero `FontId` that subsequent `push_glyph_run` calls
    /// reference. Index 0 is a reserved no-font sentinel; the
    /// first call returns 1.
    pub fn push_font(&mut self, blob: FontBlob) -> FontId {
        let id = self.fonts.len() as u32;
        self.fonts.push(blob);
        id
    }

    /// Phase 10a': append a glyph run. Caller is responsible for
    /// shaping (turning a string into glyph IDs + positions); see
    /// plan §4.4 for the layout-layer story.
    pub fn push_glyph_run(
        &mut self,
        font_id: FontId,
        font_size: f32,
        glyphs: Vec<Glyph>,
        color: [f32; 4],
    ) {
        self.ops.push(SceneOp::GlyphRun(SceneGlyphRun {
            font_id,
            font_size,
            glyphs,
            color,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
            font_axis_values: Vec::new(),
        }));
    }

    /// Roadmap C4 — append a glyph run with explicit variable-font
    /// axis values. Each `(tag, value)` pair sets the user-space
    /// position on a font axis (e.g., `(*b"wght", 700.0)` for
    /// weight 700). Tag bytes that don't match an axis in the font
    /// are silently ignored; unset axes get the font's default.
    /// All other fields default — for richer construction, build the
    /// [`SceneGlyphRun`] struct directly and push it.
    pub fn push_glyph_run_variable(
        &mut self,
        font_id: FontId,
        font_size: f32,
        glyphs: Vec<Glyph>,
        color: [f32; 4],
        font_axis_values: Vec<(SceneFontAxisTag, f32)>,
    ) {
        self.ops.push(SceneOp::GlyphRun(SceneGlyphRun {
            font_id,
            font_size,
            glyphs,
            color,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
            font_axis_values,
        }));
    }

    /// Phase 10a': append a glyph run with full control over
    /// transform and clip.
    pub fn push_glyph_run_full(
        &mut self,
        font_id: FontId,
        font_size: f32,
        glyphs: Vec<Glyph>,
        color: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
        clip_corner_radii: [f32; 4],
    ) {
        self.ops.push(SceneOp::GlyphRun(SceneGlyphRun {
            font_id,
            font_size,
            glyphs,
            color,
            transform_id,
            clip_rect,
            clip_corner_radii,
            font_axis_values: Vec::new(),
        }));
    }

    /// Phase 11b': append a `SceneShape` directly. For most cases
    /// the convenience helpers `push_shape_filled` /
    /// `push_shape_stroked` are easier to use.
    pub fn push_shape(&mut self, shape: SceneShape) {
        self.ops.push(SceneOp::Shape(shape));
    }

    /// Phase 12b' — open a nested layer scope. All subsequent
    /// `push_*` calls until the matching [`Scene::pop_layer`] paint
    /// into the layer; the layer is then composited back to the
    /// parent with the layer's alpha + blend mode + clip.
    pub fn push_layer(&mut self, layer: SceneLayer) {
        self.ops.push(SceneOp::PushLayer(layer));
    }

    /// Phase 12b' — close the most recently opened layer scope. A
    /// `pop_layer` without a matching `push_layer` will panic the
    /// renderer in debug builds; release builds skip the underflow.
    pub fn pop_layer(&mut self) {
        self.ops.push(SceneOp::PopLayer);
    }

    /// Convenience: open an alpha-only layer (no clip, normal
    /// blend mode, identity transform). Pair with [`Scene::pop_layer`].
    pub fn push_layer_alpha(&mut self, alpha: f32) {
        self.push_layer(SceneLayer::alpha(alpha));
    }

    /// Convenience: open a clip-only layer (alpha 1.0, blend
    /// Normal, identity transform) with the given clip. Pair with
    /// [`Scene::pop_layer`].
    pub fn push_layer_clip(&mut self, clip: SceneClip) {
        self.push_layer(SceneLayer::clip(clip));
    }

    /// Roadmap C3 — open an alpha-mask layer (DestIn compose).
    /// Inside this scope the consumer pushes the mask shape (image,
    /// shape, glyph run, etc.); when paired [`Scene::pop_layer`]
    /// fires, the *enclosing* layer's content survives only where
    /// this layer is opaque.
    ///
    /// Typical usage opens an outer clip layer first, then a
    /// `push_alpha_mask_layer` inner, with content between them:
    ///
    /// ```text
    /// scene.push_layer_clip(SceneClip::Rect { ... });
    /// // → render content (will be masked)
    /// scene.push_alpha_mask_layer();
    /// scene.push_image_full(mask_key, ...);
    /// scene.pop_layer(); // closes alpha mask, applies DestIn
    /// scene.pop_layer(); // closes outer
    /// ```
    pub fn push_alpha_mask_layer(&mut self) {
        self.push_layer(SceneLayer::alpha_mask());
    }

    /// Roadmap B2 — open a scroll frame: a rect-clipped layer in
    /// parent space paired with a fresh `Transform` palette entry
    /// that translates content by `-scroll_offset`.
    ///
    /// Returns the `transform_id` the caller should pass to
    /// `push_rect_transformed` / `push_image_transformed` / etc. for
    /// any primitive that should scroll inside this frame. Primitives
    /// pushed with `transform_id = 0` (identity) do not move with the
    /// scroll — useful for sticky chrome.
    ///
    /// Pair with [`Scene::pop_scroll_frame`] when done. The "scroll
    /// transform + clip layer" bundle replaces three explicit calls
    /// (`push_transform`, `push_layer` with a rect clip, then later
    /// `pop_layer`) with two — without changing the underlying op
    /// model.
    ///
    /// `clip_rect` is in parent (pre-scroll) coordinates;
    /// `scroll_offset` is `[dx, dy]` (positive = pan content
    /// up/left, matching CSS scroll semantics).
    pub fn push_scroll_frame(&mut self, clip_rect: [f32; 4], scroll_offset: [f32; 2]) -> u32 {
        let xf_id = self.push_transform(Transform::translate_2d(
            -scroll_offset[0],
            -scroll_offset[1],
        ));
        self.push_layer(SceneLayer {
            clip: SceneClip::Rect {
                rect: clip_rect,
                radii: SHARP_CLIP,
            },
            alpha: 1.0,
            blend_mode: SceneBlendMode::Normal,
            compose: SceneCompose::SrcOver,
            // Clip is in parent space; identity transform_id keeps
            // it positioned correctly regardless of the inner
            // content's scroll transform.
            transform_id: 0,
            backdrop_filter: None,
            filters: Vec::new(),
        });
        xf_id
    }

    /// Roadmap B2 — close the scroll frame opened by
    /// [`Scene::push_scroll_frame`]. Equivalent to
    /// [`Scene::pop_layer`]; named for symmetry so reading sites can
    /// grep for `pop_scroll_frame` to find scroll-frame boundaries.
    pub fn pop_scroll_frame(&mut self) {
        self.pop_layer();
    }

    /// Drop every draw op without touching `fonts`, `transforms`, or
    /// `image_sources`. Useful for the "rebuild scene per frame but
    /// reuse the asset palette" pattern: a streaming consumer
    /// doesn't have to re-register the same fonts / transforms /
    /// image sources every frame, but does want a fresh op list.
    ///
    /// Equivalent to `self.ops.clear()` but signals intent at the
    /// API level — read sites can grep for `clear_ops` to find
    /// frame boundaries.
    pub fn clear_ops(&mut self) {
        self.ops.clear();
    }

    /// Roadmap A1 — pretty-print the op list for debugging.
    ///
    /// Returns a multi-line string with one header line summarising the
    /// scene (viewport, palette sizes, op count) followed by one line
    /// per [`SceneOp`] in painter order. Each op line shows the op
    /// kind, the key geometry / color fields, and any non-default
    /// `transform_id` / `clip_rect` / `clip_corner_radii`.
    /// `PushLayer` / `PopLayer` pairs nest the indentation by two
    /// spaces per level — ops between them visibly belong to the
    /// layer scope.
    ///
    /// Output is for human reading; format is **not** stable. Don't
    /// parse it, snapshot-test it, or treat it as a contract.
    pub fn dump_ops(&self) -> String {
        use std::fmt::Write;

        let mut out = String::new();
        writeln!(
            out,
            "Scene {}x{}  transforms={}  fonts={}  images={}  surfaces={}  ops={}",
            self.viewport_width,
            self.viewport_height,
            self.transforms.len(),
            self.fonts.len(),
            self.image_sources.len(),
            self.compositor_surfaces.len(),
            self.ops.len(),
        )
        .ok();

        if self.root_alpha != 1.0 || self.root_blend_mode != SceneBlendMode::Normal {
            writeln!(
                out,
                "  root_alpha={}  root_blend_mode={:?}",
                self.root_alpha, self.root_blend_mode,
            )
            .ok();
        }

        let mut depth: usize = 0;
        for (i, op) in self.ops.iter().enumerate() {
            // PopLayer un-indents *before* its own line so the pop
            // visually closes the scope it ends.
            if matches!(op, SceneOp::PopLayer) {
                depth = depth.saturating_sub(1);
            }
            let pad = "  ".repeat(depth + 1);
            write!(out, "  {:04}{}", i, pad).ok();
            dump_op(&mut out, op);
            writeln!(out).ok();
            if matches!(op, SceneOp::PushLayer(_)) {
                depth += 1;
            }
        }

        out
    }

    /// Iterate the rect ops of the scene in painter order. Other op
    /// variants are filtered out.
    pub fn iter_rects(&self) -> impl Iterator<Item = &SceneRect> + '_ {
        self.ops.iter().filter_map(|op| match op {
            SceneOp::Rect(r) => Some(r),
            _ => None,
        })
    }

    /// Iterate the stroke ops of the scene in painter order.
    pub fn iter_strokes(&self) -> impl Iterator<Item = &SceneStroke> + '_ {
        self.ops.iter().filter_map(|op| match op {
            SceneOp::Stroke(s) => Some(s),
            _ => None,
        })
    }

    /// Iterate the gradient ops of the scene in painter order.
    pub fn iter_gradients(&self) -> impl Iterator<Item = &SceneGradient> + '_ {
        self.ops.iter().filter_map(|op| match op {
            SceneOp::Gradient(g) => Some(g),
            _ => None,
        })
    }

    /// Iterate the image ops of the scene in painter order.
    pub fn iter_images(&self) -> impl Iterator<Item = &SceneImage> + '_ {
        self.ops.iter().filter_map(|op| match op {
            SceneOp::Image(i) => Some(i),
            _ => None,
        })
    }

    /// Iterate the shape ops of the scene in painter order.
    pub fn iter_shapes(&self) -> impl Iterator<Item = &SceneShape> + '_ {
        self.ops.iter().filter_map(|op| match op {
            SceneOp::Shape(s) => Some(s),
            _ => None,
        })
    }

    /// Iterate the glyph-run ops of the scene in painter order.
    pub fn iter_glyph_runs(&self) -> impl Iterator<Item = &SceneGlyphRun> + '_ {
        self.ops.iter().filter_map(|op| match op {
            SceneOp::GlyphRun(g) => Some(g),
            _ => None,
        })
    }

    /// Phase 11b': append an arbitrary path filled with a single
    /// solid color. Identity transform, no clip.
    pub fn push_shape_filled(&mut self, path: ScenePath, color: [f32; 4]) {
        self.ops.push(SceneOp::Shape(SceneShape {
            path,
            fill_color: Some(color),
            stroke: None,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
        }));
    }

    /// Phase 11b': append an arbitrary path stroked with a single
    /// solid color and line width. Identity transform, no clip.
    pub fn push_shape_stroked(&mut self, path: ScenePath, color: [f32; 4], stroke_width: f32) {
        self.ops.push(SceneOp::Shape(SceneShape {
            path,
            fill_color: None,
            stroke: Some(ScenePathStroke {
                color,
                width: stroke_width,
            }),
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
        }));
    }

    /// Phase 11': append a sharp axis-aligned stroked rect (border).
    /// Append a fully-specified stroke op. The struct-accepting escape hatch for
    /// callers needing rounded corners + dashes + transform/clip at once (e.g.
    /// a rounded, dashed CSS border) — the positional `push_stroke*` helpers each
    /// cover only a subset.
    pub fn push_stroke_op(&mut self, stroke: SceneStroke) {
        self.ops.push(SceneOp::Stroke(stroke));
    }

    pub fn push_stroke(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        color: [f32; 4],
        stroke_width: f32,
    ) {
        self.ops.push(SceneOp::Stroke(SceneStroke {
            x0,
            y0,
            x1,
            y1,
            color,
            stroke_width,
            stroke_corner_radii: SHARP_CLIP,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
            cap: SceneStrokeCap::default(),
            join: SceneStrokeJoin::default(),
            dash_pattern: Vec::new(),
            dash_offset: 0.0,
        }));
    }

    /// Phase 11': append a stroked rounded-rect (CSS border with
    /// `border-radius`). `stroke_corner_radii` rounds the path
    /// itself, in `[top_left, top_right, bottom_right, bottom_left]`
    /// order. All-zero radii produce a sharp rectangular stroke.
    pub fn push_stroke_rounded(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        color: [f32; 4],
        stroke_width: f32,
        stroke_corner_radii: [f32; 4],
    ) {
        self.ops.push(SceneOp::Stroke(SceneStroke {
            x0,
            y0,
            x1,
            y1,
            color,
            stroke_width,
            stroke_corner_radii,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
            cap: SceneStrokeCap::default(),
            join: SceneStrokeJoin::default(),
            dash_pattern: Vec::new(),
            dash_offset: 0.0,
        }));
    }

    /// Phase 11': append a stroked rect/rounded-rect with full
    /// control over transform, clip, and clip corner radii.
    pub fn push_stroke_full(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        color: [f32; 4],
        stroke_width: f32,
        stroke_corner_radii: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
        clip_corner_radii: [f32; 4],
    ) {
        self.ops.push(SceneOp::Stroke(SceneStroke {
            x0,
            y0,
            x1,
            y1,
            color,
            stroke_width,
            stroke_corner_radii,
            transform_id,
            clip_rect,
            clip_corner_radii,
            cap: SceneStrokeCap::default(),
            join: SceneStrokeJoin::default(),
            dash_pattern: Vec::new(),
            dash_offset: 0.0,
        }));
    }

    /// Roadmap C1 — append a dashed stroked rect with explicit
    /// cap / join / dash pattern. `dash_pattern` is alternating
    /// on/off lengths in device pixels; `dash_offset` shifts the
    /// pattern phase. Empty `dash_pattern` produces a solid stroke.
    /// All other fields default to identity transform / no clip /
    /// sharp corners — for richer construction, build the
    /// [`SceneStroke`] struct directly and push it.
    pub fn push_stroke_decorated(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        color: [f32; 4],
        stroke_width: f32,
        cap: SceneStrokeCap,
        join: SceneStrokeJoin,
        dash_pattern: Vec<f32>,
    ) {
        self.ops.push(SceneOp::Stroke(SceneStroke {
            x0,
            y0,
            x1,
            y1,
            color,
            stroke_width,
            stroke_corner_radii: SHARP_CLIP,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
            cap,
            join,
            dash_pattern,
            dash_offset: 0.0,
        }));
    }

    /// Append an image rect with full control + rounded-rect clip
    /// (Phase 9'). See `push_rect_clipped_rounded` for the radii
    /// convention.
    pub fn push_image_full_rounded(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        uv: [f32; 4],
        color: [f32; 4],
        key: ImageKey,
        transform_id: u32,
        clip_rect: [f32; 4],
        clip_corner_radii: [f32; 4],
    ) {
        self.ops.push(SceneOp::Image(SceneImage {
            x0,
            y0,
            x1,
            y1,
            uv,
            color,
            key,
            transform_id,
            clip_rect,
            clip_corner_radii,
            clamp_to_uv: false,
            nearest: false,
        }));
    }

    /// Declare or update a native-compositor surface. If the key was
    /// not present, append to `compositor_surfaces` (z-order = vec
    /// position). If the key was present, update fields in place
    /// without reordering.
    ///
    /// Surfaces and `SceneOp::PushLayer` are independent: a surface
    /// may contain layers, a layer may span surfaces. Surfaces are
    /// about *cross-frame OS handoff regions*; layers are about
    /// *within-frame compositing groups*.
    pub fn declare_compositor_surface(&mut self, surface: CompositorSurface) {
        if let Some(existing) = self
            .compositor_surfaces
            .iter_mut()
            .find(|s| s.key == surface.key)
        {
            *existing = surface;
        } else {
            self.compositor_surfaces.push(surface);
        }
    }

    /// Drop a previously-declared compositor surface. No-op if the
    /// key is not present.
    pub fn undeclare_compositor_surface(&mut self, key: SurfaceKey) {
        self.compositor_surfaces.retain(|s| s.key != key);
    }

    /// Update one surface's transform without changing bounds. The
    /// transform is applied by the OS compositor at present time,
    /// not by netrender's master render — calling this does not
    /// force a content repaint.
    ///
    /// No-op if `key` is not declared.
    pub fn set_surface_transform(&mut self, key: SurfaceKey, transform: [f32; 6]) {
        if let Some(s) = self.compositor_surfaces.iter_mut().find(|s| s.key == key) {
            s.transform = transform;
        }
    }

    /// Update one surface's clip. OS-compositor metadata; does not
    /// force a content repaint. No-op if `key` is not declared.
    pub fn set_surface_clip(&mut self, key: SurfaceKey, clip: Option<[f32; 4]>) {
        if let Some(s) = self.compositor_surfaces.iter_mut().find(|s| s.key == key) {
            s.clip = clip;
        }
    }

    /// Update one surface's opacity. OS-compositor metadata; does not
    /// force a content repaint. No-op if `key` is not declared.
    pub fn set_surface_opacity(&mut self, key: SurfaceKey, opacity: f32) {
        if let Some(s) = self.compositor_surfaces.iter_mut().find(|s| s.key == key) {
            s.opacity = opacity;
        }
    }
}

impl Default for Scene {
    fn default() -> Self {
        Self::new(0, 0)
    }
}

/// Build a 2-stop `SceneGradient` for the given kind. Internal helper
/// that powers `push_linear_gradient`, `push_radial_gradient`, and
/// `push_conic_gradient` (and their `_full` variants).
fn two_stop_gradient(
    kind: GradientKind,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    params: [f32; 4],
    color0: [f32; 4],
    color1: [f32; 4],
    transform_id: u32,
    clip_rect: [f32; 4],
) -> SceneGradient {
    SceneGradient {
        x0,
        y0,
        x1,
        y1,
        kind,
        repeat: false,
        params,
        stops: vec![
            GradientStop {
                offset: 0.0,
                color: color0,
            },
            GradientStop {
                offset: 1.0,
                color: color1,
            },
        ],
        transform_id,
        clip_rect,
        clip_corner_radii: SHARP_CLIP,
    }
}
