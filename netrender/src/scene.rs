/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 3 scene representation — adds per-primitive transforms and
//! axis-aligned clip rectangles to the Phase 2 solid-rect baseline.
//! Phase 5 adds image primitives (textured rects).
//!
//! Design plan §5 Phase 3: "Lift `space.rs`, `spatial_tree.rs`,
//! `transform.rs` math from old webrender." Phase 3 uses 4×4
//! column-major matrices for generality; the 2D affine subset is the
//! initial surface (translate / rotate / scale helpers). Full spatial
//! tree hierarchy (parent → child reference chains) is deferred to the
//! later phase that ingests `BuiltDisplayList` spatial nodes.
//!
//! Backward compat: `Scene::push_rect` still works unchanged. The
//! transforms array always has index 0 = identity, so existing callers
//! that do not pass a `transform_id` render exactly as in Phase 2.

use std::collections::HashMap;

pub use netrender_device::GradientKind;

/// A 4×4 column-major transform matrix.
///
/// Column `i` occupies `m[i*4..i*4+4]`. Identity: columns are
/// `(1,0,0,0)`, `(0,1,0,0)`, `(0,0,1,0)`, `(0,0,0,1)`.
///
/// In WGSL this maps directly to `mat4x4<f32>` in a storage buffer
/// (same column-major layout, 64 bytes per element, align 16).
#[derive(Debug, Clone, Copy)]
pub struct Transform {
    /// Column-major: `m[col*4 + row]`.
    pub m: [f32; 16],
}

impl Transform {
    pub const IDENTITY: Self = Self {
        m: [
            1.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            0.0, 0.0, 0.0, 1.0,
        ],
    };

    /// 2D translation — moves by `(tx, ty)` in the XY plane.
    pub fn translate_2d(tx: f32, ty: f32) -> Self {
        Self {
            m: [
                1.0, 0.0, 0.0, 0.0,
                0.0, 1.0, 0.0, 0.0,
                0.0, 0.0, 1.0, 0.0,
                tx,  ty,  0.0, 1.0,
            ],
        }
    }

    /// 2D counter-clockwise rotation by `angle_radians` around the origin.
    pub fn rotate_2d(angle_radians: f32) -> Self {
        let (s, c) = angle_radians.sin_cos();
        Self {
            m: [
                 c,   s,  0.0, 0.0,
                -s,   c,  0.0, 0.0,
                0.0, 0.0, 1.0, 0.0,
                0.0, 0.0, 0.0, 1.0,
            ],
        }
    }

    /// 2D uniform scale by `(sx, sy)` around the origin.
    pub fn scale_2d(sx: f32, sy: f32) -> Self {
        Self {
            m: [
                sx,  0.0, 0.0, 0.0,
                0.0, sy,  0.0, 0.0,
                0.0, 0.0, 1.0, 0.0,
                0.0, 0.0, 0.0, 1.0,
            ],
        }
    }

    /// Returns the transform that applies `self` first, then `other`.
    /// Equivalent to the matrix product `other × self`.
    ///
    /// Example: `scale.then(rotate).then(translate)` applies scale,
    /// then rotation around origin, then translation.
    pub fn then(&self, other: &Transform) -> Transform {
        // C = other × self.
        // C[col*4+row] = Σ_k  other.m[k*4+row] × self.m[col*4+k]
        let a = &other.m;
        let b = &self.m;
        let mut c = [0.0f32; 16];
        for col in 0..4usize {
            for row in 0..4usize {
                let mut s = 0.0f32;
                for k in 0..4usize {
                    s += a[k * 4 + row] * b[col * 4 + k];
                }
                c[col * 4 + row] = s;
            }
        }
        Transform { m: c }
    }
}

/// One solid-colored rectangle with a per-primitive transform and an
/// optional axis-aligned device-space clip rectangle.
///
/// `x0/y0/x1/y1` are in **local space** — the transform at
/// `transform_id` maps them to device-pixel space. When
/// `transform_id == 0` (identity) the coordinates are device-pixel
/// coordinates directly (backward-compatible with Phase 2).
#[derive(Debug, Clone)]
pub struct SceneRect {
    /// Local-space left / top / right / bottom.
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// Premultiplied RGBA.
    pub color: [f32; 4],
    /// Index into `Scene::transforms`. `0` is always the identity.
    pub transform_id: u32,
    /// Axis-aligned clip rectangle in device pixels `[x0, y0, x1, y1]`.
    /// `[NEG_INFINITY, NEG_INFINITY, INFINITY, INFINITY]` disables clipping.
    pub clip_rect: [f32; 4],
    /// Per-corner radii in device pixels: `[top_left, top_right,
    /// bottom_right, bottom_left]`. All zeros = sharp axis-aligned
    /// clip (default). Non-zero radii produce a rounded-rect clip;
    /// the clip is generated via vello `push_layer` with a
    /// `kurbo::RoundedRect` shape (Phase 9').
    pub clip_corner_radii: [f32; 4],
}

pub const NO_CLIP: [f32; 4] =
    [f32::NEG_INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::INFINITY];

/// Sharp / axis-aligned clip — all four corner radii at zero. Used as
/// the default `clip_corner_radii` value in Scene helper methods that
/// don't accept rounded-rect parameters.
pub const SHARP_CLIP: [f32; 4] = [0.0, 0.0, 0.0, 0.0];

/// Opaque identifier for a cached GPU texture. Caller-assigned; any
/// unique `u64` works (hash of path, monotonic counter, etc.).
pub type ImageKey = u64;

/// CPU-side pixel data for one image. Format: RGBA8Unorm, row-major,
/// tightly packed (`width * 4` bytes per row). sRGB handling is deferred
/// to Phase 7; for now the bytes are treated as linear values.
#[derive(Debug, Clone)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    /// Raw RGBA8 bytes; `len()` must equal `width * height * 4`.
    pub bytes: Vec<u8>,
}

/// One stop in an N-stop gradient ramp.
///
/// Phase 8D bundles linear, radial, and conic gradients under one
/// primitive type. Each gradient carries an arbitrary-length stops
/// vec; consecutive entries with offsets `[a, b]` define a segment
/// over which the color interpolates linearly.
#[derive(Debug, Clone, Copy)]
pub struct GradientStop {
    /// Position along the gradient parameter `t`, in `[0, 1]`.
    pub offset: f32,
    /// Premultiplied RGBA at this position.
    pub color: [f32; 4],
}

/// One analytic gradient rectangle (Phase 8D unified).
///
/// `kind` selects linear / radial / conic, which determines how the
/// fragment shader maps each pixel to a `t` value. `params` carries
/// kind-specific configuration in a 4-float slot:
///
/// - Linear: `[start_x, start_y, end_x, end_y]`. `t = projection of
///   pixel onto the gradient line`.
/// - Radial: `[cx, cy, rx, ry]`. Set `rx == ry` for circular.
///   `t = length((pixel - center) / radii)`.
/// - Conic:  `[cx, cy, start_angle, _pad]`. `start_angle` is the seam
///   in radians (with y+ down, atan2 increases clockwise). `t =
///   fract((atan2(dy, dx) - start_angle) / 2π)`.
///
/// Once `t` is known, `stops` defines the color: clamps to first/last
/// stop for `t` outside `[0, 1]` (or outside the stops' offset range);
/// otherwise interpolates between the two adjacent stops bracketing
/// `t`. All stop colors are **premultiplied**.
///
/// A gradient is rendered through the opaque pipeline iff every stop
/// color has `alpha >= 1.0`; otherwise the alpha pipeline runs.
#[derive(Debug, Clone)]
pub struct SceneGradient {
    /// Local-space rect bounds.
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// Which gradient family this primitive uses.
    pub kind: GradientKind,
    /// Kind-dependent parameter slot (see struct docs).
    pub params: [f32; 4],
    /// Color stops along the gradient parameter, sorted by `offset`
    /// ascending. Phase 8D supports arbitrary lengths; 2 is the
    /// minimum for a meaningful gradient.
    pub stops: Vec<GradientStop>,
    /// Index into `Scene::transforms`; `0` = identity.
    pub transform_id: u32,
    /// Device-space axis-aligned clip; `NO_CLIP` disables clipping.
    pub clip_rect: [f32; 4],
    /// Per-corner clip radii (see `SceneRect::clip_corner_radii`).
    pub clip_corner_radii: [f32; 4],
}

/// One textured rectangle. UV corners map the image onto the rect;
/// the tint color is multiplied element-wise with the sampled value
/// (premultiplied; `[1,1,1,1]` = no tint).
#[derive(Debug, Clone)]
pub struct SceneImage {
    /// Local-space corners (same coordinate system as `SceneRect`).
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// UV corners `[u0, v0, u1, v1]` in normalised `[0, 1]` space.
    /// `[0, 0, 1, 1]` maps the full image to the rect.
    pub uv: [f32; 4],
    /// Premultiplied RGBA tint. `[1, 1, 1, 1]` is a no-op.
    pub color: [f32; 4],
    /// Cache key for the GPU texture (see `Scene::set_image_source`).
    pub key: ImageKey,
    /// Index into `Scene::transforms`; `0` = identity.
    pub transform_id: u32,
    /// Device-space axis-aligned clip; `NO_CLIP` disables clipping.
    pub clip_rect: [f32; 4],
    /// Per-corner clip radii (see `SceneRect::clip_corner_radii`).
    pub clip_corner_radii: [f32; 4],
}

/// Phase 11' stroked rect / rounded-rect primitive — for borders,
/// edge outlines, and other line-decoration use cases. Strokes are
/// centered on the path; the painted region extends `stroke_width / 2`
/// inside and outside the path.
///
/// `x0/y0/x1/y1` define the path being stroked (the geometric center
/// of the resulting line). `stroke_corner_radii` rounds the path
/// itself (CSS `border-radius` behaviour). `clip_rect` /
/// `clip_corner_radii` clip the stroke output the same way they do
/// for fills — orthogonal to the path geometry.
#[derive(Debug, Clone)]
pub struct SceneStroke {
    /// Local-space rect bounds of the stroked path.
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// Premultiplied RGBA stroke color.
    pub color: [f32; 4],
    /// Stroke width in device pixels (path is the geometric center;
    /// painted region extends ±width/2).
    pub stroke_width: f32,
    /// Per-corner radii of the stroked path itself, in device pixels:
    /// `[top_left, top_right, bottom_right, bottom_left]`. All zeros
    /// produce a sharp rectangular stroke; non-zero radii produce a
    /// rounded-rect stroke (CSS `border-radius`).
    pub stroke_corner_radii: [f32; 4],
    /// Index into `Scene::transforms`; `0` = identity.
    pub transform_id: u32,
    /// Device-space axis-aligned clip; `NO_CLIP` disables clipping.
    pub clip_rect: [f32; 4],
    /// Per-corner clip radii (see `SceneRect::clip_corner_radii`).
    pub clip_corner_radii: [f32; 4],
}

/// Phase 10a' opaque handle into [`Scene::fonts`]. Returned by
/// [`Scene::push_font`]. Values are stable indices into the per-
/// frame font palette; index `0` is reserved for "no font".
pub type FontId = u32;

/// Phase 10a' font payload. Wraps a CPU-side TTF / OTF blob plus an
/// index for font collections (TTC). The translator builds a
/// `peniko::FontData` from this on demand; the wrapper exists so
/// netrender's Scene API doesn't leak peniko types.
#[derive(Debug, Clone)]
pub struct FontBlob {
    /// Raw font bytes (TTF / OTF / TTC). `Arc`-shared so multiple
    /// scenes can reference the same font without copying.
    pub data: std::sync::Arc<Vec<u8>>,
    /// Index within the collection. `0` for single-font files.
    pub index: u32,
}

/// Phase 10a' single glyph entry — id + position. Matches
/// `vello::Glyph`'s shape so the translator passes through with
/// minimal conversion. Caller is responsible for shaping (turning
/// strings into glyph IDs + positions); netrender doesn't do
/// layout. See plan §4.4.
#[derive(Debug, Clone, Copy)]
pub struct Glyph {
    /// Glyph index within the font's outline table.
    pub id: u32,
    /// Glyph origin x in local space (typically the baseline left
    /// edge after shaping advance).
    pub x: f32,
    /// Glyph origin y in local space (baseline).
    pub y: f32,
}

/// Phase 10a' glyph run primitive — a sequence of glyphs from one
/// font, painted with one solid color. Vello's
/// `Scene::draw_glyphs(font).font_size(s).brush(c).draw(...)`
/// builder is the rasterization target.
#[derive(Debug, Clone)]
pub struct SceneGlyphRun {
    /// Font palette index. Use [`Scene::push_font`] to register a
    /// font and obtain this id.
    pub font_id: FontId,
    /// Font size in pixels per em.
    pub font_size: f32,
    /// Glyph sequence. Each carries an id (font-internal) and a
    /// local-space origin position; the translator hands them to
    /// vello unchanged.
    pub glyphs: Vec<Glyph>,
    /// Premultiplied RGBA brush color for the entire run.
    pub color: [f32; 4],
    /// Index into `Scene::transforms`; `0` = identity.
    pub transform_id: u32,
    /// Device-space axis-aligned clip; `NO_CLIP` disables clipping.
    pub clip_rect: [f32; 4],
    /// Per-corner clip radii (see `SceneRect::clip_corner_radii`).
    pub clip_corner_radii: [f32; 4],
}

/// Phase 11b' path operation. The `ScenePath` builder produces a
/// `Vec<PathOp>` that the vello translator converts into a
/// `kurbo::BezPath`. Coordinates are in local space; the
/// primitive's `transform_id` maps them to device pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathOp {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    QuadTo(f32, f32, f32, f32),
    CubicTo(f32, f32, f32, f32, f32, f32),
    Close,
}

/// Phase 11b' arbitrary path. Build via the move_to / line_to /
/// quad_to / cubic_to / close methods, or construct directly
/// with `ops`. Used by [`SceneShape`].
#[derive(Debug, Clone, Default)]
pub struct ScenePath {
    pub ops: Vec<PathOp>,
}

impl ScenePath {
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }

    pub fn with_capacity(n: usize) -> Self {
        Self { ops: Vec::with_capacity(n) }
    }

    pub fn move_to(&mut self, x: f32, y: f32) -> &mut Self {
        self.ops.push(PathOp::MoveTo(x, y));
        self
    }

    pub fn line_to(&mut self, x: f32, y: f32) -> &mut Self {
        self.ops.push(PathOp::LineTo(x, y));
        self
    }

    pub fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) -> &mut Self {
        self.ops.push(PathOp::QuadTo(cx, cy, x, y));
        self
    }

    pub fn cubic_to(
        &mut self,
        c1x: f32, c1y: f32,
        c2x: f32, c2y: f32,
        x: f32, y: f32,
    ) -> &mut Self {
        self.ops.push(PathOp::CubicTo(c1x, c1y, c2x, c2y, x, y));
        self
    }

    pub fn close(&mut self) -> &mut Self {
        self.ops.push(PathOp::Close);
        self
    }

    /// Local-space axis-aligned bounding box of the path's control
    /// points. Used by the tile-cache filter; conservative (the
    /// actual path stays inside the convex hull of the control
    /// points, so this is an upper bound).
    pub fn local_aabb(&self) -> Option<[f32; 4]> {
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        let mut got_any = false;
        for op in &self.ops {
            let mut update = |x: f32, y: f32| {
                got_any = true;
                if x < min_x { min_x = x; }
                if y < min_y { min_y = y; }
                if x > max_x { max_x = x; }
                if y > max_y { max_y = y; }
            };
            match *op {
                PathOp::MoveTo(x, y) | PathOp::LineTo(x, y) => update(x, y),
                PathOp::QuadTo(cx, cy, x, y) => { update(cx, cy); update(x, y); }
                PathOp::CubicTo(c1x, c1y, c2x, c2y, x, y) => {
                    update(c1x, c1y); update(c2x, c2y); update(x, y);
                }
                PathOp::Close => {}
            }
        }
        if got_any {
            Some([min_x, min_y, max_x, max_y])
        } else {
            None
        }
    }
}

/// Phase 11b' stroke style. `width` in device pixels; future fields
/// (cap / join / dash / miter limit) when consumers need them.
#[derive(Debug, Clone, Copy)]
pub struct ScenePathStroke {
    pub color: [f32; 4],
    pub width: f32,
}

/// Phase 11b' arbitrary-path primitive. Carries both an optional
/// fill and an optional stroke so a single push can produce a CSS /
/// SVG-style "filled then stroked" shape without duplicating the
/// path data. At least one of `fill_color` or `stroke` must be set
/// or the shape is silently no-op.
#[derive(Debug, Clone)]
pub struct SceneShape {
    pub path: ScenePath,
    /// Premultiplied RGBA fill color. `None` skips the fill.
    pub fill_color: Option<[f32; 4]>,
    /// Stroke style. `None` skips the stroke.
    pub stroke: Option<ScenePathStroke>,
    /// Index into `Scene::transforms`; `0` = identity.
    pub transform_id: u32,
    /// Device-space axis-aligned clip; `NO_CLIP` disables clipping.
    pub clip_rect: [f32; 4],
    /// Per-corner clip radii (see `SceneRect::clip_corner_radii`).
    pub clip_corner_radii: [f32; 4],
}

/// A flat list of primitives to be rendered into one frame.
///
/// Phase 3 adds `transforms` (a palette of 4×4 matrices) and per-rect
/// `transform_id` / `clip_rect`. Phase 4 sorts for correct depth order.
/// Phase 5 adds `images` (textured rects) and `image_sources` (pixel data).
///
/// Draw order: rects are at painter indices 0..N_rects; images follow at
/// indices N_rects..N_total. Images therefore paint "in front of" all rects
/// in depth — correct for overlays.
#[derive(Debug, Clone)]
pub struct Scene {
    /// Viewport size in device pixels.
    pub viewport_width: u32,
    pub viewport_height: u32,
    /// Solid-color primitives in painter order (back-to-front).
    pub rects: Vec<SceneRect>,
    /// Textured-rect primitives in painter order (back-to-front).
    /// These paint on top of all rects.
    pub images: Vec<SceneImage>,
    /// Analytic gradients (linear / radial / conic, N-stop) in
    /// painter order (back-to-front). Phase 8D unifies the three
    /// gradient families into one list — push order is preserved
    /// across kinds, including within-frame interleaving.
    pub gradients: Vec<SceneGradient>,
    /// Phase 11' stroked-rect / rounded-rect primitives — borders,
    /// edge outlines, line decorations. Painter order: strokes paint
    /// after rects but before gradients/images, matching natural
    /// CSS render order (background → border → contents).
    pub strokes: Vec<SceneStroke>,
    /// Phase 11b' arbitrary-path primitives — filled + stroked
    /// shapes for SVG-style content, custom node frames, etc.
    /// Painter order: shapes paint last, after images.
    pub shapes: Vec<SceneShape>,
    /// Phase 10a' glyph runs — text. Painter order: glyph runs
    /// paint after shapes (text-on-top is the typical case).
    pub glyph_runs: Vec<SceneGlyphRun>,
    /// Phase 10a' font palette. Index `0` is reserved (panic on
    /// push_glyph_run with `font_id = 0`); real fonts start at
    /// index 1.
    pub fonts: Vec<FontBlob>,
    /// Transform palette. Index 0 is always identity.
    pub transforms: Vec<Transform>,
    /// CPU-side pixel data keyed by `ImageKey`. On first `prepare()`,
    /// each entry is uploaded to the GPU and cached there. Subsequent
    /// frames may omit data for already-cached keys.
    pub image_sources: HashMap<ImageKey, ImageData>,
}

impl Scene {
    pub fn new(viewport_width: u32, viewport_height: u32) -> Self {
        Self {
            viewport_width,
            viewport_height,
            rects: Vec::new(),
            images: Vec::new(),
            gradients: Vec::new(),
            strokes: Vec::new(),
            shapes: Vec::new(),
            glyph_runs: Vec::new(),
            // Index 0 reserved as a no-font sentinel; real fonts
            // start at index 1.
            fonts: vec![FontBlob {
                data: std::sync::Arc::new(Vec::new()),
                index: 0,
            }],
            transforms: vec![Transform::IDENTITY], // index 0 = identity
            image_sources: HashMap::new(),
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
        self.rects.push(SceneRect {
            x0, y0, x1, y1,
            color,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
        });
    }

    /// Append a rect with an explicit transform id.
    pub fn push_rect_transformed(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        color: [f32; 4],
        transform_id: u32,
    ) {
        self.rects.push(SceneRect {
            x0, y0, x1, y1,
            color,
            transform_id,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
        });
    }

    /// Append a rect with an explicit transform and a device-space
    /// axis-aligned clip.
    pub fn push_rect_clipped(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        color: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
    ) {
        self.rects.push(SceneRect {
            x0, y0, x1, y1,
            color,
            transform_id,
            clip_rect,
            clip_corner_radii: SHARP_CLIP,
        });
    }

    /// Append a rect with a rounded-rect clip (Phase 9'). `clip_corner_radii`
    /// is `[top_left, top_right, bottom_right, bottom_left]` in device
    /// pixels. All-zero radii degenerate to the same result as
    /// `push_rect_clipped` (a sharp axis-aligned clip).
    pub fn push_rect_clipped_rounded(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        color: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
        clip_corner_radii: [f32; 4],
    ) {
        self.rects.push(SceneRect {
            x0, y0, x1, y1,
            color,
            transform_id,
            clip_rect,
            clip_corner_radii,
        });
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
        x0: f32, y0: f32, x1: f32, y1: f32,
        key: ImageKey,
        data: ImageData,
    ) {
        self.image_sources.entry(key).or_insert(data);
        self.images.push(SceneImage {
            x0, y0, x1, y1,
            uv: [0.0, 0.0, 1.0, 1.0],
            color: [1.0, 1.0, 1.0, 1.0],
            key,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
        });
    }

    /// Phase 8D general API: push an arbitrary-kind, arbitrary-stops
    /// gradient. The 2-stop convenience methods below build a
    /// `SceneGradient` and forward to this.
    pub fn push_gradient(&mut self, gradient: SceneGradient) {
        self.gradients.push(gradient);
    }

    /// 2-stop linear gradient (Phase 8A convenience; preserved post-8D).
    pub fn push_linear_gradient(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        start: [f32; 2],
        end: [f32; 2],
        color0: [f32; 4],
        color1: [f32; 4],
    ) {
        self.gradients.push(two_stop_gradient(
            GradientKind::Linear,
            x0, y0, x1, y1,
            [start[0], start[1], end[0], end[1]],
            color0, color1,
            0, NO_CLIP,
        ));
    }

    /// 2-stop linear gradient with full control over transform and clip.
    pub fn push_linear_gradient_full(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        start: [f32; 2],
        end: [f32; 2],
        color0: [f32; 4],
        color1: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
    ) {
        self.gradients.push(two_stop_gradient(
            GradientKind::Linear,
            x0, y0, x1, y1,
            [start[0], start[1], end[0], end[1]],
            color0, color1,
            transform_id, clip_rect,
        ));
    }

    /// 2-stop radial gradient (Phase 8B convenience). For circular,
    /// pass `radii = [r, r]`. Color0 at center, color1 at the
    /// elliptical boundary (clamps beyond).
    pub fn push_radial_gradient(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        center: [f32; 2],
        radii: [f32; 2],
        color0: [f32; 4],
        color1: [f32; 4],
    ) {
        self.gradients.push(two_stop_gradient(
            GradientKind::Radial,
            x0, y0, x1, y1,
            [center[0], center[1], radii[0], radii[1]],
            color0, color1,
            0, NO_CLIP,
        ));
    }

    /// 2-stop conic gradient (Phase 8C convenience). `t = 0` at
    /// `start_angle`, sweeping clockwise (with y-down screen coords)
    /// back to the seam at `t = 1`.
    pub fn push_conic_gradient(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        center: [f32; 2],
        start_angle: f32,
        color0: [f32; 4],
        color1: [f32; 4],
    ) {
        self.gradients.push(two_stop_gradient(
            GradientKind::Conic,
            x0, y0, x1, y1,
            [center[0], center[1], start_angle, 0.0],
            color0, color1,
            0, NO_CLIP,
        ));
    }

    /// 2-stop conic gradient with full control over transform and clip.
    pub fn push_conic_gradient_full(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        center: [f32; 2],
        start_angle: f32,
        color0: [f32; 4],
        color1: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
    ) {
        self.gradients.push(two_stop_gradient(
            GradientKind::Conic,
            x0, y0, x1, y1,
            [center[0], center[1], start_angle, 0.0],
            color0, color1,
            transform_id, clip_rect,
        ));
    }

    /// 2-stop radial gradient with full control over transform and clip.
    pub fn push_radial_gradient_full(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        center: [f32; 2],
        radii: [f32; 2],
        color0: [f32; 4],
        color1: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
    ) {
        self.gradients.push(two_stop_gradient(
            GradientKind::Radial,
            x0, y0, x1, y1,
            [center[0], center[1], radii[0], radii[1]],
            color0, color1,
            transform_id, clip_rect,
        ));
    }

    /// Append an image rect with full control over UV, tint, transform,
    /// and clip.
    pub fn push_image_full(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        uv: [f32; 4],
        color: [f32; 4],
        key: ImageKey,
        transform_id: u32,
        clip_rect: [f32; 4],
    ) {
        self.images.push(SceneImage {
            x0, y0, x1, y1,
            uv,
            color,
            key,
            transform_id,
            clip_rect,
            clip_corner_radii: SHARP_CLIP,
        });
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
        self.glyph_runs.push(SceneGlyphRun {
            font_id,
            font_size,
            glyphs,
            color,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
        });
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
        self.glyph_runs.push(SceneGlyphRun {
            font_id,
            font_size,
            glyphs,
            color,
            transform_id,
            clip_rect,
            clip_corner_radii,
        });
    }

    /// Phase 11b': append a `SceneShape` directly. For most cases
    /// the convenience helpers `push_shape_filled` /
    /// `push_shape_stroked` are easier to use.
    pub fn push_shape(&mut self, shape: SceneShape) {
        self.shapes.push(shape);
    }

    /// Phase 11b': append an arbitrary path filled with a single
    /// solid color. Identity transform, no clip.
    pub fn push_shape_filled(&mut self, path: ScenePath, color: [f32; 4]) {
        self.shapes.push(SceneShape {
            path,
            fill_color: Some(color),
            stroke: None,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
        });
    }

    /// Phase 11b': append an arbitrary path stroked with a single
    /// solid color and line width. Identity transform, no clip.
    pub fn push_shape_stroked(
        &mut self,
        path: ScenePath,
        color: [f32; 4],
        stroke_width: f32,
    ) {
        self.shapes.push(SceneShape {
            path,
            fill_color: None,
            stroke: Some(ScenePathStroke { color, width: stroke_width }),
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
        });
    }

    /// Phase 11': append a sharp axis-aligned stroked rect (border).
    pub fn push_stroke(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        color: [f32; 4],
        stroke_width: f32,
    ) {
        self.strokes.push(SceneStroke {
            x0, y0, x1, y1,
            color,
            stroke_width,
            stroke_corner_radii: SHARP_CLIP,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
        });
    }

    /// Phase 11': append a stroked rounded-rect (CSS border with
    /// `border-radius`). `stroke_corner_radii` rounds the path
    /// itself, in `[top_left, top_right, bottom_right, bottom_left]`
    /// order. All-zero radii produce a sharp rectangular stroke.
    pub fn push_stroke_rounded(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        color: [f32; 4],
        stroke_width: f32,
        stroke_corner_radii: [f32; 4],
    ) {
        self.strokes.push(SceneStroke {
            x0, y0, x1, y1,
            color,
            stroke_width,
            stroke_corner_radii,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
        });
    }

    /// Phase 11': append a stroked rect/rounded-rect with full
    /// control over transform, clip, and clip corner radii.
    pub fn push_stroke_full(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        color: [f32; 4],
        stroke_width: f32,
        stroke_corner_radii: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
        clip_corner_radii: [f32; 4],
    ) {
        self.strokes.push(SceneStroke {
            x0, y0, x1, y1,
            color,
            stroke_width,
            stroke_corner_radii,
            transform_id,
            clip_rect,
            clip_corner_radii,
        });
    }

    /// Append an image rect with full control + rounded-rect clip
    /// (Phase 9'). See `push_rect_clipped_rounded` for the radii
    /// convention.
    pub fn push_image_full_rounded(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        uv: [f32; 4],
        color: [f32; 4],
        key: ImageKey,
        transform_id: u32,
        clip_rect: [f32; 4],
        clip_corner_radii: [f32; 4],
    ) {
        self.images.push(SceneImage {
            x0, y0, x1, y1,
            uv,
            color,
            key,
            transform_id,
            clip_rect,
            clip_corner_radii,
        });
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
        x0, y0, x1, y1,
        kind,
        params,
        stops: vec![
            GradientStop { offset: 0.0, color: color0 },
            GradientStop { offset: 1.0, color: color1 },
        ],
        transform_id,
        clip_rect,
        clip_corner_radii: SHARP_CLIP,
    }
}
