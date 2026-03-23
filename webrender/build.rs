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

/// Preprocess assembled WR GLSL source for naga's Vulkan-style GLSL 4.50 frontend.
///
/// naga's GLSL frontend targets Vulkan GLSL, which differs from desktop GL GLSL in
/// several ways that require source-level patching before naga sees the source.
///
/// Transformations applied:
///
/// 1. `#version` → `#version 450`  (naga requires 450)
/// 2. `#extension` directives stripped  (naga rejects unknown extensions)
/// 3. `precision ...;` statements stripped  (GLES-only, invalid in 4.50 core)
/// 4. Combined sampler uniforms split into texture + global sampler:
///    `uniform sampler2D sName;` → `layout(binding=N, set=0) uniform texture2D sName;`
///    A single shared `layout(binding=0, set=1) uniform sampler global_sampler;` is
///    injected once, covering all texture samples.
/// 5. Remaining uniforms get `layout(binding=N, set=0)`.
/// 6. `texture(sName, coord)` call sites get the required combined-type wrapper:
///    → `texture(sampler2D(sName, global_sampler), coord)`
/// 7. The TEX_SAMPLE macro body is rewritten to use the split types so any
///    indirect `texture(sampler, ...)` in macros works correctly.
/// 8. Vertex stage: `in`/`attribute` declarations without `layout(location=N)` get
///    sequential location qualifiers, fixing the BindingCollision validation error.
/// 9. Fragment stage: `out vec4` declarations without `layout(location=N)` get
///    `layout(location=0)`.
///
/// Precision qualifiers (`highp`, `mediump`, `lowp`) are stripped from every
/// emitted declaration: they are valid in GLES but illegal in GLSL 450 core.
#[cfg(feature = "wgpu_backend")]
fn strip_precision(s: &str) -> String {
    // Precision qualifiers always precede a type token, so they are always
    // followed by a space.  A simple token-delete is safe here.
    let mut out = s.to_string();
    for q in &["highp ", "mediump ", "lowp "] {
        while out.contains(q) {
            out = out.replace(q, "");
        }
    }
    // Also strip precision macros whose #define expansion would reintroduce
    // a precision qualifier inside naga's preprocessor.  After stripping,
    // `#define YUV_PRECISION highp` becomes `#define YUV_PRECISION` (empty),
    // and all uses expand to nothing.
    for q in &[" highp\n", " mediump\n", " lowp\n"] {
        while out.contains(q) {
            out = out.replace(q, "\n");
        }
    }
    // Handle end-of-file (no trailing newline).
    for q in &[" highp", " mediump", " lowp"] {
        if out.ends_with(q) {
            let new_len = out.len() - q.len();
            out.truncate(new_len);
        }
    }
    out
}

/// Tokens that begin control-flow statements or storage-qualifier declarations
/// rather than user-defined function definitions.
#[cfg(feature = "wgpu_backend")]
const NOT_FUNC_START: &[&str] = &[
    "if", "else", "for", "while", "do", "switch", "return",
    "struct", "uniform", "in", "out", "varying", "attribute",
    "flat", "smooth", "noperspective", "layout", "PER_INSTANCE",
];

/// Replace whole-word occurrences of `old` with `new_val` in a string.
/// Word boundaries are non-alphanumeric, non-underscore characters.
#[cfg(feature = "wgpu_backend")]
fn replace_word(s: &str, old: &str, new_val: &str) -> String {
    if old.is_empty() { return s.to_string(); }
    let mut result = String::with_capacity(s.len() + 32);
    let chars: Vec<char> = s.chars().collect();
    let old_chars: Vec<char> = old.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if i + old_chars.len() <= chars.len()
            && chars[i..i + old_chars.len()] == old_chars[..]
        {
            // Check word boundary before.
            let before_ok = i == 0 || {
                let c = chars[i - 1];
                !(c.is_alphanumeric() || c == '_')
            };
            // Check word boundary after.
            let after_ok = i + old_chars.len() >= chars.len() || {
                let c = chars[i + old_chars.len()];
                !(c.is_alphanumeric() || c == '_')
            };
            if before_ok && after_ok {
                result.push_str(new_val);
                i += old_chars.len();
                continue;
            }
        }
        result.push(chars[i]);
        i += 1;
    }
    result
}

/// Strip the trailing `// comment` portion of a GLSL source line.
#[cfg(feature = "wgpu_backend")]
#[inline]
fn strip_glsl_comment(s: &str) -> &str {
    match s.find("//") { Some(i) => s[..i].trim_end(), None => s }
}

/// For each function PROTOTYPE found in the assembled GLSL source,
/// find the corresponding DEFINITION that appears AFTER the prototype.
/// Collect the definition AND all non-function code between the last
/// driver-section function and the definition (the "specific shader
/// preamble": struct definitions, helper functions, varyings) and move
/// the entire block to the prototype position.
///
/// This satisfies naga's requirement that callee function handles are
/// always lower than caller handles.  WebRender's shader assembly puts
/// "driver" code (brush.glsl / ps_quad.glsl) before "specific" code
/// (brush_solid.glsl, etc.) via `#include`, so definitions referenced
/// by the prototyped function always have higher handles without this
/// reordering.
#[cfg(feature = "wgpu_backend")]
fn move_definitions_before_prototypes(src: &str) -> String {
    use std::collections::HashMap;

    let lines: Vec<&str> = src.lines().collect();
    let n = lines.len();

    // ── Pass 1: collect prototype chunks and definition chunks ────────────────
    // Each item: (function_name, start_line_incl, end_line_excl)
    let mut prototypes: Vec<(String, usize, usize)> = Vec::new();
    let mut defs:       Vec<(String, usize, usize)> = Vec::new();

    let mut block_depth: i32 = 0;
    let mut i = 0;

    while i < n {
        let code = strip_glsl_comment(lines[i].trim());

        if block_depth > 0 {
            for c in code.chars() {
                match c { '{' => block_depth += 1, '}' => block_depth -= 1, _ => {} }
            }
            i += 1;
            continue;
        }

        if code.is_empty() || code.starts_with('#') || code.starts_with("//") {
            i += 1;
            continue;
        }

        let first = code.split_whitespace().next().unwrap_or("");

        if NOT_FUNC_START.contains(&first) {
            for c in code.chars() {
                match c { '{' => block_depth += 1, '}' => block_depth -= 1, _ => {} }
            }
            i += 1;
            continue;
        }

        if !code.contains('(') {
            for c in code.chars() {
                match c { '{' => block_depth += 1, '}' => block_depth -= 1, _ => {} }
            }
            i += 1;
            continue;
        }

        {
            let paren = code.find('(').unwrap();
            if code[..paren].contains('=') {
                for c in code.chars() {
                    match c { '{' => block_depth += 1, '}' => block_depth -= 1, _ => {} }
                }
                i += 1;
                continue;
            }
        }

        let chunk_start = i;
        let mut paren_depth: i32 = 0;
        let mut brace_depth: i32 = 0;
        let mut sig_closed  = false;
        let mut body_open   = false;
        let mut j = i;

        'accum: while j < n && (j - chunk_start) < 500 {
            let lc = strip_glsl_comment(lines[j].trim());
            for c in lc.chars() {
                match c {
                    '(' => paren_depth += 1,
                    ')' => {
                        paren_depth -= 1;
                        if paren_depth <= 0 { paren_depth = 0; sig_closed = true; }
                    }
                    '{' if sig_closed => {
                        brace_depth += 1;
                        body_open = true;
                    }
                    '}' if body_open => {
                        brace_depth -= 1;
                        if brace_depth <= 0 {
                            j += 1;
                            break 'accum;
                        }
                    }
                    _ => {}
                }
            }
            j += 1;
            if sig_closed && !body_open && (lc.ends_with(';') || lc.ends_with(");")) {
                break 'accum;
            }
        }

        if !sig_closed {
            for c in code.chars() {
                match c { '{' => block_depth += 1, '}' => block_depth -= 1, _ => {} }
            }
            i += 1;
            continue;
        }

        let sig_first = strip_glsl_comment(lines[chunk_start].trim());
        let name_opt = sig_first.find('(')
            .map(|p| sig_first[..p].trim_end())
            .and_then(|before| before.split_whitespace().last())
            .map(|s| s.to_string());

        if let Some(name) = name_opt {
            if body_open && brace_depth <= 0 {
                defs.push((name, chunk_start, j));
            } else if !body_open {
                prototypes.push((name, chunk_start, j));
            }
        }

        i = j;
    }

    // ── Pass 2: identify "specific blocks" to move ───────────────────────────
    // For each prototype with a matching definition AFTER it, we need to move
    // the definition to the prototype position.  But the definition may depend
    // on structs and helper functions defined between the last "driver" function
    // and the definition.
    //
    // Strategy: find the LAST function definition or prototype that appears
    // BEFORE the prototype being processed — that's the end of the "driver
    // prefix".  Everything AFTER the end of the LAST function that appears
    // before the definition (but which is itself NOT part of a move) up to
    // and including the definition is the "specific block" to move.
    //
    // Since multiple prototypes may need moving (e.g., brush_vs and
    // text_shader_main in brush.glsl), we process them and collect the
    // largest contiguous block that covers all definitions.

    let mut def_by_name: HashMap<String, (usize, usize)> = HashMap::new();
    for (name, start, end) in &defs {
        def_by_name.insert(name.clone(), (*start, *end));
    }

    // Collect prototypes that need moves, sorted by prototype start line.
    let mut to_move: Vec<(String, usize, usize, usize, usize)> = Vec::new();
    for (name, ps, pe) in &prototypes {
        if let Some(&(ds, de)) = def_by_name.get(name.as_str()) {
            if ds > *ps {
                to_move.push((name.clone(), *ps, *pe, ds, de));
            }
        }
    }

    if to_move.is_empty() {
        return src.to_string();
    }

    to_move.sort_by_key(|t| t.1); // sort by prototype start

    // The "specific block" = everything from after the last `main()` definition
    // (which is always the final function in the driver section) up to and
    // including the last definition being moved.  `main()` is the ultimate
    // caller in brush.glsl / ps_quad.glsl, so everything after it belongs to
    // the "specific" shader (struct types, helper functions, prototyped
    // function bodies).
    //
    // We look for the last definition named "main" that appears BETWEEN the
    // first prototype and the earliest definition being moved.  If no such
    // main exists (shouldn't happen for WR shaders), fall back to the end of
    // the last prototype being moved.
    let earliest_def_start = to_move.iter().map(|t| t.3).min().unwrap();
    let first_proto_start = to_move[0].1;

    let main_end = defs.iter()
        .filter(|(name, s, e)| name == "main" && *s >= first_proto_start && *e <= earliest_def_start)
        .map(|(_, _, e)| *e)
        .max();

    let specific_block_start = main_end.unwrap_or_else(|| {
        // Fallback: use the end of the last function/prototype between
        // first prototype end and earliest definition start.
        let last_proto_end = to_move.last().unwrap().2;
        let mut boundaries: Vec<usize> = Vec::new();
        for (_, _, e) in &defs {
            if *e > last_proto_end && *e <= earliest_def_start {
                boundaries.push(*e);
            }
        }
        boundaries.into_iter().max().unwrap_or(last_proto_end)
    });

    // The "specific block end" = end of the last definition being moved.
    let specific_block_end = to_move.iter().map(|t| t.4).max().unwrap();

    // The insertion point = right before the FIRST function DEFINITION in the
    // driver section (between the first prototype and the earliest specific
    // definition).  This ensures all #define constants that appear between
    // the prototypes and the driver definitions are above the moved code.
    let first_driver_def_start = defs.iter()
        .filter(|(_, s, _)| *s > first_proto_start && *s < earliest_def_start)
        .map(|(_, s, _)| *s)
        .min();

    let insertion_point = first_driver_def_start.unwrap_or(to_move[0].1);

    // ── Pass 3: reconstruct ──────────────────────────────────────────────────
    let mut result = String::with_capacity(src.len() + 512);
    let mut li = 0;
    let mut specific_emitted = false;
    while li < n {
        // Skip prototypes being moved (they are no longer needed since the
        // definitions will appear before the driver functions).
        let proto_match = to_move.iter().find(|(_, ps, _, _, _)| li == *ps);
        if let Some((_, _, pe, _, _)) = proto_match {
            li = *pe;
            continue;
        }

        // At the insertion point, emit the specific block first.
        if li == insertion_point && !specific_emitted {
            specific_emitted = true;
            for l in specific_block_start..specific_block_end {
                result.push_str(lines[l]);
                result.push('\n');
            }
            // Don't skip current line — fall through to emit it normally
        }

        // Skip lines that are part of the specific block (they've been moved)
        if li >= specific_block_start && li < specific_block_end {
            li += 1;
            continue;
        }

        result.push_str(lines[li]);
        result.push('\n');
        li += 1;
    }
    result
}

/// Remove switch fall-through patterns that naga's WGSL emitter cannot handle.
///
/// Two transformations are applied to the assembled GLSL source:
///
/// **1. Cascade fall-through** — a bare `case X:` or `default:` label (nothing
/// following the colon) that is immediately followed by another case/default
/// label has the target case's body duplicated after it:
///
/// ```glsl
/// case A:           →   case A: { body; break; }
/// case B: { body; break; }   case B: { body; break; }
/// ```
///
/// **2. Missing break** — a `default:` body at switch-case depth that exits
/// without `break`/`return`/`discard` gets a `break;` appended before the
/// switch-closing `}`:
///
/// ```glsl
/// default:               →   default:
///     stmt;                      stmt;
/// }                              break;
///                            }
/// ```
///
/// Both patterns appear in WR's cs_border_*, cs_clip_box_shadow,
/// cs_line_decoration, and ps_text_run shaders.
#[cfg(feature = "wgpu_backend")]
fn fix_switch_fallthrough(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let n = lines.len();

    // True if the trimmed, comment-stripped line is a bare case/default label
    // with nothing after the colon (no `{`, `;`, or code).
    let bare_case = |raw: &str| -> bool {
        let c = strip_glsl_comment(raw.trim());
        if c.is_empty() { return false; }
        let starts = c.starts_with("case ") || c == "default:" || c.starts_with("default:");
        if !starts || !c.contains(':') { return false; }
        let after = c[c.rfind(':').unwrap_or(0) + 1..].trim();
        after.is_empty() && !c.contains('{') && !c.contains(';')
    };

    // True if the line starts a case or default label (code may follow the colon).
    let is_case = |raw: &str| -> bool {
        let c = strip_glsl_comment(raw.trim());
        (c.starts_with("case ") || c == "default:" || c.starts_with("default:")) && c.contains(':')
    };

    // True if the line contains an explicit case-level terminator.
    // Handles both standalone `break;` and inline `case X: stmt; break;` /
    // `default: stmt; break;` patterns (where `is_term` checks the WHOLE line).
    let is_term = |raw: &str| -> bool {
        let c = strip_glsl_comment(raw.trim());
        // Standalone or prefixed terminator: starts with keyword AND ends with `;`
        if (c.starts_with("break")    || c.starts_with("return") ||
            c.starts_with("discard") || c.starts_with("continue")) && c.ends_with(';')
        {
            return true;
        }
        // Inline terminator at end: `default: stmt; break;` or `case X: stmt; return y;`
        // — the last statement in the line is a terminator.
        c.ends_with("break;") || c.ends_with("discard;") || c.ends_with("continue;")
    };

    // Collect the body that belongs to the case at `case_line`, returning
    // (body_lines_to_insert, end_index_exclusive).
    // Statements are returned WITHOUT an outer { } wrapper so they are
    // inserted at the CASE level — naga requires case-level terminators and
    // does not accept `break` / `return` inside a nested block as satisfying
    // the fall-through check.
    let extract_body = |lines: &[&str], case_line: usize, n: usize| -> (Vec<String>, usize) {
        let code  = strip_glsl_comment(lines[case_line].trim());
        let colon = code.rfind(':').unwrap_or(0);
        let after = code[colon + 1..].trim();

        if after.starts_with('{') {
            // `case X: { … }` — block starts on same line as label.
            // Extract the CONTENTS of the block (not including the outer { }).
            let mut result = vec![];
            let mut depth: i32 = 1;
            let mut j = case_line + 1;
            while j < n && depth > 0 {
                let lc = strip_glsl_comment(lines[j].trim());
                // Track depth changes ON this line before deciding to include it.
                let mut new_depth = depth;
                for ch in lc.chars() {
                    match ch { '{' => new_depth += 1, '}' => new_depth -= 1, _ => {} }
                }
                if new_depth > 0 {
                    result.push(lines[j].to_string());
                }
                // If new_depth == 0 this line is (or contains) the closing `}`.
                // We skip it — we don't want to emit it as part of the body.
                depth = new_depth;
                j += 1;
            }
            (result, j)
        } else if !after.is_empty() {
            // Inline statement after colon (e.g. `case X: return y;`).
            let raw   = lines[case_line];
            let indent: String = raw[..raw.len() - raw.trim_start().len()].to_string();
            (vec![format!("{}    {}", indent, after)], case_line + 1)
        } else {
            // Body starts on lines after the label.  Collect until the next
            // case label at depth 0 or the `}` closing the switch.
            let mut result = vec![];
            let mut depth: i32 = 0;
            let mut j = case_line + 1;
            while j < n {
                let lc = strip_glsl_comment(lines[j].trim());
                if depth == 0 && (is_case(lines[j]) || lc == "}" || lc == "};") { break; }
                for ch in lc.chars() { match ch { '{' => depth += 1, '}' => depth -= 1, _ => {} } }
                result.push(lines[j].to_string());
                j += 1;
                if depth < 0 { result.pop(); break; }
            }
            (result, j)
        }
    };

    // ── Pass 1: cascade fall-through ─────────────────────────────────────────
    let mut p1: Vec<String> = Vec::with_capacity(n + 64);
    {
        let mut i = 0;
        while i < n {
            let raw = lines[i];
            if bare_case(raw) {
                // Next non-blank line.
                let mut j = i + 1;
                while j < n && lines[j].trim().is_empty() { j += 1; }

                if j < n && is_case(lines[j]) {
                    // Cascade detected: collect all bare labels in this run.
                    let mut group: Vec<usize> = vec![i];
                    while j < n {
                        if lines[j].trim().is_empty() { j += 1; continue; }
                        if bare_case(lines[j]) {
                            // Include in run only if followed by another case start.
                            let mut k = j + 1;
                            while k < n && lines[k].trim().is_empty() { k += 1; }
                            if k < n && is_case(lines[k]) {
                                group.push(j); j += 1;
                            } else {
                                break; // j is the last label with body following.
                            }
                        } else {
                            break;
                        }
                    }
                    // j = the last (non-bare or last-bare-before-body) case label.
                    let (body, _) = extract_body(&lines, j, n);
                    // When the body contains variable declarations, rename them in
                    // duplicate copies to avoid VariableAlreadyDeclared errors.
                    // naga does not scope switch cases separately, so the same
                    // variable name can't appear twice.
                    let decl_names: Vec<String> = body.iter().filter_map(|b| {
                        let bt = b.trim();
                        for kw in &["vec2 ", "vec3 ", "vec4 ",
                                    "ivec2 ", "ivec3 ", "ivec4 ",
                                    "uvec2 ", "uvec3 ", "uvec4 ",
                                    "float ", "int ", "uint ", "bool ",
                                    "mat2 ", "mat3 ", "mat4 "] {
                            if bt.starts_with(kw) {
                                // Extract variable name (token after type keyword).
                                let after = &bt[kw.len()..];
                                let name: String = after.chars()
                                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                                    .collect();
                                if !name.is_empty() {
                                    return Some(name);
                                }
                            }
                        }
                        None
                    }).collect();
                    for (gi, &li) in group.iter().enumerate() {
                        p1.push(lines[li].to_string());
                        if decl_names.is_empty() {
                            for bl in &body { p1.push(bl.clone()); }
                        } else {
                            // Rename declared variables with a unique suffix.
                            for bl in &body {
                                let mut line = bl.clone();
                                for name in &decl_names {
                                    let renamed = format!("{}_dup{}", name, gi);
                                    // Use word-boundary replacement to avoid
                                    // renaming substrings of longer identifiers.
                                    line = replace_word(&line, name, &renamed);
                                }
                                p1.push(line);
                            }
                        }
                    }
                    i = j; // Continue at last label so its body is emitted normally.
                    continue;
                }
            }
            p1.push(raw.to_string());
            i += 1;
        }
    }

    // ── Pass 2: fix `case X: { … }` blocks so naga sees a case-level break ──
    //
    // Root cause (confirmed via naga source):
    //   naga's parse_statement for `{…}` blocks uses `get_or_insert` to set the
    //   outer case_terminator when block_terminator is non-None.  This locks
    //   case_terminator to point AFTER the block (to the Block statement itself,
    //   not to a Break).  So `ctx.body[idx-1]` = Block ≠ Break, and fall_through
    //   stays `true`.  Adding an outer `break;` after `}` doesn't help: by the
    //   time `break;` is parsed, case_terminator is already set (get_or_insert
    //   is a no-op on Some).
    //
    // Fix: REMOVE the terminator from *inside* the block so that block_terminator
    //   is None.  The `{…}` block is kept intact for variable-scoping purposes.
    //   After the block's closing `}`, a case-level `break;` is emitted.  Now:
    //   • block has no inner terminator → block_terminator = None
    //   • block doesn't set case_terminator via get_or_insert
    //   • the outer `break;` sets case_terminator pointing to Statement::Break
    //   • ctx.body[idx-1] = Break → fall_through = false ✓
    //
    // For `return expr;` inside blocks (ps_text_run helper functions): we also
    // remove the return and add an outer `break;`. The function's implicit or
    // explicit return after the switch handles the control flow.
    let p1r: Vec<&str> = p1.iter().map(|s| s.as_str()).collect();
    let n2 = p1r.len();
    let mut p2: Vec<String> = Vec::with_capacity(n2 + 16);
    {
        let mut i = 0;
        while i < n2 {
            let raw = p1r[i];
            let code = strip_glsl_comment(raw.trim());
            // Detect `case X: {` or `default: {` — label immediately opening a block.
            let is_case_with_block =
                (code.starts_with("case ") || code.starts_with("default:") || code == "default:")
                && code.contains(':') && {
                    let colon = code.rfind(':').unwrap_or(0);
                    code[colon + 1..].trim().starts_with('{')
                };
            if is_case_with_block {
                let indent = &raw[..raw.len() - raw.trim_start().len()];

                // Collect all lines from i+1 until the matching `}`.
                let mut depth: i32 = 1; // `{` on the case line opened depth 1
                let mut j = i + 1;
                let mut block_indices: Vec<usize> = Vec::new();
                while j < n2 && depth > 0 {
                    let lc = strip_glsl_comment(p1r[j].trim());
                    block_indices.push(j);
                    let delta = lc.chars().fold(0i32, |d, ch| match ch {
                        '{' => d + 1, '}' => d - 1, _ => d,
                    });
                    depth += delta;
                    j += 1;
                }
                // block_indices now contains all lines through the closing `}`.

                // Find the LAST directly-nested terminator (at block depth 0).
                // This is the terminator that causes block_terminator to be set,
                // which in turn locks the outer case_terminator via get_or_insert.
                // Removing it ensures block_terminator = None.
                let mut inner_depth: i32 = 0;
                let mut last_term_bi: Option<usize> = None;
                for (bi, &li) in block_indices.iter().enumerate() {
                    let lc = strip_glsl_comment(p1r[li].trim());
                    // Check at current depth BEFORE updating (depth 0 = direct child).
                    if inner_depth == 0 {
                        let is_term_stmt =
                            (lc.starts_with("break") || lc.starts_with("return") ||
                             lc.starts_with("discard") || lc.starts_with("continue"))
                            && lc.ends_with(';');
                        if is_term_stmt {
                            last_term_bi = Some(bi);
                        }
                    }
                    let delta = lc.chars().fold(0i32, |d, ch| match ch {
                        '{' => d + 1, '}' => d - 1, _ => d,
                    });
                    inner_depth += delta;
                }

                // Emit case line as-is (includes `{`).
                p2.push(raw.to_string());
                // Emit block lines, skipping the last direct terminator.
                // Remember WHAT the terminator was so we can restore its semantics.
                let mut removed_term = "break;".to_string();
                for (bi, &li) in block_indices.iter().enumerate() {
                    if last_term_bi == Some(bi) {
                        removed_term = strip_glsl_comment(p1r[li].trim()).to_string();
                        continue; // omit so block has no inner terminator
                    }
                    p2.push(p1r[li].to_string());
                }
                // Emit case-level terminator after the block's closing `}`.
                // Use the same terminator that was removed (break/return/discard).
                p2.push(format!("{}{}", indent, removed_term.trim()));
                i = j;
            } else {
                p2.push(raw.to_string());
                i += 1;
            }
        }
    }

    // ── Pass 3: missing `break` before switch-closing `}` ────────────────────
    // Track which brace depths are switch bodies so we know when `}` closes a
    // switch.  When it does and the prior code lacked a terminator, insert break.
    //
    // IMPORTANT: the switch keyword detection MUST happen BEFORE processing the
    // characters of the same line, so that `switch (...) {` (all on one line)
    // has its `{` pushed immediately.  Detecting it AFTER the char loop would
    // leave the `{` un-pushed and let the sticky flag leak into subsequent code
    // (including `#ifdef WR_FRAGMENT_SHADER` sections), causing spurious breaks.
    let p2r: Vec<&str> = p2.iter().map(|s| s.as_str()).collect();
    let n3 = p2r.len();
    let mut p3: Vec<String> = Vec::with_capacity(n3 + 8);

    let mut brace_depth: i32 = 0;
    // Stack of brace depths where a switch body is open.
    // Entry = brace depth AFTER the opening `{` of the switch body.
    let mut sw_depths: Vec<i32> = Vec::new();
    // Set to true when a `switch (...)` keyword is detected on the current or
    // previous line.  The next `{` encountered will be the switch body open.
    let mut next_open_is_switch = false;
    let mut last_was_term = false; // last non-empty code line was a terminator
    let mut last_was_switch_open = false; // last non-empty code opened a switch

    for raw in &p2r {
        let code = strip_glsl_comment(raw.trim());

        // ── Detect switch keyword BEFORE processing characters ────────────────
        // This ensures that for `switch (...) {` all on one line the `{` is
        // caught in the char loop below (not on the *next* iteration).
        // Preprocessor directives (`#ifdef` etc.) are excluded so that the flag
        // does not leak from an inactive `#ifdef WR_VERTEX_SHADER` block into
        // the `#ifdef WR_FRAGMENT_SHADER` block or vice-versa.
        if !code.starts_with('#') {
            let is_switch_kw = code.starts_with("switch")
                && code[6..].trim_start().starts_with('(');
            if is_switch_kw {
                next_open_is_switch = true;
            }
        }

        // ── Insert missing break before case/default labels inside switches ───
        // If we are inside a switch body and the current line is a case/default
        // label, but the previous code line was not a terminator, insert break.
        let indent = &raw[..raw.len() - raw.trim_start().len()];
        if !sw_depths.is_empty() && !last_was_term
            && (code.starts_with("case ") || code.starts_with("default:") || code == "default:")
            && code.contains(':')
        {
            if !last_was_switch_open {
                p3.push(format!("{}    break;", indent));
            }
        }

        // ── Character-by-character depth tracking ────────────────────────────
        // Process `{` and `}` in order so that lines like `} else {` are
        // handled correctly without false positives.
        let mut temp_depth = brace_depth;
        for ch in code.chars() {
            match ch {
                '}' => {
                    // Check BEFORE decrementing: sw_depths stores the depth
                    // *after* the switch-opening `{`, which equals temp_depth
                    // right when we're about to close that brace.
                    if sw_depths.last() == Some(&temp_depth) {
                        if !last_was_term {
                            p3.push(format!("{}    break;", indent));
                        }
                        sw_depths.pop();
                    }
                    temp_depth -= 1;
                }
                '{' => {
                    temp_depth += 1;
                    if next_open_is_switch {
                        sw_depths.push(temp_depth);
                        next_open_is_switch = false;
                    }
                }
                _ => {}
            }
        }

        p3.push(raw.to_string());
        brace_depth = temp_depth;

        if !code.is_empty() && !code.starts_with("//") {
            last_was_term = is_term(raw);
            last_was_switch_open = code.ends_with('{') &&
                (code.starts_with("switch") || code == "{");
        }
    }

    // ── Pass 4: move `default:` to the last position in each switch ──────────
    // Some naga versions reject (or mishandle) a `default:` case that appears
    // before other `case X:` labels.  Reorder each switch so `default:` is
    // always the last case.
    let p3r: Vec<&str> = p3.iter().map(|s| s.as_str()).collect();
    let n4 = p3r.len();
    let mut p4: Vec<String> = Vec::with_capacity(n4 + 4);
    {
        let mut i = 0;
        while i < n4 {
            let raw = p3r[i];
            let code = strip_glsl_comment(raw.trim());

            // Detect the start of a switch block (not inside a `#` directive).
            let is_switch_start = !code.starts_with('#')
                && code.starts_with("switch")
                && code[6..].trim_start().starts_with('(');

            if !is_switch_start {
                p4.push(raw.to_string());
                i += 1;
                continue;
            }

            // Emit the switch header line(s) up to and including the opening `{`.
            // Usually `switch (...) {` is all on one line.
            let mut header_end = i;
            {
                let mut depth: i32 = 0;
                let mut j = i;
                while j < n4 {
                    let lc = strip_glsl_comment(p3r[j].trim());
                    for ch in lc.chars() {
                        match ch {
                            '{' => { depth += 1; }
                            '}' => { depth -= 1; }
                            _ => {}
                        }
                    }
                    header_end = j;
                    if depth > 0 { break; } // found the opening `{`
                    j += 1;
                }
            }
            for li in i..=header_end {
                p4.push(p3r[li].to_string());
            }
            i = header_end + 1;

            // Collect case sections at depth 0 (relative to switch body).
            // A section = case/default label line(s) + body lines.
            // Depth 0 = directly inside the switch body.
            let mut sections: Vec<Vec<String>> = Vec::new();
            let mut default_idx: Option<usize> = None;
            let mut cur: Vec<String> = Vec::new();
            let mut depth: i32 = 0;

            loop {
                if i >= n4 { break; }
                let raw2 = p3r[i];
                let lc = strip_glsl_comment(raw2.trim());

                // Compute depth change.
                let new_depth = lc.chars().fold(depth, |d, ch| match ch {
                    '{' => d + 1, '}' => d - 1, _ => d
                });

                // Switch-closing `}` at depth 0.
                if depth == 0 && new_depth < 0 {
                    if !cur.is_empty() {
                        if default_idx.is_none() {
                            if let Some(fl) = cur.first() {
                                let fc = strip_glsl_comment(fl.trim());
                                if fc == "default:" || fc.starts_with("default:") {
                                    default_idx = Some(sections.len());
                                }
                            }
                        }
                        sections.push(std::mem::take(&mut cur));
                    }
                    // Move default to end if it's not already last.
                    if let Some(di) = default_idx {
                        if di + 1 < sections.len() {
                            let def_sec = sections.remove(di);
                            sections.push(def_sec);
                        }
                    }
                    for sec in sections {
                        for ln in sec { p4.push(ln); }
                    }
                    p4.push(raw2.to_string()); // closing `}`
                    i += 1;
                    break;
                }

                // New case/default section starts at depth 0.
                if depth == 0 && is_case(raw2) {
                    if !cur.is_empty() {
                        if default_idx.is_none() {
                            if let Some(fl) = cur.first() {
                                let fc = strip_glsl_comment(fl.trim());
                                if fc == "default:" || fc.starts_with("default:") {
                                    default_idx = Some(sections.len());
                                }
                            }
                        }
                        sections.push(std::mem::take(&mut cur));
                    }
                }

                cur.push(raw2.to_string());
                depth = new_depth;
                i += 1;
            }
        }
    }

    // ── Pass 5: convert return-only switches to if-else chains ───────────────
    // naga's fall_through mechanism only recognises Statement::Break.  A switch
    // case ending with `return expr;` at case level leaves fall_through = true
    // for non-last cases (Return ≠ Break in naga's internal check), causing
    // Unimplemented("fall-through switch case block").
    //
    // If EVERY case in a switch has a body consisting solely of `return expr;`
    // statements (possibly with comments or trailing break; from earlier passes),
    // the switch is semantically equivalent to an if-else chain.  Converting it
    // avoids the switch entirely, so no fall_through tracking is needed.
    let p4r: Vec<&str> = p4.iter().map(|s| s.as_str()).collect();
    let n5 = p4r.len();
    let mut p5: Vec<String> = Vec::with_capacity(n5 + 8);

    // Extract selector expression from `switch (EXPR) {`, or None.
    let switch_expr = |raw: &str| -> Option<String> {
        let code = strip_glsl_comment(raw.trim());
        if code.starts_with('#') || !code.starts_with("switch") { return None; }
        let rest = code[6..].trim_start();
        if !rest.starts_with('(') { return None; }
        let mut depth = 0i32;
        for (k, ch) in rest.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => { depth -= 1; if depth == 0 { return Some(rest[1..k].trim().to_string()); } }
                _ => {}
            }
        }
        None
    };

    // True when a slice of case-body lines contains only return/break/comments.
    let is_return_only_body = |body: &[String]| -> bool {
        let has_return = body.iter().any(|ln| {
            let c = strip_glsl_comment(ln.trim());
            c.starts_with("return") && c.ends_with(';')
        });
        has_return && body.iter().all(|ln| {
            let c = strip_glsl_comment(ln.trim());
            c.is_empty()
                || c.starts_with("//")
                || (c.starts_with("return") && c.ends_with(';'))
                || (c.starts_with("break") && c.ends_with(';'))
        })
    };

    let mut i5 = 0;
    while i5 < n5 {
        let raw = p4r[i5];
        if let Some(sel_expr) = switch_expr(raw) {
            let sw_indent = raw[..raw.len() - raw.trim_start().len()].to_string();

            // Advance past the switch header to find the opening `{`.
            let mut hdr_depth: i32 = 0;
            let mut j5 = i5;
            loop {
                let lc = strip_glsl_comment(p4r[j5].trim());
                for ch in lc.chars() {
                    match ch { '{' => hdr_depth += 1, '}' => hdr_depth -= 1, _ => {} }
                }
                j5 += 1;
                if hdr_depth > 0 || j5 >= n5 { break; }
            }

            // Collect switch body lines until the matching closing `}`.
            let mut body_depth: i32 = 1;
            let mut sw_body: Vec<usize> = Vec::new();
            while j5 < n5 && body_depth > 0 {
                let lc = strip_glsl_comment(p4r[j5].trim());
                let delta = lc.chars().fold(0i32, |d, ch| match ch {
                    '{' => d + 1, '}' => d - 1, _ => d,
                });
                body_depth += delta;
                if body_depth > 0 { sw_body.push(j5); }
                j5 += 1;
            }
            // j5 now points to the line after the switch-closing `}`.

            // Partition sw_body into case sections (label_line + body_lines).
            let mut sections: Vec<Vec<String>> = Vec::new();
            let mut cur_sec: Vec<String> = Vec::new();
            let mut sec_depth: i32 = 0;
            for &li in &sw_body {
                let lc = strip_glsl_comment(p4r[li].trim());
                if sec_depth == 0 && is_case(p4r[li]) {
                    if !cur_sec.is_empty() { sections.push(std::mem::take(&mut cur_sec)); }
                }
                cur_sec.push(p4r[li].to_string());
                let delta = lc.chars().fold(0i32, |d, ch| match ch {
                    '{' => d + 1, '}' => d - 1, _ => d,
                });
                sec_depth += delta;
            }
            if !cur_sec.is_empty() { sections.push(cur_sec); }

            // Determine whether all case bodies are return-only.
            let all_return = !sections.is_empty() && sections.iter().all(|sec| {
                is_return_only_body(&sec[1..])
            });

            if !all_return {
                // Not a return-only switch: emit verbatim.
                for k in i5..j5 { p5.push(p4r[k].to_string()); }
                i5 = j5;
                continue;
            }

            // Emit as an if-else chain.
            let mut first_case = true;
            for sec in &sections {
                let label_code = strip_glsl_comment(sec[0].trim());
                let is_default = label_code == "default:"
                    || label_code.starts_with("default:");

                // Body: all lines except bare `break;` added by earlier passes.
                let body_emit: Vec<&String> = sec[1..].iter()
                    .filter(|ln| {
                        let c = strip_glsl_comment(ln.trim());
                        !(c.starts_with("break") && c.ends_with(';'))
                    })
                    .collect();

                if is_default {
                    p5.push(format!("{}}} else {{", sw_indent));
                    for bl in &body_emit { p5.push((*bl).clone()); }
                } else {
                    // Extract value: strip "case " prefix and trailing ":".
                    let colon = label_code.rfind(':').unwrap_or(label_code.len());
                    let val = label_code[5..colon].trim(); // skip "case "
                    if first_case {
                        p5.push(format!("{}if ({} == {}) {{", sw_indent, sel_expr, val));
                        first_case = false;
                    } else {
                        p5.push(format!("{}}} else if ({} == {}) {{",
                            sw_indent, sel_expr, val));
                    }
                    for bl in &body_emit { p5.push((*bl).clone()); }
                }
            }
            // Close the last branch.
            p5.push(format!("{}}}", sw_indent));
            i5 = j5;
        } else {
            p5.push(raw.to_string());
            i5 += 1;
        }
    }

    // ── Pass 6: replace `return;` at switch-case level with flag + break ────
    // naga's WGSL writer only accepts `break;` as a switch case terminator.
    // `return;` inside a switch case is flagged as fall-through.  Fix: replace
    // case-level `return;` with `_naga_early_ret = true; break;`, add a
    // `bool _naga_early_ret = false;` before the switch, and wrap the code
    // after the switch in `if (!_naga_early_ret) { ... }`.
    // Only apply to switches that have mixed return/break cases.
    let p5_lines: Vec<&str> = p5.iter().map(|s| s.as_str()).collect();
    let n6 = p5_lines.len();
    let mut p6: Vec<String> = Vec::with_capacity(n6 + 16);
    {
        let mut i = 0;
        while i < n6 {
            let raw = p5_lines[i];
            if let Some(_sel_expr) = switch_expr(raw) {
                let sw_indent = raw[..raw.len() - raw.trim_start().len()].to_string();

                // Find the switch body (from opening `{` to closing `}`).
                let mut hdr_depth: i32 = 0;
                let mut j = i;
                loop {
                    let lc = strip_glsl_comment(p5_lines[j].trim());
                    for ch in lc.chars() {
                        match ch { '{' => hdr_depth += 1, '}' => hdr_depth -= 1, _ => {} }
                    }
                    j += 1;
                    if hdr_depth > 0 || j >= n6 { break; }
                }

                let mut body_depth: i32 = 1;
                let switch_body_start = j;
                while j < n6 && body_depth > 0 {
                    let lc = strip_glsl_comment(p5_lines[j].trim());
                    let delta = lc.chars().fold(0i32, |d, ch| match ch {
                        '{' => d + 1, '}' => d - 1, _ => d,
                    });
                    body_depth += delta;
                    j += 1;
                }
                let switch_end = j; // line after switch-closing `}`

                // Check if any case-level return exists in the switch body.
                let has_case_return = (switch_body_start..switch_end).any(|k| {
                    let c = strip_glsl_comment(p5_lines[k].trim());
                    (c.starts_with("return") || c == "return;") && c.ends_with(';')
                });

                if !has_case_return {
                    // No case-level returns: emit verbatim.
                    for k in i..switch_end { p6.push(p5_lines[k].to_string()); }
                    i = switch_end;
                    continue;
                }

                // Emit flag + switch with return→break conversion.
                p6.push(format!("{}bool _naga_early_ret = false;", sw_indent));
                for k in i..switch_end {
                    let c = strip_glsl_comment(p5_lines[k].trim());
                    if (c.starts_with("return") || c == "return;") && c.ends_with(';')
                        && k >= switch_body_start && k < switch_end
                    {
                        let line_indent = &p5_lines[k][..p5_lines[k].len() - p5_lines[k].trim_start().len()];
                        p6.push(format!("{}_naga_early_ret = true;", line_indent));
                        p6.push(format!("{}break;", line_indent));
                    } else {
                        p6.push(p5_lines[k].to_string());
                    }
                }

                // Find the closing `}` of main() or end of function.
                // Wrap remaining code until the function's closing `}` in
                // `if (!_naga_early_ret) { ... }`.
                // Simpler approach: find the NEXT closing `}` at depth 0 (function end).
                let mut func_end = switch_end;
                let mut depth: i32 = 0;
                while func_end < n6 {
                    let lc = strip_glsl_comment(p5_lines[func_end].trim());
                    for ch in lc.chars() {
                        match ch { '{' => depth += 1, '}' => depth -= 1, _ => {} }
                    }
                    if depth < 0 {
                        // This is the function closing `}`.
                        break;
                    }
                    func_end += 1;
                }

                // Emit the code between switch end and function end wrapped in guard.
                if func_end > switch_end {
                    p6.push(format!("{}if (!_naga_early_ret) {{", sw_indent));
                    for k in switch_end..func_end {
                        p6.push(p5_lines[k].to_string());
                    }
                    p6.push(format!("{}}}", sw_indent));
                }

                // Emit the function closing `}` line.
                if func_end < n6 {
                    p6.push(p5_lines[func_end].to_string());
                }

                i = func_end + 1;
                continue;
            }
            p6.push(raw.to_string());
            i += 1;
        }
    }

    p6.join("\n") + if src.ends_with('\n') { "\n" } else { "" }
}

/// Scan the leading tokens of a GLSL declaration line and return the first
/// GLSL storage/interface qualifier found.
///
/// The function skips known non-storage qualifiers and WR-specific macros that
/// can appear before the storage qualifier (e.g. `flat`, `PER_INSTANCE`).
/// Returns `None` if the first non-prefix token is not a storage qualifier —
/// meaning this is not an interface variable declaration.
#[cfg(feature = "wgpu_backend")]
fn storage_qual(code: &str) -> Option<&'static str> {
    for token in code.split_whitespace() {
        match token {
            "in"        => return Some("in"),
            "out"       => return Some("out"),
            "varying"   => return Some("varying"),
            "attribute" => return Some("attribute"),
            // Allowed to precede the storage qualifier:
            // interpolation qualifiers (GLSL built-in) and WR instance macros.
            "flat" | "smooth" | "noperspective" | "PER_INSTANCE" => {}
            _ => return None,
        }
    }
    None
}

/// Resolve `#ifdef WR_VERTEX_SHADER` / `#ifdef WR_FRAGMENT_SHADER` / `#endif`
/// conditionals based on the current compilation stage.  Lines inside the
/// ACTIVE stage block are kept; lines inside the INACTIVE stage block are
/// removed.  Code outside any stage conditional is always kept.
///
/// Handles nested `#ifdef` / `#if` / `#endif` correctly: only conditionals
/// whose directive matches `WR_VERTEX_SHADER` or `WR_FRAGMENT_SHADER` at the
/// outermost ifdef depth are resolved.  Other `#ifdef` / `#endif` pairs inside
/// the stage block are passed through for naga's preprocessor.
#[cfg(feature = "wgpu_backend")]
fn resolve_stage_ifdefs(src: &str, stage: naga::ShaderStage) -> String {
    let active_define = match stage {
        naga::ShaderStage::Vertex   => "WR_VERTEX_SHADER",
        naga::ShaderStage::Fragment => "WR_FRAGMENT_SHADER",
        _ => return src.to_string(),
    };
    let inactive_define = match stage {
        naga::ShaderStage::Vertex   => "WR_FRAGMENT_SHADER",
        naga::ShaderStage::Fragment => "WR_VERTEX_SHADER",
        _ => return src.to_string(),
    };

    let mut out = String::with_capacity(src.len());
    // Track nesting of #ifdef/#endif to handle inner #ifdef pairs.
    // state: 0 = outside any stage block,
    //        1 = inside active stage block (emit),
    //       -1 = inside inactive stage block (skip)
    let mut stage_state: i32 = 0;
    let mut inner_depth: i32 = 0; // #ifdef nesting depth inside a stage block

    for line in src.lines() {
        let trimmed = line.trim();
        // Normalize #endif variants: `#endif`, `#endif //comment`, `#endif /* comment */`
        let is_endif = trimmed == "#endif"
            || trimmed.starts_with("#endif ")
            || trimmed.starts_with("#endif//");

        if stage_state == 0 {
            // Outside any stage block.
            if trimmed == &format!("#ifdef {}", active_define) {
                stage_state = 1;
                inner_depth = 0;
                // Don't emit the #ifdef line itself — naga doesn't need it
                // since we've resolved it.
                continue;
            } else if trimmed == &format!("#ifdef {}", inactive_define) {
                stage_state = -1;
                inner_depth = 0;
                continue;
            }
            out.push_str(line);
            out.push('\n');
        } else if stage_state == 1 {
            // Inside active stage block — emit lines, track inner nesting.
            if trimmed.starts_with("#ifdef ")
                || trimmed.starts_with("#if ")
                || trimmed.starts_with("#ifndef ")
            {
                inner_depth += 1;
                out.push_str(line);
                out.push('\n');
            } else if is_endif {
                if inner_depth > 0 {
                    inner_depth -= 1;
                    out.push_str(line);
                    out.push('\n');
                } else {
                    // Closing #endif for the active stage block.
                    stage_state = 0;
                }
            } else {
                out.push_str(line);
                out.push('\n');
            }
        } else {
            // stage_state == -1: inside inactive stage block — skip lines.
            if trimmed.starts_with("#ifdef ")
                || trimmed.starts_with("#if ")
                || trimmed.starts_with("#ifndef ")
            {
                inner_depth += 1;
            } else if is_endif {
                if inner_depth > 0 {
                    inner_depth -= 1;
                } else {
                    stage_state = 0;
                }
            }
            // All lines in inactive block are skipped.
        }
    }

    out
}

/// Decompose matrix varyings into column-vector varyings.
///
/// naga 26 does not set the IO_SHAREABLE flag on matrix types (mat3/mat4), so
/// any `flat varying matN foo;` causes a `NotIOShareableType` validation error.
/// This function rewrites them:
///
///   `flat varying mat4 foo;`
///   →  `flat varying vec4 foo_c0;`
///      `flat varying vec4 foo_c1;`
///      `flat varying vec4 foo_c2;`
///      `flat varying vec4 foo_c3;`
///      `mat4 foo;`   // plain global (not IO)
///
/// Then it injects glue code in `main()`:
///   - Vertex:   before the closing `}`, decompose mat into column varyings.
///   - Fragment: after the opening `{`, reconstruct mat from column varyings.
///
/// If a varying is declared inside an `#ifdef FOO` block, the column varyings,
/// global, and glue code are all wrapped in the same `#ifdef FOO`.
#[cfg(feature = "wgpu_backend")]
fn decompose_matrix_varyings(src: &str, stage: naga::ShaderStage) -> String {
    // Describes one matrix varying that needs decomposition.
    struct MatVarying {
        name:      String,   // e.g. "v_color_mat"
        qualifiers: String,  // e.g. "flat" (everything before "varying")
        mat_kw:    String,   // "mat3" or "mat4"
        vec_kw:    String,   // "vec3" or "vec4"
        cols:      usize,    // 3 or 4
        guard:     Option<String>, // enclosing #ifdef condition, e.g. "WR_FEATURE_YUV"
    }

    let lines: Vec<&str> = src.lines().collect();
    let mut varyings: Vec<MatVarying> = Vec::new();

    // ── Phase 1: detect matrix varying declarations ──
    // Track outermost #ifdef for guard context.
    let mut ifdef_stack: Vec<String> = Vec::new();

    for line in &lines {
        let trimmed = line.trim();

        // Track #ifdef / #endif nesting.
        if trimmed.starts_with("#ifdef ") {
            ifdef_stack.push(trimmed["#ifdef ".len()..].trim().to_string());
            continue;
        } else if trimmed.starts_with("#ifndef ") {
            ifdef_stack.push(format!("!{}", trimmed["#ifndef ".len()..].trim()));
            continue;
        } else if trimmed.starts_with("#if ") {
            ifdef_stack.push(trimmed["#if ".len()..].trim().to_string());
            continue;
        } else if trimmed == "#endif" || trimmed.starts_with("#endif ")
                   || trimmed.starts_with("#endif//") {
            ifdef_stack.pop();
            continue;
        } else if trimmed.starts_with("#elif ") || trimmed == "#else" || trimmed.starts_with("#else ") {
            // Replace top of stack with the new branch condition (approximate).
            ifdef_stack.pop();
            ifdef_stack.push(trimmed.to_string());
            continue;
        }

        let code = match trimmed.find("//") {
            Some(i) => trimmed[..i].trim_end(),
            None    => trimmed,
        };
        if !code.ends_with(';') { continue; }

        let tokens: Vec<&str> = code.trim_end_matches(';').split_whitespace().collect();
        let vary_pos = tokens.iter().position(|&t| t == "varying");
        if vary_pos.is_none() { continue; }
        let vary_pos = vary_pos.unwrap();

        let name = match tokens.last() {
            Some(n) => *n,
            None => continue,
        };
        let type_idx = tokens.len() - 2;
        if type_idx <= vary_pos { continue; }
        let mat_kw = tokens[type_idx];

        let (vec_kw, cols) = match mat_kw {
            "mat3" => ("vec3", 3usize),
            "mat4" => ("vec4", 4usize),
            _      => continue,
        };

        let qualifiers = tokens[..vary_pos].join(" ");
        let guard = ifdef_stack.last().cloned();
        varyings.push(MatVarying {
            name: name.to_string(),
            qualifiers,
            mat_kw: mat_kw.to_string(),
            vec_kw: vec_kw.to_string(),
            cols,
            guard,
        });
    }

    if varyings.is_empty() {
        return src.to_string();
    }

    // ── Phase 2: rewrite lines ──
    let is_vertex = stage == naga::ShaderStage::Vertex;
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + varyings.len() * 8);

    for line in &lines {
        let trimmed = line.trim();
        let code = match trimmed.find("//") {
            Some(i) => trimmed[..i].trim_end(),
            None    => trimmed,
        };

        // Check if this line declares one of our matrix varyings.
        let mut matched_varying = None;
        if code.ends_with(';') {
            for mv in &varyings {
                let has_varying = code.split_whitespace().any(|t| t == "varying");
                let has_name    = code.trim_end_matches(';').split_whitespace().last() == Some(mv.name.as_str());
                let has_mat     = code.split_whitespace().any(|t| t == mv.mat_kw.as_str());
                if has_varying && has_name && has_mat {
                    matched_varying = Some(mv);
                    break;
                }
            }
        }

        if let Some(mv) = matched_varying {
            let indent = &line[..line.len() - trimmed.len()];
            let qual = if mv.qualifiers.is_empty() {
                String::new()
            } else {
                format!("{} ", mv.qualifiers)
            };
            // Emit column varyings.
            for c in 0..mv.cols {
                out.push(format!(
                    "{}{}varying {} {}_c{};",
                    indent, qual, mv.vec_kw, mv.name, c
                ));
            }
            // Emit a plain global for the mat (not IO).
            out.push(format!("{}{} {};", indent, mv.mat_kw, mv.name));
        } else {
            out.push(line.to_string());
        }
    }

    // ── Phase 3: inject glue code in main() ──
    let result = out.join("\n");
    let mut final_lines: Vec<String> = result.lines().map(|l| l.to_string()).collect();

    if is_vertex {
        // Vertex: insert decomposition before the closing `}` of main().
        if let Some(main_close) = find_main_close(&final_lines) {
            let mut glue = Vec::new();
            for mv in &varyings {
                if let Some(ref guard) = mv.guard {
                    glue.push(format!("#ifdef {}", guard));
                }
                for c in 0..mv.cols {
                    glue.push(format!("    {}_c{} = {}[{}];", mv.name, c, mv.name, c));
                }
                if mv.guard.is_some() {
                    glue.push("#endif".to_string());
                }
            }
            for (i, g) in glue.iter().enumerate() {
                final_lines.insert(main_close + i, g.clone());
            }
        }
    } else {
        // Fragment: insert reconstruction after the opening `{` of main().
        if let Some(main_open) = find_main_open(&final_lines) {
            let mut glue = Vec::new();
            for mv in &varyings {
                if let Some(ref guard) = mv.guard {
                    glue.push(format!("#ifdef {}", guard));
                }
                let cols: Vec<String> = (0..mv.cols)
                    .map(|c| format!("{}_c{}", mv.name, c))
                    .collect();
                glue.push(format!(
                    "    {} = {}({});",
                    mv.name, mv.mat_kw, cols.join(", ")
                ));
                if mv.guard.is_some() {
                    glue.push("#endif".to_string());
                }
            }
            for (i, g) in glue.iter().enumerate() {
                final_lines.insert(main_open + 1 + i, g.clone());
            }
        }
    }

    final_lines.join("\n") + if src.ends_with('\n') { "\n" } else { "" }
}

/// Find the line index of the closing `}` of `void main()`.
/// Returns the index of the `}` line.
#[cfg(feature = "wgpu_backend")]
fn find_main_close(lines: &[String]) -> Option<usize> {
    let mut in_main = false;
    let mut depth: i32 = 0;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if !in_main {
            if (trimmed.starts_with("void main(") || trimmed.contains(" main("))
                && trimmed.contains('(') && !trimmed.ends_with(';')
            {
                in_main = true;
                for ch in trimmed.chars() {
                    match ch { '{' => depth += 1, '}' => depth -= 1, _ => {} }
                }
                if depth <= 0 && trimmed.contains('{') { return Some(i); }
            }
        } else {
            for ch in trimmed.chars() {
                match ch { '{' => depth += 1, '}' => depth -= 1, _ => {} }
            }
            if depth <= 0 {
                return Some(i);
            }
        }
    }
    None
}

/// Find the line index of the opening `{` of `void main()`.
/// If `{` is on the same line as `void main(`, returns that line index.
#[cfg(feature = "wgpu_backend")]
fn find_main_open(lines: &[String]) -> Option<usize> {
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if (trimmed.starts_with("void main(") || trimmed.contains(" main("))
            && trimmed.contains('(') && !trimmed.ends_with(';')
        {
            // The `{` might be on this line or the next.
            if trimmed.contains('{') {
                return Some(i);
            }
            // Check next line.
            if i + 1 < lines.len() && lines[i + 1].trim().starts_with('{') {
                return Some(i + 1);
            }
        }
    }
    None
}

/// Decompose whole-array-constructor assignments to struct members.
///
/// naga uses separate type handles for an array type declared as a struct field
/// vs. a standalone array constructor, causing `InvalidStoreTypes` on whole-array
/// assignments like `geo.local = vec2[4](a, b, c, d);`.
///
/// This rewrites them to element-by-element:
///   `geo.local[0] = a; geo.local[1] = b; ...`
#[cfg(feature = "wgpu_backend")]
fn decompose_array_struct_stores(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        // Look for: something.field = type[N](
        if let Some(eq_pos) = trimmed.find('=') {
            let lhs = trimmed[..eq_pos].trim();
            let rhs = trimmed[eq_pos + 1..].trim();
            // Check lhs is "word.word" (struct field access, no indexing)
            let is_struct_field = lhs.contains('.') && !lhs.contains('[')
                && lhs.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_alphanumeric() || c == '_'));
            if is_struct_field {
                // Check rhs starts with "typeword[N]("
                let bracket = rhs.find('[');
                let paren   = rhs.find('(');
                if let (Some(bi), Some(pi)) = (bracket, paren) {
                    if bi < pi {
                        let type_word = &rhs[..bi];
                        let count_str = &rhs[bi + 1..rhs.find(']').unwrap_or(bi + 1)];
                        let is_type = type_word.chars().all(|c| c.is_alphanumeric() || c == '_') && !type_word.is_empty();
                        if is_type {
                            if let Ok(count) = count_str.parse::<usize>() {
                                // Collect all text from after "(" until closing ");".
                                let indent = &lines[i][..lines[i].len() - lines[i].trim_start().len()];
                                let after_paren = &rhs[pi + 1..];
                                let mut full_args = after_paren.to_string();
                                let mut j = i + 1;
                                while !full_args.contains(");") && j < lines.len() {
                                    full_args.push(' ');
                                    full_args.push_str(lines[j].trim());
                                    j += 1;
                                }
                                // Strip trailing ");".
                                if let Some(close) = full_args.rfind(");") {
                                    full_args = full_args[..close].to_string();
                                }
                                // Split args by comma.
                                let args: Vec<&str> = full_args.split(',').map(|a| a.trim()).collect();
                                if args.len() == count && count > 0 {
                                    for (idx, arg) in args.iter().enumerate() {
                                        out.push(format!("{}{}[{}] = {};", indent, lhs, idx, arg));
                                    }
                                    i = j;
                                    continue;
                                }
                            }
                        }
                    }
                }
            }
        }
        out.push(lines[i].to_string());
        i += 1;
    }

    out.join("\n") + if src.ends_with('\n') { "\n" } else { "" }
}

/// Rewrite functions that take `sampler2D` parameters to use `texture2D`.
///
/// After the sampler splitting pass rewrites `uniform sampler2D sColor0` to
/// `uniform texture2D sColor0`, helper functions that accept a `sampler2D`
/// parameter become invalid.  This function:
///   1. Changes `sampler2D` parameter type to `texture2D`
///   2. Renames the parameter from `sampler` (reserved in GLSL 4.50) to `_tex`
///   3. Wraps `texture(_tex, ...)` calls inside the function body with
///      `texture(sampler2D(_tex, global_sampler), ...)`
#[cfg(feature = "wgpu_backend")]
fn rewrite_sampler_params(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let n = lines.len();
    let mut out: Vec<String> = Vec::with_capacity(n);
    let mut i = 0;

    while i < n {
        let trimmed = lines[i].trim();

        // Detect function definition with sampler2D parameter.
        // Pattern: returnType funcName(sampler2D paramName, ...)  {
        if trimmed.contains("sampler2D") && trimmed.contains('(') && !trimmed.starts_with("//")
            && !trimmed.starts_with("uniform ") && !trimmed.starts_with("#")
        {
            // Extract parameter name: find "sampler2D " then the next identifier.
            if let Some(samp_pos) = trimmed.find("sampler2D ") {
                let after = &trimmed[samp_pos + "sampler2D ".len()..];
                let param_name: String = after.chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !param_name.is_empty() {
                    let new_param = "_tex";
                    // Rewrite this line: sampler2D → texture2D, paramName → _tex
                    let mut new_line = lines[i].replace("sampler2D", "texture2D");
                    new_line = new_line.replace(&param_name, new_param);
                    out.push(new_line);
                    i += 1;

                    // Collect the function body until closing `}` at depth 0.
                    let mut depth: i32 = if trimmed.contains('{') { 1 } else { 0 };
                    while i < n {
                        let lt = lines[i].trim();
                        for ch in lt.chars() {
                            match ch { '{' => depth += 1, '}' => depth -= 1, _ => {} }
                        }
                        // Rewrite texture(paramName, ...) → texture(sampler2D(paramName, global_sampler), ...)
                        let old_tex = format!("texture({},", new_param);
                        let new_tex = format!("texture(sampler2D({}, global_sampler),", new_param);
                        let line_out = lines[i].replace(&param_name, new_param)
                                                .replace(&old_tex, &new_tex);
                        out.push(line_out);
                        i += 1;
                        if depth <= 0 { break; }
                    }
                    continue;
                }
            }
        }

        out.push(lines[i].to_string());
        i += 1;
    }

    out.join("\n") + if src.ends_with('\n') { "\n" } else { "" }
}

#[cfg(feature = "wgpu_backend")]
fn preprocess_for_naga(src: &str, stage: naga::ShaderStage) -> String {
    use std::collections::{HashMap, HashSet};

    // ── Step 0: resolve WR_VERTEX_SHADER / WR_FRAGMENT_SHADER conditionals ──
    // The assembled GLSL still contains both #ifdef WR_VERTEX_SHADER and
    // #ifdef WR_FRAGMENT_SHADER blocks.  naga's built-in preprocessor normally
    // resolves these, but our text-based function-reordering pass
    // (move_definitions_before_prototypes) needs to see only code for the
    // current stage.  Resolve the simple `#ifdef WR_*_SHADER` / `#endif`
    // conditionals here so that all downstream passes work correctly.
    let src = resolve_stage_ifdefs(src, stage);

    // ── Step 0b: decompose matrix varyings into column vectors ──
    // naga 26 does not set IO_SHAREABLE on mat3/mat4, causing NotIOShareableType.
    // Replace each `flat varying matN X;` with N column-vector varyings + a
    // plain global mat, then inject glue code in main() to transfer columns.
    let src = decompose_matrix_varyings(&src, stage);

    // ── Step 0c: decompose whole-array stores to struct members ──
    // naga uses separate type handles for struct-embedded arrays vs standalone
    // array constructors, causing InvalidStoreTypes on `s.field = type[N](…)`.
    let src = decompose_array_struct_stores(&src);

    // ── Combined-sampler type table ──────────────────────────────────────────
    // Maps the GLSL combined sampler type keyword to the separate Vulkan-GLSL
    // texture type.  The constructor keyword (used in `texture()` wrappers) is
    // the same as the combined type.
    const SAMPLER_TYPES: &[(&str, &str)] = &[
        ("sampler2D",      "texture2D"),
        ("isampler2D",     "itexture2D"),
        ("usampler2D",     "utexture2D"),
        ("sampler2DArray", "texture2DArray"),
        ("sampler2DRect",  "texture2DRect"),
        ("sampler2DMS",    "texture2DMS"),
        ("samplerCube",    "textureCube"),
    ];

    // ── Pre-scan: identify combined-sampler variable names ───────────────────
    // Build a set of uniform variable names whose type is a combined sampler so
    // that Pass 2 can rewrite `texture(sName, ...)` call sites.
    let mut sampler_names: HashSet<String>  = HashSet::new();
    // Map: sampler var-name → combined type ("sampler2D", "isampler2D", ...)
    let mut sampler_type_map: HashMap<String, &'static str> = HashMap::new();

    for raw_line in src.lines() {
        let trimmed = raw_line.trim_start();
        let code = match trimmed.find("//") {
            Some(i) => trimmed[..i].trim_end(),
            None => trimmed,
        };
        if !code.starts_with("uniform ") || !code.ends_with(';') {
            continue;
        }
        let after_uniform = &code["uniform ".len()..code.len() - 1];
        let tokens: Vec<&str> = after_uniform.split_whitespace().collect();
        for &(samp_ty, _tex_ty) in SAMPLER_TYPES {
            if tokens.contains(&samp_ty) {
                if let Some(&name) = tokens.last() {
                    sampler_names.insert(name.to_string());
                    // First-occurrence wins: the true #ifdef branch always
                    // precedes the false #elif branch in the assembled source.
                    sampler_type_map.entry(name.to_string()).or_insert(samp_ty);
                }
            }
        }
    }

    // ── Pass 1: line-by-line rewriting ───────────────────────────────────────
    let mut name_to_binding: HashMap<String, u32> = HashMap::new();
    let mut next_binding: u32 = 0;
    let mut next_attr_loc: u32 = 0;     // vertex attribute input locations
    let mut next_vary_loc: u32 = 0;     // varying interface locations (vertex out / fragment in)
    let is_vertex   = stage == naga::ShaderStage::Vertex;
    let is_fragment = stage == naga::ShaderStage::Fragment;
    let mut global_sampler_injected = false;

    let mut out: Vec<String> = Vec::with_capacity(src.lines().count() + 4);

    for raw_line in src.lines() {
        let trimmed = raw_line.trim_start();
        let code = match trimmed.find("//") {
            Some(i) => trimmed[..i].trim_end(),
            None => trimmed,
        };
        let indent = &raw_line[..raw_line.len() - trimmed.len()];

        if trimmed.starts_with("#version") {
            out.push("#version 450".to_string());
            // Inject the shared sampler right after the version so it is
            // available to all shader stages.
            if !global_sampler_injected {
                out.push("layout(binding = 0, set = 1) uniform sampler global_sampler;".to_string());
                global_sampler_injected = true;
            }

        } else if trimmed.starts_with("#extension") {
            // naga rejects unknown #extension directives even in dead branches.

        } else if code.starts_with("precision ") && code.ends_with(';') {
            // GLES precision statements are invalid in GLSL 4.50 core.

        } else if code.starts_with("uniform ") && code.ends_with(';')
            && !code.starts_with("uniform struct ")
        {
            // Determine the variable name (last whitespace-delimited token).
            let var_name = code.trim_end_matches(';')
                .split_whitespace()
                .last()
                .unwrap_or("unknown")
                .to_string();

            // Assign a stable binding index.
            let binding = *name_to_binding.entry(var_name.clone()).or_insert_with(|| {
                let b = next_binding;
                next_binding += 1;
                b
            });

            if let Some(&samp_ty) = sampler_type_map.get(&var_name) {
                // Replace the combined sampler type with the Vulkan-GLSL texture type.
                // Drop precision qualifiers (HIGHP_SAMPLER_FLOAT macro etc.) — they
                // are irrelevant for texture2D/itexture2D declarations.
                let tex_ty = SAMPLER_TYPES
                    .iter()
                    .find(|&&(s, _)| s == samp_ty)
                    .map(|&(_, t)| t)
                    .unwrap_or("texture2D");
                out.push(format!(
                    "{}layout(binding = {}, set = 0) uniform {} {};",
                    indent, binding, tex_ty, var_name
                ));
            } else {
                out.push(format!(
                    "{}layout(binding = {}, set = 0) {}",
                    indent, binding, strip_precision(trimmed)
                ));
            }

        } else if code.ends_with(';') && !code.contains("layout(") {
            // Detect interface variable declarations:
            // [flat | smooth | noperspective | PER_INSTANCE] [varying | in | out | attribute] ...;
            // These need explicit location qualifiers to prevent BindingCollision.
            match storage_qual(code) {
                Some("attribute") | Some("in") if is_vertex => {
                    // Vertex attribute inputs — unique sequential locations.
                    out.push(format!("{}layout(location = {}) {}", indent, next_attr_loc, strip_precision(trimmed)));
                    next_attr_loc += 1;
                }
                Some("in") | Some("varying") if is_fragment => {
                    // Fragment varying inputs — must match vertex varying output locations.
                    out.push(format!("{}layout(location = {}) {}", indent, next_vary_loc, strip_precision(trimmed)));
                    next_vary_loc += 1;
                }
                Some("out") | Some("varying") if is_vertex => {
                    // Vertex varying outputs — must match fragment varying input locations.
                    out.push(format!("{}layout(location = {}) {}", indent, next_vary_loc, strip_precision(trimmed)));
                    next_vary_loc += 1;
                }
                Some("out") if is_fragment => {
                    // Fragment render-target output: WR uses a single colour target.
                    out.push(format!("{}layout(location = 0) {}", indent, strip_precision(trimmed)));
                }
                _ => {
                    out.push(raw_line.to_string());
                }
            }

        } else if trimmed.starts_with("#define TEX_SAMPLE(") {
            // Rewrite the macro body so the `sampler` parameter (which will be a
            // `texture2D` variable) is wrapped with the required combined-type
            // constructor before passing it to `texture()`.
            //
            // Original:  texture(sampler, tex_coord.xy)
            // Rewritten: texture(sampler2D(sampler, global_sampler), tex_coord.xy)
            let rewritten = raw_line.replace(
                "texture(sampler, ",
                "texture(sampler2D(sampler, global_sampler), ",
            );
            out.push(rewritten);

        } else {
            out.push(raw_line.to_string());
        }
    }

    let intermediate = out.join("\n");

    // ── Pass 2: rewrite texture() call sites ─────────────────────────────────
    // For each known sampler variable, replace the direct `texture(sName, ...)` form
    // with the Vulkan-GLSL `texture(sampler2D(sName, global_sampler), ...)` wrapper.
    // texelFetch / texelFetchOffset / textureSize work with bare texture2D and are
    // left untouched.
    let mut result = intermediate;
    for samp_name in &sampler_names {
        let old = format!("texture({},", samp_name);
        // Determine the combined-type constructor: sampler2D for float samplers,
        // isampler2D for integer, etc.  In practice WR never calls texture() on
        // integer samplers (they only use texelFetch), so sampler2D is always correct
        // for the texture() wrapper.
        let new = format!("texture(sampler2D({}, global_sampler),", samp_name);
        result = result.replace(&old, &new);
    }

    // ── Pass 3: rewrite functions with sampler2D parameters ──────────────────
    // Some shaders define helper functions like:
    //   vec4 sampleInUvRect(sampler2D sampler, vec2 uv, vec4 uvRect) { ... }
    // After sampler splitting, the uniform is now texture2D but the parameter
    // is still sampler2D.  Additionally, `sampler` is a reserved keyword in
    // GLSL 4.50.  Rewrite: change parameter type to texture2D, rename param,
    // and wrap internal texture() calls with the combined sampler constructor.
    result = rewrite_sampler_params(&result);

    // Global precision-qualifier strip: highp/mediump/lowp are GLES-only and
    // invalid in GLSL 4.50 core.  They can appear inside function bodies and
    // struct/uniform blocks where the per-line Pass 1 handler doesn't reach.
    result = strip_precision(&result);

    // Fix GLSL switch fall-through patterns that naga's WGSL emitter rejects.
    // fix_switch_fallthrough() applies up to five passes:
    //   1. Cascade labels — bare `case A:` → duplicate next case's body.
    //   2. Block-scoped terminators — `case X: { break; }` → remove inner break,
    //      keep {} for variable scoping, add case-level break after }.
    //   3. Missing break before switch-closing `}` → insert one.
    //   4. `default:` in the middle of a switch → reorder to last position.
    //   5. Return-only switches (all cases use `return expr;`) → convert to
    //      if-else chain so no switch fall-through tracking is needed.
    result = fix_switch_fallthrough(&result);

    // Reorder function definitions to satisfy naga's ForwardDependency check.
    // For each prototyped function whose definition appears later (brush_vs,
    // pattern_vertex, etc.), move the definition AND its preamble (struct types,
    // helper functions, varying declarations from the "specific" shader) to
    // where the prototype was.  This gives callee functions lower naga handles
    // than their callers.
    result = move_definitions_before_prototypes(&result);

    result
}

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

    // naga's validator can panic on certain malformed intermediate IR (e.g.,
    // index-out-of-bounds in the flow analyser).  Catch any internal panic and
    // convert it to a graceful skip-with-warning so the build never crashes.
    let glsl_owned = glsl.to_string();
    let name_s = name.to_string();
    let config_s = config.to_string();

    let outcome = std::panic::catch_unwind(move || {
        let module = glsl::Frontend::default()
            .parse(&glsl::Options::from(stage), &glsl_owned)
            .map_err(|e| format!(
                "GLSL->naga parse failed [shader={} config={:?}]: {:?}", name_s, config_s, e
            ))?;

        let info = Validator::new(ValidationFlags::all(), Capabilities::all())
            .validate(&module)
            .map_err(|e| format!(
                "naga validation failed [shader={} config={:?}]: {:?}", name_s, config_s, e
            ))?;
        wgsl::write_string(&module, &info, wgsl::WriterFlags::empty()).map_err(|e| format!(
            "WGSL emit failed [shader={} config={:?}]: {:?}", name_s, config_s, e
        ))
    });

    match outcome {
        Ok(inner) => inner,
        Err(panic_val) => {
            let msg = if let Some(s) = panic_val.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = panic_val.downcast_ref::<&str>() {
                s.to_string()
            } else {
                "unknown panic".to_string()
            };
            Err(format!("naga panicked [shader={} config={:?}]: {}", name, config, msg))
        }
    }
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

            // Preprocess: version-bump, strip #extension and precision,
            // split sampler2D declarations, assign locations and bindings.
            let vert_glsl = preprocess_for_naga(&vert_glsl, naga::ShaderStage::Vertex);
            let frag_glsl = preprocess_for_naga(&frag_glsl, naga::ShaderStage::Fragment);

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
                    if let Err(ref e) = vert_res {
                        println!("cargo:warning=WGSL translation skipped [{}#{}]: (vert) {}", shader_name, config, e);
                    }
                    if let Err(ref e) = frag_res {
                        println!("cargo:warning=WGSL translation skipped [{}#{}]: (frag) {}", shader_name, config, e);
                    }
                    fail_count += 1;
                }
            }
        }
    }

    // Stage 4f achieved 61/61 (100%) WGSL translation.  Any regressions from
    // future shader changes will surface as `cargo:warning` lines below.
    println!(
        "cargo:warning=WGSL translation: {}/{} variants succeeded",
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
