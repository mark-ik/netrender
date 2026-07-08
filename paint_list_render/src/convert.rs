/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Scalar / colour / transform / path converters from `paint_list_api`
//! vocabulary to netrender primitives.


use netrender::{
    GradientStop as NrGradientStop,
    SceneBlendMode, ScenePath, SceneStrokeCap, SceneStrokeJoin, Transform,
};
use paint_list_api::{
    self as ple, ColorF,
};

pub(crate) fn rect_corners(rect: &paint_list_api::LayoutRect) -> (f32, f32, f32, f32) {
    (rect.min.x, rect.min.y, rect.max.x, rect.max.y)
}

pub(crate) fn color_to_array(color: &ColorF) -> [f32; 4] {
    [color.r, color.g, color.b, color.a]
}

pub(crate) fn layout_transform_to_scene(t: &paint_list_api::LayoutTransform) -> Transform {
    // PaintCmd carries `Transform3D` (4x4 column-major in euclid's
    // m11..m44 naming); netrender's `Transform.m` is also 4x4
    // column-major. Project field-by-field.
    Transform {
        m: [
            t.m11, t.m12, t.m13, t.m14, t.m21, t.m22, t.m23, t.m24, t.m31, t.m32, t.m33, t.m34,
            t.m41, t.m42, t.m43, t.m44,
        ],
    }
}

pub(crate) fn mix_blend_mode_to_scene(mode: paint_list_api::MixBlendMode) -> SceneBlendMode {
    use paint_list_api::MixBlendMode as M;
    match mode {
        M::Normal => SceneBlendMode::Normal,
        M::Multiply => SceneBlendMode::Multiply,
        M::Screen => SceneBlendMode::Screen,
        M::Overlay => SceneBlendMode::Overlay,
        M::Darken => SceneBlendMode::Darken,
        M::Lighten => SceneBlendMode::Lighten,
        // netrender's enum is the small CSS-canonical set; the
        // higher-fidelity modes (ColorDodge/ColorBurn/HardLight/etc.)
        // fall back to Normal until netrender grows full coverage.
        _ => SceneBlendMode::Normal,
    }
}

pub(crate) fn gradient_stops(stops: &[paint_list_api::GradientStop]) -> Vec<NrGradientStop> {
    stops
        .iter()
        .map(|s| NrGradientStop {
            offset: s.offset,
            color: [s.color.r, s.color.g, s.color.b, s.color.a],
        })
        .collect()
}

/// Map the vocabulary line cap to netrender's. 1:1.
pub(crate) fn stroke_cap_to_scene(c: ple::StrokeCap) -> SceneStrokeCap {
    match c {
        ple::StrokeCap::Butt => SceneStrokeCap::Butt,
        ple::StrokeCap::Round => SceneStrokeCap::Round,
        ple::StrokeCap::Square => SceneStrokeCap::Square,
    }
}

/// Map the vocabulary line join to netrender's. 1:1.
pub(crate) fn stroke_join_to_scene(j: ple::StrokeJoin) -> SceneStrokeJoin {
    match j {
        ple::StrokeJoin::Bevel => SceneStrokeJoin::Bevel,
        ple::StrokeJoin::Miter => SceneStrokeJoin::Miter,
        ple::StrokeJoin::Round => SceneStrokeJoin::Round,
    }
}

/// Split an optional `DashPattern` into netrender's `(intervals, offset)` shape;
/// `None` → solid (empty intervals).
pub(crate) fn dash_to_scene(dash: &Option<ple::DashPattern>) -> (Vec<f32>, f32) {
    match dash {
        Some(d) => (d.intervals.clone(), d.offset),
        None => (Vec::new(), 0.0),
    }
}

/// Rebuild a `netrender::ScenePath` from the serializable `PathData` command
/// sequence (the shape `DrawStroke` / `DrawPath` carry). 1:1 verb mapping.
pub(crate) fn path_data_to_scene_path(pd: &ple::PathData) -> ScenePath {
    use ple::PathCommand as C;
    let mut p = ScenePath::with_capacity(pd.commands.len());
    for cmd in &pd.commands {
        match *cmd {
            C::MoveTo(pt) => {
                p.move_to(pt.x, pt.y);
            },
            C::LineTo(pt) => {
                p.line_to(pt.x, pt.y);
            },
            C::QuadTo { control, to } => {
                p.quad_to(control.x, control.y, to.x, to.y);
            },
            C::CurveTo { control1, control2, to } => {
                p.cubic_to(control1.x, control1.y, control2.x, control2.y, to.x, to.y);
            },
            C::Close => {
                p.close();
            },
        }
    }
    p
}
