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

//! Extracts a certificate's own SubjectPublicKeyInfo and re-encodes
//! it as a standalone public-key PEM -- a general X.509 utility, not
//! credentials-agent-specific, hence its home here rather than in
//! credentials-agent-common (the same placement reasoning already
//! used for Permission/CertType). Genuinely new functionality --
//! confirmed nothing already in this workspace did this before
//! building it; cert-issuer/server's own CSR-parsing code captures a
//! CSR's SubjectPublicKeyInfo DER for a different purpose (passing it
//! straight to the CA for signing) and never re-encodes it as PEM at
//! all.

use rsa::RsaPublicKey;
use rsa::pkcs8::{DecodePublicKey, EncodePublicKey, LineEnding};

#[derive(Debug, thiserror::Error)]
pub enum PublicKeyExtractionError {
    #[error("failed to parse certificate PEM: {0}")]
    InvalidPem(String),
    #[error("failed to parse certificate DER: {0}")]
    InvalidDer(String),
    #[error("failed to parse the certificate's SubjectPublicKeyInfo as an RSA public key: {0}")]
    InvalidPublicKey(String),
    #[error("failed to re-encode the extracted public key as PEM: {0}")]
    Reencoding(String),
}

/// Parses `certificate_pem` (a PEM-encoded X.509 certificate) and
/// returns just its own SubjectPublicKeyInfo, re-encoded as a
/// standalone public-key PEM -- the same PEM shape
/// `EscrowedSecret::encrypt`, `escrow_key`, and every other
/// public-key-PEM consumer in this workspace already expects.
pub fn extract_public_key_pem(certificate_pem: &str) -> Result<String, PublicKeyExtractionError> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(certificate_pem.as_bytes())
        .map_err(|e| PublicKeyExtractionError::InvalidPem(e.to_string()))?;
    let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents)
        .map_err(|e| PublicKeyExtractionError::InvalidDer(e.to_string()))?;

    let spki_der = cert.tbs_certificate.subject_pki.raw;

    let public_key = RsaPublicKey::from_public_key_der(spki_der)
        .map_err(|e| PublicKeyExtractionError::InvalidPublicKey(e.to_string()))?;

    public_key
        .to_public_key_pem(LineEnding::LF)
        .map_err(|e| PublicKeyExtractionError::Reencoding(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs8::EncodePrivateKey;

    #[test]
    fn extracted_public_key_matches_the_original_key_used_to_sign_the_certificate() {
        let mut rng = rand::rng();
        let private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let original_public_key_pem = private_key
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap();

        let private_key_pem = private_key.to_pkcs8_pem(LineEnding::LF).unwrap();
        let key_pair = rcgen::KeyPair::from_pem(&private_key_pem).unwrap();
        let params =
            rcgen::CertificateParams::new(vec!["test.polar.svc.cluster.local".to_string()])
                .unwrap();
        let cert = params.self_signed(&key_pair).unwrap();
        let certificate_pem = cert.pem();

        let extracted_public_key_pem = extract_public_key_pem(&certificate_pem).unwrap();

        assert_eq!(extracted_public_key_pem, original_public_key_pem);
    }

    #[test]
    fn malformed_certificate_pem_is_a_real_error_not_a_panic() {
        let result = extract_public_key_pem("not a real certificate");
        assert!(matches!(
            result,
            Err(PublicKeyExtractionError::InvalidPem(_))
        ));
    }
}
