/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Demo card grid — consumer reference for `netrender`'s primitive
//! surface, plus a manual-eyeball regression check.
//!
//! Renders a 3×2 grid of cards into a single 720×420 frame and saves
//! it to `netrender/examples/output/demo_card_grid.png`. Each card
//! exercises a different combination of primitives so a visual diff
//! after a change to the rasterizer is easy to spot:
//!
//!   1. plain rounded card
//!   2. rounded card + stroke border
//!   3. linear-gradient card + border
//!   4. image card (generated checker payload) + rounded clip
//!   5. radial-gradient card + drop shadow underneath
//!   6. painter-order probe — image with a "badge" rect drawn after
//!      it in the consumer code; surfaces the fixed type-Vec painter
//!      order by showing the badge *under* the image
//!
//! Text labels render best-effort via system fonts (Arial /
//! Segoe UI / DejaVu / Liberation, in that order). On hosts with no
//! match the labels are skipped — the rest of the scene still
//! renders.
//!
//! Run with:
//!   cargo run --example demo_card_grid
//!
//! Override the output path with `NETRENDER_DEMO_OUT=/tmp/foo.png`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use netrender::{ColorLoad, ImageData, NetrenderOptions, Scene, boot, create_netrender_instance};
use netrender_text::parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, Layout, LayoutContext, StyleProperty,
    fontique,
};

const VW: u32 = 720;
const VH: u32 = 420;
const TILE: u32 = 64;

const CARD_W: f32 = 200.0;
const CARD_H: f32 = 180.0;
const GUTTER: f32 = 20.0;
const MARGIN_X: f32 = 20.0;
const MARGIN_Y: f32 = 20.0;
const RADIUS: f32 = 14.0;

// Premultiplied RGBA helpers. The Scene API is premultiplied
// throughout (per plan §6.3); the `rgba` helper does the multiply
// for the common case of "I have a straight-alpha color".
fn rgba(r: f32, g: f32, b: f32, a: f32) -> [f32; 4] {
    [r * a, g * a, b * a, a]
}

fn opaque(r: f32, g: f32, b: f32) -> [f32; 4] {
    [r, g, b, 1.0]
}

/// Top-left corner of card at column `col` (0-based, 0..3) and row
/// `row` (0..2).
fn card_origin(col: u32, row: u32) -> (f32, f32) {
    let x = MARGIN_X + (CARD_W + GUTTER) * col as f32;
    let y = MARGIN_Y + (CARD_H + GUTTER) * row as f32;
    (x, y)
}

fn card_rect(col: u32, row: u32) -> [f32; 4] {
    let (x, y) = card_origin(col, row);
    [x, y, x + CARD_W, y + CARD_H]
}

/// Build a 64×64 RGBA8 checker pattern as a stand-in for "image
/// content." Two-color, 8×8 squares — visible enough to confirm
/// sampling at any zoom.
fn checker_image() -> ImageData {
    const SZ: u32 = 64;
    let mut bytes = Vec::with_capacity((SZ * SZ * 4) as usize);
    for y in 0..SZ {
        for x in 0..SZ {
            let on = ((x / 8) + (y / 8)) % 2 == 0;
            let pixel = if on {
                [80, 130, 200, 255]
            } else {
                [40, 60, 110, 255]
            };
            bytes.extend_from_slice(&pixel);
        }
    }
    ImageData::from_bytes(SZ, SZ, bytes)
}

/// Holds parley state across the whole demo. Shaping/font setup is
/// best-effort — when `try_setup` returns `None` (no system font
/// found), the demo still draws every card; only the labels are
/// skipped. Bundling a font in an example would add ~1MB to the
/// repo and a license attribution; we deliberately don't.
struct TextShaper {
    font_cx: FontContext,
    layout_cx: LayoutContext<[f32; 4]>,
    family_name: String,
    /// Per-shaper FontRegistry so all 6 card labels register the
    /// shared font exactly once in `scene.fonts` (instead of one
    /// FontBlob per push_label call).
    font_registry: netrender::FontRegistry,
}

impl TextShaper {
    fn try_setup() -> Option<Self> {
        let candidates = [
            r"C:\Windows\Fonts\segoeui.ttf",
            r"C:\Windows\Fonts\arial.ttf",
            "/System/Library/Fonts/Helvetica.ttc",
            "/Library/Fonts/Arial.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
        ];
        for path in candidates {
            if let Ok(bytes) = std::fs::read(path) {
                eprintln!("demo_card_grid: loaded {} ({} bytes)", path, bytes.len());
                let mut font_cx = FontContext::new();
                let blob = fontique::Blob::new(Arc::new(bytes));
                let (family_id, _) = font_cx
                    .collection
                    .register_fonts(blob, None)
                    .into_iter()
                    .next()
                    .expect("register_fonts on a real TTF returns at least one family");
                let family_name = font_cx
                    .collection
                    .family_name(family_id)
                    .expect("registered family has a name")
                    .to_owned();
                return Some(Self {
                    font_cx,
                    layout_cx: LayoutContext::new(),
                    family_name,
                    font_registry: netrender::FontRegistry::new(),
                });
            }
        }
        eprintln!("demo_card_grid: no system font found, labels will be skipped");
        None
    }

    /// Shape `text` and push it as glyph runs at `(x, y)` (top-left
    /// of the layout box). Color is premultiplied RGBA. Real
    /// shaping handles ascenders/descenders, kerning, and BiDi —
    /// the previous fixed-pitch hack got none of that.
    fn push_label(
        &mut self,
        scene: &mut Scene,
        text: &str,
        x: f32,
        y: f32,
        size: f32,
        color: [f32; 4],
    ) {
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, true);
        builder.push_default(StyleProperty::FontSize(size));
        builder.push_default(StyleProperty::Brush(color));
        builder.push_default(StyleProperty::FontFamily(FontFamily::named(
            &self.family_name,
        )));
        let mut layout: Layout<[f32; 4]> = builder.build(text);
        // Single-line labels: pass the card width as max so any
        // wrapping (none here, but defensive) lands cleanly inside.
        layout.break_all_lines(Some(CARD_W));
        layout.align(Alignment::Start, AlignmentOptions::default());
        netrender_text::push_layout_with_registry(scene, &mut self.font_registry, &layout, [x, y]);
    }
}

/// Label color: near-white, opaque (premultiplied).
const LABEL_COLOR: [f32; 4] = [0.95, 0.95, 0.95, 1.0];
const LABEL_SIZE: f32 = 16.0;

/// Inputs Card 5 needs to composite its drop shadow under the card
/// body. The shadow mask itself is built render-graph-side by main()
/// before the scene is assembled.
struct ShadowDef {
    image_key: u64,
    /// Full-target rect of the mask texture in scene-space (matches
    /// `dim` argument to `build_box_shadow_mask`).
    mask_dim: u32,
    /// Where to draw the shadow image, in scene-space. Typically the
    /// card rect inflated and offset.
    target_rect: [f32; 4],
    /// Premultiplied tint applied to the alpha-only mask.
    color: [f32; 4],
}

fn build_cards(
    scene: &mut Scene,
    shaper: &mut Option<TextShaper>,
    image_key: u64,
    shadow: &ShadowDef,
) {
    // Page background — slate. Single full-viewport rect.
    scene.push_rect(0.0, 0.0, VW as f32, VH as f32, opaque(0.10, 0.11, 0.13));

    // Card 1 — plain rounded rect, no border.
    {
        let r = card_rect(0, 0);
        scene.push_rect_clipped_rounded(
            r[0],
            r[1],
            r[2],
            r[3],
            opaque(0.22, 0.30, 0.45),
            0,
            r,
            [RADIUS; 4],
        );
        if let Some(s) = shaper.as_mut() {
            s.push_label(
                scene,
                "Rounded",
                r[0] + 14.0,
                r[3] - 32.0,
                LABEL_SIZE,
                LABEL_COLOR,
            );
        }
    }

    // Card 2 — rounded rect + stroke border.
    {
        let r = card_rect(1, 0);
        scene.push_rect_clipped_rounded(
            r[0],
            r[1],
            r[2],
            r[3],
            opaque(0.18, 0.36, 0.30),
            0,
            r,
            [RADIUS; 4],
        );
        // Stroke is 2px wide; netrender expands its extent inward
        // to keep within the card rect (per stroke filter inflate).
        scene.push_stroke_rounded(
            r[0],
            r[1],
            r[2],
            r[3],
            opaque(0.55, 0.85, 0.75),
            2.0,
            [RADIUS; 4],
        );
        if let Some(s) = shaper.as_mut() {
            s.push_label(
                scene,
                "Border",
                r[0] + 14.0,
                r[3] - 32.0,
                LABEL_SIZE,
                LABEL_COLOR,
            );
        }
    }

    // Card 3 — linear gradient bg + border. Diagonal violet ramp.
    {
        let r = card_rect(2, 0);
        scene.push_linear_gradient_full(
            r[0],
            r[1],
            r[2],
            r[3],
            [r[0], r[1]],
            [r[2], r[3]],
            opaque(0.45, 0.20, 0.55),
            opaque(0.95, 0.55, 0.85),
            0,
            r,
        );
        scene.push_stroke_rounded(
            r[0],
            r[1],
            r[2],
            r[3],
            opaque(0.95, 0.85, 0.95),
            2.0,
            [RADIUS; 4],
        );
        if let Some(s) = shaper.as_mut() {
            s.push_label(
                scene,
                "Gradient",
                r[0] + 14.0,
                r[3] - 32.0,
                LABEL_SIZE,
                LABEL_COLOR,
            );
        }
    }

    // Card 4 — image filling the card with a rounded clip. Tinted
    // dim so labels are still readable on top.
    {
        let r = card_rect(0, 1);
        // Backstop rect so non-painted image pixels still show the
        // card body (image-cropping is via UV, not clip, in this
        // example, so this also doubles as the painter-order context
        // for card 6).
        scene.push_rect_clipped_rounded(
            r[0],
            r[1],
            r[2],
            r[3],
            opaque(0.05, 0.05, 0.06),
            0,
            r,
            [RADIUS; 4],
        );
        scene.push_image_full_rounded(
            r[0],
            r[1],
            r[2],
            r[3],
            [0.0, 0.0, 1.0, 1.0],
            opaque(0.85, 0.85, 0.85), // mild tint
            image_key,
            0,
            r,
            [RADIUS; 4],
        );
        if let Some(s) = shaper.as_mut() {
            s.push_label(
                scene,
                "Image",
                r[0] + 14.0,
                r[3] - 32.0,
                LABEL_SIZE,
                LABEL_COLOR,
            );
        }
    }

    // Card 5 — radial gradient + a drop shadow that correctly sits
    // *under* the card body. After the op-list refactor, painter
    // order is consumer push order: push the shadow image first,
    // then the card's gradient on top. (Pre-refactor the shadow
    // image always painted over the gradient because images came
    // after rects/gradients in the fixed type-Vec order.)
    {
        let r = card_rect(1, 1);

        // 1. Shadow image — painted first, sits under the card body.
        let s_rect = shadow.target_rect;
        let dim_f = shadow.mask_dim as f32;
        scene.push_image_full(
            s_rect[0],
            s_rect[1],
            s_rect[2],
            s_rect[3],
            [
                s_rect[0] / dim_f,
                s_rect[1] / dim_f,
                s_rect[2] / dim_f,
                s_rect[3] / dim_f,
            ],
            shadow.color,
            shadow.image_key,
            0,
            netrender::NO_CLIP,
        );

        // 2. Card body — radial gradient + border on top of shadow.
        scene.push_radial_gradient_full(
            r[0],
            r[1],
            r[2],
            r[3],
            [(r[0] + r[2]) * 0.5, (r[1] + r[3]) * 0.5],
            [CARD_W * 0.6, CARD_H * 0.6],
            opaque(0.95, 0.65, 0.30),
            opaque(0.40, 0.10, 0.05),
            0,
            r,
        );
        scene.push_stroke_rounded(
            r[0],
            r[1],
            r[2],
            r[3],
            opaque(1.0, 0.85, 0.55),
            2.0,
            [RADIUS; 4],
        );
        if let Some(s) = shaper.as_mut() {
            s.push_label(
                scene,
                "Radial + shadow",
                r[0] + 14.0,
                r[3] - 32.0,
                LABEL_SIZE,
                LABEL_COLOR,
            );
        }
    }

    // Card 6 — painter-order receipt. Image fills the card, a small
    // "badge" rect overlays the bottom-right corner. The badge is
    // pushed AFTER the image; in op-list painter order that means it
    // paints on top — visible in the rendered output.
    //
    // (Pre-refactor, this card was the regression evidence: the
    // fixed type-Vec painter order put rects under images, and the
    // badge was invisible despite being pushed last in consumer
    // code. Now the rendered result tracks the consumer's intent.)
    {
        let r = card_rect(2, 1);
        scene.push_rect_clipped_rounded(
            r[0],
            r[1],
            r[2],
            r[3],
            opaque(0.05, 0.05, 0.06),
            0,
            r,
            [RADIUS; 4],
        );
        scene.push_image_full_rounded(
            r[0],
            r[1],
            r[2],
            r[3],
            [0.0, 0.0, 1.0, 1.0],
            opaque(0.85, 0.85, 0.85),
            image_key,
            0,
            r,
            [RADIUS; 4],
        );
        // Magenta badge in the bottom-right 64×32 of the card,
        // pushed after the image — paints over it.
        let badge = [r[2] - 80.0, r[3] - 48.0, r[2] - 16.0, r[3] - 16.0];
        scene.push_rect_clipped_rounded(
            badge[0],
            badge[1],
            badge[2],
            badge[3],
            rgba(0.95, 0.20, 0.55, 1.0),
            0,
            r, // clip to card so the badge can't escape rounded corners
            [6.0, 6.0, 6.0, 6.0],
        );
        if let Some(s) = shaper.as_mut() {
            s.push_label(
                scene,
                "Z-order probe",
                r[0] + 14.0,
                r[3] - 32.0,
                LABEL_SIZE,
                LABEL_COLOR,
            );
        }
    }
}

fn output_path() -> PathBuf {
    if let Ok(custom) = std::env::var("NETRENDER_DEMO_OUT") {
        return PathBuf::from(custom);
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("output")
        .join("demo_card_grid.png")
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    let file = std::fs::File::create(path)
        .unwrap_or_else(|e| panic!("creating {}: {}", path.display(), e));
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().expect("png header");
    writer.write_image_data(rgba).expect("png pixels");
}

fn main() {
    let handles = boot().expect("wgpu boot");
    let renderer = create_netrender_instance(
        handles.clone(),
        NetrenderOptions {
            tile_cache_size: Some(TILE),
            enable_vello: true,
            enable_tile_dirty_overlay: false,
            tile_dirty_overlay_window_frames: 0,
            backends: None,
        },
    )
    .expect("create_netrender_instance");

    // Scene assets — image source and (best-effort) font.
    const IMAGE_KEY: u64 = 0x00DE_C0DE_CA4D;
    const SHADOW_KEY: u64 = 0x000D_1205_DAD0;

    // Drop shadow under card 5 — built via the render-graph helper.
    // The output texture is registered with the rasterizer under
    // SHADOW_KEY and composited as an ordinary image primitive.
    let r5 = card_rect(1, 1);
    let shadow_dim = VH; // single mask covering the whole scene
                         // 12px CSS-style soft shadow. Internally cascades through several
                         // 5-tap binomial passes — see Renderer::build_box_shadow_mask
                         // and blur_kernel_plan.
    renderer.build_box_shadow_mask(
        SHADOW_KEY,
        shadow_dim,
        [r5[0], r5[1], r5[2], r5[3]],
        RADIUS,
        12.0,
        false,
    );

    // Scene build: image_sources is on the Scene, the shadow is a
    // GPU-only Path B image registered above and threaded into
    // build_cards via ShadowDef so Card 5 can place it ahead of its
    // own gradient (correct under-card painter order).
    let mut shaper = TextShaper::try_setup();
    let mut scene = Scene::new(VW, VH);
    scene.set_image_source(IMAGE_KEY, checker_image());

    let shadow = ShadowDef {
        image_key: SHADOW_KEY,
        mask_dim: shadow_dim,
        // Inflate a touch + offset down-right so the halo peeks
        // out below and to the right of the card.
        target_rect: [r5[0] - 6.0, r5[1] + 4.0, r5[2] + 6.0, r5[3] + 14.0],
        color: rgba(0.0, 0.0, 0.0, 0.55),
    };
    build_cards(&mut scene, &mut shaper, IMAGE_KEY, &shadow);

    // Render to a transient target, read back, write PNG.
    let target = handles.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("demo_card_grid target"),
        size: wgpu::Extent3d {
            width: VW,
            height: VH,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor {
        label: Some("demo_card_grid view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });

    renderer.render_vello(&scene, &view, ColorLoad::Clear(wgpu::Color::BLACK));
    let bytes = renderer.wgpu_device.read_rgba8_texture(&target, VW, VH);

    let out = output_path();
    write_png(&out, VW, VH, &bytes);
    println!("demo_card_grid: wrote {}", out.display());
}
