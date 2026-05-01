// cs_clip_rectangle.wgsl — Phase 9A rounded-rect clip mask.
//
// Renders a fullscreen-quad pass into an `Rgba8Unorm` target,
// outputting per-pixel coverage `[0, 1]` for a rounded rectangle.
// All four output channels are set to the coverage value, so the
// resulting texture works as either a coverage mask (read .a) or
// a grayscale image (read .rgb).
//
// The fast-path variant (`HAS_ROUNDED_CORNERS = false`) is Phase 9C:
// skip the SDF math and output 1.0 inside the rect / 0 outside,
// giving a hard-edged step. For non-zero radii (the default),
// `sdRoundedRect` is the standard signed-distance function from
// Inigo Quilez's catalogue, smoothed across one pixel via
// `clamp(0.5 - d, 0, 1)`.
//
// Bind group:
//   0 — params: ClipParams (uniform, FRAGMENT)
//
// `params.bounds` and `params.radii` are in **target-pixel** space —
// the caller supplies them already aligned with the output texture's
// pixel grid. The vertex shader generates a fullscreen quad; the
// fragment shader reads `position.xy` (which wgpu reports in
// framebuffer pixel coords) and feeds it directly to the SDF.

override HAS_ROUNDED_CORNERS: bool = true;

struct ClipParams {
    /// Rect bounds in target-pixel space: `(x0, y0, x1, y1)`.
    bounds: vec4<f32>,
    /// Uniform corner radius. (Per-corner radii is a Phase 9+ extension;
    /// for 9A all four corners use `radii.x`.)
    radii: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> params: ClipParams;

struct VsOut {
    @builtin(position) position: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Fullscreen TriangleStrip from vertex_index 0..3.
    let corner = vec2<f32>(
        f32(vi & 1u),
        f32((vi >> 1u) & 1u),
    );
    // NDC: (-1, -1) bottom-left, (1, 1) top-right; flip Y for screen.
    let pos = vec2<f32>(corner.x * 2.0 - 1.0, 1.0 - corner.y * 2.0);
    var out: VsOut;
    out.position = vec4<f32>(pos, 0.0, 1.0);
    return out;
}

/// Signed distance from a point `p` to a rounded rectangle centered
/// at the origin with half-extents `b` and corner radius `r`.
/// Negative inside, zero on the boundary, positive outside.
fn sdRoundedRect(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2<f32>(r, r);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.position.xy;
    let center = (params.bounds.xy + params.bounds.zw) * 0.5;
    let half_size = (params.bounds.zw - params.bounds.xy) * 0.5;

    var coverage: f32;
    if (HAS_ROUNDED_CORNERS) {
        // Clamp the radius so it can't exceed the smaller half-extent
        // (otherwise the SDF degenerates).
        let r = clamp(params.radii.x, 0.0, min(half_size.x, half_size.y));
        let d = sdRoundedRect(p - center, half_size, r);
        coverage = clamp(0.5 - d, 0.0, 1.0);
    } else {
        // Fast path: axis-aligned step function. Anti-alias the edge
        // across one pixel via the same clamp trick.
        let q = abs(p - center) - half_size;
        let d = max(q.x, q.y);
        coverage = clamp(0.5 - d, 0.0, 1.0);
    }

    return vec4<f32>(coverage, coverage, coverage, coverage);
}
