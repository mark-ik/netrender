/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 13' path (b′) receipts — sub-phases 5.1, 5.2, 5.3.
//!
//! Verifies the path-(b′) pipeline end-to-end:
//!
//! - 5.1 plumbing — `Renderer::render_with_compositor` runs through
//!   to `Compositor::present_frame`; master-texture pool allocates
//!   once and reuses across frames.
//! - 5.2 dirty tracking — `LayerPresent.dirty` is the OR of
//!   tile-intersection / newly-declared / bounds-changed; declare/
//!   destroy lifecycle events forwarded.
//! - 5.3 master handoff — z-order preserved through `frame.layers`;
//!   consumer can encode + submit `copy_texture_to_texture` from
//!   `frame.master` using `frame.handles`, blits only on dirty.

use netrender::{
    Compositor, CompositorSurface, LayerPresent, NetrenderOptions, PresentedFrame, Scene,
    SurfaceKey, boot, create_netrender_instance,
};

const TILE_SIZE: u32 = 64;

#[derive(Default)]
struct RecordingCompositor {
    declares: Vec<(SurfaceKey, [f32; 4])>,
    destroys: Vec<SurfaceKey>,
    /// One entry per `present_frame` call. Each entry records the
    /// `(width, height, format)` of the master texture and the
    /// number of `LayerPresent` entries.
    presents: Vec<(u32, u32, wgpu::TextureFormat, usize)>,
    /// One entry per `present_frame` call: the cloned `LayerPresent`
    /// list for inspection.
    layer_records: Vec<Vec<RecordedLayer>>,
    /// Optional GPU-blit hook. When set, `present_frame` encodes
    /// `copy_texture_to_texture` for each dirty layer into a
    /// destination texture sized to the layer's source rect.
    /// Increments `blit_count` once per actual blit. Stays `None`
    /// for tests that don't need to exercise the GPU copy path.
    do_gpu_blits: bool,
    blit_count: usize,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct RecordedLayer {
    key: SurfaceKey,
    source_rect: [u32; 4],
    world_transform: [f32; 6],
    clip: Option<[f32; 4]>,
    opacity: f32,
    dirty: bool,
}

impl Compositor for RecordingCompositor {
    fn declare_surface(&mut self, key: SurfaceKey, world_bounds: [f32; 4]) {
        self.declares.push((key, world_bounds));
    }

    fn destroy_surface(&mut self, key: SurfaceKey) {
        self.destroys.push(key);
    }

    fn present_frame(&mut self, frame: PresentedFrame<'_>) {
        let size = frame.master.size();
        self.presents.push((
            size.width,
            size.height,
            frame.master.format(),
            frame.layers.len(),
        ));
        let layers: Vec<RecordedLayer> = frame
            .layers
            .iter()
            .map(|l: &LayerPresent| RecordedLayer {
                key: l.key,
                source_rect: l.source_rect_in_master,
                world_transform: l.world_transform,
                clip: l.clip,
                opacity: l.opacity,
                dirty: l.dirty,
            })
            .collect();
        self.layer_records.push(layers);

        if self.do_gpu_blits {
            for layer in frame.layers {
                if !layer.dirty {
                    continue;
                }
                let [x0, y0, x1, y1] = layer.source_rect_in_master;
                let (w, h) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
                if w == 0 || h == 0 {
                    continue;
                }
                let dest = frame
                    .handles
                    .device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("p13' blit dest"),
                        size: wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: frame.master.format(),
                        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    });
                let mut enc =
                    frame
                        .handles
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("p13' blit encoder"),
                        });
                enc.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: frame.master,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x: x0, y: y0, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &dest,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                );
                frame.handles.queue.submit([enc.finish()]);
                self.blit_count += 1;
            }
        }
    }
}

fn make_renderer() -> netrender::Renderer {
    let handles = boot().expect("wgpu boot");
    create_netrender_instance(
        handles,
        NetrenderOptions {
            tile_cache_size: Some(TILE_SIZE),
            enable_vello: true,
            ..Default::default()
        },
    )
    .expect("create_netrender_instance")
}

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn base() -> vello::peniko::Color {
    vello::peniko::Color::new([0.0, 0.0, 0.0, 0.0])
}

// ── 5.1 — plumbing ────────────────────────────────────────────

/// 5.1 done condition — master-texture pool allocates exactly once
/// across two consecutive `render_with_compositor` calls at the
/// same viewport / format.
#[test]
fn p13prime_path_b_master_pool_reuses_across_frames() {
    let renderer = make_renderer();
    let mut compositor = RecordingCompositor::default();

    let dim = 256u32;
    let mut scene = Scene::new(dim, dim);
    scene.push_rect(0.0, 0.0, dim as f32, dim as f32, [1.0, 0.0, 0.0, 1.0]);

    renderer.render_with_compositor(&scene, FORMAT, &mut compositor, base());
    renderer.render_with_compositor(&scene, FORMAT, &mut compositor, base());

    assert_eq!(
        renderer.vello_master_allocations(),
        Some(1),
        "master pool should allocate once and reuse across the second frame",
    );
    assert_eq!(compositor.presents.len(), 2);
    for (i, p) in compositor.presents.iter().enumerate() {
        assert_eq!(p.0, dim, "frame {i} master width");
        assert_eq!(p.1, dim, "frame {i} master height");
        assert_eq!(p.2, FORMAT, "frame {i} master format");
        assert_eq!(p.3, 0, "frame {i} no surfaces declared → empty layers");
    }
}

/// Master pool reallocates when the viewport size changes.
#[test]
fn p13prime_path_b_master_pool_reallocates_on_resize() {
    let renderer = make_renderer();
    let mut compositor = RecordingCompositor::default();

    let mut scene = Scene::new(128, 128);
    scene.push_rect(0.0, 0.0, 128.0, 128.0, [1.0, 0.0, 0.0, 1.0]);
    renderer.render_with_compositor(&scene, FORMAT, &mut compositor, base());

    let mut scene2 = Scene::new(256, 256);
    scene2.push_rect(0.0, 0.0, 256.0, 256.0, [0.0, 1.0, 0.0, 1.0]);
    renderer.render_with_compositor(&scene2, FORMAT, &mut compositor, base());

    assert_eq!(
        renderer.vello_master_allocations(),
        Some(2),
        "viewport resize should force a fresh allocation",
    );
}

// Note: a "format change reallocates" test would exercise the
// pool's third realloc trigger, but vello's GPU compute path
// requires `STORAGE_BINDING` usage on the target, and BGRA8 storage
// requires the `BGRA8_UNORM_STORAGE` wgpu feature which isn't in
// netrender's REQUIRED_FEATURES today. The pool's format-mismatch
// branch is exercised indirectly via the resize test (which changes
// dims) — the realloc decision OR's both. Native BGRA destination
// paths are a 5.3+ concern; see design doc §8(1).

// ── 5.2 — dirty tracking + lifecycle ─────────────────────────

/// 5.2 — second frame with unchanged scene reports all declared
/// surfaces as `dirty: false`. First frame reports `dirty: true`
/// (newly-declared / absent-last-frame).
#[test]
fn p13prime_path_b_dirty_clean_after_unchanged() {
    let renderer = make_renderer();
    let mut compositor = RecordingCompositor::default();

    let dim = 128u32;
    let mut scene = Scene::new(dim, dim);
    scene.push_rect(0.0, 0.0, dim as f32, dim as f32, [1.0, 0.0, 0.0, 1.0]);
    scene.declare_compositor_surface(CompositorSurface::new(
        SurfaceKey(1),
        [0.0, 0.0, 64.0, 64.0],
    ));
    scene.declare_compositor_surface(CompositorSurface::new(
        SurfaceKey(2),
        [64.0, 64.0, 128.0, 128.0],
    ));

    renderer.render_with_compositor(&scene, FORMAT, &mut compositor, base());
    renderer.render_with_compositor(&scene, FORMAT, &mut compositor, base());

    assert_eq!(compositor.layer_records.len(), 2);
    let frame1 = &compositor.layer_records[0];
    let frame2 = &compositor.layer_records[1];
    assert_eq!(frame1.len(), 2);
    assert_eq!(frame2.len(), 2);

    assert!(
        frame1.iter().all(|l| l.dirty),
        "frame 1: all surfaces dirty (newly-declared / absent-last-frame)",
    );
    assert!(
        frame2.iter().all(|l| !l.dirty),
        "frame 2: scene unchanged → all surfaces clean. dirty bits: {:?}",
        frame2.iter().map(|l| l.dirty).collect::<Vec<_>>(),
    );
}

/// 5.2 — bounds change with unchanged painted content reports
/// `dirty: true` even when no tile content changed.
#[test]
fn p13prime_path_b_dirty_on_bounds_change() {
    let renderer = make_renderer();
    let mut compositor = RecordingCompositor::default();

    let dim = 128u32;
    let mut scene = Scene::new(dim, dim);
    scene.push_rect(0.0, 0.0, dim as f32, dim as f32, [1.0, 0.0, 0.0, 1.0]);
    scene.declare_compositor_surface(CompositorSurface::new(
        SurfaceKey(1),
        [0.0, 0.0, 64.0, 64.0],
    ));

    renderer.render_with_compositor(&scene, FORMAT, &mut compositor, base());

    // Frame 2: re-declare with different bounds; no content changes.
    scene.declare_compositor_surface(CompositorSurface::new(
        SurfaceKey(1),
        [16.0, 16.0, 80.0, 80.0],
    ));
    renderer.render_with_compositor(&scene, FORMAT, &mut compositor, base());

    let frame2 = &compositor.layer_records[1];
    assert_eq!(frame2.len(), 1);
    assert!(
        frame2[0].dirty,
        "bounds-changed surface must report dirty even with no tile content change",
    );
    assert_eq!(
        frame2[0].source_rect,
        [16, 16, 80, 80],
        "source_rect_in_master reflects updated bounds",
    );
}

/// 5.2 — surface present last frame but absent this frame triggers
/// a `destroy_surface` event on the consumer.
#[test]
fn p13prime_path_b_destroy_forwarded_on_undeclare() {
    let renderer = make_renderer();
    let mut compositor = RecordingCompositor::default();

    let dim = 128u32;
    let mut scene = Scene::new(dim, dim);
    scene.push_rect(0.0, 0.0, dim as f32, dim as f32, [1.0, 0.0, 0.0, 1.0]);
    scene.declare_compositor_surface(CompositorSurface::new(
        SurfaceKey(1),
        [0.0, 0.0, 64.0, 64.0],
    ));
    scene.declare_compositor_surface(CompositorSurface::new(
        SurfaceKey(2),
        [64.0, 0.0, 128.0, 64.0],
    ));

    renderer.render_with_compositor(&scene, FORMAT, &mut compositor, base());
    let declares_after_frame1 = compositor.declares.len();

    scene.undeclare_compositor_surface(SurfaceKey(2));
    renderer.render_with_compositor(&scene, FORMAT, &mut compositor, base());

    assert!(
        compositor.destroys.contains(&SurfaceKey(2)),
        "destroy_surface(2) should fire after undeclare; got {:?}",
        compositor.destroys,
    );
    assert!(
        !compositor.destroys.contains(&SurfaceKey(1)),
        "still-declared surface 1 must not be destroyed",
    );
    // frame 2's layers slice has just surface 1 now.
    let frame2 = &compositor.layer_records[1];
    assert_eq!(frame2.len(), 1);
    assert_eq!(frame2[0].key, SurfaceKey(1));

    // Sanity: declares only fire for new/changed in frame 2 (none here).
    assert_eq!(
        compositor.declares.len(),
        declares_after_frame1,
        "no new declares on frame 2 (no new keys, no bounds changes)",
    );
}

// ── 5.3 — master handoff (z-order + consumer-side blit) ──────

/// 5.3 — three overlapping surfaces declared in known order; the
/// `LayerPresent` slice arrives in declaration order.
#[test]
fn p13prime_path_b_zorder_preserved() {
    let renderer = make_renderer();
    let mut compositor = RecordingCompositor::default();

    let dim = 128u32;
    let mut scene = Scene::new(dim, dim);
    scene.push_rect(0.0, 0.0, dim as f32, dim as f32, [1.0, 0.0, 0.0, 1.0]);
    // All three overlap on [16..80, 16..80].
    scene.declare_compositor_surface(CompositorSurface::new(
        SurfaceKey(10),
        [0.0, 0.0, 80.0, 80.0],
    ));
    scene.declare_compositor_surface(CompositorSurface::new(
        SurfaceKey(20),
        [16.0, 16.0, 96.0, 96.0],
    ));
    scene.declare_compositor_surface(CompositorSurface::new(
        SurfaceKey(30),
        [32.0, 32.0, 112.0, 112.0],
    ));

    renderer.render_with_compositor(&scene, FORMAT, &mut compositor, base());

    let frame1 = &compositor.layer_records[0];
    assert_eq!(frame1.len(), 3);
    assert_eq!(
        frame1[0].key,
        SurfaceKey(10),
        "z=0 (bottom): declared first"
    );
    assert_eq!(frame1[1].key, SurfaceKey(20), "z=1: declared second");
    assert_eq!(frame1[2].key, SurfaceKey(30), "z=2 (top): declared third");
}

/// 5.3 — consumer encodes + submits `copy_texture_to_texture` for
/// dirty surfaces only. Frame 1 has both surfaces newly-declared
/// (both dirty → 2 blits). Frame 2 changes content only inside
/// surface A's bounds; surface A stays dirty via tile-intersection,
/// surface B goes clean (1 blit total).
#[test]
fn p13prime_path_b_blit_dirty_only() {
    let renderer = make_renderer();
    let mut compositor = RecordingCompositor::default();
    compositor.do_gpu_blits = true;

    let dim = 128u32;
    let mut scene = Scene::new(dim, dim);
    scene.push_rect(0.0, 0.0, 64.0, 64.0, [1.0, 0.0, 0.0, 1.0]);
    // Surface A covers tile (0,0); surface B covers tile (1,1).
    // TILE_SIZE = 64, viewport = 128 → 2×2 tile grid.
    scene.declare_compositor_surface(CompositorSurface::new(
        SurfaceKey(101),
        [0.0, 0.0, 64.0, 64.0],
    ));
    scene.declare_compositor_surface(CompositorSurface::new(
        SurfaceKey(202),
        [64.0, 64.0, 128.0, 128.0],
    ));

    renderer.render_with_compositor(&scene, FORMAT, &mut compositor, base());
    assert_eq!(
        compositor.blit_count, 2,
        "frame 1: both surfaces newly-declared → 2 blits",
    );

    // Frame 2: change the rect's color (still in tile (0,0)).
    scene.clear_ops();
    scene.push_rect(0.0, 0.0, 64.0, 64.0, [0.0, 1.0, 0.0, 1.0]);
    renderer.render_with_compositor(&scene, FORMAT, &mut compositor, base());

    // Frame 2 dirty bits:
    //   surface 101: bounds [0,0,64,64] intersects dirty tile (0,0) → dirty
    //   surface 202: bounds [64,64,128,128] intersects only clean tiles → clean
    let frame2 = &compositor.layer_records[1];
    assert_eq!(frame2.len(), 2);
    let a = frame2.iter().find(|l| l.key == SurfaceKey(101)).unwrap();
    let b = frame2.iter().find(|l| l.key == SurfaceKey(202)).unwrap();
    assert!(a.dirty, "surface in dirty-tile region must be dirty");
    assert!(!b.dirty, "surface in clean-tile region must be clean");

    assert_eq!(
        compositor.blit_count, 3,
        "cumulative blits: 2 (frame 1) + 1 (frame 2 dirty surface A only)",
    );
}

// ── 5.4 — transform / clip / opacity setters ─────────────────

/// 5.4 — `set_surface_transform`, `set_surface_clip`,
/// `set_surface_opacity` between frames update `LayerPresent`
/// metadata but do **not** flip `dirty: true`. Bounds untouched →
/// source_rect_in_master stays stable across the change.
#[test]
fn p13prime_path_b_transform_only_clean() {
    let renderer = make_renderer();
    let mut compositor = RecordingCompositor::default();

    let dim = 128u32;
    let mut scene = Scene::new(dim, dim);
    scene.push_rect(0.0, 0.0, dim as f32, dim as f32, [1.0, 0.0, 0.0, 1.0]);
    scene.declare_compositor_surface(CompositorSurface::new(
        SurfaceKey(7),
        [0.0, 0.0, 64.0, 64.0],
    ));

    renderer.render_with_compositor(&scene, FORMAT, &mut compositor, base());

    // Frame 2: change all three OS-side metadata fields. No bounds
    // change, no scene content change.
    let rotated_45deg: [f32; 6] = {
        let (s, c) = (45.0_f32.to_radians()).sin_cos();
        [c, s, -s, c, 32.0, 32.0]
    };
    scene.set_surface_transform(SurfaceKey(7), rotated_45deg);
    scene.set_surface_clip(SurfaceKey(7), Some([8.0, 8.0, 56.0, 56.0]));
    scene.set_surface_opacity(SurfaceKey(7), 0.5);

    renderer.render_with_compositor(&scene, FORMAT, &mut compositor, base());

    let frame1 = &compositor.layer_records[0];
    let frame2 = &compositor.layer_records[1];
    assert_eq!(frame2.len(), 1);

    // All three OS-side metadata fields reach LayerPresent.
    assert_eq!(
        frame2[0].world_transform, rotated_45deg,
        "transform setter must flow through to LayerPresent.world_transform",
    );
    assert_eq!(frame2[0].clip, Some([8.0, 8.0, 56.0, 56.0]));
    assert_eq!(frame2[0].opacity, 0.5);

    // Core assertion: dirty stays false despite three metadata mutations.
    assert!(
        !frame2[0].dirty,
        "transform/clip/opacity mutations are OS-side metadata; \
         must not force a content repaint flag",
    );
    // Source rect tracks bounds, not transform — should be unchanged.
    assert_eq!(
        frame1[0].source_rect, frame2[0].source_rect,
        "source_rect_in_master tracks bounds, unchanged here",
    );
    // Frame 1 had identity transform.
    assert_eq!(
        frame1[0].world_transform,
        CompositorSurface::IDENTITY_TRANSFORM,
        "frame 1 default transform is identity",
    );
}

/// Regression: `LayerPresent.world_transform` composes the surface's
/// `bounds.origin` (top-left) into the user-supplied
/// `CompositorSurface.transform`, so a consumer that holds a
/// declared surface at layer-local origin (0, 0) — like macOS
/// CALayer.contents = IOSurface — gets the correct world-space
/// position via one composed transform without separately
/// remembering bounds.origin from declare.
///
/// Pre-fix, netrender passed `s.transform` through unchanged and a
/// surface declared with bounds `[16, 16, 80, 80]` + identity
/// transform arrived at the consumer as identity world_transform,
/// stacking visually at the parent's origin instead of (16, 16).
#[test]
fn p13prime_path_b_world_transform_composes_bounds_origin() {
    let renderer = make_renderer();
    let mut compositor = RecordingCompositor::default();

    let dim = 128u32;
    let mut scene = Scene::new(dim, dim);
    scene.push_rect(0.0, 0.0, dim as f32, dim as f32, [1.0, 0.0, 0.0, 1.0]);

    // Surface 1: bounds at origin (0, 0). Composed transform
    // should equal the user's transform unchanged.
    scene.declare_compositor_surface(CompositorSurface::new(
        SurfaceKey(1),
        [0.0, 0.0, 32.0, 32.0],
    ));
    // Surface 2: bounds at (16, 24). Identity user-transform.
    // Composed should be `[1, 0, 0, 1, 16, 24]`.
    scene.declare_compositor_surface(CompositorSurface::new(
        SurfaceKey(2),
        [16.0, 24.0, 80.0, 88.0],
    ));
    // Surface 3: bounds at (40, 40), with a non-trivial user
    // transform that already has its own translation (10, 5).
    // Composed should add origin to the existing translation
    // column: `[a, b, c, d, 10 + 40, 5 + 40]` = `[..., 50, 45]`,
    // linear part unchanged.
    let scale_2x_with_offset: [f32; 6] = [2.0, 0.0, 0.0, 2.0, 10.0, 5.0];
    let mut s3 = CompositorSurface::new(SurfaceKey(3), [40.0, 40.0, 100.0, 100.0]);
    s3.transform = scale_2x_with_offset;
    scene.declare_compositor_surface(s3);

    renderer.render_with_compositor(&scene, FORMAT, &mut compositor, base());

    let frame = &compositor.layer_records[0];
    assert_eq!(frame.len(), 3);

    assert_eq!(
        frame[0].world_transform,
        CompositorSurface::IDENTITY_TRANSFORM,
        "bounds.origin (0, 0) + identity transform = identity world_transform",
    );
    assert_eq!(
        frame[1].world_transform,
        [1.0, 0.0, 0.0, 1.0, 16.0, 24.0],
        "bounds.origin (16, 24) + identity transform = translate(16, 24)",
    );
    assert_eq!(
        frame[2].world_transform,
        [2.0, 0.0, 0.0, 2.0, 50.0, 45.0],
        "bounds.origin (40, 40) + scale-2x-translate(10, 5) = scale-2x-translate(50, 45); \
         linear part unchanged",
    );
}
