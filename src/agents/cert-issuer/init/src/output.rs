//! Write cert, key, and CA chain to the shared volume.
//!
//! File layout (default):
//!   /var/run/polar/certs/cert.pem
//!   /var/run/polar/certs/key.pem
//!   /var/run/polar/certs/ca.pem
//!
//! All files are written with mode 0640. The directory must already
//! exist (it's an emptyDir mounted by Kubernetes) and the init
//! container's user must have write permission.

use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OutputError {
    #[error("output directory does not exist: {0}")]
    DirMissing(String),
    #[error("output directory is not writable: {0}")]
    DirNotWritable(String),
    #[error("write failed: {0}")]
    WriteFailed(String),
}

pub struct OutputBundle<'a> {
    pub cert_pem: &'a str,
    pub key_pem: &'a str,
    pub ca_pem: &'a str,
}

pub fn write_bundle(_dir: &Path, _bundle: &OutputBundle) -> Result<(), OutputError> {
    todo!("write three files atomically (write to .tmp, rename)");
}
