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
use std::sync::Arc;

use log::warn;
use netrender::{
    ExternalTexturePlacement, FontBlob, FontId, Glyph as NrGlyph, GradientKind,
    GradientStop as NrGradientStop, ImageData, ImageKey as NrImageKey, NO_CLIP, SHARP_CLIP, Scene,
    SceneBlendMode, SceneClip, SceneLayer, SceneOp, ScenePath, ScenePathStroke, ScenePattern,
    SceneShape, SceneStroke, SceneStrokeCap, SceneStrokeJoin, Transform, peniko,
};
use paint_list_api::{
    self as ple, ColorF, FontInstanceKey, FontResource, IdNamespace, ImageKey, ImageResource,
    PaintCmd, PaintList,
};

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
    /// Shadow box in absolute scene coords `[x0, y0, x1, y1]`.
    pub bounds: [f32; 4],
    pub corner_radius: f32,
    pub blur_radius_px: f32,
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
// Shared utilities
// =============================================================================

fn rect_corners(rect: &paint_list_api::LayoutRect) -> (f32, f32, f32, f32) {
    (rect.min.x, rect.min.y, rect.max.x, rect.max.y)
}

fn color_to_array(color: &ColorF) -> [f32; 4] {
    [color.r, color.g, color.b, color.a]
}

fn layout_transform_to_scene(t: &paint_list_api::LayoutTransform) -> Transform {
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

fn mix_blend_mode_to_scene(mode: paint_list_api::MixBlendMode) -> SceneBlendMode {
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

fn gradient_stops(stops: &[paint_list_api::GradientStop]) -> Vec<NrGradientStop> {
    stops
        .iter()
        .map(|s| NrGradientStop {
            offset: s.offset,
            color: [s.color.r, s.color.g, s.color.b, s.color.a],
        })
        .collect()
}

/// Rebuild a `netrender::ScenePath` from the serializable `PathData` command
/// sequence (the shape `DrawStroke` / `DrawPath` carry). 1:1 verb mapping.
fn path_data_to_scene_path(pd: &ple::PathData) -> ScenePath {
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

// =============================================================================
// Layer composition — merge several paint outputs into one Scene
// =============================================================================

/// First `IdNamespace` the compositor remaps layers into. Far above the small
/// per-producer namespaces (serval fonts 0, images 1; nematic/inker likewise low)
/// so a remapped key never aliases a value a producer might also be using in the
/// same merged stream. Layer `i` is remapped into `IdNamespace(BASE + i)`.
const COMPOSITE_NAMESPACE_BASE: u32 = 0xC000_0000;

/// One layer to composite, in back-to-front order (earlier layers paint behind
/// later ones). Borrows a producer's paint output — the producer (a [`PaintList`]
/// impl, or any source) stays the owner and exposes these via
/// `commands()`/`fonts()`/`images()`.
#[derive(Clone, Copy)]
pub struct CompositeLayer<'a> {
    pub commands: &'a [PaintCmd],
    pub fonts: &'a [FontResource],
    pub images: &'a [ImageResource],
}

impl<'a> CompositeLayer<'a> {
    /// A layer from an already-extracted command slice with no font/image
    /// side-tables (e.g. the orrery's scene-paint underlay, which emits only
    /// strokes + rects today).
    pub fn commands_only(commands: &'a [PaintCmd]) -> Self {
        Self { commands, fonts: &[], images: &[] }
    }
}

/// Composite several paint layers into one [`netrender::Scene`], back-to-front.
///
/// Each producer mints font/image keys in its own [`IdNamespace`], and two
/// producers can independently pick the same namespace (both default to 0), so
/// naively concatenating their side-tables would collide — the translator's
/// `FontInstanceKey -> FontId` / `ImageKey -> u64` maps would clobber one
/// producer's entry with another's. This remaps each layer's keys into a
/// composite-unique namespace and rewrites the matching references in that
/// layer's `DrawText` / `DrawImage` / `DrawRepeatingImage` commands, then
/// translates the merged stream against `viewport` (the shared final target).
/// Layers with no fonts/images pass through with their commands untouched.
pub fn composite_paint_layers(
    viewport: paint_list_api::DeviceIntSize,
    layers: &[CompositeLayer<'_>],
) -> TranslatedDisplayList {
    let (commands, fonts, images) = merge_layers(layers);
    translate_paint_cmd_stream(viewport, &commands, &fonts, &images)
}

/// Merge layers' commands + side-tables into one stream with collision-free keys.
/// Split out from [`composite_paint_layers`] so the remapping is unit-testable
/// without going through `Scene` lowering (and its font parsing).
fn merge_layers(
    layers: &[CompositeLayer<'_>],
) -> (Vec<PaintCmd>, Vec<FontResource>, Vec<ImageResource>) {
    let mut commands: Vec<PaintCmd> = Vec::new();
    let mut fonts: Vec<FontResource> = Vec::new();
    let mut images: Vec<ImageResource> = Vec::new();

    for (i, layer) in layers.iter().enumerate() {
        let ns = IdNamespace(COMPOSITE_NAMESPACE_BASE.wrapping_add(i as u32));

        // Remap this layer's font keys into `ns`, preserving the within-layer
        // index, and record old -> new for rewriting the command references.
        let mut font_remap: HashMap<FontInstanceKey, FontInstanceKey> = HashMap::new();
        for (j, fr) in layer.fonts.iter().enumerate() {
            let new_key = FontInstanceKey(ns, j as u32);
            font_remap.insert(fr.key, new_key);
            fonts.push(FontResource { key: new_key, data: fr.data.clone(), index: fr.index });
        }
        let mut image_remap: HashMap<ImageKey, ImageKey> = HashMap::new();
        for (j, ir) in layer.images.iter().enumerate() {
            let new_key = ImageKey(ns, j as u32);
            image_remap.insert(ir.key, new_key);
            images.push(ImageResource {
                key: new_key,
                width: ir.width,
                height: ir.height,
                data: ir.data.clone(),
            });
        }

        // Append commands. The common case (no fonts/images, e.g. the underlay)
        // needs no rewrite, so pass the slice through verbatim.
        if font_remap.is_empty() && image_remap.is_empty() {
            commands.extend_from_slice(layer.commands);
        } else {
            commands.extend(
                layer
                    .commands
                    .iter()
                    .map(|cmd| remap_cmd_keys(cmd, &font_remap, &image_remap)),
            );
        }
    }

    (commands, fonts, images)
}

/// Clone a command, rewriting any font/image key reference through the per-layer
/// remap. Only the three key-bearing variants carry a reference; everything else
/// is a verbatim clone.
fn remap_cmd_keys(
    cmd: &PaintCmd,
    fonts: &HashMap<FontInstanceKey, FontInstanceKey>,
    images: &HashMap<ImageKey, ImageKey>,
) -> PaintCmd {
    let mut c = cmd.clone();
    match &mut c {
        PaintCmd::DrawText(t) => {
            if let Some(&new_key) = fonts.get(&t.font_instance) {
                t.font_instance = new_key;
            }
        },
        PaintCmd::DrawImage(it) => {
            if let Some(&new_key) = images.get(&it.image_key) {
                it.image_key = new_key;
            }
        },
        PaintCmd::DrawRepeatingImage(it) => {
            if let Some(&new_key) = images.get(&it.image_key) {
                it.image_key = new_key;
            }
        },
        _ => {},
    }
    c
}

/// Register a paint list's font side-table into the scene's font
/// palette, returning the `FontInstanceKey → FontId` map that
/// `DrawText` lowering resolves through. Each `FontResource`'s bytes
/// are wrapped in a fresh `peniko::Blob` (mints a vello-dedup id) and
/// pushed via `Scene::push_font`.
fn register_fonts(scene: &mut Scene, fonts: &[FontResource]) -> HashMap<FontInstanceKey, FontId> {
    let mut map = HashMap::new();
    for fr in fonts {
        let blob = FontBlob {
            data: peniko::Blob::new(Arc::new(fr.data.clone())),
            index: fr.index,
        };
        let font_id = scene.push_font(blob);
        map.insert(fr.key, font_id);
    }
    map
}

/// Register a paint list's image side-table into the scene's image
/// sources, returning the `ImageKey → netrender ImageKey` map that
/// `DrawImage` lowering resolves through. netrender's `ImageKey` is a
/// flat `u64`; paint-list's is `(IdNamespace, u32)`, so we fold the
/// namespace + index into the `u64` directly.
///
/// The scene key MUST be derived from the consumer's `ImageResource.key`,
/// not a per-render index: the rasterizer caches `scene_key → bytes` for
/// its whole lifetime and `debug_assert`s that a re-encountered key
/// carries identical bytes. A per-render `i + 1` made every render's
/// first image collide on scene key `1` with different bytes, poisoning
/// the rasterizer lock across a multi-render session (e.g. the WPT
/// reftest runner). Producers mint `ImageResource.key` unique per image,
/// so folding it in gives a stable, collision-free scene key.
fn register_images(scene: &mut Scene, images: &[ImageResource]) -> HashMap<ImageKey, NrImageKey> {
    let mut map = HashMap::new();
    for ir in images {
        // Fold (namespace, index) into the flat u64. `| 1 << 63` keeps
        // it clear of the reserved low keys (0, demo constants) and of a
        // pure-zero namespace+index.
        let nr_key = ((ir.key.0 .0 as u64) << 32) | (ir.key.1 as u64) | (1 << 63);
        scene.set_image_source(
            nr_key,
            ImageData::from_bytes(ir.width, ir.height, ir.data.clone()),
        );
        map.insert(ir.key, nr_key);
    }
    map
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
                scene.push_shape(SceneShape {
                    path: path_data_to_scene_path(&s.path),
                    fill_color: None,
                    stroke: Some(ScenePathStroke { color: color_to_array(&s.color), width: s.width }),
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
                let stroke = p
                    .stroke
                    .as_ref()
                    .map(|st| ScenePathStroke { color: color_to_array(&st.color), width: st.width });
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
            PaintCmd::DrawBorder(border) => emit_border_first_cut(&mut scene, border, tid),
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
                    scene.push_image_full(
                        x0,
                        y0,
                        x1,
                        y1,
                        [0.0, 0.0, 1.0, 1.0], // full-image UV
                        color_to_array(&img.color),
                        nr_key,
                        tid,
                        NO_CLIP,
                    );
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
                // Inset shadows deferred (need clip-difference).
                if matches!(s.clip_mode, ple::BoxShadowClipMode::Inset) {
                    warn!("[paint translator] inset box-shadow deferred");
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

/// Base for box-shadow mask image keys, chosen well above the
/// sequential keys [`register_images`] mints (1..N) so masks never
/// collide with real images in the rasterizer's image table.
const BOX_SHADOW_MASK_KEY_BASE: u64 = 0xFFFF_0000_0000_0000;

/// Apply a column-major 4×4 transform to a 2D point `(x, y, 0, 1)`,
/// returning the transformed `(x', y')`. Used to lift a shadow box
/// from its element-local coords into absolute scene coords for the
/// GPU mask pass (the mask is built in scene space, not local space).
fn apply_transform_2d(m: &[f32; 16], x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[4] * y + m[12], m[1] * x + m[5] * y + m[13])
}

/// The `Transform` at a palette index; identity for index 0.
fn transform_at(scene: &Scene, tid: u32) -> Transform {
    scene
        .transforms
        .get(tid as usize)
        .copied()
        .unwrap_or(Transform::IDENTITY)
}

/// Column-major 4×4 matrix multiply: `a ∘ b` (apply `b` first, then
/// `a`). Used to compose a child transform with its parent so nested
/// `PushTransform`s accumulate.
fn mat_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.0f32; 16];
    for col in 0..4 {
        for row in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[k * 4 + row] * b[col * 4 + k];
            }
            out[col * 4 + row] = s;
        }
    }
    out
}

// =============================================================================
// PaintCmd per-variant emit helpers
// =============================================================================

fn emit_push_clip(scene: &mut Scene, spec: &ple::ClipSpec, tid: u32) {
    let clip = match &spec.kind {
        ple::ClipKind::Rect(rect) => {
            let (x0, y0, x1, y1) = rect_corners(rect);
            SceneClip::Rect {
                rect: [x0, y0, x1, y1],
                radii: [0.0, 0.0, 0.0, 0.0],
            }
        },
        ple::ClipKind::RoundedRect { rect, radius, .. } => {
            let (x0, y0, x1, y1) = rect_corners(rect);
            SceneClip::Rect {
                rect: [x0, y0, x1, y1],
                radii: [
                    radius.top_left.width,
                    radius.top_right.width,
                    radius.bottom_right.width,
                    radius.bottom_left.width,
                ],
            }
        },
        ple::ClipKind::Path(_) => {
            // Path clips need kurbo::BezPath reconstruction; same
            // deferred plumbing as DrawPath. Fall back to no-clip so
            // the layer still pushes and pairs correctly with PopClip.
            warn!("[paint translator] PushClip(Path) deferred; pushing unclipped layer");
            SceneClip::None
        },
    };
    // The clip layer carries the active transform so its geometry is
    // resolved in the same coordinate space as the clipped content.
    scene.push_layer(SceneLayer {
        clip,
        alpha: 1.0,
        blend_mode: SceneBlendMode::Normal,
        compose: netrender::SceneCompose::SrcOver,
        transform_id: tid,
        backdrop_filter: None,
    });
}

fn compose_with_origin(t: &Transform, ox: f32, oy: f32) -> Transform {
    // The netrender Transform is a flat 16-float array. "translate by
    // (ox, oy) then apply t" sets the translation columns: [12] += ox,
    // [13] += oy.
    let mut out = t.m;
    out[12] += ox;
    out[13] += oy;
    Transform { m: out }
}

fn emit_push_layer(scene: &mut Scene, spec: &ple::LayerSpec, tid: u32) {
    let blend_mode = mix_blend_mode_to_scene(spec.mix_blend_mode);
    let mut alpha = spec.opacity;
    // Filter-chain opacity collapses into the layer's alpha; other
    // filters need backdrop machinery and are deferred.
    for filter in &spec.filters {
        if let ple::FilterOp::Opacity(a) = filter {
            alpha *= *a;
        }
    }
    let _ = spec.raster_space; // Local vs Screen — deferred
    let _ = spec.flags;        // BLEND_CONTAINER etc. — deferred
    let _ = &spec.mask;        // alpha-mask layer — deferred
    scene.push_layer(SceneLayer {
        clip: SceneClip::None,
        alpha,
        blend_mode,
        compose: netrender::SceneCompose::SrcOver,
        transform_id: tid,
        backdrop_filter: None,
    });
}

fn emit_border_first_cut(scene: &mut Scene, border: &ple::BorderItem, tid: u32) {
    use ple::BorderStyle as BS;
    let rect = &border.placement.bounds;
    let widths = &border.widths;
    let sides = match &border.details {
        ple::BorderDetails::Normal(n) => n,
        ple::BorderDetails::NinePatch(_) => {
            warn!("[paint translator] nine-patch border deferred");
            return;
        },
    };

    // Uniform border (all four sides identical width / style / color) is the
    // common case and the only one a single stroked path can render — strokes
    // can't carry per-side colors. When it's also rounded or dashed/dotted, take
    // the stroke fast path; the 4-rect fallback below handles the rest (and
    // per-side borders, always square).
    let r = &sides.radius;
    let has_radius = r.top_left.width > 0.0 || r.top_right.width > 0.0
        || r.bottom_right.width > 0.0 || r.bottom_left.width > 0.0;
    let w = widths.top;
    let uniform_width = (widths.right - w).abs() < 0.01
        && (widths.bottom - w).abs() < 0.01
        && (widths.left - w).abs() < 0.01;
    let s = sides.top.style;
    let uniform_style = sides.right.style == s && sides.bottom.style == s && sides.left.style == s;
    let c = sides.top.color;
    let col_eq = |a: &ColorF, b: &ColorF| (a.r - b.r).abs() < 0.004 && (a.g - b.g).abs() < 0.004
        && (a.b - b.b).abs() < 0.004 && (a.a - b.a).abs() < 0.004;
    let uniform_color = col_eq(&sides.right.color, &c)
        && col_eq(&sides.bottom.color, &c) && col_eq(&sides.left.color, &c);
    // dotted/dashed need a stroke (dash pattern); solid needs a stroke only to
    // round. `double`/`groove`/etc. fall through to the (square, solid) fallback.
    let dash = match s {
        BS::Dotted => Some(vec![w.max(1.0), w.max(1.0)]),
        BS::Dashed => Some(vec![(w * 3.0).max(1.0), w.max(1.0)]),
        _ => None,
    };
    let strokeable = matches!(s, BS::Solid | BS::Dotted | BS::Dashed);

    if w > 0.01 && uniform_width && uniform_style && uniform_color && strokeable
        && (has_radius || dash.is_some())
    {
        // Stroke is centered on its path; a CSS border sits inside the
        // border-box, so inset the path by w/2 and shrink the corner radii to
        // match (radius is measured at the border-box edge).
        let h = w * 0.5;
        let inset = |r: f32| (r - h).max(0.0);
        scene.push_stroke_op(SceneStroke {
            x0: rect.min.x + h,
            y0: rect.min.y + h,
            x1: rect.max.x - h,
            y1: rect.max.y - h,
            color: color_to_array(&c),
            stroke_width: w,
            stroke_corner_radii: [
                inset(r.top_left.width),
                inset(r.top_right.width),
                inset(r.bottom_right.width),
                inset(r.bottom_left.width),
            ],
            transform_id: tid,
            clip_rect: NO_CLIP,
            clip_corner_radii: SHARP_CLIP,
            cap: SceneStrokeCap::Butt,
            join: SceneStrokeJoin::Miter,
            dash_pattern: dash.unwrap_or_default(),
            dash_offset: 0.0,
        });
        return;
    }

    // Fallback: per-side filled rects (square corners). Handles non-uniform
    // borders and styles the stroke path doesn't model yet (double/groove/...).
    if widths.top > 0.0 {
        scene.push_rect_transformed(
            rect.min.x,
            rect.min.y,
            rect.max.x,
            rect.min.y + widths.top,
            color_to_array(&sides.top.color),
            tid,
        );
    }
    if widths.bottom > 0.0 {
        scene.push_rect_transformed(
            rect.min.x,
            rect.max.y - widths.bottom,
            rect.max.x,
            rect.max.y,
            color_to_array(&sides.bottom.color),
            tid,
        );
    }
    if widths.left > 0.0 {
        scene.push_rect_transformed(
            rect.min.x,
            rect.min.y,
            rect.min.x + widths.left,
            rect.max.y,
            color_to_array(&sides.left.color),
            tid,
        );
    }
    if widths.right > 0.0 {
        scene.push_rect_transformed(
            rect.max.x - widths.right,
            rect.min.y,
            rect.max.x,
            rect.max.y,
            color_to_array(&sides.right.color),
            tid,
        );
    }
    let _ = sides.radius;
    let _ = sides.do_aa;
}

fn emit_linear_gradient(scene: &mut Scene, item: &ple::LinearGradientItem, tid: u32) {
    let rect = &item.placement.bounds;
    let g = &item.gradient;
    scene.push_gradient(netrender::SceneGradient {
        x0: rect.min.x,
        y0: rect.min.y,
        x1: rect.max.x,
        y1: rect.max.y,
        kind: GradientKind::Linear,
        repeat: matches!(g.extend_mode, ple::ExtendMode::Repeat),
        params: [
            g.start_point.x,
            g.start_point.y,
            g.end_point.x,
            g.end_point.y,
        ],
        stops: gradient_stops(&g.stops),
        transform_id: tid,
        clip_rect: NO_CLIP,
        clip_corner_radii: [0.0; 4],
    });
}

fn emit_radial_gradient(scene: &mut Scene, item: &ple::RadialGradientItem, tid: u32) {
    let rect = &item.placement.bounds;
    let g = &item.gradient;
    scene.push_gradient(netrender::SceneGradient {
        x0: rect.min.x,
        y0: rect.min.y,
        x1: rect.max.x,
        y1: rect.max.y,
        kind: GradientKind::Radial,
        repeat: matches!(g.extend_mode, ple::ExtendMode::Repeat),
        params: [g.center.x, g.center.y, g.radius.width, g.radius.height],
        stops: gradient_stops(&g.stops),
        transform_id: tid,
        clip_rect: NO_CLIP,
        clip_corner_radii: [0.0; 4],
    });
}

fn emit_conic_gradient(scene: &mut Scene, item: &ple::ConicGradientItem, tid: u32) {
    let rect = &item.placement.bounds;
    let g = &item.gradient;
    scene.push_gradient(netrender::SceneGradient {
        x0: rect.min.x,
        y0: rect.min.y,
        x1: rect.max.x,
        y1: rect.max.y,
        kind: GradientKind::Conic,
        repeat: matches!(g.extend_mode, ple::ExtendMode::Repeat),
        params: [g.center.x, g.center.y, g.angle, 0.0],
        stops: gradient_stops(&g.stops),
        transform_id: tid,
        clip_rect: NO_CLIP,
        clip_corner_radii: [0.0; 4],
    });
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use paint_list_api::{
        ColorF, CommonPlacement, DeviceIntSize, EngineId, LayoutPoint, LayoutRect, PaintCmd,
        PaintList, PrimitiveFlags, RectItem,
    };
    use serde::{Deserialize, Serialize};

    use super::*;

    /// Minimal `PaintList` impl for driving `translate_paint_list`
    /// from tests without pulling in a producer crate.
    #[derive(Clone, Debug, Default, Deserialize, Serialize)]
    struct StubPaintList {
        viewport: DeviceIntSize,
        commands: Vec<PaintCmd>,
    }

    impl PaintList for StubPaintList {
        fn engine_id(&self) -> EngineId {
            EngineId::SERVAL
        }
        fn viewport(&self) -> DeviceIntSize {
            self.viewport
        }
        fn generation_id(&self) -> u64 {
            0
        }
        fn commands(&self) -> &[PaintCmd] {
            &self.commands
        }
    }

    fn box2d(x: f32, y: f32, w: f32, h: f32) -> LayoutRect {
        LayoutRect::new(LayoutPoint::new(x, y), LayoutPoint::new(x + w, y + h))
    }

    fn placement_at(bounds: LayoutRect) -> CommonPlacement {
        CommonPlacement {
            bounds,
            flags: PrimitiveFlags::empty(),
        }
    }

    fn list_with(viewport: DeviceIntSize, cmds: Vec<PaintCmd>) -> StubPaintList {
        StubPaintList {
            viewport,
            commands: cmds,
        }
    }

    #[test]
    fn empty_list_translates_to_empty_scene() {
        let list = list_with(DeviceIntSize::new(800, 600), Vec::new());
        let scene = translate_paint_list(&list);
        assert_eq!(scene.viewport_width, 800);
        assert_eq!(scene.viewport_height, 600);
        assert_eq!(scene.ops.len(), 0);
    }

    #[test]
    fn draw_rect_emits_scene_rect() {
        let list = list_with(
            DeviceIntSize::new(800, 600),
            vec![PaintCmd::DrawRect(RectItem {
                placement: placement_at(box2d(10.0, 20.0, 100.0, 50.0)),
                color: ColorF {
                    r: 1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
            })],
        );
        let scene = translate_paint_list(&list);
        assert_eq!(scene.ops.len(), 1);
        assert!(matches!(scene.ops[0], netrender::SceneOp::Rect(_)));
    }

    /// Composing layers with no side-tables (the orrery underlay + a doc, today)
    /// concatenates command streams back-to-front into one scene.
    #[test]
    fn commands_only_layers_concatenate_into_one_scene() {
        let underlay = vec![PaintCmd::DrawRect(RectItem {
            placement: placement_at(box2d(0.0, 0.0, 10.0, 10.0)),
            color: ColorF { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },
        })];
        let doc = vec![PaintCmd::DrawRect(RectItem {
            placement: placement_at(box2d(5.0, 5.0, 10.0, 10.0)),
            color: ColorF { r: 0.0, g: 0.0, b: 1.0, a: 1.0 },
        })];
        let layers = [
            CompositeLayer::commands_only(&underlay),
            CompositeLayer::commands_only(&doc),
        ];
        let out = composite_paint_layers(DeviceIntSize::new(800, 600), &layers);
        let rects = out
            .scene
            .ops
            .iter()
            .filter(|o| matches!(o, netrender::SceneOp::Rect(_)))
            .count();
        assert_eq!(rects, 2, "both layers' rects composite into one scene");
    }

    /// Two layers that BOTH mint a font under the default namespace+index (the
    /// collision producers fall into) must come out with distinct keys, and each
    /// layer's `DrawText` rewritten to its own remapped key — otherwise the
    /// translator's `FontInstanceKey -> FontId` map clobbers one font with the
    /// other. (Pins the font/image key-namespacing the seam exists for.)
    #[test]
    fn merge_namespaces_colliding_font_keys() {
        use paint_list_api::{FontInstanceKey, FontResource, IdNamespace, TextOptions, TextRunItem};

        let collide = FontInstanceKey::new(IdNamespace(0), 0);
        let mk = |face: u8| {
            let fonts = vec![FontResource { key: collide, data: vec![face], index: 0 }];
            let cmds = vec![PaintCmd::DrawText(TextRunItem {
                placement: placement_at(box2d(0.0, 0.0, 10.0, 10.0)),
                font_instance: collide,
                font_size: 16.0,
                color: ColorF::BLACK,
                glyphs: vec![],
                options: TextOptions::default(),
            })];
            (fonts, cmds)
        };
        let (f0, c0) = mk(0xAA);
        let (f1, c1) = mk(0xBB);
        let layers = [
            CompositeLayer { commands: &c0, fonts: &f0, images: &[] },
            CompositeLayer { commands: &c1, fonts: &f1, images: &[] },
        ];
        let (commands, fonts, images) = merge_layers(&layers);

        assert!(images.is_empty());
        assert_eq!(fonts.len(), 2, "both fonts survive the merge");
        assert_ne!(fonts[0].key, fonts[1].key, "colliding keys made distinct");
        assert_eq!(fonts[0].data, vec![0xAA], "font bytes paired to the right new key");
        assert_eq!(fonts[1].data, vec![0xBB]);
        let text_keys: Vec<_> = commands
            .iter()
            .filter_map(|c| match c {
                PaintCmd::DrawText(t) => Some(t.font_instance),
                _ => None,
            })
            .collect();
        assert_eq!(
            text_keys,
            vec![fonts[0].key, fonts[1].key],
            "each layer's DrawText references its own remapped font",
        );
    }

    /// `DrawStroke` (the orrery's edge primitive — a straight or routed polyline)
    /// lowers to one stroked `SceneShape`, so edges render. (Was warn-skipped.)
    #[test]
    fn draw_stroke_emits_stroked_scene_shape() {
        use paint_list_api::{LayoutPoint, PathCommand, PathData, StrokeCap, StrokeItem, StrokeJoin};

        let list = list_with(
            DeviceIntSize::new(800, 600),
            vec![PaintCmd::DrawStroke(StrokeItem {
                placement: placement_at(box2d(0.0, 0.0, 100.0, 100.0)),
                path: PathData {
                    commands: vec![
                        PathCommand::MoveTo(LayoutPoint::new(0.0, 0.0)),
                        PathCommand::LineTo(LayoutPoint::new(100.0, 80.0)),
                    ],
                },
                color: ColorF { r: 0.0, g: 0.0, b: 0.0, a: 1.0 },
                width: 2.0,
                cap: StrokeCap::Butt,
                join: StrokeJoin::Miter,
                dash: None,
            })],
        );
        let scene = translate_paint_list(&list);
        let stroked = scene
            .ops
            .iter()
            .filter(|o| matches!(o, netrender::SceneOp::Shape(s) if s.stroke.is_some()))
            .count();
        assert_eq!(stroked, 1, "DrawStroke lowers to one stroked SceneShape");
    }

    #[test]
    fn push_pop_layer_emits_layer_pair() {
        let list = list_with(
            DeviceIntSize::new(800, 600),
            vec![
                PaintCmd::PushLayer(ple::LayerSpec {
                    opacity: 0.5,
                    ..ple::LayerSpec::default()
                }),
                PaintCmd::PopLayer,
            ],
        );
        let scene = translate_paint_list(&list);
        assert_eq!(scene.ops.len(), 2);
        assert!(matches!(scene.ops[0], netrender::SceneOp::PushLayer(_)));
        assert!(matches!(scene.ops[1], netrender::SceneOp::PopLayer));
    }

    #[test]
    fn push_transform_adds_palette_entry_and_positions_child_ops() {
        // PushTransform is a coordinate-space change, NOT a compositing
        // layer: it adds a transform palette entry and threads the id
        // onto child ops, but emits no PushLayer/PopLayer.
        let list = list_with(
            DeviceIntSize::new(800, 600),
            vec![
                PaintCmd::PushTransform(ple::TransformSpec {
                    origin: LayoutPoint::new(10.0, 20.0),
                    transform: paint_list_api::LayoutTransform::identity(),
                    kind: ple::TransformKind::Standard,
                }),
                PaintCmd::DrawRect(RectItem {
                    placement: placement_at(box2d(0.0, 0.0, 100.0, 50.0)),
                    color: ColorF {
                        r: 1.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    },
                }),
                PaintCmd::PopTransform,
            ],
        );
        let scene = translate_paint_list(&list);
        // One transform palette entry beyond identity (index 1).
        assert!(
            scene.transforms.len() >= 2,
            "transforms: {:?}",
            scene.transforms
        );
        // No layers — just the rect.
        assert_eq!(scene.ops.len(), 1);
        let rect = match &scene.ops[0] {
            netrender::SceneOp::Rect(r) => r,
            other => panic!("expected Rect, got {other:?}"),
        };
        // The rect picks up the pushed transform (non-zero id).
        assert!(
            rect.transform_id > 0,
            "rect should carry the pushed transform id"
        );
        // That transform translates by the origin (10, 20).
        let t = &scene.transforms[rect.transform_id as usize];
        assert_eq!(t.m[12], 10.0, "tx");
        assert_eq!(t.m[13], 20.0, "ty");
    }

    #[test]
    fn nested_transforms_compose() {
        // Outer translate(10, 20), inner translate(5, 5) → a rect
        // inside both should resolve to translate(15, 25).
        let list = list_with(
            DeviceIntSize::new(800, 600),
            vec![
                PaintCmd::PushTransform(ple::TransformSpec {
                    origin: LayoutPoint::new(10.0, 20.0),
                    transform: paint_list_api::LayoutTransform::identity(),
                    kind: ple::TransformKind::Standard,
                }),
                PaintCmd::PushTransform(ple::TransformSpec {
                    origin: LayoutPoint::new(5.0, 5.0),
                    transform: paint_list_api::LayoutTransform::identity(),
                    kind: ple::TransformKind::Standard,
                }),
                PaintCmd::DrawRect(RectItem {
                    placement: placement_at(box2d(0.0, 0.0, 10.0, 10.0)),
                    color: ColorF {
                        r: 0.0,
                        g: 1.0,
                        b: 0.0,
                        a: 1.0,
                    },
                }),
                PaintCmd::PopTransform,
                PaintCmd::PopTransform,
            ],
        );
        let scene = translate_paint_list(&list);
        let rect = match &scene.ops[0] {
            netrender::SceneOp::Rect(r) => r,
            other => panic!("expected Rect, got {other:?}"),
        };
        let t = &scene.transforms[rect.transform_id as usize];
        assert_eq!(t.m[12], 15.0, "composed tx (10 + 5)");
        assert_eq!(t.m[13], 25.0, "composed ty (20 + 5)");
    }

    #[test]
    fn push_clip_rect_emits_clipped_layer() {
        let list = list_with(
            DeviceIntSize::new(800, 600),
            vec![
                PaintCmd::PushClip(ple::ClipSpec {
                    kind: ple::ClipKind::Rect(box2d(0.0, 0.0, 100.0, 100.0)),
                }),
                PaintCmd::PopClip,
            ],
        );
        let scene = translate_paint_list(&list);
        assert_eq!(scene.ops.len(), 2);
        let layer = match &scene.ops[0] {
            netrender::SceneOp::PushLayer(l) => l,
            other => panic!("expected PushLayer, got {other:?}"),
        };
        assert!(matches!(layer.clip, netrender::SceneClip::Rect { .. }));
        assert!(matches!(scene.ops[1], netrender::SceneOp::PopLayer));
    }

    #[test]
    fn external_texture_routes_to_external_textures_vec() {
        use paint_list_api::ExternalTextureItem;
        let list = list_with(
            DeviceIntSize::new(800, 600),
            vec![PaintCmd::DrawExternalTexture(ExternalTextureItem {
                placement: placement_at(box2d(0.0, 0.0, 200.0, 200.0)),
                texture_key: 0xC0FFEE,
                opacity: 0.75,
                content_generation: None,
            })],
        );
        // External texture metadata lives on the full-shape translator
        // output; use translate_paint_cmd_stream to inspect it.
        let out = translate_paint_cmd_stream(list.viewport, &list.commands, &[], &[]);
        // External texture doesn't add to scene.ops; it goes into the
        // separate compositor vector via the PM-3 lowering contract.
        assert_eq!(out.scene.ops.len(), 0);
        assert_eq!(out.external_textures.len(), 1);
        assert_eq!(out.external_textures[0].texture_key, 0xC0FFEE);
        assert_eq!(out.external_textures[0].scene_op_boundary, 0);
    }
}
