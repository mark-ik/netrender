/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Vertex schema adapter (plan A3).
//!
//! Mechanical conversion from WebRender's typed `VertexDescriptor` to a
//! pair of `wgpu::VertexBufferLayout`s suitable for
//! `RenderPipelineDescriptor::vertex.buffers`. No string parsing, no
//! regex; walks the schema, accumulates byte offsets, emits
//! `wgpu::VertexAttribute` entries. shader_location indices follow GLSL
//! declaration order — vertex attrs claim 0..N, instance attrs continue
//! at N..N+M — matching glslang's `set_auto_map_locations(true)` output
//! in `gen_spirv`.

use super::super::types::{VertexAttribute, VertexAttributeKind, VertexDescriptor};

/// Owned wgpu vertex buffer layouts derived from a `VertexDescriptor`.
/// Holds the attribute Vecs alive so callers can borrow them as
/// `wgpu::VertexBufferLayout<'_>` slices for pipeline construction.
pub struct WgpuVertexLayouts {
    vertex_attrs: Vec<wgpu::VertexAttribute>,
    instance_attrs: Vec<wgpu::VertexAttribute>,
    vertex_stride: u64,
    instance_stride: u64,
}

impl WgpuVertexLayouts {
    /// Builds the layouts from a `VertexDescriptor`. Strides are rounded
    /// up to wgpu's `VERTEX_ALIGNMENT` (4 bytes); attributes get their
    /// `shader_location` in declaration order with vertex attrs first.
    pub fn from_descriptor(desc: &VertexDescriptor) -> Self {
        let mut vertex_attrs = Vec::with_capacity(desc.vertex_attributes.len());
        let mut vertex_offset: u64 = 0;
        for (i, attr) in desc.vertex_attributes.iter().enumerate() {
            vertex_attrs.push(wgpu::VertexAttribute {
                format: attribute_to_wgpu_format(attr),
                offset: vertex_offset,
                shader_location: i as u32,
            });
            vertex_offset += attr.size_in_bytes() as u64;
        }

        let instance_loc_start = desc.vertex_attributes.len() as u32;
        let mut instance_attrs = Vec::with_capacity(desc.instance_attributes.len());
        let mut instance_offset: u64 = 0;
        for (i, attr) in desc.instance_attributes.iter().enumerate() {
            instance_attrs.push(wgpu::VertexAttribute {
                format: attribute_to_wgpu_format(attr),
                offset: instance_offset,
                shader_location: instance_loc_start + i as u32,
            });
            instance_offset += attr.size_in_bytes() as u64;
        }

        // wgpu requires vertex buffer stride aligned to VERTEX_ALIGNMENT
        // (4 bytes). Round up — the GPU reads `array_stride` bytes per
        // vertex regardless of attribute layout, so trailing padding is
        // fine. Common case: aPosition u8norm count=2 = 2 bytes,
        // padded to 4.
        let vertex_stride = align_up(vertex_offset, wgpu::VERTEX_ALIGNMENT);
        let instance_stride = align_up(instance_offset, wgpu::VERTEX_ALIGNMENT);

        WgpuVertexLayouts {
            vertex_attrs,
            instance_attrs,
            vertex_stride,
            instance_stride,
        }
    }

    /// Returns `[vertex_buffer, instance_buffer]` borrowing from self,
    /// suitable for `RenderPipelineDescriptor::vertex.buffers`.
    pub fn buffers(&self) -> [wgpu::VertexBufferLayout<'_>; 2] {
        [
            wgpu::VertexBufferLayout {
                array_stride: self.vertex_stride,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &self.vertex_attrs,
            },
            wgpu::VertexBufferLayout {
                array_stride: self.instance_stride,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &self.instance_attrs,
            },
        ]
    }

    pub fn vertex_stride(&self) -> u64 { self.vertex_stride }
    pub fn instance_stride(&self) -> u64 { self.instance_stride }
    pub fn vertex_attrs(&self) -> &[wgpu::VertexAttribute] { &self.vertex_attrs }
    pub fn instance_attrs(&self) -> &[wgpu::VertexAttribute] { &self.instance_attrs }
}

/// Maps a WebRender `VertexAttribute` to its wgpu vertex format.
///
/// The (kind, count) pair determines the format. Unsupported combinations
/// panic — current shader corpus uses only the supported subset; extend
/// the match when a new combination appears.
fn attribute_to_wgpu_format(attr: &VertexAttribute) -> wgpu::VertexFormat {
    use wgpu::VertexFormat as VF;
    match (attr.kind, attr.count) {
        (VertexAttributeKind::F32, 1) => VF::Float32,
        (VertexAttributeKind::F32, 2) => VF::Float32x2,
        (VertexAttributeKind::F32, 3) => VF::Float32x3,
        (VertexAttributeKind::F32, 4) => VF::Float32x4,
        (VertexAttributeKind::I32, 1) => VF::Sint32,
        (VertexAttributeKind::I32, 2) => VF::Sint32x2,
        (VertexAttributeKind::I32, 4) => VF::Sint32x4,
        (VertexAttributeKind::U8Norm, 2) => VF::Unorm8x2,
        (VertexAttributeKind::U8Norm, 4) => VF::Unorm8x4,
        (VertexAttributeKind::U16Norm, 2) => VF::Unorm16x2,
        (VertexAttributeKind::U16Norm, 4) => VF::Unorm16x4,
        (VertexAttributeKind::U16, 2) => VF::Uint16x2,
        (VertexAttributeKind::U16, 4) => VF::Uint16x4,
        (kind, count) => panic!(
            "no wgpu VertexFormat for VertexAttribute kind={:?} count={}",
            kind, count
        ),
    }
}

/// Rounds `value` up to the nearest multiple of `align` (must be a
/// power of two). Used to satisfy wgpu's VERTEX_ALIGNMENT requirement.
fn align_up(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    // VertexDescriptor takes &'static slices, so test schemas live as
    // const tables.
    const PS_CLEAR_VERT: &[VertexAttribute] = &[VertexAttribute::quad_instance_vertex()];
    const PS_CLEAR_INST: &[VertexAttribute] = &[
        VertexAttribute::f32x4("aRect"),
        VertexAttribute::f32x4("aColor"),
    ];
    const PS_CLEAR: VertexDescriptor = VertexDescriptor {
        vertex_attributes: PS_CLEAR_VERT,
        instance_attributes: PS_CLEAR_INST,
    };

    const EMPTY: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[],
        instance_attributes: &[],
    };

    const MIXED_V: &[VertexAttribute] = &[
        VertexAttribute::f32x2("v0"),
        VertexAttribute::f32x4("v1"),
    ];
    const MIXED_I: &[VertexAttribute] = &[
        VertexAttribute::i32x4("i0"),
        VertexAttribute::f32("i1"),
    ];
    const MIXED: VertexDescriptor = VertexDescriptor {
        vertex_attributes: MIXED_V,
        instance_attributes: MIXED_I,
    };

    const STRIDE_I: &[VertexAttribute] = &[
        VertexAttribute::f32x2("a"),
        VertexAttribute::f32("b"),
        VertexAttribute::i32x4("c"),
    ];
    const STRIDE: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[],
        instance_attributes: STRIDE_I,
    };

    #[test]
    fn ps_clear_layout_matches_oracle() {
        let layouts = WgpuVertexLayouts::from_descriptor(&PS_CLEAR);

        // Vertex stride: aPosition (U8Norm count 2) = 2 bytes; padded to
        // 4 to satisfy wgpu's VERTEX_ALIGNMENT.
        assert_eq!(layouts.vertex_stride(), 4);
        assert_eq!(layouts.vertex_attrs().len(), 1);
        assert_eq!(layouts.vertex_attrs()[0].format, wgpu::VertexFormat::Unorm8x2);
        assert_eq!(layouts.vertex_attrs()[0].offset, 0);
        assert_eq!(layouts.vertex_attrs()[0].shader_location, 0);

        // Instance stride: aRect (F32 count 4 = 16 bytes) + aColor (16) = 32.
        assert_eq!(layouts.instance_stride(), 32);
        assert_eq!(layouts.instance_attrs().len(), 2);
        assert_eq!(layouts.instance_attrs()[0].format, wgpu::VertexFormat::Float32x4);
        assert_eq!(layouts.instance_attrs()[0].offset, 0);
        assert_eq!(layouts.instance_attrs()[0].shader_location, 1);
        assert_eq!(layouts.instance_attrs()[1].format, wgpu::VertexFormat::Float32x4);
        assert_eq!(layouts.instance_attrs()[1].offset, 16);
        assert_eq!(layouts.instance_attrs()[1].shader_location, 2);

        // shader_location indices [0, 1, 2] match the bindings.json
        // reflection oracle for ps_clear (aPosition=0, aRect=1, aColor=2).
        // The format mismatch (Unorm8x2 vs vec2<f32> reported by naga) is
        // expected — wgpu auto-converts u8norm vertex data to vec2<f32>
        // on shader read, same as GL.
    }

    #[test]
    fn empty_descriptor_yields_zero_strides() {
        let layouts = WgpuVertexLayouts::from_descriptor(&EMPTY);
        assert_eq!(layouts.vertex_stride(), 0);
        assert_eq!(layouts.instance_stride(), 0);
        assert!(layouts.vertex_attrs().is_empty());
        assert!(layouts.instance_attrs().is_empty());
    }

    #[test]
    fn locations_continue_across_vertex_to_instance() {
        // Vertex attrs claim 0..N; instance attrs continue at N..N+M
        // (matches glslang's declaration-order auto-mapping in gen_spirv).
        let layouts = WgpuVertexLayouts::from_descriptor(&MIXED);
        let v_locs: Vec<u32> = layouts.vertex_attrs().iter().map(|a| a.shader_location).collect();
        let i_locs: Vec<u32> = layouts.instance_attrs().iter().map(|a| a.shader_location).collect();
        assert_eq!(v_locs, vec![0, 1]);
        assert_eq!(i_locs, vec![2, 3]);
    }

    #[test]
    fn stride_accumulates_offsets_correctly() {
        // f32x2 (8) + f32 (4) + i32x4 (16) = 28 bytes; aligned up to 28
        // (already a multiple of 4 — VERTEX_ALIGNMENT).
        let layouts = WgpuVertexLayouts::from_descriptor(&STRIDE);
        assert_eq!(layouts.instance_stride(), 28);
        let offsets: Vec<u64> = layouts.instance_attrs().iter().map(|a| a.offset).collect();
        assert_eq!(offsets, vec![0, 8, 12]);
    }
}
