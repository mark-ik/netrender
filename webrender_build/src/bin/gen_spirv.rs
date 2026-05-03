/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Compiles WebRender's GLSL shader corpus to SPIR-V artifacts for the wgpu device.
//!
//! Run from the workspace root:
//!
//!   cargo run -p webrender_build --features shader-gen --bin gen_spirv \
//!       [res_dir] [out_dir]
//!
//! Defaults: res_dir = webrender/res, out_dir = webrender/res/spirv
//!
//! Artifacts are named {shader}[_{FEATURE,LIST}].{vert,frag}.spv
//! Empty feature sets produce {shader}.vert.spv / {shader}.frag.spv
//!
//! Regenerate whenever webrender/res/*.glsl changes.

use shaderc::{CompileOptions, Compiler, ShaderKind, TargetEnv};
use std::borrow::Cow;
use std::fs;
use std::path::PathBuf;
use webrender_build::shader::{build_shader_strings, ShaderVersion};
use webrender_build::shader_features::{get_shader_features, ShaderFeatureFlags};

fn main() {
    let mut args = std::env::args().skip(1);
    let res_dir = PathBuf::from(args.next().unwrap_or_else(|| "webrender/res".into()));
    let out_dir = PathBuf::from(args.next().unwrap_or_else(|| "webrender/res/spirv".into()));

    assert!(res_dir.exists(), "res dir not found: {}", res_dir.display());
    fs::create_dir_all(&out_dir).expect("failed to create output dir");

    let compiler = Compiler::new().expect("shaderc unavailable");

    let get_source = {
        let dir = res_dir.clone();
        move |name: &str| -> Cow<'static, str> {
            let src = fs::read_to_string(dir.join(format!("{}.glsl", name)))
                .unwrap_or_else(|_| panic!("shader not found: {}", name));
            Cow::Owned(src)
        }
    };

    let features = get_shader_features(ShaderFeatureFlags::GL);

    let mut ok = 0usize;
    let mut errors: Vec<String> = Vec::new();

    // Sort for deterministic output.
    let mut sorted: Vec<_> = features.iter().collect();
    sorted.sort_by_key(|(name, _)| *name);

    for (shader_name, configs) in &sorted {
        for config in *configs {
            let feature_list: Vec<&str> = if config.is_empty() {
                vec![]
            } else {
                config.split(',').collect()
            };

            let (vert_glsl, frag_glsl) = build_shader_strings(
                ShaderVersion::Gl,
                &feature_list,
                shader_name,
                &get_source,
            );

            let stem = if config.is_empty() {
                shader_name.to_string()
            } else {
                // Replace commas with underscores for filesystem-safe names.
                format!("{}_{}", shader_name, config.replace(',', "_"))
            };

            let vert_glsl = preprocess_for_vulkan(&vert_glsl);
            let frag_glsl = preprocess_for_vulkan(&frag_glsl);

            for (glsl, kind, ext) in [
                (&vert_glsl, ShaderKind::Vertex, "vert"),
                (&frag_glsl, ShaderKind::Fragment, "frag"),
            ] {
                let filename = format!("{}.{}.spv", stem, ext);
                // Per-stage DescriptorSet assignment: vert stage uses
                // set 0, frag uses set 1. Prevents binding collisions
                // when both stages declare different resources at the
                // same binding number — wgpu requires layout consistency
                // within a single descriptor set across stages of one
                // pipeline.
                let stage_set: u32 = match ext {
                    "vert" => 0,
                    "frag" => 1,
                    _ => 0,
                };
                match compile(&compiler, glsl, kind, shader_name) {
                    Ok(spirv) => match split_combined_samplers(&spirv) {
                        Ok(split) => {
                            let bound = assign_sequential_bindings(&split, stage_set);
                            let out_path = out_dir.join(&filename);
                            fs::write(&out_path, &bound).unwrap_or_else(|e| {
                                panic!("write {}: {}", out_path.display(), e)
                            });
                            println!("  ok  {} ({} bytes)", filename, bound.len());
                            ok += 1;
                        }
                        Err(e) => {
                            let msg = format!("FAIL {} -- spirv-opt: {}", filename, e);
                            eprintln!("  {}", msg);
                            errors.push(msg);
                        }
                    },
                    Err(e) => {
                        let msg = format!("FAIL {} -- {}", filename, e);
                        eprintln!("  {}", msg);
                        errors.push(msg);
                    }
                }
            }
        }
    }

    println!("\n{} artifacts written, {} errors", ok, errors.len());
    if !errors.is_empty() {
        std::process::exit(1);
    }
}

/// Wraps bare non-opaque uniforms in UBO blocks so Vulkan SPIR-V accepts them.
/// glslang rejects `uniform mat4 uTransform;` outside a block when targeting Vulkan;
/// wrapping it makes it a proper UBO while preserving the member name for SPIR-V reflection.
fn preprocess_for_vulkan(glsl: &str) -> String {
    // Upgrade version directive to 460 for Vulkan target. glslang has quirks
    // with older versions (150) when TargetEnv::Vulkan is set — some constructs
    // that are syntactically valid in 150 are rejected as if they're version-gated.
    let glsl = if let Some(nl) = glsl.find('\n') {
        let first = &glsl[..nl];
        let rest = &glsl[nl..];
        if first.starts_with("#version ") {
            format!("#version 460{}", rest)
        } else {
            glsl.to_string()
        }
    } else {
        glsl.to_string()
    };

    // uTransform is the only non-opaque (non-sampler) uniform in WebRender's GL corpus.
    // Vulkan GLSL requires non-opaque uniforms to live inside a UBO block.
    glsl.replace(
        "uniform mat4 uTransform;",
        "uniform WrLocals { mat4 uTransform; };",
    )
}

/// Walks SPIR-V words and rewrites `OpDecorate <id> DescriptorSet <set>`
/// to a fixed `stage_set` value, then reassigns `OpDecorate <id> Binding
/// <n>` literals so each variable gets a unique sequential binding within
/// that set, in declaration-encounter order.
///
/// Two reasons for the dual rewrite:
///
/// 1. shaderc/glslang's `set_auto_bind_uniforms` does not actually
///    distribute bindings for our combined-sampler GLSL (verified
///    empirically: per-kind `set_binding_base` had zero effect, all
///    variables still came out at binding 0).
/// 2. wgpu requires bind-group layouts to be consistent across stages of
///    a single pipeline. If vert.spv and frag.spv both renumber bindings
///    starting from 0 within set 0, they collide (e.g. vert binding 0 =
///    UBO, frag binding 0 = sampled_texture). Putting each stage in its
///    own descriptor set (vert→0, frag→1) avoids the collision: each
///    stage's bindings are isolated.
///
/// Pure SPIR-V byte transform — doesn't touch any other instruction.
/// Runs after `--split-combined-image-sampler` so each split image+sampler
/// pair gets its own distinct binding number within the stage set.
fn assign_sequential_bindings(spirv: &[u8], stage_set: u32) -> Vec<u8> {
    use std::collections::HashMap;

    let mut words: Vec<u32> = spirv
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    // SPIR-V opcode 71 = OpDecorate. Decoration enum: 33 = Binding,
    // 34 = DescriptorSet. OpDecorate format: header | target_id |
    // decoration | [operands]. For Binding/DescriptorSet, operand[2]
    // (== words[idx+3]) is the literal value.

    let mut id_to_set: HashMap<u32, u32> = HashMap::new();
    let mut binding_decoration_indices: Vec<(usize, u32)> = Vec::new();
    let mut set_decoration_indices: Vec<usize> = Vec::new();

    let mut idx = 5; // skip header
    while idx < words.len() {
        let header = words[idx];
        let opcode = header & 0xffff;
        let wc = (header >> 16) as usize;
        if wc == 0 {
            break;
        }
        if opcode == 71 && wc >= 4 {
            let target = words[idx + 1];
            let decoration = words[idx + 2];
            match decoration {
                33 => binding_decoration_indices.push((idx, target)),
                34 => {
                    set_decoration_indices.push(idx);
                    // Track the original set so later binding lookup works
                    // (we'll rewrite to stage_set after).
                    id_to_set.insert(target, words[idx + 3]);
                }
                _ => {}
            }
        }
        idx += wc;
    }

    // Rewrite all DescriptorSet decorations to stage_set.
    for set_idx in &set_decoration_indices {
        words[set_idx + 3] = stage_set;
    }

    // Reassign bindings per (original) set group, then place in stage_set.
    // Since we're putting everything in stage_set, "per set" collapses to
    // a single counter — but iterating in encounter order preserves the
    // declaration-order assignment (matches GL device's expectation that
    // first-declared resources get the lowest binding numbers).
    let mut next_binding: u32 = 0;
    for (word_idx, _target_id) in binding_decoration_indices {
        words[word_idx + 3] = next_binding;
        next_binding += 1;
    }

    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
}

/// Runs `spirv-opt --split-combined-image-sampler` on the given SPIR-V bytes
/// to convert glslang's combined-sampler output (Pattern B) into the
/// separated form (Pattern A: `OpLoad image` + `OpLoad sampler` +
/// `OpSampledImage`) that naga's SPIR-V frontend expects. Without this
/// pass, naga's `parse_image_sample` fails with `InvalidId` on every
/// shader that calls `texture(sampler2D_var, ...)`.
///
/// Requires `spirv-opt` on PATH (install via VulkanSDK or build
/// SPIRV-Tools from source). Build-time-only requirement; the resulting
/// `.spv` files are committed and consumers don't need spirv-opt.
fn split_combined_samplers(spirv: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("spirv-opt")
        .args(["--split-combined-image-sampler", "-", "-o", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn spirv-opt: {} (is it on PATH?)", e))?;

    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(spirv)
        .map_err(|e| format!("write stdin: {}", e))?;

    let output = child
        .wait_with_output()
        .map_err(|e| format!("wait spirv-opt: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "spirv-opt exit {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(output.stdout)
}

fn compile(
    compiler: &Compiler,
    glsl: &str,
    kind: ShaderKind,
    name: &str,
) -> Result<Vec<u8>, String> {
    let mut opts = CompileOptions::new().ok_or("CompileOptions::new() failed")?;

    // Target Vulkan 1.1 SPIR-V so wgpu can consume it on all backends.
    opts.set_target_env(TargetEnv::Vulkan, shaderc::EnvVersion::Vulkan1_1 as u32);

    // WebRender's GLSL uses GL-style named uniform binding (no layout qualifiers).
    // Tell glslang to assign binding indices automatically when targeting Vulkan.
    // Note: empirically this leaves all bindings at 0 for our combined-sampler
    // GLSL — shaderc/glslang's auto-bind appears to no-op on combined samplers.
    // We post-process via assign_sequential_bindings() below to actually
    // distribute binding numbers.
    opts.set_auto_bind_uniforms(true);

    // WebRender's varyings and attributes have no explicit location qualifiers.
    opts.set_auto_map_locations(true);

    compiler
        .compile_into_spirv(glsl, kind, &format!("{}.glsl", name), "main", Some(&opts))
        .map(|artifact| artifact.as_binary_u8().to_vec())
        .map_err(|e| e.to_string())
}
