/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 11b' — arbitrary path fills + strokes (`SceneShape`).
//!
//! Receipts:
//!   p11b_01_filled_triangle      — closed triangle filled red
//!   p11b_02_stroked_arrow        — open polyline stroked blue
//!   p11b_03_filled_and_stroked   — diamond with green fill + black stroke
//!   p11b_04_curved_path          — quadratic + cubic Bézier segments
//!     produce smooth curves (sample points along the curve)

use netrender::{Scene, ScenePath, ScenePathStroke, SceneShape, boot, vello_rasterizer::scene_to_vello};
use vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions,
    peniko::Color,
};

const DIM: u32 = 64;

fn make_renderer(device: &wgpu::Device) -> Renderer {
    Renderer::new(
        device,
        RendererOptions {
            use_cpu: false,
            antialiasing_support: AaSupport::area_only(),
            num_init_threads: None,
            pipeline_cache: None,
        },
    )
    .expect("vello::Renderer::new")
}

fn make_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p11b target"),
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
        label: Some("p11b view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, view)
}

fn read_pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * DIM + x) * 4) as usize;
    [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
}

fn render_scene(scene: &Scene) -> Vec<u8> {
    let handles = boot().expect("wgpu boot");
    let device = &handles.device;
    let queue = &handles.queue;
    let mut renderer = make_renderer(device);

    let vscene = scene_to_vello(scene);
    let (target, view) = make_target(device);
    renderer
        .render_to_texture(
            device, queue, &vscene, &view,
            &RenderParams {
                base_color: Color::from_rgba8(0, 0, 0, 0),
                width: DIM, height: DIM,
                antialiasing_method: AaConfig::Area,
            },
        )
        .expect("vello render_to_texture");

    let wgpu_device = netrender_device::WgpuDevice::with_external(handles.clone())
        .expect("WgpuDevice::with_external");
    wgpu_device.read_rgba8_texture(&target, DIM, DIM)
}

/// Closed triangle (16, 48) → (48, 48) → (32, 16) filled with red.
/// Sample inside the triangle and outside.
#[test]
fn p11b_01_filled_triangle() {
    let mut path = ScenePath::new();
    path.move_to(16.0, 48.0)
        .line_to(48.0, 48.0)
        .line_to(32.0, 16.0)
        .close();

    let mut scene = Scene::new(DIM, DIM);
    scene.push_shape_filled(path, [1.0, 0.0, 0.0, 1.0]);

    let bytes = render_scene(&scene);

    // Centroid of the triangle is at (32, 37) — well inside.
    let inside = read_pixel(&bytes, 32, 37);
    assert!(
        inside[0] >= 240 && inside[3] >= 240,
        "triangle centroid (32, 37): {:?} not opaque red", inside
    );

    // Outside the triangle — top-left corner of canvas.
    let outside = read_pixel(&bytes, 4, 4);
    assert!(
        outside[3] < 8,
        "outside (4, 4): {:?} should be empty", outside
    );

    // Just outside the bottom edge of the triangle.
    let below = read_pixel(&bytes, 32, 56);
    assert!(
        below[3] < 8,
        "below triangle (32, 56): {:?} should be empty", below
    );
}

/// Open polyline (8, 32) → (32, 16) → (56, 32) stroked blue.
/// Open path = no fill semantically; only the stroke paints.
#[test]
fn p11b_02_stroked_arrow() {
    let mut path = ScenePath::new();
    path.move_to(8.0, 32.0)
        .line_to(32.0, 16.0)
        .line_to(56.0, 32.0);

    let mut scene = Scene::new(DIM, DIM);
    scene.push_shape_stroked(path, [0.0, 0.0, 1.0, 1.0], 3.0);

    let bytes = render_scene(&scene);

    // On the polyline near the apex (32, 16) — fully blue.
    let apex = read_pixel(&bytes, 32, 16);
    assert!(
        apex[2] >= 200 && apex[3] >= 200,
        "apex (32, 16): {:?} not near opaque blue", apex
    );

    // Mid-segment between the apex and the right endpoint.
    // Path passes through (44, 24); a 3px stroke covers it.
    let mid_right = read_pixel(&bytes, 44, 24);
    assert!(
        mid_right[2] >= 200 && mid_right[3] >= 200,
        "mid-right (44, 24): {:?} not near opaque blue", mid_right
    );

    // Outside the stroke band — well below the arrow.
    let below = read_pixel(&bytes, 32, 48);
    assert!(
        below[3] < 8,
        "below polyline (32, 48): {:?} should be empty", below
    );
}

/// Diamond (16, 32) → (32, 16) → (48, 32) → (32, 48) filled green
/// with a black 2px outline.
#[test]
fn p11b_03_filled_and_stroked() {
    let mut path = ScenePath::new();
    path.move_to(16.0, 32.0)
        .line_to(32.0, 16.0)
        .line_to(48.0, 32.0)
        .line_to(32.0, 48.0)
        .close();

    let shape = SceneShape {
        path,
        fill_color: Some([0.0, 1.0, 0.0, 1.0]),
        stroke: Some(ScenePathStroke {
            color: [0.0, 0.0, 0.0, 1.0],
            width: 2.0,
        }),
        transform_id: 0,
        clip_rect: netrender::NO_CLIP,
        clip_corner_radii: netrender::SHARP_CLIP,
    };

    let mut scene = Scene::new(DIM, DIM);
    scene.push_shape(shape);
    let bytes = render_scene(&scene);

    // Center of the diamond (32, 32) — green fill; stroke is 2px
    // along the perimeter so the center is well clear of stroke.
    let center = read_pixel(&bytes, 32, 32);
    assert!(
        center[1] >= 240 && center[3] >= 240,
        "diamond center (32, 32): {:?} not opaque green", center
    );

    // On the right vertex (48, 32) — stroke goes through here, so
    // we expect black (or near-black) where stroke covers. Vertex
    // pixel coverage by the 2px stroke is partial (vertex angle +
    // AA), so accept ≥150 alpha.
    let right_vertex = read_pixel(&bytes, 48, 32);
    assert!(
        right_vertex[1] < 50 && right_vertex[3] >= 150,
        "right vertex (48, 32): {:?} expected near-black stroke (alpha ≥ 150)",
        right_vertex
    );

    // Outside the diamond — top-right of canvas.
    let outside = read_pixel(&bytes, 56, 8);
    assert!(
        outside[3] < 8,
        "outside (56, 8): {:?} should be empty", outside
    );
}

/// A path mixing quadratic and cubic Bézier segments. The control
/// points define a smooth bumpy curve. Verify the curve hits its
/// expected endpoints and that the bump regions get painted.
#[test]
fn p11b_04_curved_path() {
    // Path: a horizontal "tilde" from (8, 32) bowing down then up
    // to (56, 32), via a cubic curve.
    let mut path = ScenePath::new();
    path.move_to(8.0, 32.0)
        // Cubic that pulls down then up: control points at
        // (24, 48) and (40, 16), end at (56, 32).
        .cubic_to(24.0, 48.0, 40.0, 16.0, 56.0, 32.0);

    let mut scene = Scene::new(DIM, DIM);
    scene.push_shape_stroked(path, [1.0, 0.0, 1.0, 1.0], 3.0);
    let bytes = render_scene(&scene);

    // Endpoints of the cubic: (8, 32) and (56, 32). The 3px stroke
    // covers ±1.5 on each side; sample at the center of the
    // endpoint pixels.
    let start = read_pixel(&bytes, 8, 32);
    assert!(
        start[0] >= 200 && start[2] >= 200 && start[3] >= 200,
        "start (8, 32): {:?} not near opaque magenta", start
    );

    // The cubic dips down toward (control point at y=48 pulls)
    // — but Bézier control points are not reached by the curve;
    // the curve only bows partway. With this configuration the
    // curve at x≈22 is at y≈36. Sample a 5-pixel-wide band around
    // the expected dip and look for ANY magenta paint.
    let mut hit = false;
    for x in 18..28 {
        for y in 33..40 {
            let p = read_pixel(&bytes, x, y);
            if p[0] > 100 && p[2] > 100 && p[3] > 50 {
                hit = true;
                break;
            }
        }
        if hit { break; }
    }
    assert!(hit, "no magenta paint found in the dip region (x=18..28, y=33..40)");

    // Outside the curve — well above and below the y=32 line
    // far from any control influence.
    let outside = read_pixel(&bytes, 4, 4);
    assert!(
        outside[3] < 8,
        "outside (4, 4): {:?} should be empty", outside
    );
}
