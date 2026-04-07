/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let target = env::var("TARGET").unwrap();
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let out_dir = PathBuf::from(out_dir);

    println!("cargo:rerun-if-changed=res/wrench.exe.manifest");
    if target.contains("windows") {
        let src = PathBuf::from("res/wrench.exe.manifest");
        let mut dst = out_dir
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_owned();
        dst.push("wrench.exe.manifest");
        fs::copy(&src, &dst).unwrap();
    }

    println!("cargo:rerun-if-changed=src/composite.cpp");

    cc::Build::new()
        .cpp(true)
        .file("src/composite.cpp")
        .compile("wr_composite");

    if target.contains("windows") {
        // Find Windows SDK lib path for dcomp.lib (DirectComposition)
        let sdk_lib_paths = [
            r"C:\Program Files (x86)\Windows Kits\10\Lib\10.0.26100.0\um\x64",
            r"C:\Program Files (x86)\Windows Kits\10\Lib\10.0.22621.0\um\x64",
            r"C:\Program Files (x86)\Windows Kits\10\Lib\10.0.22000.0\um\x64",
            r"C:\Program Files (x86)\Windows Kits\10\Lib\10.0.19041.0\um\x64",
        ];
        for path in sdk_lib_paths.iter() {
            if std::path::Path::new(path).exists() {
                println!("cargo:rustc-link-search=native={}", path);
                break;
            }
        }
        println!("cargo:rustc-link-lib=dcomp");
    }
}
