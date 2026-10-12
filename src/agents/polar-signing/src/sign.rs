//! Making a signed document.

use crate::canonical::CanonicalError;
use crate::cert;
use crate::document::{Algorithm, Document, SignatureBlock, signing_input};
use ed25519_dalek::Signer as _;
use ed25519_dalek::pkcs8::DecodePrivateKey as _;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignError {
    #[error("the payload is an object")]
    PayloadNotAnObject,
    #[error("no certificate to sign with")]
    EmptyChain,
    #[error("the signing certificate does not parse: {0}")]
    BadCertificate(String),
    #[error("the private key does not parse: {0}")]
    BadKey(String),
    #[error("the signing certificate's key is neither Ed25519 nor ECDSA P-256")]
    UnsupportedKeyType,
    #[error("the private key does not belong to the signing certificate")]
    KeyCertificateMismatch,
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
}

/// Signs `payload` with `key_pem`, which must be the key of the first certificate in
/// `certificate_chain_pem` (the signer's, then whatever links it to the CA). Both algorithms sign
/// deterministically, so the same inputs give the same signature.
pub fn sign(
    payload: Value,
    certificate_chain_pem: &[String],
    key_pem: &str,
) -> Result<Document, SignError> {
    if !payload.is_object() {
        return Err(SignError::PayloadNotAnObject);
    }
    let leaf_pem = certificate_chain_pem.first().ok_or(SignError::EmptyChain)?;
    let leaf_der = cert::der_from_pem(leaf_pem).map_err(SignError::BadCertificate)?;
    let leaf = cert::parse(&leaf_der).map_err(SignError::BadCertificate)?;
    let (algorithm, public_key) = cert::key_of(&leaf).ok_or(SignError::UnsupportedKeyType)?;

    let key_der = x509_parser::pem::parse_x509_pem(key_pem.as_bytes())
        .map(|(_, block)| block.contents)
        .map_err(|e| SignError::BadKey(e.to_string()))?;
    let message = signing_input(&payload)?;

    let value = match algorithm {
        Algorithm::Ed25519 => {
            let key = ed25519_dalek::SigningKey::from_pkcs8_der(&key_der)
                .map_err(|e| SignError::BadKey(e.to_string()))?;
            if key.verifying_key().as_bytes().as_slice() != public_key.as_slice() {
                return Err(SignError::KeyCertificateMismatch);
            }
            key.sign(&message).to_bytes().to_vec()
        }
        Algorithm::EcdsaP256Sha256 => {
            let key = p256::ecdsa::SigningKey::from_pkcs8_der(&key_der)
                .map_err(|e| SignError::BadKey(e.to_string()))?;
            let point = key.verifying_key().to_encoded_point(false);
            if point.as_bytes() != public_key.as_slice() {
                return Err(SignError::KeyCertificateMismatch);
            }
            let signature: p256::ecdsa::Signature = key.sign(&message);
            signature.to_bytes().to_vec()
        }
    };

    Ok(Document {
        payload,
        signature: SignatureBlock {
            algorithm,
            value,
            certificate_chain: certificate_chain_pem.to_vec(),
        },
    })
}
