/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

// Compiles Scudo as upstream does (compiler-rt/lib/scudo/standalone/CMakeLists.txt): its sources and the C wrappers,
// C++17 without exceptions, RTTI or the C++ standard library, crc32_hw.cpp with the CRC32 instructions. Two changes:
// the wrappers' names carry the prefix `polar_scudo_`, so they do not take the place of musl's malloc in a static
// binary, and the configuration is ours (SCUDO_USE_CUSTOM_CONFIG), chosen by POLAR_SCUDO_CONFIG.
//
// Without SCUDO_SRC nothing is compiled, so `cargo check --workspace` works anywhere; a binary that uses the allocator
// then fails to link, saying which symbols are missing, and the crate's tests (which run on it) are left out.

use std::env;
use std::path::PathBuf;

const SOURCES: &[&str] = &[
    "checksum.cpp",
    "common.cpp",
    "condition_variable_linux.cpp",
    "flags_parser.cpp",
    "flags.cpp",
    "linux.cpp",
    "mem_map.cpp",
    "mem_map_linux.cpp",
    "release.cpp",
    "report.cpp",
    "report_linux.cpp",
    "string_utils.cpp",
    "timing.cpp",
    "wrappers_c.cpp",
];

fn build(src: &PathBuf, config: &PathBuf) -> cc::Build {
    let mut b = cc::Build::new();
    b.cpp(true)
        .cpp_link_stdlib(None)
        .include(config)
        .include(src)
        .include(src.join("include"))
        .define("SCUDO_USE_CUSTOM_CONFIG", None)
        .define("SCUDO_PREFIX_NAME", "polar_scudo_")
        .flag("-std=c++17")
        .flag("-fno-exceptions")
        .flag("-fno-rtti")
        .flag("-nostdinc++")
        .flag("-fvisibility=hidden")
        .opt_level(3)
        .warnings(false);
    b
}

fn main() {
    println!("cargo:rerun-if-env-changed=SCUDO_SRC");
    println!("cargo:rerun-if-env-changed=POLAR_SCUDO_CONFIG");
    println!("cargo:rerun-if-changed=configs");
    println!("cargo::rustc-check-cfg=cfg(polar_scudo_built)");

    let Some(src) = env::var_os("SCUDO_SRC").map(PathBuf::from) else {
        println!("cargo:warning=polar-scudo: SCUDO_SRC is not set, so Scudo is not compiled");
        return;
    };
    let name = env::var("POLAR_SCUDO_CONFIG").unwrap_or_else(|_| "default".into());
    let config = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("configs")
        .join(&name);
    assert!(
        config.join("custom_scudo_config.h").exists(),
        "polar-scudo: no configuration {name} (configs/{name}/custom_scudo_config.h)"
    );

    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let crc = match arch.as_str() {
        "x86_64" => "-mcrc32",
        "aarch64" => "-mcrc",
        _ => "",
    };
    let mut hw = build(&src, &config);
    hw.file(src.join("crc32_hw.cpp"));
    if !crc.is_empty() {
        hw.flag_if_supported(crc);
    }
    hw.compile("polar_scudo_crc32");

    let mut lib = build(&src, &config);
    for f in SOURCES {
        lib.file(src.join(f));
    }
    lib.compile("polar_scudo");
    // The tests run on the allocator, so only when it was compiled.
    println!("cargo:rustc-cfg=polar_scudo_built");
}
