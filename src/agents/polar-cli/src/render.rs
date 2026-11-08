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
