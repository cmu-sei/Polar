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

// cert_issuer_common/src/keypair.rs

use rcgen::{CertificateParams, DistinguishedName, KeyPair, SanType, string::Ia5String};

#[derive(Debug, Clone, clap::ValueEnum, Default)]
pub enum KeyAlgorithm {
    #[default]
    Ed25519,
    EcdsaP256,
    /// 4096-bit RSA, matching EscrowKeypair::generate()'s own,
    /// established convention. Genuinely different from Ed25519/
    /// EcdsaP256: those are signing-only algorithms, right for
    /// ordinary mTLS client authentication, but incapable of
    /// encryption. This variant exists for the one case that
    /// actually needs an encryption-capable key -- an agent's own
    /// CertType::Service escrow identity, which other agents encrypt
    /// payloads *to* (KeyUsage: KeyEncipherment, not
    /// DigitalSignature; see cert-issuer-common's own CertType::Service
    /// doc comment).
    ///
    /// Generated directly via rsa::RsaPrivateKey, not
    /// rcgen::KeyPair::generate_for -- rcgen's own RSA key generation
    /// only works with the aws_lc_rs backend, which this workspace
    /// deliberately doesn't use (a C/C++ build dependency not worth
    /// taking on; see this crate's own commit history for the
    /// discussion). The externally-generated key is loaded into
    /// rcgen via KeyPair::from_pem for the actual CSR signing step,
    /// the same approach already proven working in this crate's own
    /// public_key.rs tests.
    Rsa,
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

/// Generate a keypair and a CSR with `identity` as the sole DNS SAN.
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
///
/// Moved here from cert-issuer/client (its original, sole caller)
/// once CryptoActor's own issuance-fallback flow (polar-crypto)
/// needed this same, pure, local CSR-generation logic without
/// pulling in cert-client's own HandshakeClient/HTTP machinery --
/// cert-issuer-common is the crate already shared between
/// cert-issuer and every one of its consumers, with no HTTP
/// dependency of its own.
pub fn generate_csr(
    identity: &str,
    algorithm: &KeyAlgorithm,
    extra_sans: &[String],
) -> Result<CsrOutput, KeypairError> {
    if identity.is_empty() {
        return Err(KeypairError::EmptyIdentity);
    }

    let key_pair = match algorithm {
        KeyAlgorithm::Ed25519 => KeyPair::generate_for(&rcgen::PKCS_ED25519)
            .map_err(|e| KeypairError::CsrFailed(e.to_string()))?,
        KeyAlgorithm::EcdsaP256 => KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
            .map_err(|e| KeypairError::CsrFailed(e.to_string()))?,
        KeyAlgorithm::Rsa => {
            // Generated directly, not via KeyPair::generate_for --
            // see KeyAlgorithm::Rsa's own doc comment for why.
            let mut rng = rand::rng();
            let rsa_private_key = rsa::RsaPrivateKey::new(&mut rng, 4096)
                .map_err(|e| KeypairError::CsrFailed(e.to_string()))?;
            let rsa_private_key_pem = rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(
                &rsa_private_key,
                rsa::pkcs8::LineEnding::LF,
            )
            .map_err(|e| KeypairError::CsrFailed(e.to_string()))?;
            KeyPair::from_pem(&rsa_private_key_pem)
                .map_err(|e| KeypairError::CsrFailed(e.to_string()))?
        }
    };

    let csr_pem = csr_for_key(identity, extra_sans, &key_pair)?;

    // PKCS#8 PEM. The "BEGIN PRIVATE KEY" label (not "BEGIN ED25519
    // PRIVATE KEY") is what the cert issuer's parser expects and what
    // standard tooling produces.
    let private_key_pem = key_pair.serialize_pem();

    Ok(CsrOutput {
        csr_pem,
        private_key_pem,
    })
}

/// A CSR for `identity` and `extra_sans`, signed by `key`, which need not be a key this process can see: a key held
/// by the custody daemon (`docs/architecture/key-custody.md`) signs its own CSR, and `generate_csr` signs with the key
/// it makes. The identity is the first SAN, then the extra names, each checked as `generate_csr` checks them.
pub fn csr_for_key(
    identity: &str,
    extra_sans: &[String],
    key: &impl rcgen::SigningKey,
) -> Result<String, KeypairError> {
    if identity.is_empty() {
        return Err(KeypairError::EmptyIdentity);
    }
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
        .serialize_request(key)
        .map_err(|e| KeypairError::CsrFailed(e.to_string()))?;

    let csr_pem = csr
        .pem()
        .map_err(|e| KeypairError::CsrFailed(e.to_string()))?;

    Ok(csr_pem)
}
