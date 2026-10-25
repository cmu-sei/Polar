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

/// What a certificate is for, as the extended key usage says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    Server,
    Client,
}

/// A bundle already in a directory, judged for use again.
#[derive(Debug, PartialEq, Eq)]
pub enum ExistingBundle {
    /// There is no `cert.pem`: nothing was written here yet.
    Absent,
    /// The bundle can be used as it is: its certificate is signed by `ca.pem`, valid now, names the identity and
    /// every requested name, has the requested usage, and `key.pem` is its key. `not_after` is when it expires.
    Reusable { not_after: String },
    /// A bundle is there but cannot be used, and why.
    Unusable(String),
}

/// Whether the bundle `write_bundle` left in `dir` can be used again by the same pod, at `now`.
///
/// A pod whose init container runs again without the pod being replaced (its sandbox recreated after a node
/// restart, a StatefulSet's pod restarted in place) keeps its UID, and with it its instance binding: the
/// credentials agent refuses it a second certificate while the first is valid (`AlreadyHasValidCertificate`).
/// The first one is still in the pod's own emptyDir, and it is the same certificate the agent would be refusing to
/// duplicate, so using it again issues nothing new. Anything short of a complete, matching, valid bundle is
/// `Unusable`, and the caller asks for a new certificate as before.
pub fn existing_bundle(
    dir: &Path,
    identity: &str,
    names: &[String],
    usage: Usage,
    now: std::time::SystemTime,
) -> ExistingBundle {
    use x509_parser::prelude::{ASN1Time, GeneralName};

    let read = |name: &str| std::fs::read(dir.join(name));
    let cert_pem = match read("cert.pem") {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ExistingBundle::Absent,
        Err(e) => return ExistingBundle::Unusable(format!("cert.pem cannot be read: {e}")),
    };
    let (key_pem, ca_pem) = match (read("key.pem"), read("ca.pem")) {
        (Ok(key), Ok(ca)) => (key, ca),
        (Err(e), _) => return ExistingBundle::Unusable(format!("key.pem cannot be read: {e}")),
        (_, Err(e)) => return ExistingBundle::Unusable(format!("ca.pem cannot be read: {e}")),
    };

    let unusable = |why: String| ExistingBundle::Unusable(why);
    let (_, cert_block) = match x509_parser::pem::parse_x509_pem(&cert_pem) {
        Ok(parsed) => parsed,
        Err(e) => return unusable(format!("cert.pem is not PEM: {e}")),
    };
    let cert = match cert_block.parse_x509() {
        Ok(cert) => cert,
        Err(e) => return unusable(format!("cert.pem is not a certificate: {e}")),
    };
    let (_, ca_block) = match x509_parser::pem::parse_x509_pem(&ca_pem) {
        Ok(parsed) => parsed,
        Err(e) => return unusable(format!("ca.pem is not PEM: {e}")),
    };
    let ca = match ca_block.parse_x509() {
        Ok(ca) => ca,
        Err(e) => return unusable(format!("ca.pem is not a certificate: {e}")),
    };

    if let Err(e) = cert.verify_signature(Some(ca.public_key())) {
        return unusable(format!("the certificate is not signed by ca.pem: {e}"));
    }

    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let at = match ASN1Time::from_timestamp(secs) {
        Ok(at) => at,
        Err(e) => return unusable(format!("the time to judge it at is out of range: {e}")),
    };
    let not_after = cert.validity().not_after;
    if !cert.validity().is_valid_at(at) {
        return unusable(format!(
            "the certificate is not valid now (valid {} to {not_after})",
            cert.validity().not_before
        ));
    }

    let dns_names: Vec<&str> = match cert.subject_alternative_name() {
        Ok(Some(san)) => san
            .value
            .general_names
            .iter()
            .filter_map(|n| match n {
                GeneralName::DNSName(name) => Some(*name),
                _ => None,
            })
            .collect(),
        Ok(None) => vec![],
        Err(e) => return unusable(format!("its subject alternative names cannot be read: {e}")),
    };
    if let Some(missing) = std::iter::once(identity)
        .chain(names.iter().map(String::as_str))
        .find(|wanted| !dns_names.contains(wanted))
    {
        return unusable(format!("the certificate does not name '{missing}'"));
    }

    let has_usage = match cert.extended_key_usage() {
        Ok(Some(eku)) => match usage {
            Usage::Server => eku.value.server_auth,
            Usage::Client => eku.value.client_auth,
        },
        Ok(None) => false,
        Err(e) => return unusable(format!("its extended key usage cannot be read: {e}")),
    };
    if !has_usage {
        return unusable(format!("the certificate is not for {usage:?} use"));
    }

    let key = match std::str::from_utf8(&key_pem)
        .map_err(|e| e.to_string())
        .and_then(|pem| rcgen::KeyPair::from_pem(pem).map_err(|e| e.to_string()))
    {
        Ok(key) => key,
        Err(e) => return unusable(format!("key.pem is not a private key: {e}")),
    };
    if key.public_key_raw() != cert.public_key().subject_public_key.data.as_ref() {
        return unusable("key.pem is not the certificate's key".to_string());
    }

    ExistingBundle::Reusable {
        not_after: not_after.to_string(),
    }
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
