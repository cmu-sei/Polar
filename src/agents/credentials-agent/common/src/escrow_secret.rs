/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

//! A type-safe wrapper around an encrypted secret -- the only way to
//! construct one from plaintext is through `encrypt()`; there is no
//! ergonomic, accidental path from a plain `String`/`Vec<u8>` sitting
//! in memory to something labeled `EscrowedSecret`, the same
//! "make the unsafe state unrepresentable" principle already used for
//! `DynamicNodeKey`/`BackendJobIdentity` in the graph library.
//!
//! Unlike those two types, `EscrowedSecret` genuinely has to cross a
//! process boundary -- the encrypting agent builds one locally, then
//! serializes it to publish over Cassini; the Credentials Agent
//! deserializes it on receipt. Deserializing an already-encrypted
//! value from wire bytes is a distinct, necessary operation,
//! reconstructing a value that was already, provably produced by
//! `encrypt()` in a different process -- not a second path to
//! fabricate one from plaintext, which is the property this type
//! actually protects.
//!
//! Encryption scheme: a standard hybrid cryptosystem, using only
//! FIPS-140-approved primitives -- RSA-OAEP (SHA-256) wraps a
//! randomly-generated AES-256 key; AES-256-GCM encrypts the actual
//! payload. RSA-OAEP alone is not used directly on the payload: it
//! has a hard plaintext-size ceiling (roughly 440 bytes at
//! 4096-bit/SHA-256-OAEP) that an arbitrary secret should not be
//! assumed to fit under.

use aes_gcm::aead::{Aead, Generate, Key as AesKey, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use rkyv::{Archive, Deserialize, Serialize};
use rsa::pkcs8::DecodePublicKey;
use rsa::{Oaep, RsaPrivateKey, RsaPublicKey};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use sha2_011::Sha256;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum EscrowError {
    #[error("failed to parse recipient public key: {0}")]
    InvalidPublicKey(String),
    #[error("RSA-OAEP key-wrapping failed: {0}")]
    KeyWrap(String),
    #[error("AES-GCM encryption failed")]
    Encryption,
    #[error("RSA-OAEP key-unwrapping failed: {0}")]
    KeyUnwrap(String),
    #[error("AES-GCM decryption failed -- wrong key, or the ciphertext was tampered with")]
    Decryption,
    #[error("decrypted AES key had the wrong length -- corrupted or tampered ciphertext")]
    MalformedKey,
}

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub struct EscrowedSecret {
    ciphertext: Vec<u8>,
    nonce: [u8; 12],
    /// The AES-256 key, RSA-OAEP-encrypted to `encrypted_to_pem`.
    encrypted_key: Vec<u8>,
    /// The PEM-encoded public key this secret is encrypted to --
    /// metadata, not itself sensitive. Lets a holder confirm they
    /// have the matching private key before attempting decryption,
    /// without guessing.
    encrypted_to_pem: String,
}

impl EscrowedSecret {
    /// The only public constructor -- the one place plaintext ever
    /// becomes an `EscrowedSecret`.
    pub fn encrypt(plaintext: &[u8], recipient_public_key_pem: &str) -> Result<Self, EscrowError> {
        let recipient_public_key = RsaPublicKey::from_public_key_pem(recipient_public_key_pem)
            .map_err(|e| EscrowError::InvalidPublicKey(e.to_string()))?;

        Self::encrypt_to_parsed_key(plaintext, &recipient_public_key, recipient_public_key_pem)
    }

    /// The same, real operation as `encrypt`, for callers that
    /// already hold a parsed `RsaPublicKey` -- e.g. a caller with its
    /// own, real cache keyed by recipient identity, avoiding a real,
    /// repeated PEM-parse cost on every single call (see
    /// polar-crypto's own commit history for the actual motivation).
    ///
    /// `recipient_public_key_pem` is still required, deliberately:
    /// `encrypted_to_pem`'s own field is real, meaningful wire
    /// metadata (letting a holder confirm they have the matching
    /// private key before attempting decryption at all), not
    /// something this function can safely omit or re-derive for
    /// free. Callers with an already-parsed key almost always still
    /// have the original PEM string sitting right alongside it too
    /// (from whatever query produced both) -- passing it through
    /// directly here is genuinely free, unlike re-serializing the
    /// parsed key back to PEM would be.
    pub fn encrypt_to_parsed_key(
        plaintext: &[u8],
        recipient_public_key: &RsaPublicKey,
        recipient_public_key_pem: &str,
    ) -> Result<Self, EscrowError> {
        let aes_key = AesKey::<Aes256Gcm>::generate();
        let nonce = Nonce::generate();
        let cipher = Aes256Gcm::new(&aes_key);

        let ciphertext = cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| EscrowError::Encryption)?;

        let mut rng = rand::rng();
        let encrypted_key = recipient_public_key
            .encrypt(&mut rng, Oaep::<Sha256>::new(), aes_key.as_slice())
            .map_err(|e| EscrowError::KeyWrap(e.to_string()))?;

        Ok(Self {
            ciphertext,
            nonce: nonce.into(),
            encrypted_key,
            encrypted_to_pem: recipient_public_key_pem.to_string(),
        })
    }

    /// Decrypts using `private_key`, returning the plaintext wrapped
    /// in `Zeroizing` -- the caller (the decrypt-reencrypt-destroy
    /// relay handler) is responsible for letting this go out of scope
    /// immediately once re-encryption to the actual recipient
    /// completes. The wrapper guarantees the drop zeroes the memory
    /// rather than merely deallocating it, matching the hard
    /// requirement in "Secret escrow" (credentials-agent-architecture.md)
    /// that this can't be left to Rust's ordinary `Drop`.
    pub fn decrypt(&self, private_key: &RsaPrivateKey) -> Result<Zeroizing<Vec<u8>>, EscrowError> {
        let aes_key_bytes = private_key
            .decrypt(Oaep::<Sha256>::new(), &self.encrypted_key)
            .map_err(|e| EscrowError::KeyUnwrap(e.to_string()))?;
        let aes_key_bytes = Zeroizing::new(aes_key_bytes);

        let aes_key = AesKey::<Aes256Gcm>::try_from(aes_key_bytes.as_slice())
            .map_err(|_| EscrowError::MalformedKey)?;
        let cipher = Aes256Gcm::new(&aes_key);
        let nonce =
            Nonce::try_from(self.nonce.as_slice()).map_err(|_| EscrowError::MalformedKey)?;

        let plaintext = cipher
            .decrypt(&nonce, self.ciphertext.as_slice())
            .map_err(|_| EscrowError::Decryption)?;

        Ok(Zeroizing::new(plaintext))
    }

    /// The PEM-encoded public key this secret is encrypted to.
    pub fn encrypted_to_pem(&self) -> &str {
        &self.encrypted_to_pem
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs8::{DecodePublicKey, EncodePublicKey};

    fn test_recipient_keypair() -> (RsaPrivateKey, String) {
        use std::sync::OnceLock;
        static KEYPAIR: OnceLock<(RsaPrivateKey, String)> = OnceLock::new();
        KEYPAIR
            .get_or_init(|| {
                let mut rng = rand::rng();
                let private_key = RsaPrivateKey::new(&mut rng, 4096).unwrap();
                let public_key_pem = private_key
                    .to_public_key()
                    .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
                    .unwrap();
                (private_key, public_key_pem)
            })
            .clone()
    }

    #[test]
    fn round_trips_the_actual_plaintext() {
        let (private_key, public_key_pem) = test_recipient_keypair();
        let plaintext = b"a real secret, like a git access token";

        let secret = EscrowedSecret::encrypt(plaintext, &public_key_pem).unwrap();
        let decrypted = secret.decrypt(&private_key).unwrap();

        assert_eq!(decrypted.as_slice(), plaintext);
    }

    #[test]
    fn ciphertext_never_contains_the_plaintext_verbatim() {
        let (_private_key, public_key_pem) = test_recipient_keypair();
        let plaintext = b"a-genuinely-distinctive-marker-string";

        let secret = EscrowedSecret::encrypt(plaintext, &public_key_pem).unwrap();

        // A weak but real sanity check: the ciphertext must not
        // contain the plaintext's own bytes as a literal substring
        // anywhere -- if it did, this wouldn't be encryption at all.
        assert!(
            !secret
                .ciphertext
                .windows(plaintext.len())
                .any(|window| window == plaintext.as_slice())
        );
    }

    #[test]
    fn decryption_fails_with_the_wrong_private_key() {
        let (_private_key, public_key_pem) = test_recipient_keypair();
        let plaintext = b"a real secret";
        let secret = EscrowedSecret::encrypt(plaintext, &public_key_pem).unwrap();

        let mut rng = rand::rng();
        let wrong_private_key = RsaPrivateKey::new(&mut rng, 4096).unwrap();

        let result = secret.decrypt(&wrong_private_key);
        assert!(matches!(result, Err(EscrowError::KeyUnwrap(_))));
    }

    #[test]
    fn tampered_ciphertext_fails_to_decrypt() {
        let (private_key, public_key_pem) = test_recipient_keypair();
        let plaintext = b"a real secret";
        let mut secret = EscrowedSecret::encrypt(plaintext, &public_key_pem).unwrap();

        // Flip a bit in the ciphertext -- AES-GCM's own authentication
        // tag must catch this, not silently return garbage plaintext.
        secret.ciphertext[0] ^= 0xFF;

        let result = secret.decrypt(&private_key);
        assert!(matches!(result, Err(EscrowError::Decryption)));
    }

    #[test]
    fn encrypted_to_pem_matches_what_was_passed_in() {
        let (_private_key, public_key_pem) = test_recipient_keypair();
        let secret = EscrowedSecret::encrypt(b"data", &public_key_pem).unwrap();

        assert_eq!(secret.encrypted_to_pem(), public_key_pem);
    }

    #[test]
    fn two_encryptions_of_the_same_plaintext_produce_different_ciphertext() {
        // The random nonce (and random AES key) must make this true --
        // otherwise an observer could tell two escrowed secrets are
        // identical just by comparing ciphertext bytes.
        let (_private_key, public_key_pem) = test_recipient_keypair();
        let plaintext = b"same secret, encrypted twice";

        let first = EscrowedSecret::encrypt(plaintext, &public_key_pem).unwrap();
        let second = EscrowedSecret::encrypt(plaintext, &public_key_pem).unwrap();

        assert_ne!(first.ciphertext, second.ciphertext);
    }

    #[test]
    fn encrypt_to_parsed_key_genuinely_decrypts_correctly_with_the_matching_private_key() {
        // A real, direct proof this alternate constructor -- built
        // for callers holding an already-parsed RsaPublicKey, e.g.
        // polar-crypto's own, real per-identity cache -- is genuinely
        // equivalent to the ordinary encrypt(), not just "compiles
        // and doesn't panic."
        let (private_key, public_key_pem) = test_recipient_keypair();
        let public_key = RsaPublicKey::from_public_key_pem(&public_key_pem).unwrap();
        let plaintext = b"a real secret, encrypted via the already-parsed-key path";

        let secret =
            EscrowedSecret::encrypt_to_parsed_key(plaintext, &public_key, &public_key_pem).unwrap();

        let decrypted = secret.decrypt(&private_key).unwrap();
        assert_eq!(&*decrypted, plaintext);
    }

    #[test]
    fn encrypt_to_parsed_key_genuinely_uses_the_passed_in_pem_string_directly() {
        // encrypted_to_pem is real, meaningful wire metadata -- this
        // confirms encrypt_to_parsed_key genuinely stores the exact
        // string it was handed, not something silently re-derived
        // from the parsed key (which, being a different, equally
        // valid PEM encoding of the same key, could still genuinely
        // differ byte-for-byte from the original).
        let (_private_key, public_key_pem) = test_recipient_keypair();
        let public_key = RsaPublicKey::from_public_key_pem(&public_key_pem).unwrap();

        let secret =
            EscrowedSecret::encrypt_to_parsed_key(b"data", &public_key, &public_key_pem).unwrap();

        assert_eq!(secret.encrypted_to_pem(), public_key_pem);
    }
}
