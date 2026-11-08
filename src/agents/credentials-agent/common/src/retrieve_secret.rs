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

//! Retrieve half of the general secret-escrow mechanism -- the
//! decrypt-reencrypt-destroy relay credentials-agent-architecture.md's
//! own "Secret escrow" section describes, deliberately deferred when
//! store was built. Genuinely a general-case retrieve: re-encrypts to
//! the requester's own, already-stable mTLS certificate, which
//! already exists for any real Cassini participant -- unlike the one
//! bootstrap case (an agent retrieving its own escrow keypair for the
//! very first time) where no such stable key exists yet, requiring
//! the separate ephemeral X25519 exchange this document also names.
//!
//! No public key travels in the request at all -- deliberately.
//! The target to re-encrypt to is always the requester's own,
//! broker-verified identity (published_by), looked up server-side via
//! the same certificate-lookup machinery certificate.rs already
//! provides. Letting a caller specify an arbitrary target key instead
//! would let a compromised or malicious agent redirect someone else's
//! stored secret to an attacker-controlled key -- deriving the target
//! from verified identity closes that off entirely.

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub struct RetrieveSecretRequest {
    pub name: String,
}

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub enum RetrieveSecretReply {
    Found {
        secret: crate::escrow_secret::EscrowedSecret,
    },
    /// No secret was ever stored under this name -- deliberately not
    /// a Refused variant, mirroring CertificateReply::NotFound/
    /// AgentEscrowKeyReply::NotFound's own precedent: this is an
    /// absence, not a security or policy rejection.
    NotFound,
    Refused {
        reason: RetrieveRefusalReason,
    },
    /// The requester is known but policy does not let it have this secret. Checked before anything is
    /// said about whether the secret exists, so a requester that may not have it learns nothing about it.
    Denied {
        reason: String,
    },
    /// The request was genuinely valid -- a real secret exists, the
    /// requester's own identity was genuinely verified -- but a real
    /// infrastructure failure prevented the relay itself from
    /// completing. Mirrors StoreSecretReply::Failed's own distinction
    /// exactly: not a deterministic rejection retrying would never
    /// fix.
    Failed {
        detail: String,
    },
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    Archive,
    SerdeDeserialize,
    SerdeSerialize,
)]
pub enum RetrieveRefusalReason {
    /// The publish carrying this request opted out of sharing its
    /// own identity (TcpClientMessage::publish_anonymous) -- mirrors
    /// StoreRefusalReason::AnonymousPublisherNotAllowed's own
    /// reasoning exactly: with no verified identity, there is no
    /// target to re-encrypt to at all, and no way to confirm the
    /// requester is actually who they'd need to be to receive this
    /// secret.
    AnonymousPublisherNotAllowed,
    /// The requester's own published_by identity has no current,
    /// valid mTLS certificate on record -- genuinely unusual, given
    /// they would need one to be talking to Cassini via mTLS at all,
    /// but named as its own, explicit refusal rather than silently
    /// falling through to Failed.
    RequesterCertificateNotFound,
    /// The requester has no escrow key on record, so there is nothing to encrypt the secret to. A secret
    /// is only ever re-encrypted to the requester's agent escrow key (its RSA service identity), never to
    /// its mTLS key, which signs and does not encrypt.
    RequesterEscrowKeyNotFound,
    /// The name is not one a secret can have. Names become part of a policy resource, so they are
    /// limited to letters, digits and `._:/-`, 1 to 128 of them.
    InvalidSecretName,
}

/// Whether `name` can be the name of a secret: it is used to build a policy resource, so nothing in it
/// may be able to end that resource early.
pub fn is_valid_secret_name(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escrow_secret::EscrowedSecret;

    fn sample_secret() -> EscrowedSecret {
        let mut rng = rand::rng();
        let private_key = rsa::RsaPrivateKey::new(&mut rng, 4096).unwrap();
        let public_key_pem = rsa::pkcs8::EncodePublicKey::to_public_key_pem(
            &private_key.to_public_key(),
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();
        EscrowedSecret::encrypt(b"a-real-secret", &public_key_pem).unwrap()
    }

    #[test]
    fn retrieve_secret_request_round_trips_through_serde_json() {
        let original = RetrieveSecretRequest {
            name: "gitlab-token".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: RetrieveSecretRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn retrieve_secret_request_round_trips_through_rkyv() {
        let original = RetrieveSecretRequest {
            name: "gitlab-token".to_string(),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: RetrieveSecretRequest =
            rkyv::from_bytes::<RetrieveSecretRequest, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn retrieve_secret_reply_found_round_trips_through_both() {
        let original = RetrieveSecretReply::Found {
            secret: sample_secret(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: RetrieveSecretReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: RetrieveSecretReply =
            rkyv::from_bytes::<RetrieveSecretReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored_rkyv);
    }

    #[test]
    fn retrieve_secret_reply_not_found_round_trips_through_both() {
        let original = RetrieveSecretReply::NotFound;

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: RetrieveSecretReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: RetrieveSecretReply =
            rkyv::from_bytes::<RetrieveSecretReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored_rkyv);
    }

    #[test]
    fn retrieve_secret_reply_refused_round_trips_with_correct_reason() {
        let original = RetrieveSecretReply::Refused {
            reason: RetrieveRefusalReason::RequesterCertificateNotFound,
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: RetrieveSecretReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: RetrieveSecretReply =
            rkyv::from_bytes::<RetrieveSecretReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored_rkyv);
    }

    #[test]
    fn secret_names_that_could_end_a_policy_resource_early_are_not_names() {
        for good in ["gitlab-token", "github-sei", "a.b_c:d/e-1"] {
            assert!(is_valid_secret_name(good), "{good}");
        }
        for bad in [
            "",
            "has space",
            "quote\"",
            "a\"; permit(",
            "new\nline",
            "ünï",
            &"x".repeat(129),
        ] {
            assert!(!is_valid_secret_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn retrieve_secret_reply_denied_and_new_refusals_round_trip() {
        for original in [
            RetrieveSecretReply::Denied {
                reason: "no".into(),
            },
            RetrieveSecretReply::Refused {
                reason: RetrieveRefusalReason::RequesterEscrowKeyNotFound,
            },
            RetrieveSecretReply::Refused {
                reason: RetrieveRefusalReason::InvalidSecretName,
            },
        ] {
            let json = serde_json::to_string(&original).unwrap();
            assert_eq!(
                original,
                serde_json::from_str::<RetrieveSecretReply>(&json).unwrap()
            );
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
                .unwrap()
                .to_vec();
            assert_eq!(
                original,
                rkyv::from_bytes::<RetrieveSecretReply, rkyv::rancor::Error>(&bytes).unwrap()
            );
        }
    }

    #[test]
    fn retrieve_secret_reply_failed_round_trips_through_both() {
        let original = RetrieveSecretReply::Failed {
            detail: "ledger append failed".to_string(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: RetrieveSecretReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: RetrieveSecretReply =
            rkyv::from_bytes::<RetrieveSecretReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored_rkyv);
    }
}
