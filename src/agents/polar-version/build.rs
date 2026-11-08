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

// The platform version and the target, for src/lib.rs.
//
// The version is the repository's VERSION file. The workspace's Nix build sees only src/agents (src/agents/workspace.nix
// gives crane a source rooted there), so it reads the file itself and passes its contents as POLAR_VERSION; every other
// build (the development shell, the development and CI containers, a checkout) reads the file, three directories up. Only
// the file's contents reach the binary, so a commit that leaves VERSION alone leaves every binary as it was.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=POLAR_VERSION");
    let version = match env::var("POLAR_VERSION") {
        Ok(given) => given,
        Err(_) => {
            let file =
                PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../../VERSION");
            println!("cargo:rerun-if-changed={}", file.display());
            std::fs::read_to_string(&file).unwrap_or_else(|e| {
                panic!(
                    "polar-version: cannot read {} ({e}). Build from a checkout of the repository, or set POLAR_VERSION \
                     to the platform version, as the workspace's Nix build does.",
                    file.display()
                )
            })
        }
    };
    let version = version.trim();
    // Whether it is a version is src/lib.rs's to judge, at compile time. Here only that it cannot be read as another
    // instruction to cargo.
    if version.is_empty()
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'+')
    {
        panic!("polar-version: VERSION holds {version:?}, which is not X.Y.Z or X.Y.Z+dev");
    }
    println!("cargo:rustc-env=POLAR_PLATFORM_VERSION={version}");
    println!(
        "cargo:rustc-env=POLAR_TARGET={}",
        env::var("TARGET").unwrap()
    );
}
