// ps_text_run.wgsl — Phase 10a.1 grayscale text shader.
//
// One per-instance quad samples a single glyph slot from the R8Unorm
// glyph atlas. Coverage is the atlas sample; output is the
// premultiplied tint × coverage. Always alpha-blended.
//
// Bind group (group 0):
//   0 — instances:      array<GlyphInstance>  (storage, read-only, VERTEX)
//   1 — transforms:     array<Transform>      (storage, read-only, VERTEX)
//   2 — per_frame:      PerFrame              (uniform, VERTEX)
//   3 — atlas_texture:  texture_2d<f32>       (FRAGMENT, R8Unorm)
//   4 — atlas_sampler:  sampler               (FRAGMENT, NonFiltering)
//
// Instance struct (80-byte stride, std430) — same layout as
// `ImageInstance` so `write_image_instance` can populate it
// unchanged. The only Phase-5-vs-10a delta is what's bound at slot 3
// (R8 atlas vs RGBA8 image cache entry) and the fragment-side sample
// swizzle (`.r` × tint vs. `.rgba` × tint).
//
//   rect          vec4<f32>  offset  0 — local-space [x0,y0,x1,y1]
//   uv_rect       vec4<f32>  offset 16 — atlas UV [u0,v0,u1,v1]
//   color         vec4<f32>  offset 32 — premultiplied RGBA tint
//   clip          vec4<f32>  offset 48 — device-space clip rect
//   transform_id  u32        offset 64
//   z_depth       f32        offset 68 — NDC depth in [0,1]; 0=near
//                              8 bytes implicit padding → stride 80

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

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Sample first (uniform control flow) so the shader is portable
    // across drivers that hoist textureSample.
    let coverage = textureSample(atlas_texture, atlas_sampler, in.uv).r;
    let p = in.position.xy;
    if (p.x < in.clip.x || p.y < in.clip.y || p.x >= in.clip.z || p.y >= in.clip.w) {
        discard;
    }
    // `in.color` is premultiplied; multiplying by coverage keeps
    // it premultiplied. Output is then directly composable with
    // `BlendState::PREMULTIPLIED_ALPHA_BLENDING`.
    return in.color * coverage;
}
