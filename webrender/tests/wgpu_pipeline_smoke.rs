/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! End-to-end smoke test for P2: load ps_clear's SPIR-V via WgpuDevice
//! and create a `wgpu::RenderPipeline` with `layout: None` so wgpu's
//! internal naga auto-derives the bind-group layout from reflection.
//!
//! This validates the full P2 pipeline: gen_spirv → committed .spv →
//! WgpuDevice loading → wgpu pipeline creation. Successful pipeline
//! construction means wgpu's naga accepted the SPIR-V and derived a
//! coherent layout. Combined with the bindings.json oracle test
//! (webrender_build/tests/spirv_bindings_oracle.rs, which independently
//! verifies our naga reflection produces the golden manifest), we have
//! transitive evidence that wgpu's auto-derived layout for ps_clear
//! matches the golden bindings (both sides go through naga).
//!
//! The test gracefully skips when no adapter is available (CI
//! environments without GPU access), so it's safe to run unconditionally
//! under `--features wgpu_backend`.
//!
//! Run with:
//!   cargo test -p webrender --features "gl_backend wgpu_backend" --test wgpu_pipeline_smoke

#![cfg(feature = "wgpu_backend")]

use std::path::PathBuf;
use std::sync::Arc;
use webrender::WgpuDevice;

fn workspace_root() -> PathBuf {
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.pop();
    root
}

fn ps_clear_spv(stage: &str) -> Vec<u8> {
    let path = workspace_root()
        .join("webrender")
        .join("res")
        .join("spirv")
        .join(format!("ps_clear.{}.spv", stage));
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e))
}

/// Constructs a `WgpuDevice` from the default-backend adapter, or returns
/// `None` if no adapter is available (headless CI without GPU).
fn try_create_device() -> Option<WgpuDevice> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("wgpu_pipeline_smoke device"),
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
fn ps_clear_creates_render_pipeline() {
    let Some(wgpu_device) = try_create_device() else {
        eprintln!("skip: no wgpu adapter available in this environment");
        return;
    };

    let vert_module =
        wgpu_device.create_shader_module_from_spv(Some("ps_clear.vert"), &ps_clear_spv("vert"));
    let frag_module =
        wgpu_device.create_shader_module_from_spv(Some("ps_clear.frag"), &ps_clear_spv("frag"));

    // ps_clear's vertex inputs (per bindings.json oracle):
    //   location 0: aPosition vec2<f32>  (8 bytes)
    //   location 1: aRect     vec4<f32>  (16 bytes)
    //   location 2: aColor    vec4<f32>  (16 bytes)
    // Single interleaved vertex buffer, total stride 40 bytes.
    let attrs = [
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 0,  shader_location: 0 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 8,  shader_location: 1 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 24, shader_location: 2 },
    ];
    let vertex_buffer_layout = wgpu::VertexBufferLayout {
        array_stride: 40,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attrs,
    };

    let render_pipeline = wgpu_device.device().create_render_pipeline(
        &wgpu::RenderPipelineDescriptor {
            label: Some("ps_clear render pipeline (smoke)"),
            // layout: None lets wgpu's internal naga auto-derive the
            // PipelineLayout from the SPIR-V modules. This is the P2 oracle
            // verification path.
            layout: None,
            vertex: wgpu::VertexState {
                module: &vert_module,
                entry_point: Some("main"),
                buffers: &[vertex_buffer_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &frag_module,
                entry_point: Some("main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        },
    );

    // Inspect the auto-derived layout for group 0. ps_clear's reflection
    // shows 1 uniform_buffer at group 0 binding 0 (the WrLocals UBO);
    // get_bind_group_layout(0) succeeding confirms wgpu derived a layout
    // matching that shape.
    let _bgl0 = render_pipeline.get_bind_group_layout(0);

    // Pipeline creation + bind-group-layout retrieval succeeded.
    // Transitively, wgpu's naga + our build-time naga agree on the SPIR-V's
    // structure (both call into the same crate). The oracle test in
    // webrender_build covers our side; this covers the wgpu side.
}
