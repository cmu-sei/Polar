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

// cert_issuer_init/src/output.rs

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OutputError {
    #[error("output directory does not exist: {0}")]
    DirMissing(PathBuf),
    #[error("write failed: {0}")]
    WriteFailed(#[from] std::io::Error),
}

pub struct OutputBundle<'a> {
    pub cert_pem: &'a str,
    pub key_pem: &'a str,
    pub ca_pem: &'a str,
}

/// Write the cert bundle to `dir` atomically.
///
/// Each file is written to a `.tmp` sibling and renamed into place.
/// This means a reader of the shared emptyDir never sees a partial
/// write — the workload container either gets all three files from
/// a previous run or all three from the current run, never a mix.
///
/// Permissions:
///   cert.pem  0644 — public, world-readable
///   key.pem   0640 — secret, readable only by owner and group
///   ca.pem    0644 — public, world-readable
///
/// The pod spec is responsible for ensuring the init container and
/// workload container share a GID that can read key.pem at 0640.
pub fn write_bundle(dir: &Path, bundle: &OutputBundle<'_>) -> Result<(), OutputError> {
    if !dir.exists() {
        return Err(OutputError::DirMissing(dir.to_path_buf()));
    }

    write_atomic(dir, "cert.pem", bundle.cert_pem.as_bytes(), 0o644)?;
    write_atomic(dir, "key.pem", bundle.key_pem.as_bytes(), 0o640)?;
    write_atomic(dir, "ca.pem", bundle.ca_pem.as_bytes(), 0o644)?;

    Ok(())
}

fn write_atomic(dir: &Path, filename: &str, contents: &[u8], mode: u32) -> Result<(), OutputError> {
    let final_path = dir.join(filename);
    let tmp_path = dir.join(format!("{filename}.tmp"));

    // Write to .tmp first. If this fails partway through, the
    // existing file (if any) is untouched.
    std::fs::write(&tmp_path, contents)?;
    std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(mode))?;

    // Atomic rename. On Linux with a local filesystem this is
    // guaranteed atomic by the kernel. The emptyDir backing store
    // is always a local tmpfs, so this holds.
    std::fs::rename(&tmp_path, &final_path)?;

    Ok(())
}
