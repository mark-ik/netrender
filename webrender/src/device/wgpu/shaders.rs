/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `GpuShaders` impl for `WgpuDevice` (P4i).
//!
//! Loads committed SPIR-V from `webrender/res/spirv/{stem}.{stage}.spv`,
//! creates wgpu `ShaderModule`s, builds a `RenderPipeline` via the
//! P3 vertex schema adapter and `layout: None` (lets wgpu's internal
//! naga auto-derive the bind-group layout from SPIR-V reflection —
//! works for the 97/125 reflectable stages after the P2-spike fixes).

use std::cell::RefCell;

use super::super::traits::GpuShaders;
use super::super::types::{ShaderError, TextureSlot, VertexDescriptor};
use super::types::{WgpuProgram, WgpuUniformLocation};
use super::vertex_layout::WgpuVertexLayouts;
use super::WgpuDevice;

/// Builds the SPIR-V artifact stem from a base shader name + features.
/// Mirrors `gen_spirv.rs`'s naming exactly: features sorted, then
/// underscore-joined onto the base. Empty feature list = bare base name.
pub(super) fn shader_stem(base: &str, features: &[&str]) -> String {
    if features.is_empty() {
        base.to_string()
    } else {
        let mut sorted: Vec<&str> = features.to_vec();
        sorted.sort_unstable();
        format!("{}_{}", base, sorted.join("_"))
    }
}

/// Reads a committed `.spv` artifact for the given stem + stage.
/// Path is `{webrender crate manifest dir}/res/spirv/{stem}.{stage}.spv`.
/// Works for dev/test builds; production deployments should use
/// `include_bytes!` via a build.rs-generated lookup table (TODO).
pub(super) fn load_committed_spv(stem: &str, stage: &str) -> Result<Vec<u8>, std::io::Error> {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("res");
    path.push("spirv");
    path.push(format!("{}.{}.spv", stem, stage));
    std::fs::read(path)
}

impl GpuShaders for WgpuDevice {
    type Program = WgpuProgram;
    type UniformLocation = WgpuUniformLocation;

    fn create_program(
        &mut self,
        base_filename: &'static str,
        features: &[&'static str],
    ) -> Result<Self::Program, ShaderError> {
        let stem = shader_stem(base_filename, features);
        let vert_bytes = load_committed_spv(&stem, "vert").map_err(|e| {
            ShaderError::Compilation(stem.clone(), format!("load vert: {}", e))
        })?;
        let frag_bytes = load_committed_spv(&stem, "frag").map_err(|e| {
            ShaderError::Compilation(stem.clone(), format!("load frag: {}", e))
        })?;

        let vert_module =
            self.create_shader_module_from_spv(Some(&format!("{}.vert", stem)), &vert_bytes);
        let frag_module =
            self.create_shader_module_from_spv(Some(&format!("{}.frag", stem)), &frag_bytes);

        // WrLocals UBO is mat4 (64 bytes). Sized for that even when the
        // particular shader doesn't sample uTransform — wasted 64 bytes
        // per program is negligible.
        let uniform_buffer = self.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("WgpuProgram[{}] uTransform UBO", stem)),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(WgpuProgram {
            vert_module,
            frag_module,
            pipeline: RefCell::new(None),
            uniform_buffer,
            stem,
        })
    }

    fn create_program_linked(
        &mut self,
        base_filename: &'static str,
        features: &[&'static str],
        descriptor: &VertexDescriptor,
    ) -> Result<Self::Program, ShaderError> {
        let mut program = self.create_program(base_filename, features)?;
        self.link_program(&mut program, descriptor)?;
        Ok(program)
    }

    fn link_program(
        &mut self,
        program: &mut Self::Program,
        descriptor: &VertexDescriptor,
    ) -> Result<(), ShaderError> {
        // Build pipeline using the descriptor's vertex layout.
        // `layout: None` lets wgpu's internal naga auto-derive the
        // PipelineLayout from SPIR-V (works for 97/125 stages post-P2).
        let layouts = WgpuVertexLayouts::from_descriptor(descriptor);
        let buffers = layouts.buffers();

        // Filter empty layouts for shaders without per-vertex inputs
        // (rare; mostly the cs_* compute-style render-target shaders).
        let nonempty: Vec<wgpu::VertexBufferLayout<'_>> = buffers
            .iter()
            .filter(|b| !b.attributes.is_empty())
            .cloned()
            .collect();

        // Color target format: BGRA8 matches our preferred_color_formats().
        // Per-target pipeline cache (for RGBA8 etc.) is a P5+ concern.
        let pipeline = self.device().create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some(&format!("WgpuProgram[{}] pipeline", program.stem)),
                layout: None,
                vertex: wgpu::VertexState {
                    module: &program.vert_module,
                    entry_point: Some("main"),
                    buffers: &nonempty,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &program.frag_module,
                    entry_point: Some("main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Bgra8Unorm,
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
        *program.pipeline.borrow_mut() = Some(pipeline);
        Ok(())
    }

    fn delete_program(&mut self, _program: Self::Program) {
        // Drop releases the modules + pipeline + uniform buffer.
    }

    fn get_uniform_location(
        &self,
        _program: &Self::Program,
        _name: &str,
    ) -> Self::UniformLocation {
        // wgpu has no name-based uniform location lookup; uniforms are
        // bound by group/binding index. Return a placeholder; callers
        // that store the result and pass it back to set_uniforms get
        // the WrLocals UBO write regardless of the name.
        WgpuUniformLocation
    }

    fn bind_shader_samplers<S>(
        &mut self,
        _program: &Self::Program,
        _bindings: &[(&'static str, S)],
    )
    where
        S: Into<TextureSlot> + Copy,
    {
        // GL: sets sampler uniform values at named locations to texture
        // unit indices. In wgpu, sampler-to-binding mapping is baked
        // into the pipeline layout (auto-derived from SPIR-V). The
        // renderer's BindGroup construction picks up the correct
        // bindings by index at draw time. No-op here.
    }
}
