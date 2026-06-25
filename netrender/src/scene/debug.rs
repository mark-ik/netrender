/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A1 op-list inspector: pretty-printers for `dump_ops`.

use super::*;

pub(super) fn dump_op(out: &mut String, op: &SceneOp) {
    use std::fmt::Write;
    match op {
        SceneOp::Rect(r) => {
            write!(
                out,
                "Rect      [{:.1}..{:.1}, {:.1}..{:.1}]  color={}",
                r.x0,
                r.x1,
                r.y0,
                r.y1,
                fmt_color(r.color)
            )
            .ok();
            dump_modifiers(out, r.transform_id, r.clip_rect, r.clip_corner_radii);
        }
        SceneOp::Stroke(s) => {
            write!(
                out,
                "Stroke    [{:.1}..{:.1}, {:.1}..{:.1}]  width={}  color={}",
                s.x0,
                s.x1,
                s.y0,
                s.y1,
                s.stroke_width,
                fmt_color(s.color)
            )
            .ok();
            if s.stroke_corner_radii != SHARP_CLIP {
                write!(out, "  stroke_radii={:?}", s.stroke_corner_radii).ok();
            }
            dump_modifiers(out, s.transform_id, s.clip_rect, s.clip_corner_radii);
        }
        SceneOp::Gradient(g) => {
            write!(
                out,
                "Gradient  [{:.1}..{:.1}, {:.1}..{:.1}]  kind={:?}  stops={}",
                g.x0,
                g.x1,
                g.y0,
                g.y1,
                g.kind,
                g.stops.len()
            )
            .ok();
            dump_modifiers(out, g.transform_id, g.clip_rect, g.clip_corner_radii);
        }
        SceneOp::Image(i) => {
            write!(
                out,
                "Image     [{:.1}..{:.1}, {:.1}..{:.1}]  key={}",
                i.x0, i.x1, i.y0, i.y1, i.key
            )
            .ok();
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
                "Pattern   [{:.1}..{:.1}, {:.1}..{:.1}]  tile={}  scale={:?}",
                p.extent[0], p.extent[2], p.extent[1], p.extent[3], p.tile, p.scale,
            )
            .ok();
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
            write!(
                out,
                "GlyphRun  font={}  size={}  glyphs={}  color={}",
                g.font_id,
                g.font_size,
                g.glyphs.len(),
                fmt_color(g.color)
            )
            .ok();
            dump_modifiers(out, g.transform_id, g.clip_rect, g.clip_corner_radii);
        }
        SceneOp::PushLayer(l) => {
            write!(
                out,
                "PushLayer alpha={}  blend={:?}  clip={}",
                l.alpha,
                l.blend_mode,
                fmt_clip(&l.clip)
            )
            .ok();
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
        write!(
            out,
            "  clip=[{:.1}..{:.1}, {:.1}..{:.1}]",
            clip_rect[0], clip_rect[2], clip_rect[1], clip_rect[3]
        )
        .ok();
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
                format!(
                    "Rect[{:.1}..{:.1}, {:.1}..{:.1}]",
                    rect[0], rect[2], rect[1], rect[3]
                )
            } else {
                format!(
                    "Rect[{:.1}..{:.1}, {:.1}..{:.1}]+radii{:?}",
                    rect[0], rect[2], rect[1], rect[3], radii
                )
            }
        }
        SceneClip::Path(p) => format!("Path(ops={})", p.ops.len()),
    }
}
