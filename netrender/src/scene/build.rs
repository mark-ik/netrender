/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! `Scene` builder methods (part 1): constructors, rects, images, gradients,
//! patterns, glyph runs.

use super::*;

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
