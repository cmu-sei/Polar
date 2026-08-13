// cert_issuer_init/src/keypair.rs

use rcgen::{CertificateParams, DistinguishedName, KeyPair, SanType, string::Ia5String};

#[derive(Debug, Clone, clap::ValueEnum, Default)]
pub enum KeyAlgorithm {
    #[default]
    Ed25519,
    EcdsaP256,
}

#[derive(Debug, thiserror::Error)]
pub enum KeypairError {
    #[error("identity must not be empty")]
    EmptyIdentity,
    #[error("CSR generation failed: {0}")]
    CsrFailed(String),
    #[error("invalid SAN: {0}")]
    InvalidSan(String),
    #[error("wildcard SANs are not permitted: {0}")]
    WildcardSanRejected(String),
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
pub fn generate_csr(
    identity: &str,
    algorithm: &KeyAlgorithm,
    extra_sans: &[String],
) -> Result<CsrOutput, KeypairError> {
    if identity.is_empty() {
        return Err(KeypairError::EmptyIdentity);
    }

    let pkcs = match algorithm {
        KeyAlgorithm::Ed25519 => &rcgen::PKCS_ED25519,
        KeyAlgorithm::EcdsaP256 => &rcgen::PKCS_ECDSA_P256_SHA256,
    };

    let key_pair =
        KeyPair::generate_for(pkcs).map_err(|e| KeypairError::CsrFailed(e.to_string()))?;

    // Identity SAN is always first and always present.
    let mut sans = vec![identity.to_string()];

    // Extra SANs are additive — syntactic validation only here,
    // the server re-validates before signing.
    for san in extra_sans {
        let san = san.trim().to_string();
        if san.is_empty() {
            return Err(KeypairError::InvalidSan(san));
        }
        if san.contains('*') {
            return Err(KeypairError::WildcardSanRejected(san));
        }
        sans.push(san);
    }

    let san_types = sans
        .iter()
        .map(|s| {
            Ia5String::try_from(s.as_str())
                .map(SanType::DnsName)
                .map_err(|e| KeypairError::CsrFailed(e.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut params =
        CertificateParams::new(vec![]).map_err(|e| KeypairError::CsrFailed(e.to_string()))?;

    // Identity SAN first, followed by any validated extra SANs.
    // The server re-validates and overrides these before signing.
    params.subject_alt_names = san_types;

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
