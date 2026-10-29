//! Dhall to YAML, by `dhall-to-yaml`.
//!
//! The tool does not carry a Dhall evaluator: `dhall-to-yaml` is the reference one, and what it
//! prints is what a plan's reviewers read. This only runs it and makes its failures readable.

use anyhow::{Result, bail};
use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;

/// Evaluate a Dhall file to YAML. `documents` renders a list as one YAML document per element.
pub fn dhall_to_yaml(path: &Path, documents: bool) -> Result<String> {
    let mut command = Command::new("dhall-to-yaml");
    if documents {
        command.arg("--documents");
    }
    command.arg("--file").arg(path);
    let output = match command.output() {
        Ok(output) => output,
        Err(e) if e.kind() == ErrorKind::NotFound => bail!(
            "dhall-to-yaml is not installed or not on PATH; it is part of dhall-yaml \
             (https://github.com/dhall-lang/dhall-haskell), and renders {}",
            path.display()
        ),
        Err(e) => bail!("cannot run dhall-to-yaml: {e}"),
    };
    if !output.status.success() {
        bail!(
            "dhall-to-yaml could not render {}:\n{}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim_end()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
