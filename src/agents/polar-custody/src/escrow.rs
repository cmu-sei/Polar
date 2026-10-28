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

//! The pod's escrow key: P-256, decryption only (docs/architecture/key-custody.md, "The crypto change"). Made here when
//! nothing is escrowed for the workload yet, or retrieved into here from escrow by the custodian; never given out,
//! except once, sealed to the credentials agent's key, for escrow. What a caller may ask of it is to unwrap a secret's
//! AES key (`polar_escrow::unwrap_p256`), so the daemon never sees a payload.

use p256::pkcs8::{DecodePrivateKey as _, EncodePrivateKey as _, EncodePublicKey as _, LineEnding};
use zeroize::Zeroizing;

/// A P-256 private key, zeroed when it goes (`p256::SecretKey` is `ZeroizeOnDrop`).
pub struct EscrowKey {
    secret: p256::SecretKey,
}

// The key itself is never shown.
impl std::fmt::Debug for EscrowKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EscrowKey")
            .field("key", &"<withheld>")
            .finish()
    }
}

impl EscrowKey {
    /// A new key, from the operating system's randomness.
    pub fn generate() -> Result<Self, String> {
        loop {
            let mut bytes = Zeroizing::new([0u8; 32]);
            ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), bytes.as_mut())
                .map_err(|_| "no randomness".to_string())?;
            if let Ok(secret) = p256::SecretKey::from_bytes(&(*bytes).into()) {
                return Ok(Self { secret });
            }
        }
    }

    /// The key a retrieval opened to: PKCS#8 PEM, P-256 only. An RSA escrow key, from before the change, is refused
    /// by its type (no migration: docs/architecture/key-custody.md, "Decided").
    pub fn from_pkcs8_pem(pem: &str) -> Result<Self, String> {
        match p256::SecretKey::from_pkcs8_pem(pem) {
            Ok(secret) => Ok(Self { secret }),
            Err(_) if pem.contains("PRIVATE KEY") && rsa_like(pem) => Err(
                "the escrowed key is an RSA key, from before the escrow key became P-256: it is not taken (no migration)"
                    .to_string(),
            ),
            Err(_) => Err("the escrowed key is not a P-256 private key".to_string()),
        }
    }

    /// Its PKCS#8 PEM, wiped when dropped: for its CSR and for sealing it to escrow, nothing else.
    pub(crate) fn pkcs8_pem(&self) -> Result<Zeroizing<String>, String> {
        self.secret
            .to_pkcs8_pem(LineEnding::LF)
            .map(|pem| Zeroizing::new(pem.to_string()))
            .map_err(|e| format!("the key does not encode: {e}"))
    }

    /// Its public key, SPKI PEM: what its certificate holds, and what secrets are sealed to.
    pub fn public_key_pem(&self) -> Result<String, String> {
        self.secret
            .public_key()
            .to_public_key_pem(LineEnding::LF)
            .map_err(|e| format!("the public key does not encode: {e}"))
    }

    /// A CSR for the key, naming `identity`: the workload's escrow certificate (`CertType::Service`).
    pub fn csr(&self, identity: &str) -> Result<String, String> {
        let pem = self.pkcs8_pem()?;
        cert_issuer_common::keypair::csr_for_private_key_pem(identity, &[], &pem)
            .map_err(|e| format!("cannot make the escrow key's CSR: {e}"))
    }

    /// The key sealed to `recipient` (the credentials agent's, as the daemon pinned it): the one way it leaves.
    pub fn sealed_to(
        &self,
        recipient: &rsa::RsaPublicKey,
    ) -> Result<polar_escrow::EscrowedSecret, String> {
        use rsa::pkcs8::EncodePublicKey as _;
        let recipient_pem = recipient
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .map_err(|e| format!("the credentials agent's key does not encode: {e}"))?;
        let pem = self.pkcs8_pem()?;
        polar_escrow::EscrowedSecret::encrypt_to_parsed_key(
            pem.as_bytes(),
            recipient,
            &recipient_pem,
        )
        .map_err(|e| format!("cannot seal the escrow key: {e}"))
    }

    /// The AES key `wrapped` opens to under this key.
    pub fn unwrap(&self, wrapped: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
        polar_escrow::unwrap_p256(&self.secret, wrapped).map_err(|e| e.to_string())
    }

    /// The scalar, for a test to look for where it must never be.
    #[cfg(test)]
    pub(crate) fn secret_bytes(&self) -> Vec<u8> {
        self.secret.to_bytes().to_vec()
    }
}

/// Whether `pem` holds an RSA key: an RSA PKCS#8 key's algorithm identifier is rsaEncryption.
fn rsa_like(pem: &str) -> bool {
    use rsa::pkcs8::DecodePrivateKey as _;
    rsa::RsaPrivateKey::from_pkcs8_pem(pem).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_sealed_to_the_key_unwraps_through_it_and_two_keys_differ() {
        let key = EscrowKey::generate().unwrap();
        let other = EscrowKey::generate().unwrap();
        assert_ne!(key.secret_bytes(), other.secret_bytes());
        let secret =
            polar_escrow::EscrowedSecret::encrypt(b"a token", &key.public_key_pem().unwrap())
                .unwrap();
        let opened = secret
            .decrypt_with(polar_escrow::Suite::HpkeP256, |wrapped| {
                key.unwrap(wrapped).map(|k| k.to_vec())
            })
            .unwrap();
        assert_eq!(opened.as_slice(), b"a token");
        assert!(other.unwrap(secret.wrapped_key()).is_err());
    }

    #[test]
    fn the_key_retrieved_is_the_key_escrowed_and_an_rsa_key_is_refused_by_its_type() {
        let key = EscrowKey::generate().unwrap();
        let again = EscrowKey::from_pkcs8_pem(&key.pkcs8_pem().unwrap()).unwrap();
        assert_eq!(again.secret_bytes(), key.secret_bytes());
        let rsa = rsa::RsaPrivateKey::new(&mut rand::rng(), 1024).unwrap();
        let rsa_pem =
            rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(&rsa, rsa::pkcs8::LineEnding::LF).unwrap();
        let refused = EscrowKey::from_pkcs8_pem(&rsa_pem).unwrap_err();
        assert!(
            refused.contains("RSA") && refused.contains("no migration"),
            "{refused}"
        );
        assert!(EscrowKey::from_pkcs8_pem("not a key").is_err());
    }

    #[test]
    fn sealed_for_escrow_it_opens_to_the_key_under_the_recipient_s_key_only() {
        let key = EscrowKey::generate().unwrap();
        let recipient = rsa::RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap();
        let sealed = key.sealed_to(&recipient.to_public_key()).unwrap();
        let opened = sealed.decrypt(&recipient).unwrap();
        let again = EscrowKey::from_pkcs8_pem(std::str::from_utf8(&opened).unwrap()).unwrap();
        assert_eq!(again.secret_bytes(), key.secret_bytes());
        let stranger = rsa::RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap();
        assert!(sealed.decrypt(&stranger).is_err());
    }

    #[test]
    fn its_csr_carries_its_own_key_and_names_the_workload() {
        let key = EscrowKey::generate().unwrap();
        let csr = key
            .csr("kube-consumer.polar.serviceaccount.cluster.local")
            .unwrap();
        let parsed = rcgen::CertificateSigningRequestParams::from_pem(&csr).unwrap();
        assert_eq!(
            cert_issuer_common::csr_san::csr_dns_sans(&csr).unwrap(),
            vec!["kube-consumer.polar.serviceaccount.cluster.local".to_string()]
        );
        let spki =
            p256::pkcs8::DecodePublicKey::from_public_key_pem(&key.public_key_pem().unwrap())
                .map(|k: p256::PublicKey| k.to_sec1_bytes().to_vec())
                .unwrap();
        assert!(rcgen::PublicKeyData::der_bytes(&parsed.public_key).ends_with(&spki));
    }

    #[test]
    fn the_key_is_never_shown() {
        let key = EscrowKey::generate().unwrap();
        let hex: String = key
            .secret_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(!format!("{key:?}").contains(&hex));
    }
}
