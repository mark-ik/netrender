/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Reflects WebRender's committed SPIR-V corpus and emits a bindings
//! manifest. See `webrender_build/src/spirv_reflect.rs` for the
//! reflection logic and the assignment doc A1 / wgpu device plan for the
//! verification-oracle role.
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

use std::fs;
use std::path::PathBuf;
use webrender_build::spirv_reflect::reflect_dir;

fn main() {
    let mut args = std::env::args().skip(1);
    let spirv_dir = PathBuf::from(args.next().unwrap_or_else(|| "webrender/res/spirv".into()));
    let out_path = PathBuf::from(
        args.next()
            .unwrap_or_else(|| "webrender/res/spirv/bindings.json".into()),
    );

    let result = reflect_dir(&spirv_dir);

    for (file, err) in &result.errors {
        eprintln!("  FAIL {} -- {}", file, err);
    }
    let stage_count: usize = result
        .manifest
        .shaders
        .values()
        .map(|s| s.vert.is_some() as usize + s.frag.is_some() as usize)
        .sum();
    for shader in result.manifest.shaders.keys() {
        if let Some(_) = result.manifest.shaders[shader].vert {
            println!("  ok  {}.vert.spv", shader);
        }
        if let Some(_) = result.manifest.shaders[shader].frag {
            println!("  ok  {}.frag.spv", shader);
        }
    }

    let json = serde_json::to_string_pretty(&result.manifest).expect("serialize");
    fs::write(&out_path, json).unwrap_or_else(|e| panic!("write {}: {}", out_path.display(), e));

    println!(
        "\n{} shader stages reflected, {} errors. wrote {}",
        stage_count,
        result.errors.len(),
        out_path.display(),
    );
    if !result.errors.is_empty() {
        std::process::exit(1);
    }
}
