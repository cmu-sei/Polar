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

// Links Scudo, which nix-scudo builds with our configuration (config/custom_scudo_config.h): src/agents/workspace.nix
// puts the library for each target at $POLAR_SCUDO_LIB_DIR/<target>/libpolar_scudo.a. The workspace's Nix build, the
// development shell, the development container and the CI container all set the variable, so every build is the same
// `cargo build` and links the same library. The wrappers' names carry the prefix `polar_scudo_`, so they do not take
// the place of musl's malloc in a static binary.
//
// Beside it, libpolar_mem.a: LLVM libc's memcpy and memmove, linked whole, so they are defined before the libc is
// searched wherever the first reference to either comes on the link line, and every binary calls them and not the
// libc's (docs/architecture/broker-baseline.md, "The fourth set"; tests/mem.rs).

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=POLAR_SCUDO_LIB_DIR");
    let target = env::var("TARGET").unwrap();
    let Some(dir) = env::var_os("POLAR_SCUDO_LIB_DIR") else {
        panic!(
            "polar-scudo: POLAR_SCUDO_LIB_DIR is not set. Build in the development shell (nix develop), the development \
             container or through the workspace's Nix build, which set it to the Scudo that Polar's flake builds."
        );
    };
    let dir = PathBuf::from(dir).join(&target);
    for name in ["libpolar_scudo.a", "libpolar_mem.a"] {
        let lib = dir.join(name);
        if !lib.is_file() {
            panic!(
                "polar-scudo: no {}. Polar's binaries are built for x86_64 and aarch64 Linux, musl, and tested on the \
                 host's gnu target; Scudo is not built for {target}.",
                lib.display()
            );
        }
        println!("cargo:rerun-if-changed={}", lib.display());
    }
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=static=polar_scudo");
    println!("cargo:rustc-link-lib=static:+whole-archive=polar_mem");
}
