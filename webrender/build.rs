/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

extern crate webrender_build;

use std::borrow::Cow;
use std::env;
use std::fs::{canonicalize, read_dir, File};
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use webrender_build::shader::*;
use webrender_build::shader_features::{ShaderFeatureFlags, get_shader_features};

// glsopt is known to leak, but we don't particularly care.
#[no_mangle]
pub extern "C" fn __lsan_default_options() -> *const u8 {
    b"detect_leaks=0\0".as_ptr()
}

/// Compute the shader path for insertion into the include_str!() macro.
/// This makes for more compact generated code than inserting the literal
/// shader source into the generated file.
///
/// If someone is building on a network share, I'm sorry.
fn escape_include_path(path: &Path) -> String {
    let full_path = canonicalize(path).unwrap();
    let full_name = full_path.as_os_str().to_str().unwrap();
    let full_name = full_name.replace("\\\\?\\", "");
    let full_name = full_name.replace("\\", "/");

    full_name
}

fn write_unoptimized_shaders(
    mut glsl_files: Vec<PathBuf>,
    shader_file: &mut File,
) -> Result<(), std::io::Error> {
    writeln!(
        shader_file,
        "  pub static ref UNOPTIMIZED_SHADERS: HashMap<&'static str, SourceWithDigest> = {{"
    )?;
    writeln!(shader_file, "    let mut shaders = HashMap::new();")?;

    // Sort the file list so that the shaders.rs file is filled
    // deterministically.
    glsl_files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

    for glsl in glsl_files {
        // Compute the shader name.
        assert!(glsl.is_file());
        let shader_name = glsl.file_name().unwrap().to_str().unwrap();
        let shader_name = shader_name.replace(".glsl", "");

        // Compute a digest of the #include-expanded shader source. We store
        // this as a literal alongside the source string so that we don't need
        // to hash large strings at runtime.
        let mut hasher = DefaultHasher::new();
        let base = glsl.parent().unwrap();
        assert!(base.is_dir());
        ShaderSourceParser::new().parse(
            Cow::Owned(shader_source_from_file(&glsl)),
            &|f| Cow::Owned(shader_source_from_file(&base.join(&format!("{}.glsl", f)))),
            &mut |s| hasher.write(s.as_bytes()),
        );
        let digest: ProgramSourceDigest = hasher.into();

        writeln!(
            shader_file,
            "    shaders.insert(\"{}\", SourceWithDigest {{ source: include_str!(\"{}\"), digest: \"{}\"}});",
            shader_name,
            escape_include_path(&glsl),
            digest,
        )?;
    }
    writeln!(shader_file, "    shaders")?;
    writeln!(shader_file, "  }};")?;

    Ok(())
}

#[derive(Clone, Debug)]
struct ShaderOptimizationInput {
    shader_name: &'static str,
    config: String,
    gl_version: ShaderVersion,
}

#[derive(Debug)]
struct ShaderOptimizationOutput {
    full_shader_name: String,
    gl_version: ShaderVersion,
    vert_file_path: PathBuf,
    frag_file_path: PathBuf,
    digest: ProgramSourceDigest,
}

#[derive(Debug)]
struct ShaderOptimizationError {
    shader: ShaderOptimizationInput,
    message: String,
}

/// Prepends the line number to each line of a shader source.
fn enumerate_shader_source_lines(shader_src: &str) -> String {
    // For some reason the glsl-opt errors are offset by 1 compared
    // to the provided shader source string.
    let mut out = format!("0\t|");
    for (n, line) in shader_src.split('\n').enumerate() {
        let line_number = n + 1;
        out.push_str(&format!("{}\t|{}\n", line_number, line));
    }
    out
}

fn write_optimized_shaders(
    shader_dir: &Path,
    shader_file: &mut File,
    out_dir: &str,
) -> Result<(), std::io::Error> {
    writeln!(
        shader_file,
        "  pub static ref OPTIMIZED_SHADERS: HashMap<(ShaderVersion, &'static str), OptimizedSourceWithDigest> = {{"
    )?;
    writeln!(shader_file, "    let mut shaders = HashMap::new();")?;

    // The full set of optimized shaders can be quite large, so only optimize
    // for the GL version we expect to be used on the target platform. If a different GL
    // version is used we will simply fall back to the unoptimized shaders.
    let shader_versions = match env::var("CARGO_CFG_TARGET_OS").as_ref().map(|s| &**s) {
        Ok("android") | Ok("windows") => [ShaderVersion::Gles],
        _ => [ShaderVersion::Gl],
    };

    let mut shaders = Vec::default();
    for &gl_version in &shader_versions {
        let mut flags = ShaderFeatureFlags::all();
        if gl_version != ShaderVersion::Gl {
            flags.remove(ShaderFeatureFlags::GL);
        }
        if gl_version != ShaderVersion::Gles {
            flags.remove(ShaderFeatureFlags::GLES);
            flags.remove(ShaderFeatureFlags::TEXTURE_EXTERNAL);
        }
        if !matches!(
            env::var("CARGO_CFG_TARGET_OS").as_ref().map(|s| &**s),
            Ok("android")
        ) {
            flags.remove(ShaderFeatureFlags::TEXTURE_EXTERNAL_ESSL1);
        }
        // The optimizer cannot handle the required EXT_YUV_target extension
        flags.remove(ShaderFeatureFlags::TEXTURE_EXTERNAL_BT709);
        flags.remove(ShaderFeatureFlags::DITHERING);

        for (shader_name, configs) in get_shader_features(flags) {
            for config in configs {
                shaders.push(ShaderOptimizationInput {
                    shader_name,
                    config,
                    gl_version,
                });
            }
        }
    }

    let outputs = build_parallel::compile_objects::<_, _, ShaderOptimizationError, _>(
        &|shader: &ShaderOptimizationInput| {
            println!("Optimizing shader {:?}", shader);
            let target = match shader.gl_version {
                ShaderVersion::Gl => glslopt::Target::OpenGl,
                ShaderVersion::Gles => glslopt::Target::OpenGles30,
            };
            let glslopt_ctx = glslopt::Context::new(target);

            let features = shader
                .config
                .split(",")
                .filter(|f| !f.is_empty())
                .collect::<Vec<_>>();

            let (vert_src, frag_src) =
                build_shader_strings(shader.gl_version, &features, shader.shader_name, &|f| {
                    Cow::Owned(shader_source_from_file(
                        &shader_dir.join(&format!("{}.glsl", f)),
                    ))
                });

            let full_shader_name = if shader.config.is_empty() {
                shader.shader_name.to_string()
            } else {
                format!("{}_{}", shader.shader_name, shader.config.replace(",", "_"))
            };

            // Compute a digest of the optimized shader sources. We store this
            // as a literal alongside the source string so that we don't need
            // to hash large strings at runtime.
            let mut hasher = DefaultHasher::new();

            let [vert_file_path, frag_file_path] = [
                (glslopt::ShaderType::Vertex, vert_src, "vert"),
                (glslopt::ShaderType::Fragment, frag_src, "frag"),
            ]
            .map(|(shader_type, shader_src, extension)| {
                let output = glslopt_ctx.optimize(shader_type, shader_src.clone());
                if !output.get_status() {
                    let source = enumerate_shader_source_lines(&shader_src);
                    return Err(ShaderOptimizationError {
                        shader: shader.clone(),
                        message: format!("{}\n{}", source, output.get_log()),
                    });
                }

                let shader_path = Path::new(out_dir).join(format!(
                    "{}_{:?}.{}",
                    full_shader_name, shader.gl_version, extension
                ));
                write_optimized_shader_file(
                    &shader_path,
                    output.get_output().unwrap(),
                    &shader.shader_name,
                    &features,
                    &mut hasher,
                );
                Ok(shader_path)
            });

            let vert_file_path = vert_file_path?;
            let frag_file_path = frag_file_path?;

            println!("Finished optimizing shader {:?}", shader);

            Ok(ShaderOptimizationOutput {
                full_shader_name,
                gl_version: shader.gl_version,
                vert_file_path,
                frag_file_path,
                digest: hasher.into(),
            })
        },
        &shaders,
    );

    match outputs {
        Ok(mut outputs) => {
            // Sort the shader list so that the shaders.rs file is filled
            // deterministically.
            outputs.sort_by(|a, b| {
                (a.gl_version, a.full_shader_name.clone())
                    .cmp(&(b.gl_version, b.full_shader_name.clone()))
            });

            for shader in outputs {
                writeln!(
                    shader_file,
                    "    shaders.insert(({}, \"{}\"), OptimizedSourceWithDigest {{",
                    shader.gl_version.variant_name(),
                    shader.full_shader_name,
                )?;
                writeln!(
                    shader_file,
                    "        vert_source: include_str!(\"{}\"),",
                    escape_include_path(&shader.vert_file_path),
                )?;
                writeln!(
                    shader_file,
                    "        frag_source: include_str!(\"{}\"),",
                    escape_include_path(&shader.frag_file_path),
                )?;
                writeln!(shader_file, "        digest: \"{}\",", shader.digest)?;
                writeln!(shader_file, "    }});")?;
            }
        }
        Err(err) => match err {
            build_parallel::Error::BuildError(err) => {
                panic!("Error optimizing shader {:?}: {}", err.shader, err.message)
            }
            _ => panic!("Error optimizing shaders."),
        },
    }

    writeln!(shader_file, "    shaders")?;
    writeln!(shader_file, "  }};")?;

    Ok(())
}

fn write_optimized_shader_file(
    path: &Path,
    source: &str,
    shader_name: &str,
    features: &[&str],
    hasher: &mut DefaultHasher,
) {
    let mut file = File::create(&path).unwrap();
    for (line_number, line) in source.lines().enumerate() {
        // We embed the shader name and features as a comment in the
        // source to make debugging easier.
        // The #version directive must be on the first line so we insert
        // the extra information on the next line.
        if line_number == 1 {
            let prelude = format!("// {}\n// features: {:?}\n\n", shader_name, features);
            file.write_all(prelude.as_bytes()).unwrap();
            hasher.write(prelude.as_bytes());
        }
        file.write_all(line.as_bytes()).unwrap();
        file.write_all("\n".as_bytes()).unwrap();
        hasher.write(line.as_bytes());
        hasher.write("\n".as_bytes());
    }
}

/// Preprocess assembled WR GLSL source for naga's GLSL 4.50 frontend.
///
/// WR shaders are written for OpenGL 3.2 (#version 150) with GLES compatibility
/// patterns. naga's GLSL frontend only accepts #version 450. This function:
///
/// 1. Replaces the #version line with `#version 450` so naga accepts the source.
/// 2. Strips #extension directives — naga cannot handle unknown extensions even
///    in dead preprocessor branches.
/// 3. Strips standalone `precision ...;` statements — GLES-only, invalid in 4.50.
/// 4. Adds `layout(binding=N, set=0)` to each `uniform` resource declaration that
///    lacks an explicit binding — required by naga's Vulkan-style GLSL frontend.
///    Binding indices are assigned per unique variable name for stability across
///    #ifdef branches that redeclare the same sampler under different types.
#[cfg(feature = "wgpu_backend")]
fn preprocess_for_naga(src: &str) -> String {
    use std::collections::HashMap;

    let mut name_to_binding: HashMap<String, u32> = HashMap::new();
    let mut next_binding: u32 = 0;

    let mut lines = Vec::with_capacity(src.lines().count());
    for line in src.lines() {
        let trimmed = line.trim_start();
        // Strip inline // comment for structural checks.
        let code = match trimmed.find("//") {
            Some(i) => trimmed[..i].trim_end(),
            None => trimmed,
        };
        if trimmed.starts_with("#version") {
            // Replace with the version naga's GLSL frontend accepts.
            lines.push("#version 450".to_string());
        } else if trimmed.starts_with("#extension") {
            // Strip — naga rejects unknown extensions even in dead #ifdef blocks.
        } else if code.starts_with("precision ") && code.ends_with(';') {
            // Strip GLES-style precision statements — not valid in GLSL 4.50 core.
        } else if code.starts_with("uniform ") && code.ends_with(';')
            && !code.starts_with("uniform struct ")
        {
            // Add layout(binding=N, set=0) to resource declarations that lack one.
            // The last whitespace-delimited token before ';' is the variable name.
            let var_name = code.trim_end_matches(';')
                .split_whitespace()
                .last()
                .unwrap_or("unknown")
                .to_string();
            let binding = *name_to_binding.entry(var_name).or_insert_with(|| {
                let b = next_binding;
                next_binding += 1;
                b
            });
            let indent = &line[..line.len() - trimmed.len()];
            lines.push(format!("{}layout(binding = {}, set = 0) {}", indent, binding, trimmed));
        } else {
            lines.push(line.to_string());
        }
    }
    lines.join("\n")
}

/// Translate fully-assembled GLSL source to WGSL via naga 26.
/// Translate fully-assembled GLSL source to WGSL via naga 26.
/// Returns `Ok(wgsl)` on success, or `Err(diagnostic)` if naga rejects the shader.
/// Callers should emit `cargo:warning` for failures and skip the variant.
#[cfg(feature = "wgpu_backend")]
fn translate_to_wgsl(
    glsl: &str,
    stage: naga::ShaderStage,
    name: &str,
    config: &str,
) -> Result<String, String> {
    use naga::{
        back::wgsl,
        front::glsl,
        valid::{Capabilities, ValidationFlags, Validator},
    };
    let module = glsl::Frontend::default()
        .parse(&glsl::Options::from(stage), glsl)
        .map_err(|e| format!(
            "GLSL->naga parse failed [shader={} config={:?}]: {:?}", name, config, e
        ))?;
    let info = Validator::new(ValidationFlags::all(), Capabilities::all())
        .validate(&module)
        .map_err(|e| format!(
            "naga validation failed [shader={} config={:?}]: {:?}", name, config, e
        ))?;
    wgsl::write_string(&module, &info, wgsl::WriterFlags::empty()).map_err(|e| format!(
        "WGSL emit failed [shader={} config={:?}]: {:?}", name, config, e
    ))
}

/// Generate WGSL shaders for the wgpu backend and write the `WGSL_SHADERS`
/// lazy_static entry into `shader_file`.
#[cfg(feature = "wgpu_backend")]
fn write_wgsl_shaders(
    shader_dir: &Path,
    out_dir: &str,
    shader_file: &mut File,
) -> Result<(), std::io::Error> {
    use std::fs;
    use webrender_build::shader_features::wgpu_shader_feature_flags;

    let wgsl_dir = Path::new(out_dir).join("wgsl");
    fs::create_dir_all(&wgsl_dir)?;

    writeln!(
        shader_file,
        "  pub static ref WGSL_SHADERS: HashMap<(&'static str, &'static str), WgslShaderSource> = {{"
    )?;
    writeln!(shader_file, "    let mut shaders = HashMap::new();")?;

    let features = get_shader_features(wgpu_shader_feature_flags());

    // Sort for deterministic output.
    let mut sorted_features: Vec<(&str, Vec<String>)> = features
        .iter()
        .map(|(&name, configs)| (name, configs.clone()))
        .collect();
    sorted_features.sort_by_key(|(name, _)| *name);

    let mut entries: Vec<(String, String, PathBuf, PathBuf)> = Vec::new();
    let mut success_count: u32 = 0;
    let mut fail_count: u32 = 0;

    for (shader_name, configs) in &sorted_features {
        let mut sorted_configs = configs.clone();
        sorted_configs.sort();
        for config in &sorted_configs {
            let feature_list: Vec<&str> = config
                .split(',')
                .filter(|f| !f.is_empty())
                .collect();
            let (vert_glsl, frag_glsl) = build_shader_strings(
                ShaderVersion::Gl,
                &feature_list,
                shader_name,
                &|f| Cow::Owned(shader_source_from_file(&shader_dir.join(format!("{}.glsl", f)))),
            );

            // Preprocess: fix version, strip unknown #extension directives,
            // strip GLES precision statements.
            let vert_glsl = preprocess_for_naga(&vert_glsl);
            let frag_glsl = preprocess_for_naga(&frag_glsl);

            let vert_wgsl = translate_to_wgsl(
                &vert_glsl,
                naga::ShaderStage::Vertex,
                shader_name,
                config,
            );
            let frag_wgsl = translate_to_wgsl(
                &frag_glsl,
                naga::ShaderStage::Fragment,
                shader_name,
                config,
            );

            // Filesystem-safe key: replace commas with underscores.
            let safe_key = if config.is_empty() {
                shader_name.to_string()
            } else {
                format!("{}__{}", shader_name, config.replace(',', "_"))
            };

            match (vert_wgsl, frag_wgsl) {
                (Ok(vert), Ok(frag)) => {
                    let vert_path = wgsl_dir.join(format!("{}_vs.wgsl", safe_key));
                    let frag_path = wgsl_dir.join(format!("{}_fs.wgsl", safe_key));
                    fs::write(&vert_path, &vert)?;
                    fs::write(&frag_path, &frag)?;
                    entries.push((
                        shader_name.to_string(),
                        config.clone(),
                        vert_path,
                        frag_path,
                    ));
                    success_count += 1;
                }
                (vert_res, frag_res) => {
                    let msg = vert_res.err().or_else(|| frag_res.err()).unwrap_or_default();
                    println!(
                        "cargo:warning=WGSL translation skipped [{}#{}]: {}",
                        shader_name, config, msg
                    );
                    fail_count += 1;
                }
            }
        }
    }

    // Note: it is expected that many/all shaders fail in early stages because naga's
    // GLSL frontend requires Vulkan-style separate texture+sampler while WR uses
    // combined sampler2D.  Stage 4 will add the sampler2D→texture2D preprocessing
    // pass to unlock full translation.
    println!(
        "cargo:warning=WGSL translation: {}/{} variants succeeded (0 expected until Stage 4)",
        success_count,
        success_count + fail_count
    );

    for (name, config, vert_path, frag_path) in &entries {
        writeln!(
            shader_file,
            "    shaders.insert((\"{name}\", \"{config}\"), WgslShaderSource {{ \
                vert_source: include_str!(\"{vp}\"), \
                frag_source: include_str!(\"{fp}\") \
            }});",
            name = name,
            config = config,
            vp = escape_include_path(vert_path),
            fp = escape_include_path(frag_path),
        )?;
    }

    writeln!(shader_file, "    shaders")?;
    writeln!(shader_file, "  }};")?;

    Ok(())
}

/// Stub for GL builds: `write_wgsl_shaders` is never called in GL builds
/// but must exist so the call-site in `main()` compiles in both configs.
#[cfg(not(feature = "wgpu_backend"))]
fn write_wgsl_shaders(
    _shader_dir: &Path,
    _out_dir: &str,
    _shader_file: &mut File,
) -> Result<(), std::io::Error> {
    unreachable!()
}

fn main() -> Result<(), std::io::Error> {
    // Enforce that exactly one rendering backend is selected.
    let gl_backend = std::env::var("CARGO_FEATURE_GL_BACKEND").is_ok();
    let wgpu_backend = std::env::var("CARGO_FEATURE_WGPU_BACKEND").is_ok();
    if gl_backend && wgpu_backend {
        panic!("gl_backend and wgpu_backend are mutually exclusive; enable exactly one");
    }
    if !gl_backend && !wgpu_backend {
        panic!("exactly one of gl_backend or wgpu_backend must be enabled");
    }

    let out_dir = env::var("OUT_DIR").unwrap_or("out".to_owned());

    let shaders_file_path = Path::new(&out_dir).join("shaders.rs");
    let mut glsl_files = vec![];

    println!("cargo:rerun-if-changed=res");
    let res_dir = Path::new("res");
    for entry in read_dir(res_dir)? {
        let entry = entry?;
        let path = entry.path();

        if entry.file_name().to_str().unwrap().ends_with(".glsl") {
            println!("cargo:rerun-if-changed={}", path.display());
            glsl_files.push(path.to_owned());
        }
    }

    let mut shader_file = File::create(shaders_file_path)?;

    writeln!(shader_file, "/// AUTO GENERATED BY build.rs\n")?;
    writeln!(shader_file, "use std::collections::HashMap;\n")?;
    writeln!(shader_file, "use webrender_build::shader::ShaderVersion;\n")?;
    writeln!(shader_file, "pub struct SourceWithDigest {{")?;
    writeln!(shader_file, "    pub source: &'static str,")?;
    writeln!(shader_file, "    pub digest: &'static str,")?;
    writeln!(shader_file, "}}\n")?;
    writeln!(shader_file, "pub struct OptimizedSourceWithDigest {{")?;
    writeln!(shader_file, "    pub vert_source: &'static str,")?;
    writeln!(shader_file, "    pub frag_source: &'static str,")?;
    writeln!(shader_file, "    pub digest: &'static str,")?;
    writeln!(shader_file, "}}\n")?;
    if !gl_backend {
        writeln!(shader_file, "pub struct WgslShaderSource {{")?;
        writeln!(shader_file, "    pub vert_source: &'static str,")?;
        writeln!(shader_file, "    pub frag_source: &'static str,")?;
        writeln!(shader_file, "}}\n")?;
    }
    writeln!(shader_file, "lazy_static! {{")?;

    if gl_backend {
        write_unoptimized_shaders(glsl_files, &mut shader_file)?;
        writeln!(shader_file, "")?;
        write_optimized_shaders(&res_dir, &mut shader_file, &out_dir)?;
    } else {
        // wgpu_backend: emit empty GL maps; generate WGSL shaders via naga.
        writeln!(shader_file, "  pub static ref UNOPTIMIZED_SHADERS: HashMap<&'static str, SourceWithDigest> = HashMap::new();")?;
        writeln!(shader_file, "  pub static ref OPTIMIZED_SHADERS: HashMap<(ShaderVersion, &'static str), OptimizedSourceWithDigest> = HashMap::new();")?;
        write_wgsl_shaders(&res_dir, &out_dir, &mut shader_file)?;
    }
    writeln!(shader_file, "}}")?;

    Ok(())
}
