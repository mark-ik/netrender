/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! CSS `filter` color-matrix pass.
//!
//! Applies a 4x5 affine color matrix to the input texture: one fullscreen
//! pass producing `out = clamp(M * straight(in_rgba) + bias, 0, 1)`,
//! re-premultiplied. The CSS Filter Effects shorthands (grayscale, sepia,
//! saturate, hue-rotate, brightness, contrast, invert) lower to such a matrix
//! on the CPU (`scene_filter_to_matrix`).
//!
//! Two correctness invariants (CSS Filter Effects L1):
//!   1. The math is in **sRGB-encoded** space. The input is sampled from an
//!      `Rgba8Unorm` view (no sRGB decode), so the texels reaching here are the
//!      gamma values the matrix expects; output is written without re-encoding.
//!   2. The matrix operates on **straight** (non-premultiplied) color, but
//!      netrender intermediates are **premultiplied**. So unpremultiply, apply
//!      M, clamp, then re-premultiply (alpha row is identity for these
//!      functions, but the shader applies the full matrix for generality).
//!
//! No vertex buffer — the fullscreen quad is generated from `vertex_index`
//! 0..4 (TriangleStrip), mirroring `brush_blur`.

struct ColorMatrixParams {
    // 4x5 matrix as four rows of [r, g, b, a] coefficients + a bias column.
    //   out.r = dot(row0, in_rgba) + bias.x
    //   out.g = dot(row1, in_rgba) + bias.y   (etc.)
    row0: vec4<f32>,
    row1: vec4<f32>,
    row2: vec4<f32>,
    row3: vec4<f32>,
    bias: vec4<f32>,
}

@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var input_sampler: sampler;
@group(0) @binding(2) var<uniform> params: ColorMatrixParams;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// Fullscreen quad via TriangleStrip from vertex_index 0..4.
// NDC y-up: (-1,-1) = bottom-left. UV y-down: (0,0) = top-left.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0,  1.0),
    );
    var uvs = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
    );
    var out: VertexOutput;
    out.position = vec4<f32>(positions[vi], 0.0, 1.0);
    out.uv = uvs[vi];
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(input_texture, input_sampler, in.uv); // premultiplied
    let a = texel.a;

    // Unpremultiply to straight color (guard a == 0).
    var straight_rgb: vec3<f32>;
    if (a > 0.0) {
        straight_rgb = texel.rgb / a;
    } else {
        straight_rgb = vec3<f32>(0.0, 0.0, 0.0);
    }
    let c = vec4<f32>(straight_rgb, a);

    // out = M * [r, g, b, a] + bias  (straight color).
    let out_r = dot(params.row0, c) + params.bias.x;
    let out_g = dot(params.row1, c) + params.bias.y;
    let out_b = dot(params.row2, c) + params.bias.z;
    let out_a = dot(params.row3, c) + params.bias.w;

    // SVG filter-primitive clamping to the unit interval, then re-premultiply.
    let rgb = clamp(vec3<f32>(out_r, out_g, out_b), vec3<f32>(0.0), vec3<f32>(1.0));
    let alpha = clamp(out_a, 0.0, 1.0);
    return vec4<f32>(rgb * alpha, alpha);
}
