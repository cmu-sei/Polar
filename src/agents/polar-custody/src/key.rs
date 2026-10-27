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

//! A slot's key: ECDSA P-256, made in the daemon and never given out. It signs a CSR for itself (through rcgen, which
//! never sees it) and TLS 1.3 handshakes. Held in the daemon's memory; the process keyring is the next step
//! (docs/architecture/key-custody.md, "Step 1 in detail").

use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::{DecodePrivateKey as _, EncodePublicKey as _};
use zeroize::Zeroizing;

pub struct SlotKey {
    signing: SigningKey,
    /// The public point, uncompressed: what rcgen writes into the CSR.
    public_point: Vec<u8>,
}

impl std::fmt::Debug for SlotKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlotKey")
            .field("key", &"<withheld>")
            .finish()
    }
}

impl SlotKey {
    pub fn generate() -> Result<Self, String> {
        let made = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
            .map_err(|e| format!("cannot make a key: {e}"))?;
        let der = Zeroizing::new(made.serialize_der());
        drop(made);
        let signing = SigningKey::from_pkcs8_der(&der)
            .map_err(|e| format!("cannot read the key made: {e}"))?;
        let public_point = signing
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();
        Ok(Self {
            signing,
            public_point,
        })
    }

    /// The SubjectPublicKeyInfo, DER: what a certificate for this key carries.
    pub fn spki_der(&self) -> Vec<u8> {
        self.signing
            .verifying_key()
            .to_public_key_der()
            .map(|d| d.as_bytes().to_vec())
            .unwrap_or_default()
    }

    /// The secret scalar, for a test to look for it where it must never be.
    #[cfg(test)]
    pub(crate) fn secret_bytes(&self) -> Vec<u8> {
        self.signing.to_bytes().to_vec()
    }

    /// An ECDSA P-256 SHA-256 signature over `message`, DER-encoded, as TLS carries it.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        let signature: Signature = self.signing.sign(message);
        signature.to_der().as_bytes().to_vec()
    }
}

impl rcgen::PublicKeyData for SlotKey {
    fn der_bytes(&self) -> &[u8] {
        &self.public_point
    }

    fn algorithm(&self) -> &'static rcgen::SignatureAlgorithm {
        &rcgen::PKCS_ECDSA_P256_SHA256
    }
}

impl rcgen::SigningKey for SlotKey {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rcgen::Error> {
        Ok(SlotKey::sign(self, message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier as _;
    use p256::pkcs8::DecodePublicKey as _;
    use x509_parser::prelude::FromDer as _;

    #[test]
    fn a_signature_verifies_under_the_key_s_own_public_key_and_two_keys_differ() {
        let key = SlotKey::generate().unwrap();
        let verifying = VerifyingKey::from_public_key_der(&key.spki_der()).unwrap();
        let signature = Signature::from_der(&key.sign(b"message")).unwrap();
        assert!(verifying.verify(b"message", &signature).is_ok());
        assert!(verifying.verify(b"another", &signature).is_err());
        assert_ne!(key.spki_der(), SlotKey::generate().unwrap().spki_der());
        assert!(!format!("{key:?}").contains("signing"), "never printed");
    }

    #[test]
    fn its_csr_carries_its_own_key_and_the_identity_first() {
        let key = SlotKey::generate().unwrap();
        let csr = cert_issuer_common::keypair::csr_for_key(
            "cassini.polar.serviceaccount.cluster.local",
            &["cassini-ip-svc.polar.svc.cluster.local".to_string()],
            &key,
        )
        .unwrap();
        let (_, pem) = x509_parser::pem::parse_x509_pem(csr.as_bytes()).unwrap();
        let (_, request) =
            x509_parser::certification_request::X509CertificationRequest::from_der(&pem.contents)
                .unwrap();
        assert_eq!(
            request.certification_request_info.subject_pki.raw,
            key.spki_der().as_slice()
        );
        assert!(
            request.verify_signature().is_ok(),
            "signed by the key it names"
        );
        assert_eq!(
            cert_issuer_common::csr_san::csr_dns_sans(&csr).unwrap(),
            vec![
                "cassini.polar.serviceaccount.cluster.local".to_string(),
                "cassini-ip-svc.polar.svc.cluster.local".to_string()
            ]
        );
    }
}
