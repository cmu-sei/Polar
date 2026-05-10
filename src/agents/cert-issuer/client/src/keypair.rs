// cert_issuer_init/src/keypair.rs

use rcgen::{CertificateParams, DistinguishedName, KeyPair, SanType, string::Ia5String};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum KeypairError {
    #[error("identity must not be empty")]
    EmptyIdentity,
    #[error("CSR generation failed: {0}")]
    CsrFailed(String),
}

/// The output of a successful CSR generation: the PEM-encoded CSR
/// ready to POST to the cert issuer, and the private key to write
/// to the shared volume alongside the issued cert.
#[derive(Debug)]
pub struct CsrOutput {
    pub csr_pem: String,
    pub private_key_pem: String,
}

/// Generate an Ed25519 keypair and a CSR with `identity` as the
/// sole DNS SAN.
///
/// The identity must already be in DNS-safe form — callers are
/// responsible for normalizing the JWT `sub` claim via
/// `cert_issuer_common::identity::normalize_identity` before
/// calling this function. This function has no knowledge of OIDC
/// issuers or Kubernetes SA naming conventions.
///
/// The private key never leaves this function's return value.
/// The caller writes it to the shared emptyDir; the cert issuer
/// never sees it.
pub fn generate_csr(identity: &str) -> Result<CsrOutput, KeypairError> {
    if identity.is_empty() {
        return Err(KeypairError::EmptyIdentity);
    }

    // Ed25519 is the required algorithm per the architecture spec:
    // small keys, fast verification, no parameter ambiguity.
    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ED25519)
        .map_err(|e| KeypairError::CsrFailed(e.to_string()))?;

    let mut params =
        CertificateParams::new(vec![]).map_err(|e| KeypairError::CsrFailed(e.to_string()))?;

    // Exactly one DNS SAN. The cert issuer's parser rejects CSRs
    // with zero or more than one SAN.
    params.subject_alt_names = vec![SanType::DnsName(
        Ia5String::try_from(identity).map_err(|e| KeypairError::CsrFailed(e.to_string()))?,
    )];

    // Empty distinguished name. The identity is entirely in the SAN;
    // the Subject field carries no information in this protocol and
    // some CA implementations ignore it entirely for client certs.
    params.distinguished_name = DistinguishedName::new();

    let csr = params
        .serialize_request(&key_pair)
        .map_err(|e| KeypairError::CsrFailed(e.to_string()))?;

    let csr_pem = csr
        .pem()
        .map_err(|e| KeypairError::CsrFailed(e.to_string()))?;

    // PKCS#8 PEM. The "BEGIN PRIVATE KEY" label (not "BEGIN ED25519
    // PRIVATE KEY") is what the cert issuer's parser expects and what
    // standard tooling produces.
    let private_key_pem = key_pair.serialize_pem();

    Ok(CsrOutput {
        csr_pem,
        private_key_pem,
    })
}
