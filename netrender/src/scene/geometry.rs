/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Geometry & paint primitive types: transforms, rects, images, gradients,
//! patterns, and strokes.

use super::*;

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
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ],
    };

    /// 2D translation — moves by `(tx, ty)` in the XY plane.
    pub fn translate_2d(tx: f32, ty: f32) -> Self {
        Self {
            m: [
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, tx, ty, 0.0, 1.0,
            ],
        }
    }

    /// 2D counter-clockwise rotation by `angle_radians` around the origin.
    pub fn rotate_2d(angle_radians: f32) -> Self {
        let (s, c) = angle_radians.sin_cos();
        Self {
            m: [
                c, s, 0.0, 0.0, -s, c, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ],
        }
    }

    /// 2D uniform scale by `(sx, sy)` around the origin.
    pub fn scale_2d(sx: f32, sy: f32) -> Self {
        Self {
            m: [
                sx, 0.0, 0.0, 0.0, 0.0, sy, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
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
    #[cfg_attr(feature = "serde", serde(with = "super::clip_rect_serde"))]
    pub clip_rect: [f32; 4],
    /// Per-corner radii in device pixels: `[top_left, top_right,
    /// bottom_right, bottom_left]`. All zeros = sharp axis-aligned
    /// clip (default). Non-zero radii produce a rounded-rect clip;
    /// the clip is generated via vello `push_layer` with a
    /// `kurbo::RoundedRect` shape (Phase 9').
    pub clip_corner_radii: [f32; 4],
}

pub const NO_CLIP: [f32; 4] = [
    f32::NEG_INFINITY,
    f32::NEG_INFINITY,
    f32::INFINITY,
    f32::INFINITY,
];

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
    #[cfg_attr(feature = "serde", serde(with = "super::blob_serde"))]
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
        Self {
            width,
            height,
            data,
        }
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
    /// `true` for a `repeating-*-gradient`: the colorstop range tiles via
    /// `Extend::Repeat` instead of clamping (`Extend::Pad`). The producer sizes
    /// the gradient line / radius / sweep to one repeat period so the tiling
    /// reproduces the CSS pattern.
    pub repeat: bool,
    /// Kind-dependent parameter slot (see struct docs).
    pub params: [f32; 4],
    /// Color stops along the gradient parameter, sorted by `offset`
    /// ascending. Phase 8D supports arbitrary lengths; 2 is the
    /// minimum for a meaningful gradient.
    pub stops: Vec<GradientStop>,
    /// Index into `Scene::transforms`; `0` = identity.
    pub transform_id: u32,
    /// Device-space axis-aligned clip; `NO_CLIP` disables clipping.
    #[cfg_attr(feature = "serde", serde(with = "super::clip_rect_serde"))]
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
    /// Per-axis tile-size multiplier `[sx, sy]` (`[1.0, 1.0]` = native pixel
    /// size). A tile spans `image_w * sx` by `image_h * sy` in scene-local
    /// space — this carries CSS `background-size` (incl. non-uniform like
    /// `100% auto` or `16px 40px`), so the repeated tile is the resolved size,
    /// not the source resolution.
    pub scale: [f32; 2],
    /// Index into `Scene::transforms`; `0` = identity.
    pub transform_id: u32,
    /// Device-space axis-aligned clip; `NO_CLIP` disables clipping.
    #[cfg_attr(feature = "serde", serde(with = "super::clip_rect_serde"))]
    pub clip_rect: [f32; 4],
    /// Per-corner clip radii (see `SceneRect::clip_corner_radii`).
    pub clip_corner_radii: [f32; 4],
    /// Sample with nearest-neighbor instead of the default bilinear
    /// filtering (CSS `image-rendering: pixelated` / `crisp-edges`).
    #[cfg_attr(feature = "serde", serde(default))]
    pub nearest: bool,
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
    #[cfg_attr(feature = "serde", serde(with = "super::clip_rect_serde"))]
    pub clip_rect: [f32; 4],
    /// Per-corner clip radii (see `SceneRect::clip_corner_radii`).
    pub clip_corner_radii: [f32; 4],
    /// When set, the sampler is *clamped to the `uv` sub-rect*: the source is
    /// cropped to that region before drawing, so bilinear filtering at the
    /// sub-rect edges does not bleed into adjacent source pixels. Used by
    /// nine-patch (`border-image`) slicing and any sub-rect / sprite-sheet blit
    /// where neighbouring atlas cells must not leak across the seam. `false`
    /// (default) samples the whole image with the brush's extend mode.
    #[cfg_attr(feature = "serde", serde(default))]
    pub clamp_to_uv: bool,
    /// Sample with nearest-neighbor instead of the default bilinear
    /// filtering (CSS `image-rendering: pixelated` / `crisp-edges`).
    #[cfg_attr(feature = "serde", serde(default))]
    pub nearest: bool,
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
    #[cfg_attr(feature = "serde", serde(with = "super::clip_rect_serde"))]
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

