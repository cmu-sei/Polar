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
//! never sees it) and TLS 1.3 handshakes. Signed with `ring`, which rustls already uses, several times faster than the
//! pure-Rust `p256` it replaced (docs/architecture/key-custody.md, "First measurement"). Held in the daemon's memory;
//! the process keyring is the next step ("Step 1 in detail").

use rcgen::PublicKeyData as _;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _};
use std::mem::MaybeUninit;
use zeroize::Zeroize as _;
#[cfg(test)]
use zeroize::Zeroizing;

pub struct SlotKey {
    /// ring keeps the scalar (and the nonce key made from it) in plain arrays and never wipes them, so the slot holds
    /// the pair where it can: on the heap, so it is not moved again, and in a `MaybeUninit`, so `Drop` may zero it.
    /// Initialised from `generate` until `wipe`.
    pair: Box<MaybeUninit<EcdsaKeyPair>>,
    random: SystemRandom,
    /// The public point, uncompressed: what rcgen writes into the CSR.
    public_point: Vec<u8>,
    /// The key's PKCS#8, kept for a test to look for its secret where it must never be.
    #[cfg(test)]
    pkcs8: Zeroizing<Vec<u8>>,
}

impl std::fmt::Debug for SlotKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlotKey")
            .field("key", &"<withheld>")
            .finish()
    }
}

impl Drop for SlotKey {
    fn drop(&mut self) {
        self.wipe();
    }
}

impl SlotKey {
    pub fn generate() -> Result<Self, String> {
        let random = SystemRandom::new();
        // The PKCS#8 is the secret too, and ring does not wipe it either.
        let mut document = MaybeUninit::new(
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &random)
                .map_err(|_| "cannot make a key".to_string())?,
        );
        // SAFETY: initialised on the line above; read only before it is wiped below.
        let pkcs8 = unsafe { document.assume_init_ref() }.as_ref();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8, &random)
            .map(|pair| Box::new(MaybeUninit::new(pair)));
        #[cfg(test)]
        let kept = Zeroizing::new(pkcs8.to_vec());
        document.zeroize();
        let pair = pair.map_err(|e| format!("cannot read the key made: {e}"))?;
        // SAFETY: initialised on the line above.
        let public_point = unsafe { pair.assume_init_ref() }
            .public_key()
            .as_ref()
            .to_vec();
        Ok(Self {
            pair,
            random,
            public_point,
            #[cfg(test)]
            pkcs8: kept,
        })
    }

    fn pair(&self) -> &EcdsaKeyPair {
        // SAFETY: initialised in `generate`; wiped only by `wipe`, which only `Drop` and a test call, and nothing signs
        // after either.
        unsafe { self.pair.assume_init_ref() }
    }

    /// Zero the pair: its scalar, its nonce key, its public key.
    fn wipe(&mut self) {
        self.pair.zeroize();
    }

    /// The pair's bytes, for a test to see them wiped; read only after `wipe`, when every byte is set.
    #[cfg(test)]
    fn pair_bytes(&self) -> &[u8] {
        // SAFETY: the `MaybeUninit`'s whole size, all of it written by `wipe`.
        unsafe {
            std::slice::from_raw_parts(
                self.pair.as_ptr().cast::<u8>(),
                std::mem::size_of::<EcdsaKeyPair>(),
            )
        }
    }

    /// The SubjectPublicKeyInfo, DER: what a certificate for this key carries.
    pub fn spki_der(&self) -> Vec<u8> {
        self.subject_public_key_info()
    }

    /// The secret scalar, for a test to look for it where it must never be.
    #[cfg(test)]
    pub(crate) fn secret_bytes(&self) -> Vec<u8> {
        use p256::pkcs8::DecodePrivateKey as _;
        p256::SecretKey::from_pkcs8_der(&self.pkcs8)
            .expect("the key's own PKCS#8")
            .to_bytes()
            .to_vec()
    }

    /// An ECDSA P-256 SHA-256 signature over `message`, DER-encoded, as TLS carries it; none only if the system's
    /// random source fails, which ring needs for the signature's nonce.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.pair()
            .sign(&self.random, message)
            .map(|signature| signature.as_ref().to_vec())
            .unwrap_or_default()
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
        let signature = SlotKey::sign(self, message);
        if signature.is_empty() {
            return Err(rcgen::Error::RingUnspecified);
        }
        Ok(signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier as _;
    use p256::ecdsa::{Signature, VerifyingKey};
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
        let secret = p256::SecretKey::from_slice(&key.secret_bytes()).unwrap();
        assert_eq!(
            secret.public_key(),
            p256::PublicKey::from_public_key_der(&key.spki_der()).unwrap(),
            "the secret the leak test looks for is this key's"
        );
    }

    #[test]
    fn a_key_dropped_is_wiped() {
        let mut key = SlotKey::generate().unwrap();
        assert!(!key.sign(b"message").is_empty());
        key.wipe();
        assert!(
            key.pair_bytes().iter().all(|b| *b == 0),
            "ring keeps the scalar in plain arrays; the slot wipes them"
        );
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
