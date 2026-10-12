//! What the credentials agent publishes when its policy or the data about principals changes, and how a
//! leaf believes it (docs/architecture/authority-distribution.md).
//!
//! The credentials agent signs each update with its escrow key, RSASSA-PSS with SHA-256, over the payload
//! bytes prefixed with a fixed domain line, so a signature made for any other purpose cannot be replayed as an
//! update. The update carries the **bytes** that were signed, not a structure to be re-encoded: what is verified
//! is exactly what the signer wrote, and only then is it read. It also carries the fingerprint of the key that
//! signed it, so a leaf that pins a key notices at once if it is ever a different one.
//!
//! Each payload is the whole of its kind (the whole policy, the whole membership, the whole set of revoked
//! serials), never a delta, so an update that was missed is repaired by the next of its kind.

use rkyv::{Archive, Deserialize, Serialize};
use rsa::pkcs8::EncodePublicKey;
use rsa::pss::{BlindedSigningKey, Signature, VerifyingKey};
use rsa::signature::{RandomizedSigner, SignatureEncoding, Verifier};
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use std::collections::BTreeMap;

/// Where the credentials agent publishes updates, and every agent listens.
pub const AUTHORITY_UPDATES_TOPIC: &str = "polar-authority-updates";

/// Where an agent asks for the current authority, and the credentials agent answers: a leaf that has just
/// registered, or that saw a jump in the sequence. The answer is also published without being asked every half
/// hour, so a leaf that missed everything is repaired by the next one.
pub const AUTHORITY_SNAPSHOT_REQUESTS_TOPIC: &str = "polar-authority-snapshot-requests";
pub const AUTHORITY_SNAPSHOT_REPLIES_TOPIC: &str = "polar-authority-snapshot-replies";

/// The line every signed update's bytes begin with.
const DOMAIN: &[u8] = b"polar-authority-update-v1\n";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorityUpdateError {
    /// Signed by a key other than the one pinned. Nothing else about the update is believed.
    #[error("signed by key {got}, and the pinned key is {expected}")]
    UnknownKey { expected: String, got: String },
    #[error("the signature does not verify")]
    BadSignature,
    /// Signed correctly and not a payload: the signer wrote something this does not read.
    #[error("the signed payload is not an authority update: {0}")]
    Malformed(String),
    #[error("could not make the update: {0}")]
    Encoding(String),
}

/// What a payload replaces, in full.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthorityBody {
    /// The Cedar rules.
    Policy { source: String },
    /// Who is in which group: for each identity, its groups.
    Groups {
        memberships: BTreeMap<String, Vec<String>>,
    },
    /// The serials of the certificates that are revoked.
    Revoked { serials: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthorityKind {
    Policy,
    Groups,
    Revoked,
}

impl AuthorityBody {
    pub fn kind(&self) -> AuthorityKind {
        match self {
            AuthorityBody::Policy { .. } => AuthorityKind::Policy,
            AuthorityBody::Groups { .. } => AuthorityKind::Groups,
            AuthorityBody::Revoked { .. } => AuthorityKind::Revoked,
        }
    }
}

/// What the credentials agent signs.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityPayload {
    /// The credentials agent's own ordering of everything it has published: a consumer that sees a jump knows
    /// it missed one.
    pub sequence: u64,
    /// Rises with each update of this payload's kind. The latest replaces the earlier, whole.
    pub version: u64,
    pub body: AuthorityBody,
    /// Who authored it, when a person did (their username), for the record. Not what makes it believed.
    pub authored_by: Option<String>,
}

/// An authority update as it travels.
#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub struct AuthorityUpdate {
    /// The SHA-256 of the signing key's public half (its SubjectPublicKeyInfo), in hex.
    pub key_fingerprint: String,
    /// The signed bytes: a JSON `AuthorityPayload`.
    pub payload: Vec<u8>,
    /// RSASSA-PSS over the domain line and `payload`.
    pub signature: Vec<u8>,
}

/// "Send me the current authority." Anyone may ask: what comes back is public, signed, and the same for
/// everyone.
#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub struct AuthoritySnapshotRequest;

/// The current authority: the key that signs it, and the latest signed update of each kind. A leaf that has
/// no key yet pins the one here (accepted only from the credentials agent's own identity, as the broker names
/// it), and every update in it is checked against that key like any other.
#[derive(
    Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize, SerdeSerialize, SerdeDeserialize,
)]
pub struct AuthoritySnapshot {
    /// The signing key's public half, PEM.
    pub public_key_pem: String,
    /// The latest signed update of each kind, oldest sequence first.
    pub updates: Vec<AuthorityUpdate>,
}

impl AuthoritySnapshot {
    /// The key it carries, parsed.
    pub fn public_key(&self) -> Result<RsaPublicKey, AuthorityUpdateError> {
        use rsa::pkcs8::DecodePublicKey;
        RsaPublicKey::from_public_key_pem(&self.public_key_pem)
            .map_err(|e| AuthorityUpdateError::Encoding(format!("the snapshot's key: {e}")))
    }
}

/// The fingerprint of a public key: what an update says signed it, and what a leaf compares with the key it
/// pinned.
pub fn fingerprint(key: &RsaPublicKey) -> Result<String, AuthorityUpdateError> {
    use sha2::{Digest, Sha256};
    let der = key
        .to_public_key_der()
        .map_err(|e| AuthorityUpdateError::Encoding(e.to_string()))?;
    Ok(hex(&Sha256::digest(der.as_bytes())))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn signed_bytes(payload: &[u8]) -> Vec<u8> {
    let mut bytes = DOMAIN.to_vec();
    bytes.extend_from_slice(payload);
    bytes
}

/// Signs `payload` with the credentials agent's key. The signing is blinded, so it does not leak the key
/// through how long it takes.
pub fn sign(
    payload: &AuthorityPayload,
    key: &RsaPrivateKey,
) -> Result<AuthorityUpdate, AuthorityUpdateError> {
    let bytes =
        serde_json::to_vec(payload).map_err(|e| AuthorityUpdateError::Encoding(e.to_string()))?;
    let signing = BlindedSigningKey::<sha2_011::Sha256>::new(key.clone());
    let signature: Signature = signing.sign_with_rng(&mut rand::rng(), &signed_bytes(&bytes));
    Ok(AuthorityUpdate {
        key_fingerprint: fingerprint(&key.to_public_key())?,
        payload: bytes,
        signature: signature.to_vec(),
    })
}

/// Verifies `update` against the key a leaf pinned, and reads the payload only if it holds. In order: the
/// update says it was signed by the pinned key; the signature holds over the domain line and the bytes; then,
/// and only then, the bytes are read, strictly.
pub fn verify(
    update: &AuthorityUpdate,
    pinned: &RsaPublicKey,
) -> Result<AuthorityPayload, AuthorityUpdateError> {
    let expected = fingerprint(pinned)?;
    if update.key_fingerprint != expected {
        return Err(AuthorityUpdateError::UnknownKey {
            expected,
            got: update.key_fingerprint.clone(),
        });
    }
    let signature = Signature::try_from(update.signature.as_slice())
        .map_err(|_| AuthorityUpdateError::BadSignature)?;
    VerifyingKey::<sha2_011::Sha256>::new(pinned.clone())
        .verify(&signed_bytes(&update.payload), &signature)
        .map_err(|_| AuthorityUpdateError::BadSignature)?;
    serde_json::from_slice(&update.payload)
        .map_err(|e| AuthorityUpdateError::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> RsaPrivateKey {
        use std::sync::OnceLock;
        static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
        KEY.get_or_init(|| RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap())
            .clone()
    }

    fn another_key() -> RsaPrivateKey {
        use std::sync::OnceLock;
        static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
        KEY.get_or_init(|| RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap())
            .clone()
    }

    fn payload() -> AuthorityPayload {
        AuthorityPayload {
            sequence: 7,
            version: 3,
            body: AuthorityBody::Groups {
                memberships: [("alice".to_string(), vec!["operators".to_string()])].into(),
            },
            authored_by: Some("alice".into()),
        }
    }

    #[test]
    fn an_update_signed_by_the_pinned_key_verifies_and_gives_back_what_was_signed() {
        let key = key();
        let update = sign(&payload(), &key).unwrap();
        assert_eq!(
            update.key_fingerprint,
            fingerprint(&key.to_public_key()).unwrap()
        );
        assert_eq!(verify(&update, &key.to_public_key()), Ok(payload()));
    }

    #[test]
    fn every_kind_of_body_travels() {
        let key = key();
        for body in [
            AuthorityBody::Policy {
                source: r#"permit(principal, action, resource);"#.into(),
            },
            AuthorityBody::Groups {
                memberships: BTreeMap::new(),
            },
            AuthorityBody::Revoked {
                serials: vec!["01ab".into(), "02cd".into()],
            },
        ] {
            let sent = AuthorityPayload {
                sequence: 1,
                version: 1,
                body,
                authored_by: None,
            };
            let update = sign(&sent, &key).unwrap();
            assert_eq!(verify(&update, &key.to_public_key()).unwrap(), sent);
        }
    }

    #[test]
    fn a_payload_or_a_signature_that_was_touched_does_not_verify() {
        let key = key();
        let update = sign(&payload(), &key).unwrap();
        let public = key.to_public_key();

        let mut altered = update.clone();
        let at = altered.payload.len() / 2;
        altered.payload[at] ^= 1;
        assert_eq!(
            verify(&altered, &public),
            Err(AuthorityUpdateError::BadSignature)
        );

        let mut forged = update.clone();
        forged.signature[0] ^= 1;
        assert_eq!(
            verify(&forged, &public),
            Err(AuthorityUpdateError::BadSignature)
        );

        let mut truncated = update;
        truncated.signature.truncate(10);
        assert_eq!(
            verify(&truncated, &public),
            Err(AuthorityUpdateError::BadSignature)
        );
    }

    #[test]
    fn an_update_from_another_key_is_refused_by_its_fingerprint_before_anything_else() {
        let update = sign(&payload(), &another_key()).unwrap();
        let pinned = key().to_public_key();
        match verify(&update, &pinned) {
            Err(AuthorityUpdateError::UnknownKey { expected, got }) => {
                assert_eq!(expected, fingerprint(&pinned).unwrap());
                assert_eq!(got, update.key_fingerprint);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_fingerprint_that_lies_about_the_signing_key_does_not_help() {
        // Signed by another key, but says it is the pinned one: the signature is checked against the pinned
        // key and fails.
        let mut update = sign(&payload(), &another_key()).unwrap();
        let pinned = key().to_public_key();
        update.key_fingerprint = fingerprint(&pinned).unwrap();
        assert_eq!(
            verify(&update, &pinned),
            Err(AuthorityUpdateError::BadSignature)
        );
    }

    #[test]
    fn a_signature_made_for_another_purpose_cannot_be_replayed_as_an_update() {
        // The same key, the same payload bytes, signed without the domain line.
        let key = key();
        let bytes = serde_json::to_vec(&payload()).unwrap();
        let signing = BlindedSigningKey::<sha2_011::Sha256>::new(key.clone());
        let bare: Signature = signing.sign_with_rng(&mut rand::rng(), &bytes);
        let update = AuthorityUpdate {
            key_fingerprint: fingerprint(&key.to_public_key()).unwrap(),
            payload: bytes,
            signature: bare.to_vec(),
        };
        assert_eq!(
            verify(&update, &key.to_public_key()),
            Err(AuthorityUpdateError::BadSignature)
        );
    }

    #[test]
    fn a_correctly_signed_payload_that_is_not_an_update_is_malformed_not_believed() {
        let key = key();
        for bytes in [
            br#"{"sequence":1,"version":1,"body":{"kind":"policy","source":"x"},"authored_by":null,"extra":1}"#
                .to_vec(),
            br#"{"sequence":1,"version":1,"body":{"kind":"nothing"},"authored_by":null}"#.to_vec(),
            b"not json".to_vec(),
        ] {
            let signing = BlindedSigningKey::<sha2_011::Sha256>::new(key.clone());
            let signed: Signature = signing.sign_with_rng(&mut rand::rng(), &signed_bytes(&bytes));
            let update = AuthorityUpdate {
                key_fingerprint: fingerprint(&key.to_public_key()).unwrap(),
                payload: bytes,
                signature: signed.to_vec(),
            };
            assert!(
                matches!(
                    verify(&update, &key.to_public_key()),
                    Err(AuthorityUpdateError::Malformed(_))
                ),
                "{:?}",
                String::from_utf8_lossy(&update.payload)
            );
        }
    }

    #[test]
    fn a_snapshot_and_a_request_for_one_survive_the_wire() {
        let key = key();
        let snapshot = AuthoritySnapshot {
            public_key_pem: rsa::pkcs8::EncodePublicKey::to_public_key_pem(
                &key.to_public_key(),
                rsa::pkcs8::LineEnding::LF,
            )
            .unwrap(),
            updates: vec![sign(&payload(), &key).unwrap()],
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&snapshot).unwrap();
        let back = rkyv::from_bytes::<AuthoritySnapshot, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(back, snapshot);
        // The key it carries is the key that signed what it carries.
        let carried = back.public_key().unwrap();
        assert_eq!(carried, key.to_public_key());
        assert_eq!(verify(&back.updates[0], &carried), Ok(payload()));

        let request = rkyv::to_bytes::<rkyv::rancor::Error>(&AuthoritySnapshotRequest).unwrap();
        rkyv::from_bytes::<AuthoritySnapshotRequest, rkyv::rancor::Error>(&request).unwrap();
    }

    #[test]
    fn a_snapshot_whose_key_is_not_a_key_says_so() {
        let snapshot = AuthoritySnapshot {
            public_key_pem: "not a key".into(),
            updates: vec![],
        };
        assert!(matches!(
            snapshot.public_key(),
            Err(AuthorityUpdateError::Encoding(_))
        ));
    }

    #[test]
    fn an_update_survives_the_wire() {
        let key = key();
        let update = sign(&payload(), &key).unwrap();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&update).unwrap();
        let back: AuthorityUpdate =
            rkyv::from_bytes::<AuthorityUpdate, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(back, update);
        assert_eq!(verify(&back, &key.to_public_key()), Ok(payload()));
    }
}
