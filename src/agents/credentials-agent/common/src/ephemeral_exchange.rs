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

//! The ephemeral X25519 key exchange -- credentials-agent-architecture.md's
//! own "Agent escrow bootstrap" section names this for exactly one
//! case: an agent retrieving its own escrow keypair for the very
//! first time, when no stable key yet exists to receive the response
//! encrypted to normally (the general retrieve mechanism,
//! retrieve_secret.rs, re-encrypts to the requester's own,
//! already-stable mTLS certificate instead -- this exists
//! specifically because that path isn't available for this one,
//! narrow case).
//!
//! Genuinely distinct from EscrowedSecret (RSA-OAEP + AES-256-GCM):
//! this is a fresh, single-use X25519 ECDH exchange plus HKDF-SHA256
//! key derivation, never a long-lived keypair -- both ephemeral
//! keypairs involved are discarded immediately after this one
//! exchange completes, which is the entire point.
//!
//! Same "make the unsafe state unrepresentable" discipline as
//! EscrowedSecret: the only way to construct an
//! EphemeralEncryptedPayload from plaintext is through `encrypt`.

use aes_gcm::aead::{Aead, Generate, Key as AesKey, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey};
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum EphemeralExchangeError {
    #[error("AES-GCM encryption failed")]
    Encryption,
    #[error("AES-GCM decryption failed -- wrong key, or the ciphertext was tampered with")]
    Decryption,
}

/// A single, one-time response encrypted via an ephemeral X25519 key
/// exchange. The only way to construct one from plaintext is through
/// `encrypt`.
#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub struct EphemeralEncryptedPayload {
    /// The responder's own ephemeral public key for this one
    /// exchange -- the requester derives the same shared secret
    /// independently from this plus its own ephemeral private key
    /// (the one it generated before sending the request in the
    /// first place).
    responder_ephemeral_public_key: [u8; 32],
    ciphertext: Vec<u8>,
    nonce: [u8; 12],
}

impl EphemeralEncryptedPayload {
    /// Called by the responder (e.g. the Credentials Agent), given
    /// the requester's own ephemeral public key (received in their
    /// request) and the plaintext to encrypt.
    pub fn encrypt(
        plaintext: &[u8],
        requester_ephemeral_public_key: &[u8; 32],
    ) -> Result<Self, EphemeralExchangeError> {
        // EphemeralSecret::random() (the "getrandom" feature), not
        // random_from_rng with a hand-constructed RNG -- this is the
        // library's own, dedicated, audited path straight to OS
        // randomness. random_from_rng exists for cases that
        // genuinely need RNG control (deterministic testing with a
        // seeded RNG); for real key generation, random() is the
        // correct, safer default -- inventing an RNG-construction
        // path of our own here, just to satisfy a version mismatch,
        // isn't a trade worth making in cryptographic code.
        let responder_ephemeral_secret = EphemeralSecret::random();
        let responder_ephemeral_public_key = PublicKey::from(&responder_ephemeral_secret);

        let shared_secret = responder_ephemeral_secret
            .diffie_hellman(&PublicKey::from(*requester_ephemeral_public_key));

        let aes_key = derive_aes_key(shared_secret.as_bytes());

        let cipher = Aes256Gcm::new(&aes_key);
        let nonce = Nonce::generate();
        let ciphertext = cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| EphemeralExchangeError::Encryption)?;

        Ok(Self {
            responder_ephemeral_public_key: responder_ephemeral_public_key.to_bytes(),
            ciphertext,
            nonce: nonce.into(),
        })
    }

    /// Called by the requester, given its own ephemeral private key
    /// (the one it generated before sending the request -- consumed
    /// here, matching `diffie_hellman`'s own single-use semantics:
    /// once used, it's gone).
    pub fn decrypt(
        &self,
        my_ephemeral_secret: EphemeralSecret,
    ) -> Result<Zeroizing<Vec<u8>>, EphemeralExchangeError> {
        let shared_secret = my_ephemeral_secret
            .diffie_hellman(&PublicKey::from(self.responder_ephemeral_public_key));

        let aes_key = derive_aes_key(shared_secret.as_bytes());

        let cipher = Aes256Gcm::new(&aes_key);
        let nonce = Nonce::try_from(self.nonce.as_slice())
            .map_err(|_| EphemeralExchangeError::Decryption)?;

        let plaintext = cipher
            .decrypt(&nonce, self.ciphertext.as_slice())
            .map_err(|_| EphemeralExchangeError::Decryption)?;

        Ok(Zeroizing::new(plaintext))
    }
}

/// HKDF-SHA256, no salt (the X25519 shared secret is already a
/// strong, uniformly-random 32-byte value -- HKDF's own extract step
/// with no salt still produces a fine pseudorandom key from it) --
/// "info" names this specific use, so a key derived here could never
/// collide with a key derived from the same shared secret for a
/// genuinely different purpose.
/// A session the responder opens so a requester can send it something secret: a fresh X25519 keypair whose
/// public half goes to the requester and whose secret half is kept until the one message arrives. The requester
/// seals to the public half with [`EphemeralEncryptedPayload::encrypt`], which is the same call whichever way the
/// secret travels; [`unseal`](Self::unseal) opens it and uses the session up. Nothing here outlives that one
/// exchange: the secret is not clonable and is consumed by opening.
pub struct EphemeralSession {
    secret: EphemeralSecret,
    public_key: [u8; 32],
}

impl EphemeralSession {
    pub fn open() -> Self {
        let secret = EphemeralSecret::random();
        let public_key = PublicKey::from(&secret).to_bytes();
        Self { secret, public_key }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.public_key
    }

    /// Opens what a requester sealed to this session. Consumes the session either way: a payload that does not
    /// open is not tried again.
    pub fn unseal(
        self,
        sealed: &EphemeralEncryptedPayload,
    ) -> Result<Zeroizing<Vec<u8>>, EphemeralExchangeError> {
        sealed.decrypt(self.secret)
    }
}

fn derive_aes_key(shared_secret: &[u8; 32]) -> AesKey<Aes256Gcm> {
    let hkdf = Hkdf::<Sha256>::new(None, shared_secret);
    let mut okm = [0u8; 32];
    hkdf.expand(b"polar-agent-escrow-bootstrap-v1", &mut okm)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    AesKey::<Aes256Gcm>::from(okm)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- a session the responder opens, sealed to by the requester -----------------------------------------

    #[test]
    fn what_a_requester_seals_to_a_session_only_that_session_opens() {
        let session = EphemeralSession::open();
        let sealed =
            EphemeralEncryptedPayload::encrypt(b"a service-account token", &session.public_key())
                .unwrap();
        let opened = session.unseal(&sealed).unwrap();
        assert_eq!(opened.as_slice(), b"a service-account token");

        // Another session, with another key, cannot.
        let other = EphemeralSession::open();
        assert!(other.unseal(&sealed).is_err());
    }

    #[test]
    fn two_sessions_never_share_a_public_key() {
        assert_ne!(
            EphemeralSession::open().public_key(),
            EphemeralSession::open().public_key()
        );
    }

    #[test]
    fn a_sealed_payload_that_was_changed_does_not_open() {
        let session = EphemeralSession::open();
        let mut sealed =
            EphemeralEncryptedPayload::encrypt(b"token", &session.public_key()).unwrap();
        sealed.ciphertext[0] ^= 1;
        assert!(session.unseal(&sealed).is_err());
    }

    #[test]
    fn round_trips_the_actual_plaintext() {
        let requester_secret = EphemeralSecret::random();
        let requester_public_key = PublicKey::from(&requester_secret).to_bytes();

        let plaintext = b"a real escrowed keypair, for this one bootstrap exchange";
        let payload = EphemeralEncryptedPayload::encrypt(plaintext, &requester_public_key).unwrap();

        let decrypted = payload.decrypt(requester_secret).unwrap();
        assert_eq!(decrypted.as_slice(), plaintext);
    }

    #[test]
    fn ciphertext_never_contains_the_plaintext_verbatim() {
        let requester_secret = EphemeralSecret::random();
        let requester_public_key = PublicKey::from(&requester_secret).to_bytes();

        let plaintext = b"a-genuinely-distinctive-marker-string";
        let payload = EphemeralEncryptedPayload::encrypt(plaintext, &requester_public_key).unwrap();

        assert!(
            !payload
                .ciphertext
                .windows(plaintext.len())
                .any(|window| window == plaintext.as_slice())
        );
    }

    #[test]
    fn decryption_fails_with_the_wrong_ephemeral_secret() {
        let requester_secret = EphemeralSecret::random();
        let requester_public_key = PublicKey::from(&requester_secret).to_bytes();

        let plaintext = b"a real secret";
        let payload = EphemeralEncryptedPayload::encrypt(plaintext, &requester_public_key).unwrap();

        // A different, real requester's own ephemeral secret -- the
        // genuine, real proof this exchange is actually bound to the
        // specific pair of ephemeral keys used, not just any key.
        let wrong_secret = EphemeralSecret::random();

        let result = payload.decrypt(wrong_secret);
        assert!(matches!(result, Err(EphemeralExchangeError::Decryption)));
    }

    #[test]
    fn tampered_ciphertext_fails_to_decrypt() {
        let requester_secret = EphemeralSecret::random();
        let requester_public_key = PublicKey::from(&requester_secret).to_bytes();

        let plaintext = b"a real secret";
        let mut payload =
            EphemeralEncryptedPayload::encrypt(plaintext, &requester_public_key).unwrap();
        payload.ciphertext[0] ^= 0xFF;

        let result = payload.decrypt(requester_secret);
        assert!(matches!(result, Err(EphemeralExchangeError::Decryption)));
    }

    #[test]
    fn two_encryptions_of_the_same_plaintext_produce_different_ciphertext() {
        // Both a fresh responder ephemeral key and a fresh nonce each
        // time -- an observer must not be able to tell two exchanges
        // apart just by comparing ciphertext bytes.
        let requester_secret = EphemeralSecret::random();
        let requester_public_key = PublicKey::from(&requester_secret).to_bytes();

        let plaintext = b"the same secret, requested twice";
        let first = EphemeralEncryptedPayload::encrypt(plaintext, &requester_public_key).unwrap();
        let second = EphemeralEncryptedPayload::encrypt(plaintext, &requester_public_key).unwrap();

        assert_ne!(first.ciphertext, second.ciphertext);
        assert_ne!(
            first.responder_ephemeral_public_key,
            second.responder_ephemeral_public_key
        );
    }
}
