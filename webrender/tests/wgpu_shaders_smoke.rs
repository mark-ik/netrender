/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! P4i smoke: WgpuDevice's GpuShaders methods load committed SPIR-V,
//! build wgpu RenderPipelines via descriptor_to_wgpu_layouts, and
//! produce a real WgpuProgram for at least ps_clear (no textures) and
//! ps_quad_textured (after the P2 spike unblocked it).

#![cfg(feature = "wgpu_backend")]

use std::sync::Arc;
use webrender::{
    GpuShaders, VertexAttribute, VertexDescriptor, WgpuDevice,
};

fn try_create_device() -> Option<WgpuDevice> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("wgpu_shaders_smoke device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: wgpu::Trace::Off,
        experimental_features: wgpu::ExperimentalFeatures::default(),
    }))
    .ok()?;
    Some(WgpuDevice::from_parts(
        Arc::new(instance),
        Arc::new(adapter),
        Arc::new(device),
        Arc::new(queue),
        None,
        None,
    ))
}

#[test]
fn ps_clear_create_program_linked_succeeds() {
    let Some(mut wgpu_device) = try_create_device() else {
        eprintln!("skip: no wgpu adapter available");
        return;
    };

    static VERT: &[VertexAttribute] = &[VertexAttribute::quad_instance_vertex()];
    static INST: &[VertexAttribute] = &[
        VertexAttribute::f32x4("aRect"),
        VertexAttribute::f32x4("aColor"),
    ];
    static DESC: VertexDescriptor = VertexDescriptor {
        vertex_attributes: VERT,
        instance_attributes: INST,
    };

    let program = wgpu_device
        .create_program_linked("ps_clear", &[], &DESC)
        .expect("ps_clear should link successfully");

    // The pipeline was built and stashed in the program.
    assert!(program.pipeline.borrow().is_some());
    assert_eq!(program.stem, "ps_clear");

    // Uniform buffer is sized for WrLocals { mat4 } = 64 bytes.
    assert_eq!(program.uniform_buffer.size(), 64);

    wgpu_device.delete_program(program);
}

#[test]
fn ps_quad_textured_create_program_linked_succeeds() {
    // P2 spike unblocked this — naga reflection works for ps_quad_textured
    // after the spirv-opt --split-combined-image-sampler pass + binding
    // distribution. This test confirms the full pipeline (load + link
    // + auto-derived layout) works for the textured case end-to-end.
    let Some(mut wgpu_device) = try_create_device() else {
        eprintln!("skip: no wgpu adapter available");
        return;
    };

    // ps_quad_textured uses the standard quad-instance pattern (per-vertex
    // aPosition + per-instance aData).
    static VERT: &[VertexAttribute] = &[VertexAttribute::quad_instance_vertex()];
    static INST: &[VertexAttribute] = &[
        VertexAttribute {
            name: "aData",
            count: 4,
            kind: webrender::VertexAttributeKind::I32,
        },
    ];
    static DESC: VertexDescriptor = VertexDescriptor {
        vertex_attributes: VERT,
        instance_attributes: INST,
    };

    let program = wgpu_device
        .create_program_linked("ps_quad_textured", &[], &DESC)
        .expect("ps_quad_textured should link successfully");
    assert!(program.pipeline.borrow().is_some());
    assert_eq!(program.stem, "ps_quad_textured");
    wgpu_device.delete_program(program);
}

#[test]
fn unknown_shader_returns_compilation_error() {
    let Some(mut wgpu_device) = try_create_device() else {
        eprintln!("skip: no wgpu adapter available");
        return;
    };
    static DESC: VertexDescriptor = VertexDescriptor {
        vertex_attributes: &[],
        instance_attributes: &[],
    };
    let result = wgpu_device.create_program_linked("nonexistent_shader", &[], &DESC);
    assert!(result.is_err(), "unknown shader should fail to load");
}

#[test]
fn create_then_link_two_step_works() {
    // GL pattern: create_program first (loads source, doesn't link yet),
    // then link_program with the descriptor. WgpuProgram supports this
    // by leaving pipeline = None until link_program runs.
    let Some(mut wgpu_device) = try_create_device() else {
        eprintln!("skip: no wgpu adapter available");
        return;
    };
    static VERT: &[VertexAttribute] = &[VertexAttribute::quad_instance_vertex()];
    static INST: &[VertexAttribute] = &[
        VertexAttribute::f32x4("aRect"),
        VertexAttribute::f32x4("aColor"),
    ];
    static DESC: VertexDescriptor = VertexDescriptor {
        vertex_attributes: VERT,
        instance_attributes: INST,
    };

    let mut program = wgpu_device.create_program("ps_clear", &[]).expect("create");
    assert!(program.pipeline.borrow().is_none(), "pipeline absent before link");

    wgpu_device.link_program(&mut program, &DESC).expect("link");
    assert!(program.pipeline.borrow().is_some(), "pipeline present after link");

    wgpu_device.delete_program(program);
}
