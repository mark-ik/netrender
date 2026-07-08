/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! `paint_list_render` — `PaintList` → `netrender::Scene` translator.
//!
//! This is the impedance bridge between the engine-facing display-list
//! vocabulary ([`paint_list_api`]) and netrender's renderer primitives.
//! Producers emit a [`paint_list_api::PaintList`] (the closed-set
//! `PaintCmd` vocabulary — compositor primitives plus `Draw*` items);
//! this crate walks the command stream and produces a
//! [`netrender::Scene`] the renderer can rasterize.
//!
//! It depends only on `netrender` + `paint_list_api`, so any engine on
//! either side of the path-dep boundary (serval, nematic, inker, …) can
//! reuse it. The serval-specific glue that *consumes* the translator
//! output — building blurred box-shadow masks on the GPU, compositing
//! external textures, the painter message loop — stays in serval's
//! `components/paint` crate; see
//! `serval/docs/2026-05-20_paintlist_extraction_plan.md`.
//!
//! ## Mapping summary
//!
//! Most variants map 1:1 to a `netrender::SceneOp`. The painter-side
//! gaps flagged for follow-up (`DrawPath`, `DrawStroke`, nine-patch
//! borders, inset `DrawShadow`) emit a fallback or `warn!`-and-skip.
//!
//! `DrawShadow` is fully wired for outset shadows: hard shadows
//! (blur 0) lower to a solid rect; blurred ones record a
//! [`BoxShadowMaskRequest`] (built GPU-side by the painter via
//! `Renderer::build_box_shadow_mask`) and push the matching mask
//! image op into the scene at the right paint position.

use std::collections::HashMap;

use log::warn;
use netrender::{
    ExternalTexturePlacement, Glyph as NrGlyph, ImageKey as NrImageKey, NO_CLIP, SHARP_CLIP, Scene, SceneImage, SceneOp, ScenePathStroke, ScenePattern,
    SceneShape, Transform,
};
use paint_list_api::{
    self as ple, FontResource, ImageKey, ImageResource,
    PaintCmd, PaintList,
};

mod composite;
mod convert;
mod emit;

#[cfg(test)]
mod tests;

use composite::{register_fonts, register_images};
use convert::*;
use emit::*;
pub use composite::{composite_paint_layers, CompositeLayer};
/// External-texture composite metadata produced by the translator and
/// consumed by the painter's frame-render path. The translator can't
/// reach the embedder's texture registry or the renderer, so it records
/// placement + key here and the painter materializes the composite.
#[derive(Clone, Debug)]
pub struct ExternalTextureDraw {
    pub texture_key: u64,
    pub placement: ExternalTexturePlacement,
    /// Number of ordinary NetRender ops emitted before this external
    /// texture draw. The renderer uses this to restore painter order
    /// without forcing the texture through Vello's atlas path.
    pub scene_op_boundary: usize,
}

/// A blurred box-shadow mask the painter must build on the GPU
/// (`Renderer::build_box_shadow_mask`) before rasterizing the scene.
/// The translator can't reach the `Renderer`, so it records the
/// parameters here and pushes the matching image op into the scene
/// (at the right paint position); the painter materializes the mask
/// texture under `key` ahead of the render.
#[derive(Clone, Debug)]
pub struct BoxShadowMaskRequest {
    /// netrender image key the scene's shadow image op references.
    pub key: NrImageKey,
    /// Square mask texture side length (covers the scene; the shadow
    /// box is drawn at `bounds` within it, in absolute scene coords).
    pub dim: u32,
    /// Shadow box in absolute scene coords `[x0, y0, x1, y1]`. For an outset
    /// shadow this is the offset/spread box; for an inset shadow it is the
    /// unshadowed inner "hole".
    pub bounds: [f32; 4],
    pub corner_radius: f32,
    pub blur_radius_px: f32,
    /// Produce a `1 - coverage` mask: the inset-box-shadow primitive (shadow
    /// everywhere except `bounds`). `false` for the normal outset mask.
    pub invert: bool,
}

/// The full translator output: a [`netrender::Scene`] plus the side
/// channels the painter needs to finish the frame (external-texture
/// composites and blurred-shadow mask requests).
pub struct TranslatedDisplayList {
    pub scene: Scene,
    pub external_textures: Vec<ExternalTextureDraw>,
    /// Blurred box-shadow masks to build before rendering this scene.
    pub box_shadow_masks: Vec<BoxShadowMaskRequest>,
}
// =============================================================================
// PaintList → Scene entry points
// =============================================================================

/// Translate a [`PaintList`] into a [`netrender::Scene`]. External-
/// texture composite metadata stays renderer-private (used by
/// `Paint::render` to drive `render_with_compositor_and_external_textures`);
/// the public entry point returns just the Scene for testability.
pub fn translate_paint_list<L: PaintList>(list: &L) -> Scene {
    translate_paint_cmd_stream(list.viewport(), list.commands(), list.fonts(), list.images()).scene
}

/// Receive-side companion: translate a wire envelope. Thin wrapper
/// since `PaintEnvelope` itself impls `PaintList`.
pub fn translate_envelope(envelope: &paint_list_api::PaintEnvelope) -> Scene {
    translate_paint_list(envelope)
}

/// Variant that also returns the external-texture composite list and
/// box-shadow mask requests. Used by `Paint::render` to drive
/// `render_with_compositor_and_external_textures`.
pub fn translate_envelope_with_external_textures(
    envelope: &paint_list_api::PaintEnvelope,
) -> TranslatedDisplayList {
    translate_paint_cmd_stream(
        envelope.viewport(),
        envelope.commands(),
        envelope.fonts(),
        envelope.images(),
    )
}
/// Stream-form: take a viewport and a flat `PaintCmd` slice.
///
/// ## Transform model
///
/// netrender does *not* cascade a layer's transform to the ops drawn
/// inside it — every `SceneOp` carries its own `transform_id` and the
/// rasterizer resolves it directly (`vello_rasterizer.rs` indexes
/// `transforms[op.transform_id]` per op). So the compositor-coord
/// model (`PushTransform` opens a coordinate space; `Draw*` items emit
/// in local coords) only works if the translator threads the active
/// composed transform onto every op.
///
/// This function maintains a `transform_stack`: each `PushTransform`
/// composes its `(origin, transform)` with the parent (matrix-multiply,
/// parent ∘ local), pushes the result into `scene.transforms`, and
/// pushes the new id onto the stack. `PopTransform` pops. Every `Draw*`
/// op reads the stack top (`current_transform_id`, 0 = identity) and
/// passes it to the `*_transformed` / `*_full` Scene builder. A
/// `PushTransform` does *not* open a `SceneLayer` — a coordinate-space
/// change isn't a compositing group, and pushing a transformed layer
/// would distort the layer's own clip geometry.
pub fn translate_paint_cmd_stream(
    viewport: paint_list_api::DeviceIntSize,
    commands: &[PaintCmd],
    fonts: &[FontResource],
    images: &[ImageResource],
) -> TranslatedDisplayList {
    let viewport_w = viewport.width.max(0) as u32;
    let viewport_h = viewport.height.max(0) as u32;
    let mut scene = Scene::new(viewport_w, viewport_h);
    let mut external_textures = Vec::new();
    let mut box_shadow_masks: Vec<BoxShadowMaskRequest> = Vec::new();
    // Square mask side covering the whole scene; shadow boxes are drawn
    // at their absolute coords inside it.
    let mask_dim = viewport_w.max(viewport_h).max(1);
    // Register fonts + images up-front so DrawText / DrawImage can
    // resolve their keys to scene-side ids.
    let font_map = register_fonts(&mut scene, fonts);
    let image_map = register_images(&mut scene, images);
    // Native pixel size per image key — needed to turn a DrawRepeatingImage's
    // CSS `stretch_size` (the resolved background-size tile, in scene px) into
    // the per-axis brush scale `stretch_size / native`.
    let image_dims: HashMap<ImageKey, (u32, u32)> =
        images.iter().map(|ir| (ir.key, (ir.width, ir.height))).collect();
    // Composed transform ids; top = active coordinate space. Empty
    // means identity (transform_id 0).
    let mut transform_stack: Vec<u32> = Vec::new();

    for cmd in commands {
        let tid = transform_stack.last().copied().unwrap_or(0);
        match cmd {
            // ----- Compositor primitives ---------------------------------
            PaintCmd::PushClip(spec) => emit_push_clip(&mut scene, spec, tid),
            PaintCmd::PopClip => {
                // Clips ride on layers in netrender's model; PushClip pairs with PopLayer.
                scene.pop_layer();
            },
            PaintCmd::PushTransform(spec) => {
                let parent = transform_at(&scene, tid);
                let local = compose_with_origin(
                    &layout_transform_to_scene(&spec.transform),
                    spec.origin.x,
                    spec.origin.y,
                );
                let composed = Transform {
                    m: mat_mul(&parent.m, &local.m),
                };
                scene.transforms.push(composed);
                transform_stack.push((scene.transforms.len() - 1) as u32);
                // `spec.kind` (Standard / Preserve3D / Perspective) is
                // recorded for future stack-state handling; netrender
                // treats the transform as opaque math regardless.
                let _ = spec.kind;
            },
            PaintCmd::PopTransform => {
                transform_stack.pop();
            },
            PaintCmd::PushLayer(spec) => emit_push_layer(&mut scene, spec, tid),
            PaintCmd::PopLayer => {
                scene.pop_layer();
            },

            // ----- Paint primitives --------------------------------------
            PaintCmd::DrawRect(r) => {
                let (x0, y0, x1, y1) = rect_corners(&r.placement.bounds);
                scene.push_rect_transformed(x0, y0, x1, y1, color_to_array(&r.color), tid);
            },
            PaintCmd::DrawStroke(s) => {
                // Lower to netrender's arbitrary-path primitive (`SceneShape`),
                // stroke-only. This is the orrery's edge path (straight or routed
                // polyline). `ScenePathStroke` is `{color, width}` today, so
                // cap/join/dash (`s.cap`/`s.join`/`s.dash`) are not yet honored —
                // a solid butt stroke. Empty paths produce an empty shape (no-op).
                let (dash_pattern, dash_offset) = dash_to_scene(&s.dash);
                scene.push_shape(SceneShape {
                    path: path_data_to_scene_path(&s.path),
                    fill_color: None,
                    stroke: Some(ScenePathStroke {
                        color: color_to_array(&s.color),
                        width: s.width,
                        cap: stroke_cap_to_scene(s.cap),
                        join: stroke_join_to_scene(s.join),
                        dash_pattern,
                        dash_offset,
                    }),
                    transform_id: tid,
                    clip_rect: NO_CLIP,
                    clip_corner_radii: SHARP_CLIP,
                });
            },
            PaintCmd::DrawLine(line) => {
                // First-cut: emit a solid rect spanning the line's
                // local bounds. Decorated styles (wavy/dotted/dashed)
                // need stroke variants.
                let (x0, y0, x1, y1) = rect_corners(&line.placement.bounds);
                scene.push_rect_transformed(x0, y0, x1, y1, color_to_array(&line.color), tid);
            },
            PaintCmd::DrawPath(p) => {
                // Lower to `SceneShape`, carrying whichever of fill / stroke is
                // set (CSS / SVG "filled then stroked"). `ScenePathStroke` is
                // `{color, width}`, so a stroke's cap/join/dash are not yet
                // honored. A path with neither fill nor stroke is a no-op.
                let fill_color = p.fill.as_ref().map(color_to_array);
                let stroke = p.stroke.as_ref().map(|st| {
                    let (dash_pattern, dash_offset) = dash_to_scene(&st.dash);
                    ScenePathStroke {
                        color: color_to_array(&st.color),
                        width: st.width,
                        cap: stroke_cap_to_scene(st.cap),
                        join: stroke_join_to_scene(st.join),
                        dash_pattern,
                        dash_offset,
                    }
                });
                if fill_color.is_some() || stroke.is_some() {
                    scene.push_shape(SceneShape {
                        path: path_data_to_scene_path(&p.path),
                        fill_color,
                        stroke,
                        transform_id: tid,
                        clip_rect: NO_CLIP,
                        clip_corner_radii: SHARP_CLIP,
                    });
                }
            },
            PaintCmd::DrawBorder(border) => match &border.details {
                ple::BorderDetails::NinePatch(np) => {
                    emit_nine_patch(&mut scene, border, np, &image_map, tid)
                },
                ple::BorderDetails::Normal(_) => emit_border_first_cut(&mut scene, border, tid),
            },
            PaintCmd::DrawLinearGradient(g) => emit_linear_gradient(&mut scene, g, tid),
            PaintCmd::DrawRadialGradient(g) => emit_radial_gradient(&mut scene, g, tid),
            PaintCmd::DrawConicGradient(g) => emit_conic_gradient(&mut scene, g, tid),
            PaintCmd::DrawText(t) => {
                if t.glyphs.is_empty() {
                    // Empty run (cache-less probe path) — nothing to paint.
                } else if let Some(&font_id) = font_map.get(&t.font_instance) {
                    let glyphs: Vec<NrGlyph> = t
                        .glyphs
                        .iter()
                        .map(|g| NrGlyph {
                            id: g.index,
                            x: g.point.x,
                            y: g.point.y,
                        })
                        .collect();
                    scene.push_glyph_run_full(
                        font_id,
                        t.font_size,
                        glyphs,
                        color_to_array(&t.color),
                        tid,
                        NO_CLIP,
                        [0.0; 4],
                    );
                } else {
                    warn!(
                        "[paint translator] DrawText references unregistered font {:?}; skipping",
                        t.font_instance
                    );
                }
            },
            PaintCmd::DrawImage(img) => {
                if let Some(&nr_key) = image_map.get(&img.image_key) {
                    let (x0, y0, x1, y1) = rect_corners(&img.placement.bounds);
                    scene.ops.push(SceneOp::Image(SceneImage {
                        x0,
                        y0,
                        x1,
                        y1,
                        uv: [0.0, 0.0, 1.0, 1.0], // full-image UV
                        color: color_to_array(&img.color),
                        key: nr_key,
                        transform_id: tid,
                        clip_rect: NO_CLIP,
                        clip_corner_radii: SHARP_CLIP,
                        clamp_to_uv: false,
                        // `crisp-edges` and `pixelated` both lower to the
                        // nearest-neighbor sampler at this backend.
                        nearest: !matches!(img.image_rendering, ple::ImageRendering::Auto),
                    }));
                } else {
                    warn!(
                        "[paint translator] DrawImage references unregistered image {:?}; skipping",
                        img.image_key
                    );
                }
            },
            PaintCmd::DrawRepeatingImage(ri) => {
                if let Some(&nr_key) = image_map.get(&ri.image_key) {
                    let (x0, y0, x1, y1) = rect_corners(&ri.placement.bounds);
                    // Honor CSS background-size: a tile spans `stretch_size`
                    // (scene px), so the per-axis brush scale is
                    // `stretch_size / native_pixels`. Falls back to native (1:1)
                    // when the size or native dims are unusable. `tile_spacing`
                    // (the `space` gap) is still not modeled.
                    let (nw, nh) = image_dims.get(&ri.image_key).copied().unwrap_or((0, 0));
                    let sx = if nw > 0 && ri.stretch_size.width > 0.0 {
                        ri.stretch_size.width / nw as f32
                    } else {
                        1.0
                    };
                    let sy = if nh > 0 && ri.stretch_size.height > 0.0 {
                        ri.stretch_size.height / nh as f32
                    } else {
                        1.0
                    };
                    scene.ops.push(SceneOp::Pattern(ScenePattern {
                        tile: nr_key,
                        extent: [x0, y0, x1, y1],
                        scale: [sx, sy],
                        transform_id: tid,
                        clip_rect: NO_CLIP,
                        clip_corner_radii: [0.0; 4],
                        nearest: !matches!(ri.image_rendering, ple::ImageRendering::Auto),
                    }));
                } else {
                    warn!(
                        "[paint translator] DrawRepeatingImage references unregistered image {:?}; skipping",
                        ri.image_key
                    );
                }
            },
            PaintCmd::DrawExternalTexture(et) => {
                let (x0, y0, x1, y1) = rect_corners(&et.placement.bounds);
                external_textures.push(ExternalTextureDraw {
                    texture_key: et.texture_key,
                    placement: ExternalTexturePlacement::new([x0, y0, x1, y1])
                        .with_opacity(et.opacity),
                    scene_op_boundary: scene.ops.len(),
                });
            },
            PaintCmd::DrawShadow(s) => {
                if matches!(s.clip_mode, ple::BoxShadowClipMode::Inset) {
                    // Inset: the shadow fills `box_bounds` (the padding box) MINUS
                    // an inner "hole" — the box offset by `offset` and contracted by
                    // `spread` — clipped to `box_bounds`. CSS Backgrounds-3 §7.2.
                    let b = &s.box_bounds;
                    let (bx0, by0, bx1, by1) = (b.min.x, b.min.y, b.max.x, b.max.y);
                    let hx0 = bx0 + s.offset.x + s.spread_radius;
                    let hy0 = by0 + s.offset.y + s.spread_radius;
                    let hx1 = bx1 + s.offset.x - s.spread_radius;
                    let hy1 = by1 + s.offset.y - s.spread_radius;
                    if s.blur_radius <= 0.0 {
                        // Hard inset: the frame between the box and the (box-clamped)
                        // hole, as up to four solid rects in local coords + tid.
                        let col = color_to_array(&s.color);
                        let hcx0 = hx0.max(bx0);
                        let hcy0 = hy0.max(by0);
                        let hcx1 = hx1.min(bx1);
                        let hcy1 = hy1.min(by1);
                        let mut strip = |x0: f32, y0: f32, x1: f32, y1: f32| {
                            if x1 > x0 && y1 > y0 {
                                scene.push_rect_transformed(x0, y0, x1, y1, col, tid);
                            }
                        };
                        if hcx1 <= hcx0 || hcy1 <= hcy0 {
                            // Hole misses the box: the whole box is shadowed.
                            strip(bx0, by0, bx1, by1);
                        } else {
                            strip(bx0, by0, bx1, hcy0); // top
                            strip(bx0, hcy1, bx1, by1); // bottom
                            strip(bx0, hcy0, hcx0, hcy1); // left
                            strip(hcx1, hcy0, bx1, hcy1); // right
                        }
                    } else {
                        // Blurred inset: an inverse Gaussian mask of the hole,
                        // composited tinted over the box and clipped to it. The
                        // inverted mask is `1 - blurred coverage(hole)`: full shadow
                        // at the box edge, fading across the hole boundary, empty
                        // inside the hole. Lift box + hole to absolute coords (the
                        // mask lives in absolute scene space).
                        let m = transform_at(&scene, tid).m;
                        let (bax0, bay0) = apply_transform_2d(&m, bx0, by0);
                        let (bax1, bay1) = apply_transform_2d(&m, bx1, by1);
                        let (box_x0, box_x1) = (bax0.min(bax1), bax0.max(bax1));
                        let (box_y0, box_y1) = (bay0.min(bay1), bay0.max(bay1));
                        let (hax0, hay0) = apply_transform_2d(&m, hx0, hy0);
                        let (hax1, hay1) = apply_transform_2d(&m, hx1, hy1);
                        let (hole_x0, hole_x1) = (hax0.min(hax1), hax0.max(hax1));
                        let (hole_y0, hole_y1) = (hay0.min(hay1), hay0.max(hay1));

                        let key = BOX_SHADOW_MASK_KEY_BASE + box_shadow_masks.len() as u64;
                        box_shadow_masks.push(BoxShadowMaskRequest {
                            key,
                            dim: mask_dim,
                            bounds: [hole_x0, hole_y0, hole_x1, hole_y1],
                            corner_radius: 0.0,
                            blur_radius_px: s.blur_radius,
                            invert: true,
                        });

                        // Sample the inverted mask over the box rect, clipped to it.
                        // The mask covers the whole scene, so box pixels read
                        // `1 - coverage`; the clip + quad confine the shadow to the box.
                        let dim_f = mask_dim as f32;
                        scene.push_image_full(
                            box_x0,
                            box_y0,
                            box_x1,
                            box_y1,
                            [box_x0 / dim_f, box_y0 / dim_f, box_x1 / dim_f, box_y1 / dim_f],
                            color_to_array(&s.color),
                            key,
                            0, // absolute coords already; identity transform
                            [box_x0, box_y0, box_x1, box_y1],
                        );
                    }
                } else {
                    // The offset + spread box, in element-local coords.
                    let b = &s.box_bounds;
                    let lx0 = b.min.x + s.offset.x - s.spread_radius;
                    let ly0 = b.min.y + s.offset.y - s.spread_radius;
                    let lx1 = b.max.x + s.offset.x + s.spread_radius;
                    let ly1 = b.max.y + s.offset.y + s.spread_radius;

                    if s.blur_radius <= 0.0 {
                        // Hard shadow: a solid rect at the offset/spread
                        // box, in local coords + tid (exact, no GPU pass).
                        scene.push_rect_transformed(
                            lx0,
                            ly0,
                            lx1,
                            ly1,
                            color_to_array(&s.color),
                            tid,
                        );
                    } else {
                        // Blurred shadow: build a Gaussian coverage mask
                        // (painter-side GPU pass), then composite it
                        // tinted by the shadow color. The mask lives in
                        // absolute scene space, so lift the local box
                        // through the active transform; the composite
                        // image op uses identity transform with the
                        // absolute rect (no double transform).
                        let m = transform_at(&scene, tid).m;
                        let (ax0, ay0) = apply_transform_2d(&m, lx0, ly0);
                        let (ax1, ay1) = apply_transform_2d(&m, lx1, ly1);
                        // Normalize in case the transform flipped an axis.
                        let (sx0, sx1) = (ax0.min(ax1), ax0.max(ax1));
                        let (sy0, sy1) = (ay0.min(ay1), ay0.max(ay1));

                        let key = BOX_SHADOW_MASK_KEY_BASE + box_shadow_masks.len() as u64;
                        box_shadow_masks.push(BoxShadowMaskRequest {
                            key,
                            dim: mask_dim,
                            bounds: [sx0, sy0, sx1, sy1],
                            corner_radius: 0.0,
                            blur_radius_px: s.blur_radius,
                            invert: false,
                        });

                        // The blurred halo extends ~blur_radius beyond the
                        // box; inflate the sampled rect so the falloff
                        // isn't clipped. UV maps the absolute rect into
                        // the dim×dim mask texture.
                        let margin = s.blur_radius;
                        let tx0 = sx0 - margin;
                        let ty0 = sy0 - margin;
                        let tx1 = sx1 + margin;
                        let ty1 = sy1 + margin;
                        let dim_f = mask_dim as f32;
                        scene.push_image_full(
                            tx0,
                            ty0,
                            tx1,
                            ty1,
                            [tx0 / dim_f, ty0 / dim_f, tx1 / dim_f, ty1 / dim_f],
                            color_to_array(&s.color),
                            key,
                            0, // absolute coords already; identity transform
                            NO_CLIP,
                        );
                    }
                }
            },
            PaintCmd::PushShadow(_) | PaintCmd::PopAllShadows => {
                // State-stack pair; no-op until shadow integration lands.
            },
            PaintCmd::HitTest(_) => {
                // Hit-test items route to a separate netrender::hit_test
                // pass, not the Scene paint-order stream. No-op here.
            },
        }
    }

    TranslatedDisplayList {
        scene,
        external_textures,
        box_shadow_masks,
    }
}
