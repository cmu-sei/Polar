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

//! Sign and verify admin actions with a certificate's own key. Only
//! Ed25519 and ECDSA P-256 are accepted: those are the signing-capable
//! key types user and admin certs carry. RSA is escrow-only. Both are
//! RustCrypto, matching the rest of this crate.

use crate::admin_action::{AdminActionBody, AdminActionError, SignedAdminAction};
use p256::ecdsa::signature::{Signer as _, Verifier as _};
use p256::pkcs8::DecodePrivateKey as _;

const OID_ED25519: &str = "1.3.101.112";
const OID_EC_PUBLIC_KEY: &str = "1.2.840.10045.2.1";
const OID_P256: &str = "1.2.840.10045.3.1.7";

#[derive(Debug, thiserror::Error)]
pub enum SigningError {
    #[error("certificate could not be parsed: {0}")]
    BadCertificate(String),
    #[error("private key could not be parsed: {0}")]
    BadKey(String),
    #[error("unsupported key type: admin certificates must use Ed25519 or ECDSA P-256")]
    UnsupportedKeyType,
    #[error("private key does not belong to the certificate")]
    KeyCertificateMismatch,
    #[error("signature does not verify")]
    BadSignature,
    #[error(transparent)]
    Action(#[from] AdminActionError),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Alg {
    Ed25519,
    EcdsaP256,
}

struct CertInfo {
    serial: String,
    alg: Alg,
    public_key: Vec<u8>,
}

fn cert_info(cert_pem: &str) -> Result<CertInfo, SigningError> {
    let (_, block) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes())
        .map_err(|e| SigningError::BadCertificate(e.to_string()))?;
    let (_, cert) = x509_parser::parse_x509_certificate(&block.contents)
        .map_err(|e| SigningError::BadCertificate(e.to_string()))?;
    let spki = cert.public_key();
    let oid = spki.algorithm.algorithm.to_id_string();
    let alg = if oid == OID_ED25519 {
        Alg::Ed25519
    } else if oid == OID_EC_PUBLIC_KEY {
        let curve = spki
            .algorithm
            .parameters
            .as_ref()
            .and_then(|p| p.as_oid().ok())
            .map(|o| o.to_id_string());
        if curve.as_deref() == Some(OID_P256) {
            Alg::EcdsaP256
        } else {
            return Err(SigningError::UnsupportedKeyType);
        }
    } else {
        return Err(SigningError::UnsupportedKeyType);
    };
    Ok(CertInfo {
        serial: cert_issuer_common::serial::serial_from_cert(&cert),
        alg,
        public_key: spki.subject_public_key.data.to_vec(),
    })
}

/// Signs the body's payload bytes with the certificate's own private key.
/// Both algorithms sign deterministically, so no RNG is involved.
pub fn sign(
    body: &AdminActionBody,
    cert_pem: &str,
    key_pem: &str,
) -> Result<SignedAdminAction, SigningError> {
    let info = cert_info(cert_pem)?;
    let (_, key_block) = x509_parser::pem::parse_x509_pem(key_pem.as_bytes())
        .map_err(|e| SigningError::BadKey(e.to_string()))?;
    let der = key_block.contents;
    let payload = body.to_payload()?;

    let signature = match info.alg {
        Alg::Ed25519 => {
            let key = ed25519_dalek::SigningKey::from_pkcs8_der(&der)
                .map_err(|e| SigningError::BadKey(e.to_string()))?;
            if key.verifying_key().as_bytes().as_slice() != info.public_key.as_slice() {
                return Err(SigningError::KeyCertificateMismatch);
            }
            key.sign(&payload).to_bytes().to_vec()
        }
        Alg::EcdsaP256 => {
            let key = p256::ecdsa::SigningKey::from_pkcs8_der(&der)
                .map_err(|e| SigningError::BadKey(e.to_string()))?;
            let point = key.verifying_key().to_encoded_point(false);
            if point.as_bytes() != info.public_key.as_slice() {
                return Err(SigningError::KeyCertificateMismatch);
            }
            let signature: p256::ecdsa::Signature = key.sign(&payload);
            signature.to_bytes().to_vec()
        }
    };

    Ok(SignedAdminAction {
        payload,
        author_serial: info.serial,
        signature,
    })
}

/// Verifies the signature against the given certificate's public key.
/// The caller supplies the certificate it looked up for
/// `signed.author_serial` in the ledger, and this also checks that the
/// certificate's serial is the one the action claims.
pub fn verify(signed: &SignedAdminAction, author_cert_pem: &str) -> Result<(), SigningError> {
    let info = cert_info(author_cert_pem)?;
    if info.serial != signed.author_serial {
        return Err(SigningError::BadSignature);
    }
    match info.alg {
        Alg::Ed25519 => {
            let bytes: [u8; 32] =
                info.public_key.as_slice().try_into().map_err(|_| {
                    SigningError::BadCertificate("Ed25519 key is not 32 bytes".into())
                })?;
            let key = ed25519_dalek::VerifyingKey::from_bytes(&bytes)
                .map_err(|e| SigningError::BadCertificate(e.to_string()))?;
            let signature = ed25519_dalek::Signature::from_slice(&signed.signature)
                .map_err(|_| SigningError::BadSignature)?;
            key.verify_strict(&signed.payload, &signature)
                .map_err(|_| SigningError::BadSignature)
        }
        Alg::EcdsaP256 => {
            let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(&info.public_key)
                .map_err(|e| SigningError::BadCertificate(e.to_string()))?;
            let signature = p256::ecdsa::Signature::from_slice(&signed.signature)
                .map_err(|_| SigningError::BadSignature)?;
            key.verify(&signed.payload, &signature)
                .map_err(|_| SigningError::BadSignature)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin_action::AdminAction;
    use crate::events::RevocationReason;
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256, PKCS_ED25519};

    fn identity(alg: &'static rcgen::SignatureAlgorithm) -> (String, String) {
        let kp = KeyPair::generate_for(alg).unwrap();
        let cert = CertificateParams::new(vec!["admin.example".to_string()])
            .unwrap()
            .self_signed(&kp)
            .unwrap();
        (cert.pem(), kp.serialize_pem())
    }

    fn body() -> AdminActionBody {
        AdminActionBody {
            nonce: "n1".to_string(),
            action: AdminAction::Revoke {
                serial: "abc".to_string(),
                reason: RevocationReason::Compromised,
            },
        }
    }

    #[test]
    fn ed25519_signature_verifies() {
        let (cert, key) = identity(&PKCS_ED25519);
        let signed = sign(&body(), &cert, &key).unwrap();
        verify(&signed, &cert).unwrap();
        assert_eq!(signed.decode_body().unwrap(), body());
    }

    #[test]
    fn ecdsa_p256_signature_verifies() {
        let (cert, key) = identity(&PKCS_ECDSA_P256_SHA256);
        let signed = sign(&body(), &cert, &key).unwrap();
        verify(&signed, &cert).unwrap();
    }

    #[test]
    fn tampered_payload_does_not_verify() {
        let (cert, key) = identity(&PKCS_ED25519);
        let mut signed = sign(&body(), &cert, &key).unwrap();
        signed.payload[0] ^= 0xff;
        assert!(matches!(
            verify(&signed, &cert),
            Err(SigningError::BadSignature)
        ));
    }

    #[test]
    fn another_authors_certificate_does_not_verify() {
        let (cert, key) = identity(&PKCS_ED25519);
        let (other_cert, _) = identity(&PKCS_ED25519);
        let signed = sign(&body(), &cert, &key).unwrap();
        assert!(verify(&signed, &other_cert).is_err());
    }

    #[test]
    fn key_that_does_not_match_the_certificate_is_refused() {
        let (cert, _) = identity(&PKCS_ED25519);
        let (_, other_key) = identity(&PKCS_ED25519);
        assert!(matches!(
            sign(&body(), &cert, &other_key),
            Err(SigningError::KeyCertificateMismatch)
        ));
    }
}
