//! Roadmap C3 — alpha-mask layer (DestIn) receipts.
//!
//! Verifies:
//!
//! 1. `SceneCompose::SrcOver` is the default; existing `SceneLayer`
//!    construction sites stay at SrcOver.
//! 2. `SceneLayer::alpha_mask()` produces a layer with
//!    `compose: SceneCompose::DestIn`.
//! 3. `Scene::push_alpha_mask_layer` appends a PushLayer with the
//!    DestIn compose set.
//! 4. The tile-cache hash distinguishes between SrcOver and DestIn
//!    layers (changing compose invalidates).
//! 5. **GPU smoke**: a content rect masked by an image with a sharp
//!    half-and-half alpha pattern produces visible content only on
//!    the opaque side of the mask — proves the DestIn pipeline
//!    actually executes through to vello.

use netrender::scene::{
    Scene, SceneClip, SceneCompose, SceneLayer, SceneOp,
};
use netrender::tile_cache::TileCache;

const TILE: u32 = 32;

#[test]
fn pc3_default_compose_is_srcover() {
    assert_eq!(SceneCompose::default(), SceneCompose::SrcOver);
    let layer = SceneLayer::clip(SceneClip::None);
    assert_eq!(layer.compose, SceneCompose::SrcOver);
    let layer = SceneLayer::alpha(0.5);
    assert_eq!(layer.compose, SceneCompose::SrcOver);
}

#[test]
fn pc3_alpha_mask_helper_uses_destin_compose() {
    let layer = SceneLayer::alpha_mask();
    assert_eq!(layer.compose, SceneCompose::DestIn);
}

#[test]
fn pc3_push_alpha_mask_layer_appends_destin_layer() {
    let mut scene = Scene::new(64, 64);
    scene.push_alpha_mask_layer();
    match scene.ops.last().unwrap() {
        SceneOp::PushLayer(layer) => {
            assert_eq!(layer.compose, SceneCompose::DestIn);
        }
        other => panic!("expected PushLayer, got {other:?}"),
    }
}

#[test]
fn pc3_compose_change_invalidates_tile() {
    let mut scene = Scene::new(64, 64);
    scene.push_layer_clip(SceneClip::Rect {
        rect: [0.0, 0.0, 32.0, 32.0],
        radii: [0.0; 4],
    });
    scene.push_rect(0.0, 0.0, 32.0, 32.0, [1.0, 0.0, 0.0, 1.0]);
    scene.ops.push(SceneOp::PopLayer);

    let mut cache = TileCache::new(TILE);
    let _ = cache.invalidate(&scene);
    let _ = cache.invalidate(&scene);

    if let SceneOp::PushLayer(l) = &mut scene.ops[0] {
        l.compose = SceneCompose::DestIn;
    }
    let dirty = cache.invalidate(&scene);
    assert!(!dirty.is_empty(), "compose change invalidates: {}", dirty.len());
}

// ── GPU smoke ─────────────────────────────────────────────────────────

mod gpu_smoke {
    use std::sync::Arc;

    use netrender::peniko::Blob;
    use netrender::{
        boot, create_netrender_instance, ColorLoad, ImageData as NetImageData, NetrenderOptions,
        Scene,
    };
    use netrender::scene::{SceneClip, SceneOp};

    const DIM: u32 = 128;
    const TILE_SIZE: u32 = 64;

    fn make_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pc3 target"),
            size: wgpu::Extent3d { width: DIM, height: DIM, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8Unorm),
            ..Default::default()
        });
        (texture, view)
    }

    /// Build an 8×8 RGBA mask: left 4 columns opaque white, right 4
    /// columns transparent. Used as the alpha mask for the test —
    /// content should survive on the left half and disappear on the
    /// right.
    fn half_mask_image() -> NetImageData {
        let mut bytes = Vec::with_capacity(8 * 8 * 4);
        for _ in 0..8 {
            for x in 0..8 {
                if x < 4 {
                    bytes.extend_from_slice(&[255, 255, 255, 255]);
                } else {
                    bytes.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        NetImageData::from_blob(8, 8, Blob::new(Arc::new(bytes)))
    }

    #[test]
    fn pc3_alpha_mask_keeps_left_drops_right() {
        let handles = boot().expect("wgpu boot");
        let renderer = create_netrender_instance(
            handles.clone(),
            NetrenderOptions {
                tile_cache_size: Some(TILE_SIZE),
                enable_vello: true,
                ..Default::default()
            },
        )
        .expect("create_netrender_instance");

        let mut scene = Scene::new(DIM, DIM);
        scene.image_sources.insert(42, half_mask_image());

        // Outer clip layer covering the whole canvas.
        scene.push_layer_clip(SceneClip::Rect {
            rect: [0.0, 0.0, DIM as f32, DIM as f32],
            radii: [0.0; 4],
        });
        // Content: solid red rect.
        scene.push_rect(0.0, 0.0, DIM as f32, DIM as f32, [1.0, 0.0, 0.0, 1.0]);
        // Inner alpha-mask layer. The mask image is 8×8 stretched
        // across the full canvas; left half opaque, right half
        // transparent.
        scene.push_alpha_mask_layer();
        scene.push_image_full(
            0.0, 0.0, DIM as f32, DIM as f32,
            [0.0, 0.0, 1.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            42,
            0,
            netrender::NO_CLIP,
        );
        scene.ops.push(SceneOp::PopLayer); // close inner DestIn → mask applied
        scene.ops.push(SceneOp::PopLayer); // close outer

        let (target, view) = make_target(&handles.device);
        renderer.render_vello(&scene, &view, ColorLoad::Clear(wgpu::Color::BLACK));
        let bytes = renderer.wgpu_device.read_rgba8_texture(&target, DIM, DIM);

        // Sample one pixel from the left half and one from the
        // right half (well away from the mid-canvas seam to avoid
        // bilinear-bleed of the 8x8 mask).
        let pixel_at = |x: u32, y: u32| {
            let i = ((y * DIM + x) * 4) as usize;
            [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
        };
        let left = pixel_at(DIM / 8, DIM / 2);
        let right = pixel_at(DIM - DIM / 8, DIM / 2);

        eprintln!("pc3: left pixel = {:?}, right pixel = {:?}", left, right);

        // Left half should be substantially red (content survived).
        assert!(
            left[0] > 200,
            "left-half red channel should be high (content visible); got {left:?}"
        );
        // Right half should be near-black (content masked out).
        assert!(
            right[0] < 32,
            "right-half should be masked out (near-black); got {right:?}"
        );
    }
}
