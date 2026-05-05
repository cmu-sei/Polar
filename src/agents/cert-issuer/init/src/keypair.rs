//! Keypair generation and CSR construction.
//!
//! The private key is generated locally in the init container and
//! never leaves the pod. The public key goes into the CSR.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum KeypairError {
    #[error("keypair generation failed: {0}")]
    GenerationFailed(String),
    #[error("CSR construction failed: {0}")]
    CsrFailed(String),
}

#[derive(Debug)]
pub struct GeneratedCsr {
    pub csr_pem: String,
    pub private_key_pem: String,
}

/// Generate an Ed25519 keypair and a CSR with the given identity
/// as the SAN. The returned private key PEM is what the workload
/// container will read at startup; it must be written to the shared
/// volume.
pub fn generate_csr(_identity: &str) -> Result<GeneratedCsr, KeypairError> {
    todo!("generate Ed25519 keypair, build CSR with single-SAN");
}
