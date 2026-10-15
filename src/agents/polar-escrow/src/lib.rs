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

use aes_gcm::aead::{Aead, Generate, Key as AesKey, KeyInit, Payload};
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
        Self::encrypt_bound(plaintext, recipient_public_key_pem, b"")
    }

    /// As `encrypt`, and the ciphertext is bound to `context` as AES-GCM authenticated data: it opens only
    /// for a reader who supplies the same bytes (`decrypt_bound`). The context is not stored in the secret.
    /// A signed document uses it to pin each secret field to its agent, section, field and version, so a
    /// ciphertext cannot be moved into another field or document. An empty context is the secret `encrypt`
    /// has always made.
    pub fn encrypt_bound(
        plaintext: &[u8],
        recipient_public_key_pem: &str,
        context: &[u8],
    ) -> Result<Self, EscrowError> {
        let recipient_public_key = RsaPublicKey::from_public_key_pem(recipient_public_key_pem)
            .map_err(|e| EscrowError::InvalidPublicKey(e.to_string()))?;

        Self::encrypt_to_parsed_key_bound(
            plaintext,
            &recipient_public_key,
            recipient_public_key_pem,
            context,
        )
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
        Self::encrypt_to_parsed_key_bound(
            plaintext,
            recipient_public_key,
            recipient_public_key_pem,
            b"",
        )
    }

    /// `encrypt_to_parsed_key`, bound to `context` (see `encrypt_bound`).
    pub fn encrypt_to_parsed_key_bound(
        plaintext: &[u8],
        recipient_public_key: &RsaPublicKey,
        recipient_public_key_pem: &str,
        context: &[u8],
    ) -> Result<Self, EscrowError> {
        let aes_key = AesKey::<Aes256Gcm>::generate();
        let nonce = Nonce::generate();
        let cipher = Aes256Gcm::new(&aes_key);

        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: context,
                },
            )
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
        self.decrypt_bound(private_key, b"")
    }

    /// `decrypt` for a secret made with `encrypt_bound`: opens only with the context it was bound to.
    pub fn decrypt_bound(
        &self,
        private_key: &RsaPrivateKey,
        context: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, EscrowError> {
        self.decrypt_with_bound(
            |wrapped| {
                private_key
                    .decrypt(Oaep::<Sha256>::new(), wrapped)
                    .map_err(|e| e.to_string())
            },
            context,
        )
    }

    /// Decrypts when the private key cannot be handed over, as when it is held in the kernel keyring and
    /// only the keyring can use it: `unwrap` is given the RSA-OAEP-wrapped AES key and returns it
    /// unwrapped, and the rest happens here. The unwrapped key is wiped when this returns.
    pub fn decrypt_with(
        &self,
        unwrap: impl FnOnce(&[u8]) -> Result<Vec<u8>, String>,
    ) -> Result<Zeroizing<Vec<u8>>, EscrowError> {
        self.decrypt_with_bound(unwrap, b"")
    }

    /// `decrypt_with` for a secret made with `encrypt_bound`: the context is checked here, after the key is
    /// unwrapped, so a holder that only unwraps still cannot open a secret under the wrong context.
    pub fn decrypt_with_bound(
        &self,
        unwrap: impl FnOnce(&[u8]) -> Result<Vec<u8>, String>,
        context: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, EscrowError> {
        let aes_key_bytes = unwrap(&self.encrypted_key).map_err(EscrowError::KeyUnwrap)?;
        let aes_key_bytes = Zeroizing::new(aes_key_bytes);

        let aes_key = AesKey::<Aes256Gcm>::try_from(aes_key_bytes.as_slice())
            .map_err(|_| EscrowError::MalformedKey)?;
        let cipher = Aes256Gcm::new(&aes_key);
        let nonce =
            Nonce::try_from(self.nonce.as_slice()).map_err(|_| EscrowError::MalformedKey)?;

        let plaintext = cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: self.ciphertext.as_slice(),
                    aad: context,
                },
            )
            .map_err(|_| EscrowError::Decryption)?;

        Ok(Zeroizing::new(plaintext))
    }

    /// The PEM-encoded public key this secret is encrypted to.
    pub fn encrypted_to_pem(&self) -> &str {
        &self.encrypted_to_pem
    }
}

/// Why text is not a sealed secret as a signed document carries it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentTextError {
    #[error("is not standard base64")]
    NotBase64,
    #[error("is empty")]
    Empty,
    #[error("is not an escrowed secret")]
    NotASecret,
}

impl EscrowedSecret {
    /// How a signed document carries a sealed secret: standard base64 of the bytes this type has on the wire
    /// (its `rkyv` encoding), so a receiver can check its shape without opening it
    /// (docs/architecture/signed-configuration.md, "Binding").
    pub fn to_document_text(&self) -> Result<String, String> {
        use base64::Engine;
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(self).map_err(|e| e.to_string())?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes.as_slice()))
    }

    /// The inverse, for text that may or may not be one. Says what is wrong without repeating the text.
    pub fn from_document_text(text: &str) -> Result<Self, DocumentTextError> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(|_| DocumentTextError::NotBase64)?;
        if bytes.is_empty() {
            return Err(DocumentTextError::Empty);
        }
        // rkyv wants aligned bytes; a decoded Vec is not promised to be.
        let mut aligned = rkyv::util::AlignedVec::<16>::new();
        aligned.extend_from_slice(&bytes);
        rkyv::from_bytes::<Self, rkyv::rancor::Error>(&aligned)
            .map_err(|_| DocumentTextError::NotASecret)
    }
}

/// Why a part of a binding context cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "`{part}` cannot be part of a binding context: it must be 1 to 64 lowercase letters, digits and hyphens"
)]
pub struct ContextError {
    pub part: &'static str,
}

/// The context a secret in a signed document is bound to when it is encrypted (`EscrowedSecret::encrypt_bound`)
/// and when it is opened (`decrypt_bound`): the agent, the section, the version, the entry's name and the field,
/// joined by `|` (docs/architecture/signed-configuration.md, "Binding"). A ciphertext opens only under the context
/// it was made for, so it cannot be moved to another agent, section, version, entry or field.
///
/// Every part but the version is restricted to lowercase letters, digits and hyphens, so that the `|` between
/// them cannot be forged by a part that contains one. The author's tool and the agent that opens the secret both
/// call this, so they cannot disagree about the format.
pub fn binding_context(
    agent: &str,
    section: &str,
    version: u64,
    name: &str,
    field: &str,
) -> Result<Vec<u8>, ContextError> {
    fn plain(part: &'static str, value: &str) -> Result<(), ContextError> {
        let ok = !value.is_empty()
            && value.len() <= 64
            && value
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if ok {
            Ok(())
        } else {
            Err(ContextError { part })
        }
    }
    plain("agent", agent)?;
    plain("section", section)?;
    plain("name", name)?;
    plain("field", field)?;
    Ok(format!("{agent}|{section}|{version}|{name}|{field}").into_bytes())
}

#[cfg(test)]
mod context_tests {
    use super::*;

    /// One key for these tests: making an RSA key in a build without optimisation takes a while.
    fn key() -> &'static (RsaPrivateKey, String) {
        use rsa::pkcs8::EncodePublicKey;
        static KEY: std::sync::OnceLock<(RsaPrivateKey, String)> = std::sync::OnceLock::new();
        KEY.get_or_init(|| {
            let mut rng = rand::rng();
            let private = RsaPrivateKey::new(&mut rng, 2048).unwrap();
            let pem = private
                .to_public_key()
                .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
                .unwrap();
            (private, pem)
        })
    }

    fn a_secret() -> EscrowedSecret {
        EscrowedSecret::encrypt_bound(b"tok", &key().1, b"ctx").unwrap()
    }

    #[test]
    fn a_sealed_secret_survives_the_text_a_document_carries_it_in() {
        let sealed = a_secret();
        let text = sealed.to_document_text().unwrap();
        assert_eq!(EscrowedSecret::from_document_text(&text).unwrap(), sealed);
        assert!(!text.contains("tok"));
    }

    #[test]
    fn text_that_is_not_a_sealed_secret_is_told_apart_by_what_is_wrong() {
        use base64::Engine;
        let encode = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        assert_eq!(
            EscrowedSecret::from_document_text("***").unwrap_err(),
            DocumentTextError::NotBase64
        );
        assert_eq!(
            EscrowedSecret::from_document_text("").unwrap_err(),
            DocumentTextError::Empty
        );
        for bytes in [
            &b"just some bytes, nothing like a sealed secret"[..],
            &[0u8; 3][..],
        ] {
            assert_eq!(
                EscrowedSecret::from_document_text(&encode(bytes)).unwrap_err(),
                DocumentTextError::NotASecret
            );
        }
        // plain text that happens to be valid base64 is not a secret either
        assert_eq!(
            EscrowedSecret::from_document_text("ghp1").unwrap_err(),
            DocumentTextError::NotASecret
        );
        // and a sealed secret with a byte cut off is not
        let text = a_secret().to_document_text().unwrap();
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(&text)
            .unwrap();
        bytes.pop();
        assert!(EscrowedSecret::from_document_text(&encode(&bytes)).is_err());
    }

    #[test]
    fn the_context_is_agent_section_version_name_and_field_joined_by_bars() {
        assert_eq!(
            binding_context(
                "git-repo-observer",
                "credentials",
                7,
                "github-sei",
                "secret"
            )
            .unwrap(),
            b"git-repo-observer|credentials|7|github-sei|secret".to_vec()
        );
    }

    #[test]
    fn a_part_that_could_forge_a_bar_or_is_empty_is_refused_and_says_which() {
        let good = ("a", "b", 1, "c", "d");
        let with = |part: usize, value: &str| {
            let mut p = [good.0, good.1, good.3, good.4];
            p[part] = value;
            binding_context(p[0], p[1], good.2, p[2], p[3])
        };
        for (i, label) in ["agent", "section", "name", "field"]
            .into_iter()
            .enumerate()
        {
            for bad in ["", "a|b", "A", "a b", "a_b", "é", "a.b", &"a".repeat(65)] {
                assert_eq!(with(i, bad).unwrap_err().part, label, "{label}={bad:?}");
            }
            assert!(with(i, &"a".repeat(64)).is_ok());
            assert!(with(i, "a-1").is_ok());
        }
    }

    #[test]
    fn contexts_that_differ_in_any_part_differ_as_bytes() {
        let base = binding_context("a", "b", 1, "c", "d").unwrap();
        for other in [
            binding_context("x", "b", 1, "c", "d"),
            binding_context("a", "x", 1, "c", "d"),
            binding_context("a", "b", 2, "c", "d"),
            binding_context("a", "b", 1, "x", "d"),
            binding_context("a", "b", 1, "c", "x"),
        ] {
            assert_ne!(other.unwrap(), base);
        }
        // and the bar cannot be shifted from one part to the next
        assert_ne!(
            binding_context("a-b", "c", 1, "d", "e").unwrap(),
            binding_context("a", "b-c", 1, "d", "e").unwrap()
        );
    }

    #[test]
    fn a_secret_opens_only_under_the_context_it_was_made_for() {
        let (private, public_pem) = key();
        let made_for =
            binding_context("git-repo-observer", "credentials", 1, "gh", "secret").unwrap();
        let sealed = EscrowedSecret::encrypt_bound(b"tok", public_pem, &made_for).unwrap();
        assert_eq!(
            sealed.decrypt_bound(private, &made_for).unwrap().as_slice(),
            b"tok"
        );
        for other in [
            binding_context("git-repo-observer", "credentials", 2, "gh", "secret").unwrap(),
            binding_context("git-repo-observer", "credentials", 1, "other", "secret").unwrap(),
            binding_context("git-repo-controller", "credentials", 1, "gh", "secret").unwrap(),
        ] {
            assert!(sealed.decrypt_bound(private, &other).is_err());
        }
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
    fn a_secret_can_be_decrypted_by_a_holder_that_will_only_unwrap_the_key() {
        // As with a key in the kernel keyring: the private key never leaves, only the unwrap is done.
        let (private_key, public_key_pem) = test_recipient_keypair();
        let secret = EscrowedSecret::encrypt(b"kept in custody", &public_key_pem).unwrap();
        let mut unwrapped_for = 0;
        let decrypted = secret
            .decrypt_with(|wrapped| {
                unwrapped_for += wrapped.len();
                private_key
                    .decrypt(Oaep::<Sha256>::new(), wrapped)
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        assert_eq!(decrypted.as_slice(), b"kept in custody");
        assert!(
            unwrapped_for > 0,
            "it was given the wrapped key, nothing else"
        );
        // And a custody that refuses is an unwrap failure, not a panic or a partial plaintext.
        assert!(matches!(
            secret.decrypt_with(|_| Err("not today".into())),
            Err(EscrowError::KeyUnwrap(_))
        ));
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

    #[test]
    fn a_secret_bound_to_a_context_opens_only_with_that_context() {
        let (private_key, public_key_pem) = test_recipient_keypair();
        let ctx = b"git-repo-observer|credentials|token|3";
        let secret = EscrowedSecret::encrypt_bound(b"the token", &public_key_pem, ctx).unwrap();

        assert_eq!(
            secret.decrypt_bound(&private_key, ctx).unwrap().as_slice(),
            b"the token"
        );
        // Moved to another field, another version or another agent: it does not open.
        for other in [
            &b"git-repo-observer|credentials|username|3"[..],
            b"git-repo-observer|credentials|token|4",
            b"other-agent|credentials|token|3",
            b"",
        ] {
            assert!(
                matches!(
                    secret.decrypt_bound(&private_key, other),
                    Err(EscrowError::Decryption)
                ),
                "opened with {:?}",
                String::from_utf8_lossy(other)
            );
        }
        // And the plain decrypt, which binds to nothing, does not open it either.
        assert!(matches!(
            secret.decrypt(&private_key),
            Err(EscrowError::Decryption)
        ));
    }

    #[test]
    fn a_secret_bound_to_nothing_is_the_secret_encrypt_has_always_made() {
        // Secrets already stored were made without a context. They must still open, and a secret bound to the
        // empty context must be one the old decrypt opens.
        let (private_key, public_key_pem) = test_recipient_keypair();
        let old = EscrowedSecret::encrypt(b"stored before", &public_key_pem).unwrap();
        assert_eq!(
            old.decrypt_bound(&private_key, b"").unwrap().as_slice(),
            b"stored before"
        );
        let empty =
            EscrowedSecret::encrypt_bound(b"bound to nothing", &public_key_pem, b"").unwrap();
        assert_eq!(
            empty.decrypt(&private_key).unwrap().as_slice(),
            b"bound to nothing"
        );
    }

    #[test]
    fn a_holder_that_will_only_unwrap_the_key_still_checks_the_context() {
        let (private_key, public_key_pem) = test_recipient_keypair();
        let secret =
            EscrowedSecret::encrypt_bound(b"in custody", &public_key_pem, b"ctx-a").unwrap();
        let unwrap = |wrapped: &[u8]| {
            private_key
                .decrypt(Oaep::<Sha256>::new(), wrapped)
                .map_err(|e| e.to_string())
        };
        assert_eq!(
            secret
                .decrypt_with_bound(unwrap, b"ctx-a")
                .unwrap()
                .as_slice(),
            b"in custody"
        );
        assert!(matches!(
            secret.decrypt_with_bound(unwrap, b"ctx-b"),
            Err(EscrowError::Decryption)
        ));
    }

    #[test]
    fn the_context_is_not_stored_in_the_secret() {
        // The reader supplies it; it is not a field a tamperer could rewrite along with the ciphertext.
        let (_, public_key_pem) = test_recipient_keypair();
        let secret =
            EscrowedSecret::encrypt_bound(b"x", &public_key_pem, b"agent|section|field|1").unwrap();
        let bytes = format!("{secret:?}");
        assert!(!bytes.contains("agent|section"));
    }
}
