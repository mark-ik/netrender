/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Reflects WebRender's committed SPIR-V corpus and emits a bindings
//! manifest describing each shader's resource bindings + vertex inputs.
//!
//! Role (per `notes/2026-04-30_wgpu_device_plan.md` A1): verification
//! oracle for the wgpu backend's runtime auto-derived layouts. The wgpu
//! device passes `layout: None` to `create_render_pipeline` and lets
//! wgpu's internal naga reflect each `ShaderModule(SpirV)` to derive a
//! `PipelineLayout`. This binary runs the same reflection at build time
//! and emits a golden manifest. CI / a webrender_build test asserts that
//! the runtime-derived layouts match this golden — any drift surfaces in
//! review rather than as a runtime panic.
//!
//! Run from the workspace root:
//!
//!   cargo run -p webrender_build --features shader-reflect --bin reflect_spirv \
//!       [spirv_dir] [out_path]
//!
//! Defaults: spirv_dir = webrender/res/spirv, out_path = webrender/res/spirv/bindings.json
//!
//! Regenerate whenever webrender/res/spirv/*.spv changes
//! (i.e. after running gen_spirv).

use naga::front::spv;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Top-level manifest: shader name -> per-stage reflection result.
/// Sorted (BTreeMap) for deterministic byte-identical output.
#[derive(Debug, Default, Serialize, Deserialize)]
struct BindingsManifest {
    /// Map from artifact stem (e.g. "ps_clear", "brush_solid_ALPHA_PASS")
    /// to vert/frag stage info.
    shaders: BTreeMap<String, ShaderEntry>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ShaderEntry {
    vert: Option<StageReflection>,
    frag: Option<StageReflection>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StageReflection {
    /// Bind-group resources sorted by (group, binding).
    bindings: Vec<BindingEntry>,
    /// Vertex input attributes (vertex stage only; empty for fragment).
    vertex_inputs: Vec<VertexInputEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BindingEntry {
    name: String,
    group: u32,
    binding: u32,
    /// Resource kind: "uniform_buffer", "sampled_texture", "sampler",
    /// "storage_buffer", etc.
    kind: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct VertexInputEntry {
    name: String,
    location: u32,
    /// e.g. "vec4<f32>", "vec2<u32>" — naga's scalar+vector type form.
    ty: String,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let spirv_dir = PathBuf::from(args.next().unwrap_or_else(|| "webrender/res/spirv".into()));
    let out_path = PathBuf::from(
        args.next()
            .unwrap_or_else(|| "webrender/res/spirv/bindings.json".into()),
    );

    assert!(
        spirv_dir.exists(),
        "spirv dir not found: {}",
        spirv_dir.display()
    );

    let mut manifest = BindingsManifest::default();
    let mut errors: Vec<String> = Vec::new();

    let mut entries: Vec<_> = fs::read_dir(&spirv_dir)
        .expect("read spirv dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("spv"))
        .collect();
    entries.sort();

    for path in &entries {
        let file_stem = path
            .file_name()
            .and_then(|s| s.to_str())
            .expect("utf8 path");
        // Names look like "ps_clear.vert.spv" or "brush_solid_ALPHA_PASS.frag.spv".
        // Strip ".spv", split on the last '.' to get (stem, "vert"|"frag").
        let without_spv = file_stem.trim_end_matches(".spv");
        let (shader_name, stage) = match without_spv.rsplit_once('.') {
            Some(parts) => parts,
            None => {
                errors.push(format!("unexpected name (no stage): {}", file_stem));
                continue;
            }
        };

        let bytes = fs::read(path).expect("read spv");
        match reflect_module(&bytes, stage == "vert") {
            Ok(reflection) => {
                let entry = manifest
                    .shaders
                    .entry(shader_name.to_string())
                    .or_default();
                match stage {
                    "vert" => entry.vert = Some(reflection),
                    "frag" => entry.frag = Some(reflection),
                    other => errors.push(format!("unknown stage {} in {}", other, file_stem)),
                }
                println!("  ok  {}", file_stem);
            }
            Err(e) => {
                let msg = format!("FAIL {} -- {}", file_stem, e);
                eprintln!("  {}", msg);
                errors.push(msg);
            }
        }
    }

    let json = serde_json::to_string_pretty(&manifest).expect("serialize");
    fs::write(&out_path, json).unwrap_or_else(|e| panic!("write {}: {}", out_path.display(), e));

    println!(
        "\n{} shader stages reflected, {} errors. wrote {}",
        manifest
            .shaders
            .values()
            .map(|s| s.vert.is_some() as usize + s.frag.is_some() as usize)
            .sum::<usize>(),
        errors.len(),
        out_path.display(),
    );
    if !errors.is_empty() {
        std::process::exit(1);
    }
}

fn reflect_module(spirv: &[u8], is_vertex_stage: bool) -> Result<StageReflection, String> {
    if spirv.len() % 4 != 0 {
        return Err(format!("spirv length not /4: {}", spirv.len()));
    }
    let module = spv::parse_u8_slice(spirv, &spv::Options::default())
        .map_err(|e| format!("naga parse: {:?}", e))?;

    // Bindings: walk module.global_variables, filter those with binding info.
    let mut bindings: Vec<BindingEntry> = module
        .global_variables
        .iter()
        .filter_map(|(_handle, gv)| {
            let binding = gv.binding.as_ref()?;
            Some(BindingEntry {
                name: gv.name.clone().unwrap_or_default(),
                group: binding.group,
                binding: binding.binding,
                kind: classify_global(&module, gv),
            })
        })
        .collect();
    bindings.sort_by_key(|b| (b.group, b.binding));

    // Vertex inputs: only meaningful for vertex stage.
    let vertex_inputs: Vec<VertexInputEntry> = if is_vertex_stage {
        module
            .entry_points
            .iter()
            .find(|ep| matches!(ep.stage, naga::ShaderStage::Vertex))
            .map(|ep| {
                let mut inputs: Vec<VertexInputEntry> = ep
                    .function
                    .arguments
                    .iter()
                    .filter_map(|arg| {
                        let binding = arg.binding.as_ref()?;
                        let location = match binding {
                            naga::Binding::Location { location, .. } => *location,
                            _ => return None,
                        };
                        Some(VertexInputEntry {
                            name: arg.name.clone().unwrap_or_default(),
                            location,
                            ty: format_type(&module, arg.ty),
                        })
                    })
                    .collect();
                inputs.sort_by_key(|v| v.location);
                inputs
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    Ok(StageReflection {
        bindings,
        vertex_inputs,
    })
}

fn classify_global(module: &naga::Module, gv: &naga::GlobalVariable) -> String {
    let inner = &module.types[gv.ty].inner;
    match inner {
        naga::TypeInner::Image { class, .. } => match class {
            naga::ImageClass::Sampled { .. } => "sampled_texture".to_string(),
            naga::ImageClass::Depth { .. } => "depth_texture".to_string(),
            naga::ImageClass::Storage { .. } => "storage_texture".to_string(),
        },
        naga::TypeInner::Sampler { .. } => "sampler".to_string(),
        naga::TypeInner::Struct { .. } => match gv.space {
            naga::AddressSpace::Uniform => "uniform_buffer".to_string(),
            naga::AddressSpace::Storage { .. } => "storage_buffer".to_string(),
            other => format!("struct_in_{:?}", other),
        },
        other => format!("{:?}", other),
    }
}

fn format_type(module: &naga::Module, ty: naga::Handle<naga::Type>) -> String {
    let inner = &module.types[ty].inner;
    match inner {
        naga::TypeInner::Scalar(s) => format_scalar(*s),
        naga::TypeInner::Vector { size, scalar } => {
            format!("vec{}<{}>", *size as u32, format_scalar(*scalar))
        }
        naga::TypeInner::Matrix { columns, rows, scalar } => format!(
            "mat{}x{}<{}>",
            *columns as u32,
            *rows as u32,
            format_scalar(*scalar)
        ),
        other => format!("{:?}", other),
    }
}

fn format_scalar(s: naga::Scalar) -> String {
    let kind = match s.kind {
        naga::ScalarKind::Sint => "i",
        naga::ScalarKind::Uint => "u",
        naga::ScalarKind::Float => "f",
        naga::ScalarKind::Bool => "bool",
        naga::ScalarKind::AbstractInt => "abstract_int",
        naga::ScalarKind::AbstractFloat => "abstract_float",
    };
    if s.kind == naga::ScalarKind::Bool {
        kind.to_string()
    } else {
        format!("{}{}", kind, s.width * 8)
    }
}
