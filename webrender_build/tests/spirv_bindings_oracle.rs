/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Verifies that webrender/res/spirv/bindings.json is regenerable from
//! the committed SPIR-V corpus byte-identically. This is the CI-side
//! enforcement of the oracle's "no-drift" guarantee — if a shader edit
//! changes the binding contract, this test fails until the manifest is
//! re-committed.
//!
//! Run with:
//!   cargo test -p webrender_build --features shader-reflect

#![cfg(feature = "shader-reflect")]

use std::path::PathBuf;
use webrender_build::spirv_reflect::{reflect_dir, BindingsManifest};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is webrender_build/; workspace root is its parent.
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.pop();
    root
}

#[test]
fn bindings_json_matches_fresh_reflection() {
    let root = workspace_root();
    let spirv_dir = root.join("webrender").join("res").join("spirv");
    let manifest_path = spirv_dir.join("bindings.json");

    assert!(spirv_dir.exists(), "{} not found", spirv_dir.display());
    assert!(
        manifest_path.exists(),
        "{} not found — run `cargo run -p webrender_build --features shader-reflect --bin reflect_spirv` to generate",
        manifest_path.display()
    );

    let committed_text = std::fs::read_to_string(&manifest_path).expect("read manifest");
    let committed: BindingsManifest =
        serde_json::from_str(&committed_text).expect("parse committed manifest");

    let fresh = reflect_dir(&spirv_dir).manifest;

    if committed != fresh {
        // Produce a useful diff hint without dumping both manifests in full.
        let committed_keys: Vec<_> = committed.shaders.keys().collect();
        let fresh_keys: Vec<_> = fresh.shaders.keys().collect();
        let only_committed: Vec<_> =
            committed_keys.iter().filter(|k| !fresh_keys.contains(k)).collect();
        let only_fresh: Vec<_> =
            fresh_keys.iter().filter(|k| !committed_keys.contains(k)).collect();
        let differing: Vec<_> = committed
            .shaders
            .iter()
            .filter(|(k, v)| fresh.shaders.get(*k).map(|f| f != *v).unwrap_or(false))
            .map(|(k, _)| k)
            .collect();

        panic!(
            "bindings.json drift detected.\n  shaders only in committed: {:?}\n  shaders only in fresh:     {:?}\n  shaders with differing reflection: {:?}\n\nRegenerate with: cargo run -p webrender_build --features shader-reflect --bin reflect_spirv",
            only_committed, only_fresh, differing
        );
    }
}

#[test]
fn ps_clear_reflection_is_canonical() {
    // Smoke test: ps_clear is the wgpu device's smallest target and our
    // canonical example for the oracle. If its reflection changes, the
    // contract for the wgpu RenderPipeline auto-derive verification
    // changes. Pin the expected shape so a future shader edit that
    // accidentally affects ps_clear's bindings is loud.
    let root = workspace_root();
    let spirv_dir = root.join("webrender").join("res").join("spirv");
    let manifest = reflect_dir(&spirv_dir).manifest;

    let entry = manifest
        .shaders
        .get("ps_clear")
        .expect("ps_clear missing from reflection");
    let vert = entry.vert.as_ref().expect("ps_clear vert missing");
    let frag = entry.frag.as_ref().expect("ps_clear frag missing");

    // Vertex stage: 1 uniform_buffer (the WrLocals UBO) + 3 vertex inputs.
    assert_eq!(vert.bindings.len(), 1, "ps_clear vert bindings: {:?}", vert.bindings);
    assert_eq!(vert.bindings[0].kind, "uniform_buffer");
    assert_eq!(vert.vertex_inputs.len(), 3, "ps_clear vert inputs: {:?}", vert.vertex_inputs);
    let names: Vec<&str> = vert.vertex_inputs.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, vec!["aPosition", "aRect", "aColor"]);
    let locations: Vec<u32> = vert.vertex_inputs.iter().map(|v| v.location).collect();
    assert_eq!(locations, vec![0, 1, 2]);

    // Fragment stage: ps_clear has no textures or uniforms.
    assert!(frag.bindings.is_empty(), "ps_clear frag bindings non-empty: {:?}", frag.bindings);
    assert!(frag.vertex_inputs.is_empty());
}
