/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 10b.3 receipt — transform-aware subpixel-AA policy.
//!
//! 10a.4 introduced a global `text_subpixel_aa` opt-in: when true,
//! every text run rendered through the dual-source pipeline. 10b.3
//! refines that into a per-run decision based on the run's
//! transform: pure 2D translation routes to the dual-source
//! pipeline (subpixel coverage is meaningful), rotation / scale /
//! skew falls back to grayscale (LCD subpixel layout would smear
//! across the framebuffer's RGB stripe order).
//!
//! Receipts:
//!
//! 1. **`p10b3_translated_subpixel_run_uses_dual_source`** —
//!    A run with an identity transform and a `Subpixel`-format
//!    glyph still renders with per-channel R/G/B differences
//!    (the dual-source signature). Confirms the policy doesn't
//!    accidentally downgrade well-aligned runs to grayscale.
//!
//! 2. **`p10b3_rotated_run_falls_back_to_grayscale`** —
//!    Same `Subpixel`-format glyph, same `text_subpixel_aa = true`
//!    consumer opt-in, but the run carries a 90°-rotation
//!    transform. The rendered output should have R == G == B at
//!    every glyph pixel — the grayscale path's signature.
//!    Distinguishes the policy from the global toggle.
//!
//! 3. **`p10b3_translated_only_run_classifier_unit`** — exercises
//!    `Transform::is_pure_translation_2d` on identity, translation,
//!    rotation, scale, and a translation-then-scale composition,
//!    so a future change to the classifier can't silently shift
//!    routing.

use netrender::scene::Transform;
use netrender::{
    ColorLoad, FrameTarget, GlyphFormat, GlyphInstance, GlyphKey, GlyphRaster, NetrenderOptions,
    Scene, boot, create_netrender_instance,
};

const VIEWPORT: u32 = 64;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

fn dual_source_supported() -> bool {
    let handles = boot().expect("wgpu boot");
    handles.device.features().contains(wgpu::Features::DUAL_SOURCE_BLENDING)
}

/// Hand-authored 4×1 `Subpixel` raster with deliberately distinct
/// per-pixel R/G/B coverage — same fixture shape as the 10b.1
/// receipt, kept inline so this test file is self-contained.
fn synthetic_subpixel_raster_4x1() -> GlyphRaster {
    GlyphRaster {
        width: 4,
        height: 1,
        bearing_x: 0,
        bearing_y: 1,
        format: GlyphFormat::Subpixel,
        pixels: vec![
            255,   0,   0,
              0, 255,   0,
              0,   0, 255,
            255, 128,  64,
        ],
    }
}

const KEY_SUBPIXEL: GlyphKey = GlyphKey { font_id: 200, glyph_id: 0xB3, size_x64: 1 * 64 };

fn render_with_transforms(
    scene: &Scene,
) -> Vec<u8> {
    let [vw, vh] = [scene.viewport_width, scene.viewport_height];
    let handles = boot().expect("wgpu boot");
    let device = handles.device.clone();
    let renderer = create_netrender_instance(
        handles,
        NetrenderOptions {
            text_subpixel_aa: true,
            ..NetrenderOptions::default()
        },
    )
    .expect("create_netrender_instance");

    let target_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p10b3 target"),
        size: wgpu::Extent3d { width: vw, height: vh, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target_tex.create_view(&wgpu::TextureViewDescriptor::default());

    let prepared = renderer.prepare(scene);
    renderer.render(
        &prepared,
        FrameTarget { view: &target_view, format: TARGET_FORMAT, width: vw, height: vh },
        // Opaque background so the dual-source blend produces clean
        // per-channel output (subpixel blending requires opaque dest).
        ColorLoad::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }),
    );
    renderer.wgpu_device.read_rgba8_texture(&target_tex, vw, vh)
}

/// Receipt 1: a run with identity transform (which `is_pure_translation_2d`
/// classifies as translation-only) routes through the dual-source
/// pipeline and produces the per-channel coverage signature: at
/// least one rendered pixel has `R != G != B`.
#[test]
fn p10b3_translated_subpixel_run_uses_dual_source() {
    if !dual_source_supported() {
        println!(
            "  skipping: adapter lacks DUAL_SOURCE_BLENDING — \
             every run goes through the grayscale fallback regardless of policy",
        );
        return;
    }

    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    scene.set_glyph_raster(KEY_SUBPIXEL, synthetic_subpixel_raster_4x1());
    // transform_id = 0 = identity (pure translation by zero is still
    // pure translation, so the policy chooses dual-source).
    scene.push_text_run(
        vec![GlyphInstance { key: KEY_SUBPIXEL, x: 10.0, y: 20.0 }],
        [1.0, 1.0, 1.0, 1.0],
    );

    let pixels = render_with_transforms(&scene);
    let stride = (VIEWPORT * 4) as usize;
    let pixel = |x: u32, y: u32| -> [u8; 4] {
        let i = (y as usize) * stride + (x as usize) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };

    // Source pixel 0 at device col 10 → red-only.
    let p0 = pixel(10, 19);
    assert!(
        p0[0] > 200 && p0[1] == 0 && p0[2] == 0,
        "translated run should produce per-channel coverage; got {:?}", p0,
    );
}

/// Receipt 2: a run with a 90°-rotation transform falls back to
/// grayscale even though `text_subpixel_aa = true`. The rendered
/// output of every glyph pixel must satisfy `R == G == B` (the
/// grayscale signature) — the dual-source path can't produce that
/// asymmetry-free output for a `Subpixel`-format raster, so this
/// pinpoints the routing decision.
///
/// The 90° rotation moves the 4×1 horizontal strip to a 1×4 vertical
/// strip in device space, but we only need to assert "all rendered
/// glyph pixels have R == G == B", regardless of where they land —
/// the grayscale path samples `.r` of the atlas (which carries the
/// red-channel coverage byte for `Subpixel` glyphs) and broadcasts
/// it as scalar coverage. R, G, B come out equal by construction.
#[test]
fn p10b3_rotated_run_falls_back_to_grayscale() {
    let mut scene = Scene::new(VIEWPORT, VIEWPORT);
    scene.set_glyph_raster(KEY_SUBPIXEL, synthetic_subpixel_raster_4x1());

    // Add a 90° rotation transform after the identity.
    // Rotating around the origin moves screen quadrants; pin the
    // glyph at a pen position that — after rotation around (0, 0)
    // and a follow-up translation — places the rendered glyph
    // somewhere visible in the 64×64 viewport.
    //
    // Compose: rotate around origin, then translate to (32, 32).
    // `Transform::then` applies self first then other, so the
    // composed result rotates first then translates.
    let rotate = Transform::rotate_2d(std::f32::consts::FRAC_PI_2);
    let translate = Transform::translate_2d(32.0, 32.0);
    let composed = rotate.then(&translate);
    scene.transforms.push(composed);
    let rotated_id = (scene.transforms.len() - 1) as u32;

    // Sanity: the composed transform is NOT pure translation (rotation
    // perturbs the upper-left 3×3 block), so the policy must classify
    // it as "fall back to grayscale."
    assert!(
        !composed.is_pure_translation_2d(),
        "test setup invariant: rotate.then(translate) must not be \
         classified as pure translation",
    );

    // Push the run with the rotated transform.
    scene.texts.push(netrender::SceneText {
        glyphs: vec![GlyphInstance { key: KEY_SUBPIXEL, x: 0.0, y: 0.0 }],
        color: [1.0, 1.0, 1.0, 1.0],
        transform_id: rotated_id,
        clip_rect: netrender::NO_CLIP,
    });

    let pixels = render_with_transforms(&scene);
    let stride = (VIEWPORT * 4) as usize;

    // Walk the framebuffer and find pixels that have any non-trivial
    // glyph contribution (R or G or B above the cleared opaque-black
    // baseline). For every such pixel, R/G/B must be equal — the
    // grayscale path's invariant.
    let mut any_glyph_pixel = false;
    let mut violations: Vec<(u32, u32, [u8; 4])> = Vec::new();
    for y in 0..VIEWPORT {
        for x in 0..VIEWPORT {
            let i = (y as usize) * stride + (x as usize) * 4;
            let p = [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]];
            // The opaque clear is (0, 0, 0, 255). A grayscale glyph
            // raises R == G == B together; subpixel-divergence
            // raises them unequally. Filter for any glyph signal
            // (any channel non-zero).
            if p[0] != 0 || p[1] != 0 || p[2] != 0 {
                any_glyph_pixel = true;
                if !(p[0] == p[1] && p[1] == p[2]) {
                    violations.push((x, y, p));
                }
            }
        }
    }
    assert!(
        any_glyph_pixel,
        "rotated glyph should still render somewhere in the 64×64 \
         viewport — got an all-clear framebuffer",
    );
    assert!(
        violations.is_empty(),
        "rotated run was supposed to fall back to grayscale, but \
         {} pixels show per-channel divergence; first divergence at \
         ({}, {}) = {:?}",
        violations.len(),
        violations[0].0,
        violations[0].1,
        violations[0].2,
    );
}

/// Unit: pin the classifier's behavior on the canonical inputs.
/// A regression here would silently shift routing decisions.
#[test]
fn p10b3_translated_only_run_classifier_unit() {
    assert!(
        Transform::IDENTITY.is_pure_translation_2d(),
        "identity is the trivial pure translation",
    );
    assert!(
        Transform::translate_2d(7.5, -3.25).is_pure_translation_2d(),
        "translate_2d should classify as pure translation",
    );
    assert!(
        Transform::translate_2d(-1000.0, 1e9).is_pure_translation_2d(),
        "magnitude doesn't matter for the classifier",
    );
    assert!(
        !Transform::rotate_2d(0.1).is_pure_translation_2d(),
        "any non-zero rotation breaks pure-translation",
    );
    assert!(
        !Transform::scale_2d(2.0, 2.0).is_pure_translation_2d(),
        "uniform scale breaks pure-translation",
    );
    assert!(
        !Transform::scale_2d(1.0, 0.5).is_pure_translation_2d(),
        "non-uniform scale breaks pure-translation",
    );

    // Translation composed with rotation is not pure translation.
    let composed = Transform::translate_2d(10.0, 0.0).then(&Transform::rotate_2d(0.5));
    assert!(
        !composed.is_pure_translation_2d(),
        "translate-then-rotate composition is not pure translation",
    );

    // Translation composed with another translation IS pure translation.
    let two_translates =
        Transform::translate_2d(1.0, 2.0).then(&Transform::translate_2d(3.0, 4.0));
    assert!(
        two_translates.is_pure_translation_2d(),
        "chained translations remain pure translation",
    );
}
