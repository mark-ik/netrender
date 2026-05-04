// ps_text_run_dual_source.wgsl — Phase 10a.4 / 10b.1 subpixel-AA text
// shader.
//
// Requires the WGSL `dual_source_blending` extension. The pipeline
// factory (`build_brush_text_dual_source`) only attempts to build
// this module when `device.features()` contains
// `Features::DUAL_SOURCE_BLENDING`, so the enable directive below
// matches the consumer's runtime check.
//
// Same instance + binding shape as `ps_text_run.wgsl` (the grayscale
// path); the difference lives in the fragment outputs and the
// pipeline blend state. Two `@location(0)` outputs feed the
// dual-source blend equation:
//
//   dst.rgb = 1 * src.rgb + (1 - src1.rgb) * dst.rgb
//   dst.a   = 1 * src.a   + (1 - src1.a)   * dst.a
//
// where `src` is `out.color` and `src1` is `out.alpha`. The two
// outputs differ only by what they multiply the tint by:
//
//   color = (tint.rgb * cov_rgb, tint.a * cov_avg)   -- premultiplied
//   alpha = (tint.a   * cov_rgb, tint.a * cov_avg)   -- per-channel "alpha"
//
// For a per-channel coverage triple (cR, cG, cB), the framebuffer
// blend produces a per-subpixel anti-aliased result — each LCD
// sub-pixel sees its own coverage value, tripling effective
// horizontal resolution.
//
// Phase 10b.1 atlas: glyph atlas is now `Rgba8Unorm` and stores
// either an `Alpha`-format glyph as `(c, c, c, 255)` (broadcast) or
// a `Subpixel`-format glyph as `(r, g, b, 255)` (per-channel
// LCD coverage). The fragment samples `.rgb` directly: for
// broadcast `(c, c, c)` the dual-source path is bit-equivalent to
// the grayscale `ps_text_run.wgsl` path; for `(r, g, b)` it
// triples horizontal resolution at the LCD subpixel level.
//
// `cov_avg = (r + g + b) / 3` is the standard alpha-channel coverage
// for the dual-source equation (matches WebRender's averaging).

enable dual_source_blending;

struct GlyphInstance {
    rect: vec4<f32>,
    uv_rect: vec4<f32>,
    color: vec4<f32>,
    clip: vec4<f32>,
    transform_id: u32,
    z_depth: f32,
}

struct Transform {
    m: mat4x4<f32>,
}

struct PerFrame {
    u_transform: mat4x4<f32>,
}

@group(0) @binding(0) var<storage, read> instances: array<GlyphInstance>;
@group(0) @binding(1) var<storage, read> transforms: array<Transform>;
@group(0) @binding(2) var<uniform> per_frame: PerFrame;
@group(0) @binding(3) var atlas_texture: texture_2d<f32>;
@group(0) @binding(4) var atlas_sampler: sampler;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) clip: vec4<f32>,
}

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> VsOut {
    let inst = instances[instance_index];
    let corner = vec2<f32>(
        f32(vertex_index & 1u),
        f32((vertex_index >> 1u) & 1u),
    );
    let local_pos = mix(inst.rect.xy, inst.rect.zw, corner);
    let world_pos = transforms[inst.transform_id].m * vec4<f32>(local_pos, 0.0, 1.0);

    var out: VsOut;
    out.position = per_frame.u_transform * world_pos;
    out.position.z = inst.z_depth;
    out.uv = mix(inst.uv_rect.xy, inst.uv_rect.zw, corner);
    out.color = inst.color;
    out.clip = inst.clip;
    return out;
}

struct FsOut {
    @location(0) @blend_src(0) color: vec4<f32>,
    @location(0) @blend_src(1) alpha: vec4<f32>,
}

@fragment
fn fs_main(in: VsOut) -> FsOut {
    let sample = textureSample(atlas_texture, atlas_sampler, in.uv);
    let p = in.position.xy;
    if (p.x < in.clip.x || p.y < in.clip.y || p.x >= in.clip.z || p.y >= in.clip.w) {
        discard;
    }

    // 10b.1 atlas is `Rgba8Unorm`. `Alpha`-format glyphs are stored as
    // `(c, c, c, 255)` so `.rgb` reads back the broadcast triple
    // (bit-equivalent to the grayscale `ps_text_run` path).
    // `Subpixel`-format glyphs are stored as `(r, g, b, 255)` and
    // `.rgb` carries the LCD per-channel coverage. The shader is
    // format-agnostic; the atlas upload picks the right encoding.
    let cov_rgb = sample.rgb;
    let cov_avg = (cov_rgb.r + cov_rgb.g + cov_rgb.b) / 3.0;

    var out: FsOut;
    out.color = vec4<f32>(in.color.rgb * cov_rgb, in.color.a * cov_avg);
    out.alpha = vec4<f32>(in.color.a   * cov_rgb, in.color.a * cov_avg);
    return out;
}
