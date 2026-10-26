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

//! Making a signed document.

use crate::canonical::CanonicalError;
use crate::cert;
use crate::document::{Algorithm, Document, Domain, SignatureBlock, signing_input_in};
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

/// Signs `payload` as a configuration document with `key_pem`, which must be the key of the first certificate in
/// `certificate_chain_pem` (the signer's, then whatever links it to the CA). Both algorithms sign
/// deterministically, so the same inputs give the same signature.
pub fn sign(
    payload: Value,
    certificate_chain_pem: &[String],
    key_pem: &str,
) -> Result<Document, SignError> {
    sign_in(
        Domain::CONFIGURATION,
        payload,
        certificate_chain_pem,
        key_pem,
    )
}

/// Signs `payload` for `domain`, as [`sign`] does for configuration.
pub fn sign_in(
    domain: Domain,
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
    let message = signing_input_in(domain, &payload)?;

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
