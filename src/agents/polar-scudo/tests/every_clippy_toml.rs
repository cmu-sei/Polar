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

// Every clippy.toml in the workspace forbids holding an entered tracing span across an await (the lint
// `await-holding-invalid-types` for `tracing::span::Entered` and `EnteredSpan`). A crate's own clippy.toml replaces the
// workspace's rather than adding to it, so a new crate that brings one without these entries silently loses the lint
// (soft-state/observe did, 2026-10-30): an entered span held across an await corrupts the subscriber's span stack and
// panics a worker ("tried to clone a span that already closed"). The files are found by walking the workspace, so a
// new one is held to it without being listed here. Beside every_binary.rs, the workspace's other check.

use std::path::{Path, PathBuf};

/// The clippy.toml files under `dir`, leaving out build output and hidden directories.
fn clippy_tomls(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name == "target" || name.starts_with('.') {
                continue;
            }
            clippy_tomls(&path, found);
        } else if name == "clippy.toml" {
            found.push(path);
        }
    }
}

/// Whether a clippy.toml's text names both entered-span types under `await-holding-invalid-types`.
fn forbids_entered_spans_across_awaits(text: &str) -> bool {
    text.contains("await-holding-invalid-types")
        && text.contains("\"tracing::span::Entered\"")
        && text.contains("\"tracing::span::EnteredSpan\"")
}

#[test]
fn every_clippy_toml_forbids_holding_an_entered_span_across_an_await() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut found = Vec::new();
    clippy_tomls(&workspace, &mut found);
    assert!(
        found.contains(&workspace.join("clippy.toml")),
        "the workspace's own clippy.toml is found: {found:?}"
    );
    let missing: Vec<_> = found
        .iter()
        .filter(|path| {
            !std::fs::read_to_string(path)
                .map(|text| forbids_entered_spans_across_awaits(&text))
                .unwrap_or(false)
        })
        .collect();
    assert!(
        missing.is_empty(),
        "these clippy.toml files replace the workspace's without forbidding an entered span across an await; add \
         await-holding-invalid-types with tracing::span::Entered and tracing::span::EnteredSpan (copy the \
         workspace's): {missing:?}"
    );
}

#[test]
fn an_entry_is_recognised_only_with_both_span_types() {
    let both = r#"await-holding-invalid-types = [
    { path = "tracing::span::Entered", reason = "r" },
    { path = "tracing::span::EnteredSpan", reason = "r" },
]"#;
    assert!(forbids_entered_spans_across_awaits(both));
    assert!(!forbids_entered_spans_across_awaits(
        r#"await-holding-invalid-types = [ { path = "tracing::span::EnteredSpan", reason = "r" } ]"#
    ));
    assert!(!forbids_entered_spans_across_awaits("msrv = \"1.85\"\n"));
}
