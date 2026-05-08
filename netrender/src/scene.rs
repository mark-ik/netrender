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
pub use netrender_device::SurfaceKey;

/// A 4×4 column-major transform matrix.
///
/// Column `i` occupies `m[i*4..i*4+4]`. Identity: columns are
/// `(1,0,0,0)`, `(0,1,0,0)`, `(0,0,1,0)`, `(0,0,0,1)`.
///
/// In WGSL this maps directly to `mat4x4<f32>` in a storage buffer
/// (same column-major layout, 64 bytes per element, align 16).
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
    #[cfg_attr(feature = "serde", serde(with = "clip_rect_serde"))]
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
/// tightly packed (`data.len()` must equal `width * height * 4`).
/// sRGB handling is deferred to Phase 7; for now the bytes are
/// treated as linear values.
///
/// `data` is a `peniko::Blob<u8>`, which is `Arc<Vec<u8>>` plus a
/// stable `Blob::id()`. Two consumers that share the same `Blob`
/// (cloning preserves id) hand the same atlas slot to vello —
/// cross-consumer image dedup is a free consequence of Arc-shared
/// bytes. See [`ImageRegistry`] for the cross-consumer key
/// coordination story; the data unification is the necessary
/// condition for it to work.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    /// Raw RGBA8 bytes wrapped in a peniko `Blob`. Use
    /// [`ImageData::from_bytes`] for the common
    /// "I have a `Vec<u8>`" construction path.
    #[cfg_attr(feature = "serde", serde(with = "blob_serde"))]
    pub data: vello::peniko::Blob<u8>,
}

impl ImageData {
    /// Construct an `ImageData` from raw bytes. Wraps the `Vec<u8>`
    /// in `Arc::new` and a fresh `peniko::Blob`. Two `from_bytes`
    /// calls with identical content produce *different* Blob ids;
    /// to share an atlas slot across consumers, use
    /// [`ImageData::from_blob`] with a shared blob.
    pub fn from_bytes(width: u32, height: u32, bytes: Vec<u8>) -> Self {
        Self {
            width,
            height,
            data: vello::peniko::Blob::new(std::sync::Arc::new(bytes)),
        }
    }

    /// Construct an `ImageData` from an existing `peniko::Blob`.
    /// Cloning a `Blob` is an `Arc` bump that preserves the id, so
    /// two `ImageData`s constructed from clones of the same blob
    /// dedup at the vello atlas level.
    pub fn from_blob(width: u32, height: u32, data: vello::peniko::Blob<u8>) -> Self {
        Self { width, height, data }
    }
}

/// One stop in an N-stop gradient ramp.
///
/// Phase 8D bundles linear, radial, and conic gradients under one
/// primitive type. Each gradient carries an arbitrary-length stops
/// vec; consecutive entries with offsets `[a, b]` define a segment
/// over which the color interpolates linearly.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
    #[cfg_attr(feature = "serde", serde(with = "clip_rect_serde"))]
    pub clip_rect: [f32; 4],
    /// Per-corner clip radii (see `SceneRect::clip_corner_radii`).
    pub clip_corner_radii: [f32; 4],
}

/// Roadmap C2 — repeated-tile fill primitive (CSS
/// `background-image` with `repeat`). The image identified by
/// `tile` is rendered at its native size scaled by `scale`,
/// repeating to cover the `extent` rectangle.
///
/// Tiling parameters:
///
/// - `tile`: [`ImageKey`] of the image to repeat.
/// - `extent`: `[x0, y0, x1, y1]` rectangle in local space; the
///   tiled fill covers this entire rect.
/// - `scale`: tile-size multiplier (1.0 = native pixel size; 2.0
///   doubles the tile size). Negative or zero values are treated
///   as 1.0 by the rasterizer.
///
/// Compared to pushing N `SceneImage` ops by hand, one
/// `SceneOp::Pattern` covers a 256×256 area with a 64×64 tile in a
/// single push.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ScenePattern {
    /// Image to repeat.
    pub tile: ImageKey,
    /// `[x0, y0, x1, y1]` in local space.
    pub extent: [f32; 4],
    /// Tile size multiplier (1.0 = native pixel size).
    pub scale: f32,
    /// Index into `Scene::transforms`; `0` = identity.
    pub transform_id: u32,
    /// Device-space axis-aligned clip; `NO_CLIP` disables clipping.
    #[cfg_attr(feature = "serde", serde(with = "clip_rect_serde"))]
    pub clip_rect: [f32; 4],
    /// Per-corner clip radii (see `SceneRect::clip_corner_radii`).
    pub clip_corner_radii: [f32; 4],
}

/// One textured rectangle. UV corners map the image onto the rect;
/// the tint color is multiplied element-wise with the sampled value
/// (premultiplied; `[1,1,1,1]` = no tint).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
    #[cfg_attr(feature = "serde", serde(with = "clip_rect_serde"))]
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
/// Roadmap C1 — line-cap style for stroked paths. Maps 1:1 to
/// `kurbo::Cap`. Default `Butt` matches CSS `stroke-linecap: butt`
/// and the kurbo default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SceneStrokeCap {
    /// No extension past the path endpoint. CSS `butt`.
    #[default]
    Butt,
    /// Half-circle past the endpoint with radius `width / 2`.
    /// CSS `round`.
    Round,
    /// Square extension by `width / 2`. CSS `square`.
    Square,
}

/// Roadmap C1 — line-join style for stroked paths at corners. Maps
/// 1:1 to `kurbo::Join`. Default `Miter` matches CSS
/// `stroke-linejoin: miter` and the kurbo default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SceneStrokeJoin {
    /// Bevel join — corner is filled with a triangle. CSS `bevel`.
    Bevel,
    /// Miter join — extend outer edges to a sharp point. CSS
    /// `miter`.
    #[default]
    Miter,
    /// Round join — fill the corner with a circular arc. CSS
    /// `round`.
    Round,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
    #[cfg_attr(feature = "serde", serde(with = "clip_rect_serde"))]
    pub clip_rect: [f32; 4],
    /// Per-corner clip radii (see `SceneRect::clip_corner_radii`).
    pub clip_corner_radii: [f32; 4],
    /// Roadmap C1 — line-cap style for stroke endpoints. Default
    /// [`SceneStrokeCap::Butt`].
    pub cap: SceneStrokeCap,
    /// Roadmap C1 — line-join style for stroke corners. Default
    /// [`SceneStrokeJoin::Miter`].
    pub join: SceneStrokeJoin,
    /// Roadmap C1 — dash pattern in device pixels (alternating
    /// on / off lengths). Empty means a solid stroke. Maps to
    /// `kurbo::Stroke::with_dashes`.
    pub dash_pattern: Vec<f32>,
    /// Roadmap C1 — phase offset into the dash pattern in device
    /// pixels. Ignored when `dash_pattern` is empty.
    pub dash_offset: f32,
}

/// Phase 10a' opaque handle into [`Scene::fonts`]. Returned by
/// [`Scene::push_font`]. Values are stable indices into the per-
/// frame font palette; index `0` is reserved for "no font".
pub type FontId = u32;

/// Phase 10a' font payload. Wraps a CPU-side TTF / OTF blob plus an
/// index for font collections (TTC). Holds a `peniko::Blob<u8>`
/// directly: peniko mints a unique `Blob::id()` at construction and
/// preserves it through clone, which is what vello's font atlas
/// keys on for cross-frame dedup. Constructing a fresh `Blob` per
/// frame defeats that dedup; consumers should hold their `FontBlob`
/// across frames and clone it rather than rebuild from raw bytes.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FontBlob {
    /// Font bytes (TTF / OTF / TTC) wrapped in a peniko `Blob`. The
    /// blob's id is the cross-frame identity vello uses to dedup
    /// font uploads.
    #[cfg_attr(feature = "serde", serde(with = "blob_serde"))]
    pub data: vello::peniko::Blob<u8>,
    /// Index within the collection. `0` for single-font files.
    pub index: u32,
}

/// Phase 10a' single glyph entry — id + position. Matches
/// `vello::Glyph`'s shape so the translator passes through with
/// minimal conversion. Caller is responsible for shaping (turning
/// strings into glyph IDs + positions); netrender doesn't do
/// layout. See plan §4.4.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
/// Roadmap C4 — 4-byte font-variation axis tag (e.g., `*b"wght"`,
/// `*b"wdth"`, `*b"slnt"`). Bytes are ASCII per OpenType spec.
pub type SceneFontAxisTag = [u8; 4];

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
    #[cfg_attr(feature = "serde", serde(with = "clip_rect_serde"))]
    pub clip_rect: [f32; 4],
    /// Per-corner clip radii (see `SceneRect::clip_corner_radii`).
    pub clip_corner_radii: [f32; 4],
    /// Roadmap C4 — variable-font axis values in user space (e.g.,
    /// `(*b"wght", 700.0)` for weight 700). Empty means "use the
    /// font's default location." Tag bytes that don't match any
    /// axis in the font are ignored; unset axes get the font's
    /// default. Threaded through to vello via
    /// `DrawGlyphs::normalized_coords` after skrifa-side
    /// user→normalized-space conversion.
    pub font_axis_values: Vec<(SceneFontAxisTag, f32)>,
}

/// Phase 11b' path operation. The `ScenePath` builder produces a
/// `Vec<PathOp>` that the vello translator converts into a
/// `kurbo::BezPath`. Coordinates are in local space; the
/// primitive's `transform_id` maps them to device pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SceneShape {
    pub path: ScenePath,
    /// Premultiplied RGBA fill color. `None` skips the fill.
    pub fill_color: Option<[f32; 4]>,
    /// Stroke style. `None` skips the stroke.
    pub stroke: Option<ScenePathStroke>,
    /// Index into `Scene::transforms`; `0` = identity.
    pub transform_id: u32,
    /// Device-space axis-aligned clip; `NO_CLIP` disables clipping.
    #[cfg_attr(feature = "serde", serde(with = "clip_rect_serde"))]
    pub clip_rect: [f32; 4],
    /// Per-corner clip radii (see `SceneRect::clip_corner_radii`).
    pub clip_corner_radii: [f32; 4],
}

/// Roadmap C3 — compose mode controlling how a layer composites
/// back into its parent at pop time. Mirrors `peniko::Compose`
/// with a netrender-owned enum so the Scene API stays peniko-free.
///
/// `SrcOver` is the default; layer pixels paint over destination
/// pixels (the standard Porter-Duff "source-over"). `DestIn` is
/// the alpha-mask compose: destination pixels survive only where
/// source (this layer) is opaque — the mechanism behind
/// `Scene::push_alpha_mask_layer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum SceneCompose {
    /// Standard "source-over" — layer pixels paint over destination.
    #[default]
    SrcOver = 0,
    /// "Destination-in" — destination only where source is opaque.
    /// Used to mask outer-layer content by an inner mask layer.
    DestIn = 1,
}

/// Phase 12a' scene-level blend mode. Mirrors `peniko::Mix` with a
/// netrender-owned enum so the Scene API stays peniko-free. Maps
/// 1-to-1 in the translator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum SceneBlendMode {
    /// Default — straight `source-over` compositing.
    Normal = 0,
    /// `mix-blend-mode: multiply` — darken (component-wise product).
    Multiply = 1,
    /// `mix-blend-mode: screen` — lighten (1 - (1-src)*(1-dst)).
    Screen = 2,
    /// `mix-blend-mode: overlay`.
    Overlay = 3,
    /// `mix-blend-mode: darken`.
    Darken = 4,
    /// `mix-blend-mode: lighten`.
    Lighten = 5,
    // (More blend modes are exposed by peniko::Mix; add here as
    // consumers need them. The mapping in vello_rasterizer.rs
    // panics on unknown variants — keep this enum and the match
    // arm in sync.)
}

/// Phase 12b' — clip shape carried by a [`SceneLayer`].
///
/// Selecting between rect / rounded-rect / arbitrary path lets the
/// renderer skip layer overhead when the clip is the viewport, use
/// vello's fast rounded-rect path for the common rounded case, or
/// fall back to a `BezPath` for SVG-style `clipPath` (Phase 9b').
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SceneClip {
    /// No clip: the layer covers the viewport. Useful for layers
    /// whose effect is alpha or blend-mode only.
    None,
    /// Axis-aligned (optionally rounded) rect clip.
    /// `radii` is `[top_left, top_right, bottom_right, bottom_left]`.
    /// All-zero radii are a sharp clip.
    Rect { rect: [f32; 4], radii: [f32; 4] },
    /// Phase 9b' arbitrary-path clip (SVG `clipPath`-shaped).
    /// The path's local space is mapped to scene-space via
    /// [`SceneLayer::transform_id`].
    Path(ScenePath),
}

/// Roadmap D1 — backdrop filter applied to the *parent* scene
/// content beneath a layer. Currently supports `Blur(radius_px)`;
/// other CSS `backdrop-filter` values (saturate, brightness,
/// contrast, drop-shadow) can be added as new variants when a real
/// consumer needs them.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SceneFilter {
    /// Gaussian blur with the given radius in device pixels.
    /// Maps to CSS `backdrop-filter: blur(<radius>px)`.
    Blur(f32),
}

/// Phase 12b' — a nested layer scope opened by [`SceneOp::PushLayer`]
/// and closed by [`SceneOp::PopLayer`]. Every op between the matched
/// pair is rendered into the layer and composited back to the parent
/// with the given alpha + blend mode, optionally clipped by `clip`.
///
/// CSS analogues:
///   - `opacity`: `alpha < 1.0` with `blend_mode = Normal`, `clip = None`
///   - `mix-blend-mode`: `blend_mode != Normal`, `alpha = 1.0`, `clip = None`
///   - `clip-path` / `overflow: hidden border-radius`: `clip = Rect/Path`
///   - `filter`: composes with these via additional layers
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SceneLayer {
    /// Clip shape for the layer. See [`SceneClip`].
    pub clip: SceneClip,
    /// Multiplied with every pixel inside the layer when composing
    /// back to parent. `1.0` is no-op.
    pub alpha: f32,
    /// Mix mode used to composite the layer back into its parent
    /// (Normal / Multiply / Screen / Overlay / Darken / Lighten).
    pub blend_mode: SceneBlendMode,
    /// Roadmap C3 — compose mode used to composite the layer into
    /// its parent. `SrcOver` (default) is the standard layer paint;
    /// `DestIn` makes the parent visible only where this layer is
    /// opaque (alpha-mask).
    pub compose: SceneCompose,
    /// Index into `Scene::transforms` applied to the clip shape.
    /// Inner ops carry their own `transform_id`s.
    pub transform_id: u32,
    /// Roadmap D1 — when `Some`, the parent scene's content
    /// underneath this layer's clip region is filtered (typically
    /// blurred) before this layer's own ops paint over it. The
    /// renderer handles the multi-pass orchestration; consumers
    /// pass the filter description and netrender does the
    /// pre-render → blur → composite dance.
    pub backdrop_filter: Option<SceneFilter>,
}

impl SceneLayer {
    /// Convenience: a layer with the given alpha, no clip, normal
    /// blend mode, identity transform.
    pub fn alpha(alpha: f32) -> Self {
        Self {
            clip: SceneClip::None,
            alpha,
            blend_mode: SceneBlendMode::Normal,
            compose: SceneCompose::SrcOver,
            transform_id: 0,
            backdrop_filter: None,
        }
    }

    /// Convenience: a clip-only layer (alpha 1, blend Normal,
    /// identity transform) with the given clip.
    pub fn clip(clip: SceneClip) -> Self {
        Self {
            clip,
            alpha: 1.0,
            blend_mode: SceneBlendMode::Normal,
            compose: SceneCompose::SrcOver,
            transform_id: 0,
            backdrop_filter: None,
        }
    }

    /// Roadmap C3 — alpha-mask layer. Pushes a layer that, when
    /// popped, makes the *enclosing* layer's content visible only
    /// where this layer's content is opaque (peniko's
    /// `Compose::DestIn`).
    ///
    /// The intended usage pattern:
    ///
    /// ```text
    /// scene.push_layer(SceneLayer::clip(...))    // outer
    /// // → render content (will be masked)
    /// scene.push_layer(SceneLayer::alpha_mask())  // inner DestIn
    /// // → render mask (e.g., scene.push_image_full(mask_key, ...))
    /// scene.pop_layer()                          // commits the mask
    /// scene.pop_layer()                          // outer pops with content + mask applied
    /// ```
    ///
    /// A high-level helper [`Scene::push_layer_mask`] / pair
    /// orchestrates the push/pop bookkeeping for the common case.
    pub fn alpha_mask() -> Self {
        Self {
            clip: SceneClip::None,
            alpha: 1.0,
            blend_mode: SceneBlendMode::Normal,
            compose: SceneCompose::DestIn,
            transform_id: 0,
            backdrop_filter: None,
        }
    }
}

/// One draw operation in a [`Scene`]'s painter-order op list.
///
/// Each `push_*` helper on [`Scene`] appends one of these variants to
/// `Scene::ops`. The rasterizer iterates `ops` in sequence and
/// dispatches per variant. The variants are *carriers*, not new
/// primitive types — each wraps the same struct the per-type Vec
/// design used.
///
/// To traverse a scene by primitive type, prefer the `iter_*`
/// helpers ([`Scene::iter_rects`], etc.) over manual matching;
/// they're filter-iterator wrappers over `self.ops`.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SceneOp {
    /// A solid-color rectangle. See [`SceneRect`].
    Rect(SceneRect),
    /// A stroked rectangle / rounded-rect (border).
    Stroke(SceneStroke),
    /// An analytic gradient (linear / radial / conic, N-stop).
    Gradient(SceneGradient),
    /// A textured rectangle (image fill).
    Image(SceneImage),
    /// Roadmap C2 — repeated-tile fill (CSS `background-image:
    /// repeat`). See [`ScenePattern`].
    Pattern(ScenePattern),
    /// An arbitrary path (filled or stroked).
    Shape(SceneShape),
    /// A run of positioned glyphs in one font + size + color.
    GlyphRun(SceneGlyphRun),
    /// Phase 12b' — open a nested layer scope. All subsequent ops
    /// up to the matching [`SceneOp::PopLayer`] paint into the
    /// layer; the layer is then composited into the parent with
    /// the carried alpha + blend mode + clip. Layers nest.
    PushLayer(SceneLayer),
    /// Phase 12b' — close the most recently opened layer scope.
    /// Unbalanced `PopLayer`s (without a matching `PushLayer`) are
    /// the consumer's bug; the renderer panics in debug.
    PopLayer,
}

/// A flat list of primitives to be rendered into one frame.
///
/// Phase 3 adds `transforms` (a palette of 4×4 matrices) and per-rect
/// `transform_id` / `clip_rect`. Phase 4 sorts for correct depth order.
/// Phase 5 adds `images` (textured rects) and `image_sources` (pixel data).
///
/// **Painter order** (post-2026-05-04 op-list refactor): consumer
/// push order is the painter order. Every `push_*` helper appends a
/// `SceneOp` variant to `self.ops`; the rasterizer iterates `ops` in
/// sequence and dispatches per-variant. This replaces the previous
/// per-type `Vec<SceneRect>`, `Vec<SceneImage>`, … design where
/// painter order was fixed by type (rects → strokes → gradients →
/// images → shapes → glyph runs) regardless of push order. The old
/// design surfaced its limit in the `demo_card_grid` Card 6 probe:
/// a "badge" rect pushed after an image still painted under the
/// image. Op-list painter order makes consumer intent the source
/// of truth.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Scene {
    /// Viewport size in device pixels.
    pub viewport_width: u32,
    pub viewport_height: u32,
    /// Draw operations in painter order (back-to-front, push order).
    /// One entry per primitive; the rasterizer dispatches per
    /// variant. See [`SceneOp`].
    pub ops: Vec<SceneOp>,
    /// Phase 10a' font palette. Index `0` is reserved (panic on
    /// push_glyph_run with `font_id = 0`); real fonts start at
    /// index 1.
    pub fonts: Vec<FontBlob>,
    /// Phase 12a' scene-level alpha multiplier (`1.0` = unchanged,
    /// `0.0` = fully transparent). Implemented by wrapping the
    /// entire master scene in a `push_layer(blend, alpha, ...)`.
    /// Useful for whole-canvas fade transitions.
    pub root_alpha: f32,
    /// Phase 12a' scene-level blend mode. Default is
    /// [`SceneBlendMode::Normal`] (plain `source-over`); other
    /// values apply a `mix-blend-mode`-style composite over the
    /// `base_color` / target.
    pub root_blend_mode: SceneBlendMode,
    /// Transform palette. Index 0 is always identity.
    pub transforms: Vec<Transform>,
    /// CPU-side pixel data keyed by `ImageKey`. On first `prepare()`,
    /// each entry is uploaded to the GPU and cached there. Subsequent
    /// frames may omit data for already-cached keys.
    #[cfg_attr(feature = "serde", serde(with = "image_sources_serde"))]
    pub image_sources: HashMap<ImageKey, ImageData>,
    /// Native-compositor surfaces declared by the consumer. Order is
    /// z-order (first declared is bottom-most), matching the same
    /// "vec position = ordering" convention as `ops`. Read by
    /// `Renderer::render_with_compositor`; ignored by other render
    /// entry points.
    ///
    /// See
    /// [`netrender-notes/2026-05-05_compositor_handoff_path_b_prime.md`](../../netrender-notes/2026-05-05_compositor_handoff_path_b_prime.md)
    /// for the design.
    pub compositor_surfaces: Vec<CompositorSurface>,
}

/// One declared native-compositor surface.
///
/// Bounds are world-space. Transform / clip / opacity are applied by
/// the OS compositor at present time, *not* by netrender's master
/// render — they're metadata reaching the consumer's `Compositor`
/// impl via `LayerPresent`.
///
/// Order in `Scene::compositor_surfaces` is z-order: index 0 is
/// bottom-most. Use [`Scene::declare_compositor_surface`] to insert
/// or update; the helper preserves insertion order on repeat
/// declares (updates fields in place).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CompositorSurface {
    pub key: SurfaceKey,
    pub bounds: [f32; 4],
    /// 2D affine, column-major: `[a, b, c, d, tx, ty]`.
    /// Identity is `[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]`.
    pub transform: [f32; 6],
    pub clip: Option<[f32; 4]>,
    pub opacity: f32,
}

impl CompositorSurface {
    /// 2D affine identity for `transform`.
    pub const IDENTITY_TRANSFORM: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

    /// Construct a surface with default transform (identity), no
    /// clip, opacity 1.0.
    pub fn new(key: SurfaceKey, bounds: [f32; 4]) -> Self {
        Self {
            key,
            bounds,
            transform: Self::IDENTITY_TRANSFORM,
            clip: None,
            opacity: 1.0,
        }
    }
}

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
            x0, y0, x1, y1,
            color,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
        }));
    }

    /// Append a rect with an explicit transform id.
    pub fn push_rect_transformed(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        color: [f32; 4],
        transform_id: u32,
    ) {
        self.ops.push(SceneOp::Rect(SceneRect {
            x0, y0, x1, y1,
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
        x0: f32, y0: f32, x1: f32, y1: f32,
        color: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
    ) {
        self.ops.push(SceneOp::Rect(SceneRect {
            x0, y0, x1, y1,
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
        x0: f32, y0: f32, x1: f32, y1: f32,
        color: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
        clip_corner_radii: [f32; 4],
    ) {
        self.ops.push(SceneOp::Rect(SceneRect {
            x0, y0, x1, y1,
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
        x0: f32, y0: f32, x1: f32, y1: f32,
        key: ImageKey,
        data: ImageData,
    ) {
        self.image_sources.entry(key).or_insert(data);
        self.ops.push(SceneOp::Image(SceneImage {
            x0, y0, x1, y1,
            uv: [0.0, 0.0, 1.0, 1.0],
            color: [1.0, 1.0, 1.0, 1.0],
            key,
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
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
        x0: f32, y0: f32, x1: f32, y1: f32,
        start: [f32; 2],
        end: [f32; 2],
        color0: [f32; 4],
        color1: [f32; 4],
    ) {
        self.ops.push(SceneOp::Gradient(two_stop_gradient(
            GradientKind::Linear,
            x0, y0, x1, y1,
            [start[0], start[1], end[0], end[1]],
            color0, color1,
            0, NO_CLIP,
        )));
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
        self.ops.push(SceneOp::Gradient(two_stop_gradient(
            GradientKind::Linear,
            x0, y0, x1, y1,
            [start[0], start[1], end[0], end[1]],
            color0, color1,
            transform_id, clip_rect,
        )));
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
        self.ops.push(SceneOp::Gradient(two_stop_gradient(
            GradientKind::Radial,
            x0, y0, x1, y1,
            [center[0], center[1], radii[0], radii[1]],
            color0, color1,
            0, NO_CLIP,
        )));
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
        self.ops.push(SceneOp::Gradient(two_stop_gradient(
            GradientKind::Conic,
            x0, y0, x1, y1,
            [center[0], center[1], start_angle, 0.0],
            color0, color1,
            0, NO_CLIP,
        )));
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
        self.ops.push(SceneOp::Gradient(two_stop_gradient(
            GradientKind::Conic,
            x0, y0, x1, y1,
            [center[0], center[1], start_angle, 0.0],
            color0, color1,
            transform_id, clip_rect,
        )));
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
        self.ops.push(SceneOp::Gradient(two_stop_gradient(
            GradientKind::Radial,
            x0, y0, x1, y1,
            [center[0], center[1], radii[0], radii[1]],
            color0, color1,
            transform_id, clip_rect,
        )));
    }

    /// Roadmap C2 — append a repeated-tile pattern fill. The image
    /// at `tile` repeats at `image_size * scale` to cover `extent`.
    /// Identity transform, no clip; for richer construction build
    /// the [`ScenePattern`] struct directly and push it.
    pub fn push_pattern(
        &mut self,
        tile: ImageKey,
        extent: [f32; 4],
        scale: f32,
    ) {
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
        x0: f32, y0: f32, x1: f32, y1: f32,
        uv: [f32; 4],
        color: [f32; 4],
        key: ImageKey,
        transform_id: u32,
        clip_rect: [f32; 4],
    ) {
        self.ops.push(SceneOp::Image(SceneImage {
            x0, y0, x1, y1,
            uv,
            color,
            key,
            transform_id,
            clip_rect,
            clip_corner_radii: SHARP_CLIP,
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
    pub fn push_scroll_frame(
        &mut self,
        clip_rect: [f32; 4],
        scroll_offset: [f32; 2],
    ) -> u32 {
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
    pub fn push_shape_stroked(
        &mut self,
        path: ScenePath,
        color: [f32; 4],
        stroke_width: f32,
    ) {
        self.ops.push(SceneOp::Shape(SceneShape {
            path,
            fill_color: None,
            stroke: Some(ScenePathStroke { color, width: stroke_width }),
            transform_id: 0,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
        }));
    }

    /// Phase 11': append a sharp axis-aligned stroked rect (border).
    pub fn push_stroke(
        &mut self,
        x0: f32, y0: f32, x1: f32, y1: f32,
        color: [f32; 4],
        stroke_width: f32,
    ) {
        self.ops.push(SceneOp::Stroke(SceneStroke {
            x0, y0, x1, y1,
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
        x0: f32, y0: f32, x1: f32, y1: f32,
        color: [f32; 4],
        stroke_width: f32,
        stroke_corner_radii: [f32; 4],
    ) {
        self.ops.push(SceneOp::Stroke(SceneStroke {
            x0, y0, x1, y1,
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
        x0: f32, y0: f32, x1: f32, y1: f32,
        color: [f32; 4],
        stroke_width: f32,
        stroke_corner_radii: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
        clip_corner_radii: [f32; 4],
    ) {
        self.ops.push(SceneOp::Stroke(SceneStroke {
            x0, y0, x1, y1,
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
        x0: f32, y0: f32, x1: f32, y1: f32,
        color: [f32; 4],
        stroke_width: f32,
        cap: SceneStrokeCap,
        join: SceneStrokeJoin,
        dash_pattern: Vec<f32>,
    ) {
        self.ops.push(SceneOp::Stroke(SceneStroke {
            x0, y0, x1, y1,
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
        x0: f32, y0: f32, x1: f32, y1: f32,
        uv: [f32; 4],
        color: [f32; 4],
        key: ImageKey,
        transform_id: u32,
        clip_rect: [f32; 4],
        clip_corner_radii: [f32; 4],
    ) {
        self.ops.push(SceneOp::Image(SceneImage {
            x0, y0, x1, y1,
            uv,
            color,
            key,
            transform_id,
            clip_rect,
            clip_corner_radii,
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

/// Reserved id for the no-font sentinel `fonts[0]`. Picked at
/// `u64::MAX` so it never collides with peniko's
/// monotonically-incrementing counter (started at 0). See
/// `Scene::new` for the rationale.
const SENTINEL_FONT_BLOB_ID: u64 = u64::MAX;

fn sentinel_blob() -> vello::peniko::Blob<u8> {
    use std::sync::Arc;
    let arc: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(Vec::<u8>::new());
    vello::peniko::Blob::from_raw_parts(arc, SENTINEL_FONT_BLOB_ID)
}

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
            x0, y0, x1, y1,
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

// ── A2 capture/replay helpers (gated on `serde`) ─────────────────────

/// Custom (de)serialization for `clip_rect: [f32; 4]` fields. The
/// in-memory `NO_CLIP` sentinel uses `±f32::INFINITY`, which JSON
/// represents as `null` (and refuses to deserialize back as `f32`).
/// Map `NO_CLIP` to `None` on the wire and any finite rect to
/// `Some([..])` so JSON round-trip is lossless. Postcard handles
/// infinities natively but uses the same encoding for symmetry.
#[cfg(feature = "serde")]
mod clip_rect_serde {
    use super::NO_CLIP;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(rect: &[f32; 4], ser: S) -> Result<S::Ok, S::Error> {
        if *rect == NO_CLIP {
            None::<[f32; 4]>.serialize(ser)
        } else {
            Some(*rect).serialize(ser)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<[f32; 4], D::Error> {
        let opt: Option<[f32; 4]> = Deserialize::deserialize(de)?;
        Ok(opt.unwrap_or(NO_CLIP))
    }
}

/// Custom (de)serialization for `peniko::Blob<u8>` fields that
/// **preserves the blob id across round-trip**. Peniko's built-in
/// serde impl serializes only the bytes and mints a fresh id on
/// deserialize via the global `ID_COUNTER`; we bypass that and emit
/// `(bytes, id)` so a captured Scene's atlas-dedup identity survives
/// a snapshot/replay cycle.
///
/// Note that `Blob::PartialEq` is id-based (peniko/blob.rs:54-58),
/// so preserving the id is also what makes
/// `assert_eq!(original, replayed)` work in receipt tests.
///
/// Caveat: an unrelated `Blob::new(same_bytes)` call elsewhere in
/// the process still mints a fresh counter id and will not equal a
/// captured Blob with the same bytes — that's by peniko design.
#[cfg(feature = "serde")]
mod blob_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::Arc;
    use vello::peniko::Blob;

    pub fn serialize<S: Serializer>(blob: &Blob<u8>, ser: S) -> Result<S::Ok, S::Error> {
        let bytes: &[u8] = blob.data();
        let id: u64 = blob.id();
        (bytes, id).serialize(ser)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Blob<u8>, D::Error> {
        let (bytes, id): (Vec<u8>, u64) = Deserialize::deserialize(de)?;
        let arc: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(bytes);
        Ok(Blob::from_raw_parts(arc, id))
    }
}

/// Custom (de)serialization for `Scene::image_sources` that emits a
/// **sorted Vec** of entries instead of relying on `HashMap`'s
/// non-deterministic iteration order. Without this, two snapshots of
/// the same Scene could produce different byte sequences, which
/// breaks the `snapshot → replay → snapshot` byte-equality determinism
/// receipt the roadmap calls for.
#[cfg(feature = "serde")]
mod image_sources_serde {
    use super::{ImageData, ImageKey};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;

    pub fn serialize<S: Serializer>(
        map: &HashMap<ImageKey, ImageData>,
        ser: S,
    ) -> Result<S::Ok, S::Error> {
        let mut entries: Vec<(&ImageKey, &ImageData)> = map.iter().collect();
        entries.sort_by_key(|(k, _)| **k);
        entries.serialize(ser)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        de: D,
    ) -> Result<HashMap<ImageKey, ImageData>, D::Error> {
        let entries: Vec<(ImageKey, ImageData)> = Deserialize::deserialize(de)?;
        Ok(entries.into_iter().collect())
    }
}

#[cfg(feature = "serde")]
impl Scene {
    /// Roadmap A2 — postcard binary snapshot. Small (no-overhead
    /// varint encoding) and fast; the right pick for production
    /// fixtures and high-volume replay. Blob ids are preserved (see
    /// `blob_serde`).
    ///
    /// Round-trip determinism: `replay_postcard(scene.snapshot_postcard())`
    /// produces a Scene whose own `snapshot_postcard()` is byte-equal
    /// to the original (modulo the iteration-order normalisation
    /// applied in `image_sources_serde`).
    pub fn snapshot_postcard(&self) -> Vec<u8> {
        postcard::to_allocvec(self)
            .expect("Scene::snapshot_postcard: serialization should not fail on owned data")
    }

    /// Roadmap A2 — postcard binary replay. Returns the deserialised
    /// Scene or a `postcard::Error` if the bytes are malformed /
    /// version-mismatched.
    pub fn replay_postcard(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }

    /// Roadmap A2 — JSON text snapshot. Roughly 3–5× larger than
    /// postcard but human-readable, cross-tool inspectable, and
    /// diff-friendly in git. Use for fixtures that benefit from being
    /// readable in code review or for cross-language consumers.
    pub fn snapshot_json(&self) -> String {
        serde_json::to_string(self)
            .expect("Scene::snapshot_json: serialization should not fail on owned data")
    }

    /// Roadmap A2 — JSON text replay. Returns the deserialised Scene
    /// or a `serde_json::Error` if the JSON is malformed.
    pub fn replay_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

// ── A1 op-list inspector helpers ─────────────────────────────────────

fn dump_op(out: &mut String, op: &SceneOp) {
    use std::fmt::Write;
    match op {
        SceneOp::Rect(r) => {
            write!(out, "Rect      [{:.1}..{:.1}, {:.1}..{:.1}]  color={}",
                r.x0, r.x1, r.y0, r.y1, fmt_color(r.color)).ok();
            dump_modifiers(out, r.transform_id, r.clip_rect, r.clip_corner_radii);
        }
        SceneOp::Stroke(s) => {
            write!(out, "Stroke    [{:.1}..{:.1}, {:.1}..{:.1}]  width={}  color={}",
                s.x0, s.x1, s.y0, s.y1, s.stroke_width, fmt_color(s.color)).ok();
            if s.stroke_corner_radii != SHARP_CLIP {
                write!(out, "  stroke_radii={:?}", s.stroke_corner_radii).ok();
            }
            dump_modifiers(out, s.transform_id, s.clip_rect, s.clip_corner_radii);
        }
        SceneOp::Gradient(g) => {
            write!(out, "Gradient  [{:.1}..{:.1}, {:.1}..{:.1}]  kind={:?}  stops={}",
                g.x0, g.x1, g.y0, g.y1, g.kind, g.stops.len()).ok();
            dump_modifiers(out, g.transform_id, g.clip_rect, g.clip_corner_radii);
        }
        SceneOp::Image(i) => {
            write!(out, "Image     [{:.1}..{:.1}, {:.1}..{:.1}]  key={}",
                i.x0, i.x1, i.y0, i.y1, i.key).ok();
            if i.color != [1.0, 1.0, 1.0, 1.0] {
                write!(out, "  tint={}", fmt_color(i.color)).ok();
            }
            if i.uv != [0.0, 0.0, 1.0, 1.0] {
                write!(out, "  uv={:?}", i.uv).ok();
            }
            dump_modifiers(out, i.transform_id, i.clip_rect, i.clip_corner_radii);
        }
        SceneOp::Pattern(p) => {
            write!(
                out,
                "Pattern   [{:.1}..{:.1}, {:.1}..{:.1}]  tile={}  scale={}",
                p.extent[0], p.extent[2], p.extent[1], p.extent[3], p.tile, p.scale,
            ).ok();
            dump_modifiers(out, p.transform_id, p.clip_rect, p.clip_corner_radii);
        }
        SceneOp::Shape(s) => {
            let aabb = s.path.local_aabb();
            write!(out, "Shape     ops={}", s.path.ops.len()).ok();
            if let Some([x0, y0, x1, y1]) = aabb {
                write!(out, "  aabb=[{:.1}..{:.1}, {:.1}..{:.1}]", x0, x1, y0, y1).ok();
            }
            if let Some(c) = s.fill_color {
                write!(out, "  fill={}", fmt_color(c)).ok();
            }
            if let Some(stk) = s.stroke {
                write!(out, "  stroke={}@{}", fmt_color(stk.color), stk.width).ok();
            }
            dump_modifiers(out, s.transform_id, s.clip_rect, s.clip_corner_radii);
        }
        SceneOp::GlyphRun(g) => {
            write!(out, "GlyphRun  font={}  size={}  glyphs={}  color={}",
                g.font_id, g.font_size, g.glyphs.len(), fmt_color(g.color)).ok();
            dump_modifiers(out, g.transform_id, g.clip_rect, g.clip_corner_radii);
        }
        SceneOp::PushLayer(l) => {
            write!(out, "PushLayer alpha={}  blend={:?}  clip={}",
                l.alpha, l.blend_mode, fmt_clip(&l.clip)).ok();
            if l.transform_id != 0 {
                write!(out, "  transform={}", l.transform_id).ok();
            }
        }
        SceneOp::PopLayer => {
            write!(out, "PopLayer").ok();
        }
    }
}

fn dump_modifiers(out: &mut String, transform_id: u32, clip_rect: [f32; 4], radii: [f32; 4]) {
    use std::fmt::Write;
    if transform_id != 0 {
        write!(out, "  transform={}", transform_id).ok();
    }
    if clip_rect != NO_CLIP {
        write!(out, "  clip=[{:.1}..{:.1}, {:.1}..{:.1}]",
            clip_rect[0], clip_rect[2], clip_rect[1], clip_rect[3]).ok();
    }
    if radii != SHARP_CLIP {
        write!(out, "  radii={:?}", radii).ok();
    }
}

fn fmt_color(c: [f32; 4]) -> String {
    format!("[{:.2},{:.2},{:.2},{:.2}]", c[0], c[1], c[2], c[3])
}

fn fmt_clip(c: &SceneClip) -> String {
    match c {
        SceneClip::None => "None".to_string(),
        SceneClip::Rect { rect, radii } => {
            if *radii == SHARP_CLIP {
                format!("Rect[{:.1}..{:.1}, {:.1}..{:.1}]",
                    rect[0], rect[2], rect[1], rect[3])
            } else {
                format!("Rect[{:.1}..{:.1}, {:.1}..{:.1}]+radii{:?}",
                    rect[0], rect[2], rect[1], rect[3], radii)
            }
        }
        SceneClip::Path(p) => format!("Path(ops={})", p.ops.len()),
    }
}
