//! Does a certificate belong to a private key?
//!
//! The bootstrap tool uses this before it tells an operator that the one
//! certificate that can ever be issued to the bootstrap identity is safely
//! on disk: a certificate that does not match its key is unusable, and
//! the claim cannot be repeated.

use rcgen::{KeyPair, PublicKeyData};
use x509_parser::pem::parse_x509_pem;

#[derive(Debug, thiserror::Error)]
pub enum KeyMatchError {
    #[error("certificate is not valid PEM: {0}")]
    BadCertPem(String),
    #[error("certificate does not parse: {0}")]
    BadCert(String),
    #[error("private key does not parse: {0}")]
    BadKey(String),
}

/// True when the certificate's public key is the public half of `key_pem`.
/// Only the first certificate in `cert_pem` is considered (the leaf).
pub fn certificate_matches_key(cert_pem: &str, key_pem: &str) -> Result<bool, KeyMatchError> {
    let (_, pem) = parse_x509_pem(cert_pem.as_bytes())
        .map_err(|e| KeyMatchError::BadCertPem(e.to_string()))?;
    let cert = pem
        .parse_x509()
        .map_err(|e| KeyMatchError::BadCert(e.to_string()))?;
    let key = KeyPair::from_pem(key_pem).map_err(|e| KeyMatchError::BadKey(e.to_string()))?;
    Ok(cert.public_key().subject_public_key.data.as_ref() == key.der_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keypair::{KeyAlgorithm, generate_csr};

    fn self_signed(key_pem: &str) -> String {
        let key = KeyPair::from_pem(key_pem).unwrap();
        rcgen::CertificateParams::new(vec!["x".to_string()])
            .unwrap()
            .self_signed(&key)
            .unwrap()
            .pem()
    }

    #[test]
    fn a_certificate_matches_its_own_key_and_no_other() {
        let a = generate_csr("a", &KeyAlgorithm::Ed25519, &[]).unwrap();
        let b = generate_csr("b", &KeyAlgorithm::Ed25519, &[]).unwrap();
        let cert_a = self_signed(&a.private_key_pem);
        assert!(certificate_matches_key(&cert_a, &a.private_key_pem).unwrap());
        assert!(!certificate_matches_key(&cert_a, &b.private_key_pem).unwrap());
    }

    #[test]
    fn garbage_is_an_error_not_a_mismatch() {
        assert!(certificate_matches_key("nope", "nope").is_err());
    }
}
