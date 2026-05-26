/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Engine-facing primitive vocabulary for the PaintList layer.
//!
//! This is the subset of serval's `servo-paint-types` that the PaintList
//! command stream actually references, lifted into the neutral
//! netrender-workspace crate so `paint_list_api` carries no serval
//! dependency. These are deliberately **display-list / CSS-shaped**
//! types (border styles, line styles, blend modes, euclid-based
//! geometry) — distinct from `netrender`'s renderer primitives (bare
//! `f32` / `[f32; N]`, no euclid). The `paint_list_render` translator
//! bridges the two; that bridge is the reason this vocabulary stays
//! separate rather than repointing onto netrender's types.
//!
//! `MallocSizeOf` derives are intentionally dropped (servo's
//! `malloc_size_of` trait crate is not a netrender-workspace dep); the
//! types are `Clone + serde` and nothing more.

use serde::{Deserialize, Serialize};

// =============================================================================
// Geometry — euclid aliases with unit markers
// =============================================================================

/// Layout-space pixel unit marker for euclid geometry.
pub enum LayoutPixel {}
/// Device-space pixel unit marker for euclid geometry.
pub enum DevicePixel {}

pub type LayoutPoint = euclid::Point2D<f32, LayoutPixel>;
pub type LayoutRect = euclid::Box2D<f32, LayoutPixel>;
pub type LayoutSize = euclid::Size2D<f32, LayoutPixel>;
pub type LayoutSideOffsets = euclid::SideOffsets2D<f32, LayoutPixel>;
pub type LayoutTransform = euclid::Transform3D<f32, LayoutPixel, LayoutPixel>;
pub type LayoutVector2D = euclid::Vector2D<f32, LayoutPixel>;

pub type DeviceIntSize = euclid::Size2D<i32, DevicePixel>;
pub type DeviceIntSideOffsets = euclid::SideOffsets2D<i32, DevicePixel>;

/// Corner accessors for `euclid::Box2D`. Box2D stores `min` (top-left)
/// and `max` (bottom-right) directly; the cross corners are constructed
/// from those. Mirrors the ergonomics the translator expects from
/// `LayoutRect` without pulling webrender_api back in.
pub trait BoxCorners<T, U> {
    fn top_left(&self) -> euclid::Point2D<T, U>;
    fn top_right(&self) -> euclid::Point2D<T, U>;
    fn bottom_left(&self) -> euclid::Point2D<T, U>;
    fn bottom_right(&self) -> euclid::Point2D<T, U>;
}

impl<T, U> BoxCorners<T, U> for euclid::Box2D<T, U>
where
    T: Copy,
{
    fn top_left(&self) -> euclid::Point2D<T, U> {
        self.min
    }
    fn top_right(&self) -> euclid::Point2D<T, U> {
        euclid::Point2D::new(self.max.x, self.min.y)
    }
    fn bottom_left(&self) -> euclid::Point2D<T, U> {
        euclid::Point2D::new(self.min.x, self.max.y)
    }
    fn bottom_right(&self) -> euclid::Point2D<T, U> {
        self.max
    }
}

// =============================================================================
// Color
// =============================================================================

/// Unpremultiplied RGBA color, components in `0.0..=1.0`. Premultiplies
/// to netrender's `[f32; 4]` at lowering.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ColorF {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl ColorF {
    pub const BLACK: Self = Self::new(0.0, 0.0, 0.0, 1.0);
    pub const TRANSPARENT: Self = Self::new(0.0, 0.0, 0.0, 0.0);
    pub const WHITE: Self = Self::new(1.0, 1.0, 1.0, 1.0);

    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
}

// =============================================================================
// Border / line / shadow vocabulary
// =============================================================================

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum BorderStyle {
    #[default]
    None,
    Solid,
    Double,
    Dotted,
    Dashed,
    Hidden,
    Groove,
    Ridge,
    Inset,
    Outset,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum LineStyle {
    Solid,
    Dotted,
    Dashed,
    Wavy,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum BoxShadowClipMode {
    Outset,
    Inset,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct BorderRadius {
    pub top_left: LayoutSize,
    pub top_right: LayoutSize,
    pub bottom_left: LayoutSize,
    pub bottom_right: LayoutSize,
}

impl BorderRadius {
    pub fn zero() -> Self {
        Self::default()
    }

    pub fn is_zero(&self) -> bool {
        let zero = LayoutSize::zero();
        self.top_left == zero
            && self.top_right == zero
            && self.bottom_left == zero
            && self.bottom_right == zero
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct BorderSide {
    pub color: ColorF,
    pub style: BorderStyle,
}

impl Default for BorderSide {
    fn default() -> Self {
        Self {
            color: ColorF::TRANSPARENT,
            style: BorderStyle::None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct NormalBorder {
    pub left: BorderSide,
    pub right: BorderSide,
    pub top: BorderSide,
    pub bottom: BorderSide,
    pub radius: BorderRadius,
    pub do_aa: bool,
}

impl Default for NormalBorder {
    fn default() -> Self {
        Self {
            left: BorderSide::default(),
            right: BorderSide::default(),
            top: BorderSide::default(),
            bottom: BorderSide::default(),
            radius: BorderRadius::default(),
            do_aa: true,
        }
    }
}

// =============================================================================
// Gradients
// =============================================================================

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum ExtendMode {
    Clamp,
    Repeat,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum RepeatMode {
    Stretch,
    Repeat,
    Round,
    Space,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct GradientStop {
    pub offset: f32,
    pub color: ColorF,
}

// =============================================================================
// Images
// =============================================================================

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum ImageRendering {
    Auto,
    CrispEdges,
    Pixelated,
}

// =============================================================================
// Blend / transform
// =============================================================================

/// Full CSS blend-mode set. netrender's `SceneBlendMode` exposes only the
/// first six; the translator maps the rest to `Normal` (a pre-existing
/// truncation, not introduced by the extraction).
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum MixBlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
    PlusLighter,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum TransformStyle {
    Flat,
    Preserve3D,
}

// =============================================================================
// Resource identity
// =============================================================================

/// Resource-namespace coordination (carried inside [`ImageKey`] /
/// [`FontInstanceKey`]). Kept so producers can mint keys the same way
/// they did under servo-paint-types; the translator strips the namespace
/// when mapping onto netrender's flat `u64`/`u32` ids.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
pub struct IdNamespace(pub u32);

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ImageKey(pub IdNamespace, pub u32);

impl ImageKey {
    pub fn new(namespace: IdNamespace, key: u32) -> Self {
        Self(namespace, key)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct FontInstanceKey(pub IdNamespace, pub u32);

impl FontInstanceKey {
    pub fn new(namespace: IdNamespace, key: u32) -> Self {
        Self(namespace, key)
    }
}
