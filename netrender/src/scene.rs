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

    /// True iff this transform is pure 2D translation (or identity)
    /// — i.e. the upper-left 3×3 rotation/scale block is the identity
    /// and only the translation column carries non-trivial data.
    ///
    /// Phase 10b.3 transform-aware subpixel-AA policy uses this:
    /// LCD subpixel coverage is only meaningful when the glyph's
    /// horizontal axis stays aligned with the framebuffer's
    /// horizontal axis. Translation preserves that alignment;
    /// rotation, skew, and non-uniform scale don't. Translated-only
    /// runs route to the dual-source pipeline; everything else falls
    /// back to grayscale.
    ///
    /// Comparison is exact (no epsilon). Consumers that build
    /// near-identity rotations from floating-point math should
    /// snap to identity at construction time if they want them
    /// classified as translation-only.
    pub fn is_pure_translation_2d(&self) -> bool {
        // Column-major: m[col*4 + row]. Pure 2D translation means
        // columns 0/1/2 match identity and column 3's homogeneous w
        // is 1 (translation values m[12]/m[13]/m[14] may be anything).
        let m = &self.m;
        m[0] == 1.0 && m[1] == 0.0 && m[2] == 0.0 && m[3] == 0.0
            && m[4] == 0.0 && m[5] == 1.0 && m[6] == 0.0 && m[7] == 0.0
            && m[8] == 0.0 && m[9] == 0.0 && m[10] == 1.0 && m[11] == 0.0
            && m[15] == 1.0
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
}

pub const NO_CLIP: [f32; 4] =
    [f32::NEG_INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::INFINITY];

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
}

/// Phase 10a.1 glyph identity. Keys uploads into the renderer-owned
/// glyph atlas. Caller-assigned (consumer typically uses
/// `(font_id, glyph_id, size_x64)` from its shaper / font enumeration).
///
/// `size_x64` is the requested pixel size in 1/64ths of a pixel (the
/// 26.6 fixed-point convention shared by FreeType and swash). 10a.1
/// stores it but does no rasterization itself; the consumer hands in
/// pre-rasterized [`GlyphRaster`] bitmaps via `Scene::set_glyph_raster`.
/// 10a.4 will extend this with a `subpixel_x: u8` dimension when the
/// dual-source pipeline lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub font_id: u32,
    pub glyph_id: u32,
    pub size_x64: u32,
}

/// Coverage layout of a [`GlyphRaster`].
///
/// Phase 10b carry-forward: `Subpixel` is the LCD per-channel format
/// (3 bytes per pixel, R/G/B coverage triple) produced by
/// `swash::scale::Render::format(zeno::Format::Subpixel)`. The atlas
/// stores both formats in one `Rgba8Unorm` texture; the upload path
/// expands `Alpha` to `(c, c, c, 255)` and `Subpixel` to
/// `(r, g, b, 255)`. The dual-source `ps_text_run_dual_source` shader
/// then sees per-channel coverage on `Subpixel` glyphs and a
/// broadcast triple on `Alpha` glyphs (bit-equivalent to the
/// grayscale `ps_text_run` path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphFormat {
    /// One byte per pixel, single-channel coverage. Produced by
    /// `swash::scale::Render::format(zeno::Format::Alpha)` (the
    /// 10a.1–10a.5 default) and by hand-authored fixtures.
    Alpha,
    /// Three bytes per pixel, `[r, g, b]` per-channel coverage.
    /// Produced by `swash::scale::Render::format(zeno::Format::Subpixel)`.
    Subpixel,
}

impl GlyphFormat {
    /// Bytes per pixel in [`GlyphRaster::pixels`] for this format.
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            GlyphFormat::Alpha => 1,
            GlyphFormat::Subpixel => 3,
        }
    }
}

/// CPU-side rasterized glyph bitmap. Row-major, tightly packed; bytes
/// per pixel depends on [`GlyphRaster::format`]. Premultiplication
/// happens on the shader side (atlas sample × premultiplied tint).
///
/// `bearing_x` / `bearing_y` are the standard FreeType / swash glyph
/// metrics — the offset from the pen position to the bitmap's top-left
/// corner. `bearing_y` is positive *up* from the baseline (so a glyph
/// that sits on the baseline has `bearing_y = 0`; a 'g' that descends
/// has `bearing_y < 0` for its descender; an 'A' typically has
/// `bearing_y = ascent_height`).
#[derive(Debug, Clone)]
pub struct GlyphRaster {
    pub width: u32,
    pub height: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
    /// Coverage layout: see [`GlyphFormat`]. Determines how the atlas
    /// upload expands `pixels` into the `Rgba8Unorm` storage.
    pub format: GlyphFormat,
    /// Coverage bytes; `len()` must equal
    /// `width * height * format.bytes_per_pixel()`.
    pub pixels: Vec<u8>,
}

/// One placed glyph in a [`SceneText`] run. The pen position is
/// device-pixel (the baseline origin); the renderer combines it with
/// the atlas slot's bearings to compute the per-glyph quad.
#[derive(Debug, Clone, Copy)]
pub struct GlyphInstance {
    pub key: GlyphKey,
    /// Pen position in **local space** — combined with `bearing_x` /
    /// `bearing_y` from the atlas slot to compute the quad's bounds.
    /// Local space is in device pixels when `transform_id == 0`.
    pub x: f32,
    pub y: f32,
}

/// One text run: a list of placed glyphs sharing a tint color, a
/// transform, and a clip. Each glyph in the run becomes one
/// per-instance quad in the `ps_text_run` draw call; quads inside a
/// run share the run's z-depth.
///
/// A text run is rendered through the alpha-blended `ps_text_run`
/// pipeline (text is always alpha-blended — coverage is in `[0, 1]`).
#[derive(Debug, Clone)]
pub struct SceneText {
    pub glyphs: Vec<GlyphInstance>,
    /// Premultiplied RGBA tint applied to every glyph in the run.
    /// `[1, 1, 1, 1]` = white opaque text.
    pub color: [f32; 4],
    /// Index into `Scene::transforms`; `0` = identity.
    pub transform_id: u32,
    /// Device-space axis-aligned clip; `NO_CLIP` disables clipping.
    pub clip_rect: [f32; 4],
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
    /// Phase 10a.1 text runs in painter order. Each run paints in
    /// front of all rects / images / gradients (text overlays UI).
    pub texts: Vec<SceneText>,
    /// Transform palette. Index 0 is always identity.
    pub transforms: Vec<Transform>,
    /// CPU-side pixel data keyed by `ImageKey`. On first `prepare()`,
    /// each entry is uploaded to the GPU and cached there. Subsequent
    /// frames may omit data for already-cached keys.
    pub image_sources: HashMap<ImageKey, ImageData>,
    /// Phase 10a.1 CPU-side glyph bitmaps keyed by `GlyphKey`. On
    /// first `prepare()`, each entry is uploaded to the renderer's
    /// glyph atlas and cached there. Subsequent frames may omit
    /// rasters for already-cached keys.
    pub glyph_rasters: HashMap<GlyphKey, GlyphRaster>,
}

impl Scene {
    pub fn new(viewport_width: u32, viewport_height: u32) -> Self {
        Self {
            viewport_width,
            viewport_height,
            rects: Vec::new(),
            images: Vec::new(),
            gradients: Vec::new(),
            texts: Vec::new(),
            transforms: vec![Transform::IDENTITY], // index 0 = identity
            image_sources: HashMap::new(),
            glyph_rasters: HashMap::new(),
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
        });
    }

    /// Append a rect with an explicit transform and a device-space clip.
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
        });
    }

    /// Phase 10a.1: register a CPU-side glyph bitmap for `key`.
    /// Subsequent `prepare()` calls upload it to the renderer's atlas
    /// on first observation; once cached, the renderer ignores
    /// further entries for the same key.
    pub fn set_glyph_raster(&mut self, key: GlyphKey, raster: GlyphRaster) {
        self.glyph_rasters.entry(key).or_insert(raster);
    }

    /// Phase 10a.1: append one text run. The consumer hands in
    /// pre-shaped glyph instances (positions + keys); netrender
    /// rasterizes nothing in 10a.1 — the consumer must also call
    /// `set_glyph_raster` for any key that hasn't already been
    /// uploaded. 10a.3 will accept a typed font handle here.
    pub fn push_text_run(
        &mut self,
        glyphs: Vec<GlyphInstance>,
        color: [f32; 4],
    ) {
        self.texts.push(SceneText {
            glyphs,
            color,
            transform_id: 0,
            clip_rect: NO_CLIP,
        });
    }

    /// Phase 10a.1 with full control over transform and clip.
    pub fn push_text_run_full(
        &mut self,
        glyphs: Vec<GlyphInstance>,
        color: [f32; 4],
        transform_id: u32,
        clip_rect: [f32; 4],
    ) {
        self.texts.push(SceneText {
            glyphs,
            color,
            transform_id,
            clip_rect,
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
    }
}
