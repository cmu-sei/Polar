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

// Every Polar binary allocates with Scudo, and nothing else is a global allocator or a dependency that brings one: one
// allocator in every binary, on every target (docs/architecture/broker-baseline.md, "Decided"). The workspace's
// binaries are read from `cargo metadata`, so a new one is held to it without being listed here.

use std::path::Path;
use std::process::Command;

const DECLARATION: &str = "static GLOBAL: polar_scudo::Scudo = polar_scudo::Scudo;";
const OTHER_ALLOCATORS: &[&str] = &[
    "mimalloc",
    "smalloc",
    "smmalloc",
    "jemallocator",
    "tikv-jemallocator",
];

fn metadata() -> serde_json::Value {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml");
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()
        .expect("cargo metadata runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("cargo metadata is JSON")
}

#[test]
fn every_binary_declares_scudo_as_its_allocator() {
    let meta = metadata();
    let mut binaries = 0;
    let mut wrong = Vec::new();
    for package in meta["packages"].as_array().unwrap() {
        for target in package["targets"].as_array().unwrap() {
            if !target["kind"]
                .as_array()
                .unwrap()
                .iter()
                .any(|k| k == "bin")
            {
                continue;
            }
            binaries += 1;
            let path = target["src_path"].as_str().unwrap();
            let source = std::fs::read_to_string(path).unwrap();
            if source.matches("#[global_allocator]").count() != 1 || !source.contains(DECLARATION) {
                wrong.push(format!("{} ({path})", target["name"].as_str().unwrap()));
            }
        }
    }
    assert!(
        binaries > 30,
        "only {binaries} binaries found: is this the workspace?"
    );
    assert!(
        wrong.is_empty(),
        "binaries that do not allocate with Scudo alone: {wrong:#?}"
    );
}

#[test]
fn no_package_depends_on_another_allocator() {
    let meta = metadata();
    let mut found = Vec::new();
    for package in meta["packages"].as_array().unwrap() {
        for dep in package["dependencies"].as_array().unwrap() {
            let name = dep["name"].as_str().unwrap();
            if OTHER_ALLOCATORS.contains(&name) {
                found.push(format!("{} -> {name}", package["name"].as_str().unwrap()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "packages that bring another allocator: {found:#?}"
    );
}
