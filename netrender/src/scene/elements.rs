/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Text, path, layer, and op-list element types (and compositor surfaces).

use super::*;

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
    #[cfg_attr(feature = "serde", serde(with = "super::blob_serde"))]
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
    #[cfg_attr(feature = "serde", serde(with = "super::clip_rect_serde"))]
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
        Self {
            ops: Vec::with_capacity(n),
        }
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
        c1x: f32,
        c1y: f32,
        c2x: f32,
        c2y: f32,
        x: f32,
        y: f32,
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
                if x < min_x {
                    min_x = x;
                }
                if y < min_y {
                    min_y = y;
                }
                if x > max_x {
                    max_x = x;
                }
                if y > max_y {
                    max_y = y;
                }
            };
            match *op {
                PathOp::MoveTo(x, y) | PathOp::LineTo(x, y) => update(x, y),
                PathOp::QuadTo(cx, cy, x, y) => {
                    update(cx, cy);
                    update(x, y);
                }
                PathOp::CubicTo(c1x, c1y, c2x, c2y, x, y) => {
                    update(c1x, c1y);
                    update(c2x, c2y);
                    update(x, y);
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

/// Phase 11b' stroke style. `width` in device pixels. Carries cap / join /
/// dash so shape strokes (`DrawPath` / `DrawStroke`) decorate the same way rect
/// strokes (`SceneStroke`) do.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ScenePathStroke {
    pub color: [f32; 4],
    pub width: f32,
    /// Line cap for open ends. Default `Butt`.
    pub cap: SceneStrokeCap,
    /// Line join at corners. Default `Miter`.
    pub join: SceneStrokeJoin,
    /// Alternating dash / gap lengths in device px; empty = solid.
    pub dash_pattern: Vec<f32>,
    /// Dash phase offset in device px.
    pub dash_offset: f32,
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
    #[cfg_attr(feature = "serde", serde(with = "super::clip_rect_serde"))]
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

/// A single filter function, as used both for `backdrop-filter` (on the
/// *parent* content beneath a layer, D1) and CSS `filter` (on a layer's *own*
/// output — see [`SceneLayer::filters`]). `Blur` is a spatial pass; the rest are
/// per-pixel color transforms (CSS Filter Effects §2 reference functions). The
/// `f32` is the CSS amount: `Blur` a radius in device px, `HueRotate` an angle
/// in degrees, the others a unitless amount (`0.0` = identity for most, `1.0` =
/// identity for `Brightness`/`Contrast`/`Saturate`).
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SceneFilter {
    /// Gaussian blur with the given radius in device pixels.
    Blur(f32),
    Brightness(f32),
    Contrast(f32),
    Grayscale(f32),
    /// Hue rotation by an angle in degrees.
    HueRotate(f32),
    Invert(f32),
    Saturate(f32),
    Sepia(f32),
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
    /// CSS `filter` — the chain applied to this layer's *own* rendered output
    /// (post-rasterization), in order, before it composites into the parent.
    /// Empty = no filter (the common case). Distinct from `backdrop_filter`,
    /// which filters what is *behind* the layer. The renderer renders the
    /// layer's ops to an offscreen, applies the chain, then composites the
    /// result (carrying `alpha` / `blend_mode` / `clip`).
    pub filters: Vec<SceneFilter>,
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
            filters: Vec::new(),
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
            filters: Vec::new(),
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
            filters: Vec::new(),
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
