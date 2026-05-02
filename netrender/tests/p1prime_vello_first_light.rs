/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Phase 1' first-light receipt — vello renders a rect into our wgpu
//! device and we read it back.
//!
//! Smallest possible end-to-end test that proves:
//!   - vello compiles + links into our project
//!   - vello's `Renderer` boots on the wgpu device our `boot()` returns
//!   - vello renders a single filled rect into a `Rgba8Unorm` storage
//!     texture with `view_formats: &[Rgba8UnormSrgb]`
//!   - `WgpuDevice::read_rgba8_texture` reads back the bytes vello wrote
//!
//! This test doubles as the §11.6 runtime spike. If it passes, we've
//! confirmed:
//!   1. wgpu validation accepts the (Rgba8Unorm storage, Rgba8UnormSrgb
//!      view-format) pair on this adapter.
//!   2. Vello's quantization round-trip lands the expected sRGB-encoded
//!      bytes into the storage texture.
//!
//! Receipt: a 64×64 red rect renders to all-(255, 0, 0, 255) bytes.
//! Vello blends in sRGB-encoded space; for primary opaque red the
//! storage value is sRGB(1.0, 0, 0, 1.0) = (255, 0, 0, 255).

use vello::{
    AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene,
    kurbo::{Affine, Point, Rect},
    peniko::{Color, ColorStop, Fill, Gradient, color::ColorSpaceTag},
};

use netrender::boot;

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

/// Allocate the standard p1' target texture: Rgba8Unorm storage with
/// `Rgba8UnormSrgb` view-format slot reserved (for downstream
/// sRGB→linear sample-time decode per §6.1 of the rasterizer plan).
fn make_target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p1' vello target"),
        size: wgpu::Extent3d {
            width: DIM,
            height: DIM,
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
    let view = texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("p1' vello storage view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, view)
}

fn render_params() -> RenderParams {
    RenderParams {
        base_color: Color::from_rgba8(0, 0, 0, 0),
        width: DIM,
        height: DIM,
        antialiasing_method: AaConfig::Area,
    }
}

fn read_pixel(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * DIM + x) * 4) as usize;
    [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
}

fn channel_diff(a: u8, b: u8) -> u8 {
    (a as i16 - b as i16).unsigned_abs() as u8
}

fn assert_within_tol(actual: [u8; 4], expected: [u8; 4], tol: u8, where_: &str) {
    let max = [0, 1, 2, 3]
        .iter()
        .map(|&i| channel_diff(actual[i], expected[i]))
        .max()
        .unwrap();
    assert!(
        max <= tol,
        "{}: actual {:?}, expected {:?} (max channel diff = {}, tol = {})",
        where_, actual, expected, max, tol
    );
}

#[test]
fn p1prime_01_vello_renders_red_rect() {
    let handles = boot().expect("wgpu boot");
    let device = &handles.device;
    let queue = &handles.queue;

    let mut renderer = Renderer::new(
        device,
        RendererOptions {
            use_cpu: false,
            antialiasing_support: AaSupport::area_only(),
            num_init_threads: None,
            pipeline_cache: None,
        },
    )
    .expect("vello::Renderer::new");

    // Build a vello scene with one full-canvas red rect.
    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(255, 0, 0, 255),
        None,
        &Rect::new(0.0, 0.0, DIM as f64, DIM as f64),
    );

    // Target: Rgba8Unorm storage texture with an Rgba8UnormSrgb view-
    // format slot reserved. Vello writes sRGB-encoded bytes into the
    // storage view; downstream sampling through the Rgba8UnormSrgb
    // view will hardware-decode to linear (verified separately, see
    // §6.1 of the vello rasterizer plan). For this first-light test
    // we just read the raw bytes back via COPY_SRC.
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("p1' vello target"),
        size: wgpu::Extent3d {
            width: DIM,
            height: DIM,
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
    let storage_view = target.create_view(&wgpu::TextureViewDescriptor {
        label: Some("p1' vello storage view"),
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });

    renderer
        .render_to_texture(
            device,
            queue,
            &scene,
            &storage_view,
            &RenderParams {
                base_color: Color::from_rgba8(0, 0, 0, 0),
                width: DIM,
                height: DIM,
                antialiasing_method: AaConfig::Area,
            },
        )
        .expect("vello render_to_texture");

    // Read back what vello wrote. We need a WgpuDevice to use
    // read_rgba8_texture — wrap our handles. (Boot already gave us
    // the WgpuDevice-equivalent handles; netrender's read helper is
    // on WgpuDevice. Construct one over the same handles.)
    let wgpu_device = netrender_device::WgpuDevice::with_external(handles.clone())
        .expect("WgpuDevice::with_external");
    let bytes = wgpu_device.read_rgba8_texture(&target, DIM, DIM);

    assert_eq!(bytes.len(), (DIM * DIM * 4) as usize);

    // Every pixel: red, opaque. Storage holds sRGB-encoded values;
    // sRGB(1.0) at the endpoints is identity, so primary red round-
    // trips to (255, 0, 0, 255).
    let mut max_diff: u8 = 0;
    let mut diff_count = 0usize;
    for chunk in bytes.chunks_exact(4) {
        for (i, &expected) in [255u8, 0, 0, 255].iter().enumerate() {
            let d = (chunk[i] as i16 - expected as i16).unsigned_abs() as u8;
            if d > 2 {
                diff_count += 1;
            }
            max_diff = max_diff.max(d);
        }
    }
    assert_eq!(
        diff_count, 0,
        "p1' first-light: {} channel values differ from (255,0,0,255) by >2 (max diff = {})",
        diff_count, max_diff
    );
}

/// Probe vello's storage-texture alpha convention. We hand vello a
/// straight-alpha half-opaque red `Color::from_rgba8(255, 0, 0, 128)`.
///
/// Receipt: storage holds `(255, 0, 0, 128)` — vello stores
/// **straight-alpha** sRGB-encoded values, not premultiplied. Verified
/// in `vello_shaders/shader/fine.wgsl` lines 1390-1395, where vello
/// explicitly divides RGB by alpha (`fg.rgb * a_inv`) before
/// `textureStore`. Internal blend math is premultiplied; the output
/// stage unpremultiplies for storage.
///
/// Why this matters for §6.1 of the rasterizer plan: when we sample
/// vello's output through an `Rgba8UnormSrgb` view-format, hardware
/// sRGB→linear decode produces straight-alpha linear values
/// (alpha is unmodified). The compositor must premultiply at sample
/// time before blending. The plan as drafted assumed premultiplied
/// storage; this test pins down the actual convention.
#[test]
fn p1prime_02_alpha_storage_is_straight() {
    let handles = boot().expect("wgpu boot");
    let device = &handles.device;
    let queue = &handles.queue;

    let mut renderer = make_renderer(device);

    let mut scene = Scene::new();
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgba8(255, 0, 0, 128),
        None,
        &Rect::new(0.0, 0.0, DIM as f64, DIM as f64),
    );

    let (target, view) = make_target(device);
    renderer
        .render_to_texture(device, queue, &scene, &view, &render_params())
        .expect("vello render_to_texture (half-alpha)");

    let wgpu_device = netrender_device::WgpuDevice::with_external(handles.clone())
        .expect("WgpuDevice::with_external");
    let bytes = wgpu_device.read_rgba8_texture(&target, DIM, DIM);

    // Storage: straight-alpha sRGB-encoded. RGB unaffected by alpha.
    for &(x, y) in &[(8, 8), (32, 32), (DIM - 8, DIM - 8)] {
        let actual = read_pixel(&bytes, x, y);
        assert_within_tol(
            actual,
            [255, 0, 0, 128],
            2,
            &format!("straight-alpha pixel ({}, {})", x, y),
        );
    }
}

/// Probe vello's gradient interpolation color-space behavior. A
/// horizontal red→blue linear gradient at midpoint should land near
/// (128, 0, 128) in storage — corresponding to per-channel linear
/// interpolation in sRGB-encoded space.
///
/// **Plan correction**: §3.3 of the rasterizer plan claimed that
/// `Gradient::with_interpolation_cs(LinearSrgb)` opts a gradient into
/// linear-light interpolation. **It does not**, on the GPU compute
/// path. `vello_encoding/src/encoding.rs:289-339` extracts only
/// `gradient.kind`, `stops`, `extend`, and `alpha` from the brush, and
/// `vello_encoding/src/ramp_cache.rs:86,97` hard-codes
/// `to_alpha_color::<Srgb>()` for every stop before the linear lerp.
/// `interpolation_cs` is honored only by the `vello_hybrid` (CPU/sparse
/// strips) path, not by the GPU compute renderer we use.
///
/// This test pins the current behavior: the LinearSrgb override
/// produces the same midpoint as the default. If upstream vello ever
/// wires `interpolation_cs` through the GPU path, this test will start
/// failing — at which point we can re-enable the linear-light branch
/// in netrender.
#[test]
fn p1prime_03_gradient_default_is_srgb_encoded() {
    let handles = boot().expect("wgpu boot");
    let device = &handles.device;
    let queue = &handles.queue;

    let mut renderer = make_renderer(device);

    fn render_gradient(
        renderer: &mut Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cs: Option<ColorSpaceTag>,
    ) -> wgpu::Texture {
        let mut scene = Scene::new();
        let mut grad = Gradient::new_linear(
            Point::new(0.0, (DIM as f64) / 2.0),
            Point::new(DIM as f64, (DIM as f64) / 2.0),
        )
        .with_stops([
            ColorStop::from((0.0, Color::from_rgba8(255, 0, 0, 255))),
            ColorStop::from((1.0, Color::from_rgba8(0, 0, 255, 255))),
        ]);
        if let Some(cs) = cs {
            grad = grad.with_interpolation_cs(cs);
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            &grad,
            None,
            &Rect::new(0.0, 0.0, DIM as f64, DIM as f64),
        );
        let (target, view) = make_target(device);
        renderer
            .render_to_texture(device, queue, &scene, &view, &render_params())
            .expect("vello render_to_texture (gradient)");
        target
    }

    let wgpu_device = netrender_device::WgpuDevice::with_external(handles.clone())
        .expect("WgpuDevice::with_external");

    // Default: sRGB-encoded interp. Midpoint of red→blue lands at
    // (128, 0, 128) in storage.
    let target_default = render_gradient(&mut renderer, device, queue, None);
    let bytes_default = wgpu_device.read_rgba8_texture(&target_default, DIM, DIM);
    let mid_default = read_pixel(&bytes_default, DIM / 2, DIM / 2);
    assert_within_tol(
        mid_default,
        [128, 0, 128, 255],
        4,
        "default sRGB-encoded gradient midpoint",
    );

    // LinearSrgb override: GPU compute path ignores it; midpoint must
    // match the default. If this assertion fails, vello upstream has
    // wired interpolation_cs through and our plan can be revised.
    let target_linear = render_gradient(
        &mut renderer,
        device,
        queue,
        Some(ColorSpaceTag::LinearSrgb),
    );
    let bytes_linear = wgpu_device.read_rgba8_texture(&target_linear, DIM, DIM);
    let mid_linear = read_pixel(&bytes_linear, DIM / 2, DIM / 2);
    assert_within_tol(
        mid_linear,
        mid_default,
        2,
        "LinearSrgb override should equal default (vello GPU path ignores interpolation_cs)",
    );
}
