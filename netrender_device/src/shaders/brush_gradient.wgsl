// brush_gradient.wgsl — Phase 8D unified analytic gradient.
//
// One shader specializes into linear / radial / conic via the
// `GRADIENT_KIND` override constant. N-stop ramps live in a shared
// storage buffer (binding 3); per-instance `stops_offset` /
// `stops_count` index into it.
//
// Bind group:
//   0 — instances:  array<GradientInstance>  (storage, VERTEX read)
//   1 — transforms: array<Transform>         (storage, VERTEX read)
//   2 — per_frame:  PerFrame                 (uniform, VERTEX)
//   3 — stops:      array<Stop>              (storage, FRAGMENT read)
//
// Instance struct (64-byte stride, WGSL std430):
//   rect          vec4<f32>  offset  0 — local-space corners [x0,y0,x1,y1]
//   params        vec4<f32>  offset 16 — kind-dependent:
//                                          Linear: (start.xy, end.xy)
//                                          Radial: (center.xy, radii.xy)
//                                          Conic:  (center.xy, start_angle, _pad)
//   clip          vec4<f32>  offset 32 — device-space clip [x0,y0,x1,y1]
//   transform_id  u32        offset 48 — index into transforms[]
//   z_depth       f32        offset 52 — NDC depth in [0,1]; 0=near/front
//   stops_offset  u32        offset 56 — first stop index
//   stops_count   u32        offset 60 — number of stops
//
// Stop struct (32-byte stride):
//   color         vec4<f32>  offset  0 — premultiplied RGBA
//   offset        vec4<f32>  offset 16 — .x = position in [0, 1];
//                                         .yzw padding to vec4 alignment

override GRADIENT_KIND: u32 = 0u;

const KIND_LINEAR: u32 = 0u;
const KIND_RADIAL: u32 = 1u;
const KIND_CONIC: u32 = 2u;

const TWO_PI: f32 = 6.283185307179586;

struct GradientInstance {
    rect: vec4<f32>,
    params: vec4<f32>,
    clip: vec4<f32>,
    transform_id: u32,
    z_depth: f32,
    stops_offset: u32,
    stops_count: u32,
}

struct Stop {
    color: vec4<f32>,
    offset_pad: vec4<f32>,
}

struct Transform {
    m: mat4x4<f32>,
}

struct PerFrame {
    u_transform: mat4x4<f32>,
}

@group(0) @binding(0)
var<storage, read> instances: array<GradientInstance>;

@group(0) @binding(1)
var<storage, read> transforms: array<Transform>;

@group(0) @binding(2)
var<uniform> per_frame: PerFrame;

@group(0) @binding(3)
var<storage, read> stops: array<Stop>;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) local_pos: vec2<f32>,
    // Linear pre-computes t at the corner; rasterizer interpolates.
    // Radial / conic ignore this and recompute per-fragment.
    @location(1) gradient_t: f32,
    @location(2) @interpolate(flat) params: vec4<f32>,
    @location(3) @interpolate(flat) clip: vec4<f32>,
    @location(4) @interpolate(flat) stops_offset: u32,
    @location(5) @interpolate(flat) stops_count: u32,
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
    out.local_pos = local_pos;
    out.params = inst.params;
    out.clip = inst.clip;
    out.stops_offset = inst.stops_offset;
    out.stops_count = inst.stops_count;

    var t_at_vertex: f32 = 0.0;
    if (GRADIENT_KIND == KIND_LINEAR) {
        let p0 = inst.params.xy;
        let p1 = inst.params.zw;
        let dir = p1 - p0;
        let len_sq = max(dot(dir, dir), 1e-9);
        t_at_vertex = dot(local_pos - p0, dir) / len_sq;
    }
    out.gradient_t = t_at_vertex;

    return out;
}

/// Sample the N-stop ramp at parameter `t`, interpolating between the
/// two stops whose offsets bracket `t`. Clamps to first / last stop
/// for `t` outside the valid range.
fn sample_stops(start: u32, count: u32, t: f32) -> vec4<f32> {
    if (count == 0u) {
        return vec4<f32>(0.0);
    }
    if (count == 1u) {
        return stops[start].color;
    }

    let first = stops[start];
    if (t <= first.offset_pad.x) {
        return first.color;
    }

    let last_idx = start + count - 1u;
    let last = stops[last_idx];
    if (t >= last.offset_pad.x) {
        return last.color;
    }

    var result: vec4<f32> = last.color;
    for (var i: u32 = 0u; i < count - 1u; i = i + 1u) {
        let a = stops[start + i];
        let b = stops[start + i + 1u];
        if (t < b.offset_pad.x) {
            let span = max(b.offset_pad.x - a.offset_pad.x, 1e-9);
            let local_t = (t - a.offset_pad.x) / span;
            result = mix(a.color, b.color, local_t);
            break;
        }
    }
    return result;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.position.xy;
    if (p.x < in.clip.x || p.y < in.clip.y || p.x >= in.clip.z || p.y >= in.clip.w) {
        discard;
    }

    var t: f32 = 0.0;
    if (GRADIENT_KIND == KIND_LINEAR) {
        t = in.gradient_t;
    } else if (GRADIENT_KIND == KIND_RADIAL) {
        let center = in.params.xy;
        let radii = in.params.zw;
        let safe_radii = max(radii, vec2<f32>(1e-9, 1e-9));
        let d = (in.local_pos - center) / safe_radii;
        t = length(d);
    } else { // KIND_CONIC
        let center = in.params.xy;
        let start_angle = in.params.z;
        let d = in.local_pos - center;
        let raw = atan2(d.y, d.x);
        t = fract((raw - start_angle) / TWO_PI);
    }

    return sample_stops(in.stops_offset, in.stops_count, t);
}
