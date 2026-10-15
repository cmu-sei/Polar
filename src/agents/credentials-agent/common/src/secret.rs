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

//! Store half of the general secret-escrow store/retrieve mechanism
//! (credentials-agent-architecture.md's own "Secret escrow" section).
//! Retrieve is deliberately not built yet -- it depends on the agent
//! escrow-key query (item B in that same document's "What is
//! deferred"), which doesn't exist yet either. Store stands on its
//! own regardless: a secret encrypted to the Credentials Agent's own,
//! already-queryable escrow key can be validated and persisted today.

use crate::escrow_secret::EscrowedSecret;
use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use std::collections::BTreeMap;

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub struct StoreSecretRequest {
    /// The storing agent's own chosen identifier -- e.g. "gitlab-token".
    /// A second store under the same name supersedes the first; see
    /// CredentialsState's own folding of SecretStored, in state.rs.
    pub name: String,
    pub secret: EscrowedSecret,
    /// Facts about the secret that policy can speak of, from the moment it is stored: each is an attribute of
    /// the secret's policy entity (a label `kind` is `resource.kind`). Bounded and well formed
    /// (`validate_labels`). A label cannot change once stored, so a replacement of the value must carry the same
    /// ones. Written before labels existed, a request has none.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// The most labels a secret can have.
pub const MAX_LABELS: usize = 8;
/// The longest a label's key, and its value, can be.
pub const MAX_LABEL_KEY: usize = 32;
pub const MAX_LABEL_VALUE: usize = 128;
/// Keys a policy gives a secret itself, so a label cannot take them: `owner` comes from the ledger, `namespace` from
/// the name, and `frozen` is the annotation a policy document can add.
pub const RESERVED_LABEL_KEYS: [&str; 3] = ["owner", "namespace", "frozen"];

/// The identity whose namespace a secret's name is in: what comes before the first `/`, when something comes
/// after it. `git-repo-observer/github` is in `git-repo-observer`'s. A name with no `/`, or nothing on either
/// side of it, is in nobody's namespace, and neither is `agent-escrow-keypair:<identity>`, which has its own
/// rule (docs/architecture/credentials-policy.md, "Storing is governed by policy").
pub fn namespace_of(name: &str) -> Option<&str> {
    let (namespace, rest) = name.split_once('/')?;
    (!namespace.is_empty() && !rest.is_empty()).then_some(namespace)
}

/// What is wrong with a set of labels, if anything, naming the key at fault and never repeating a value. A key is
/// lowercase letters, digits and underscores starting with a letter (a plain Cedar attribute name); a value is
/// printable ASCII; neither is empty; and the reserved keys are refused.
pub fn validate_labels(labels: &BTreeMap<String, String>) -> Result<(), String> {
    if labels.len() > MAX_LABELS {
        return Err(format!(
            "a secret has at most {MAX_LABELS} labels, and this one has {}",
            labels.len()
        ));
    }
    for (key, value) in labels {
        let mut chars = key.chars();
        let key_ok = key.len() <= MAX_LABEL_KEY
            && chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !key_ok {
            return Err(format!(
                "label key {key:?} must be lowercase letters, digits and underscores, starting with a letter, 1 to {MAX_LABEL_KEY} characters"
            ));
        }
        if RESERVED_LABEL_KEYS.contains(&key.as_str()) {
            return Err(format!("label key {key:?} is reserved"));
        }
        let value_ok = (1..=MAX_LABEL_VALUE).contains(&value.len())
            && value.chars().all(|c| (' '..='~').contains(&c));
        if !value_ok {
            return Err(format!(
                "the value of label {key:?} must be printable ASCII, 1 to {MAX_LABEL_VALUE} characters"
            ));
        }
    }
    Ok(())
}

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub enum StoreSecretReply {
    Stored,
    Refused {
        reason: StoreRefusalReason,
    },
    /// The request was genuinely valid -- correctly encrypted to this
    /// agent's own current escrow key, from a real, attributed
    /// publisher -- but a real infrastructure failure (the ledger
    /// append itself) prevented it from actually being persisted.
    /// Distinct from Refused: this isn't a deterministic rejection
    /// retrying would never fix, it's the opposite -- the caller
    /// should not assume the secret is safely stored, and retrying
    /// may well succeed.
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
pub enum StoreRefusalReason {
    /// The submitted EscrowedSecret is encrypted to some public key
    /// other than the Credentials Agent's own, current escrow key --
    /// accepting and storing it anyway would silently produce
    /// something this agent could never actually decrypt later,
    /// corrupting the entire point of storing it. The caller should
    /// re-query the current escrow key (it may have rotated) and
    /// re-encrypt.
    WrongEscrowKey,
    /// The publish carrying this request opted out of sharing its
    /// own identity (TcpClientMessage::publish_anonymous) -- a
    /// legitimate choice for most publishes, but not this one. This
    /// system's entire value as a true observability platform
    /// depends on knowing who stored what; an anonymous store would
    /// be an unattributable entry the audit trail can never explain.
    AnonymousPublisherNotAllowed,
    /// A secret by this name is held, and it was stored by another identity. Only the identity that
    /// stored a secret may replace it: the owner is what policy speaks of (`resource.owner`), and a store
    /// that could change it would let any agent take a secret over, or lock its owner out of it, by
    /// storing the same name.
    NameTaken,
    /// The name is not one a secret can have (`is_valid_secret_name`): it is used to build a policy resource, so
    /// nothing in it may end that resource early.
    InvalidSecretName,
    /// The labels are not well formed (`validate_labels`).
    InvalidLabels,
    /// A secret by this name is held, stored by the requester, with other labels. A label cannot change once
    /// stored; store under a new name to change one.
    LabelsChanged,
    /// The requester has no client certificate on record, so policy has nothing to read its permissions from.
    RequesterCertificateNotFound,
    /// Policy does not let this identity store this secret, and neither does the floor that applies when policy
    /// does not speak: an identity may store under its own namespace.
    PolicyDenied,
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn labels(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_request_with_labels_round_trips_through_both_encodings() {
        let original = StoreSecretRequest {
            name: "git-repo-observer.polar/github".to_string(),
            secret: sample_secret(),
            labels: labels(&[("kind", "token")]),
        };
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(
            serde_json::from_str::<StoreSecretRequest>(&json).unwrap(),
            original
        );
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored = rkyv::from_bytes::<StoreSecretRequest, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn a_request_written_before_labels_existed_still_reads_with_none() {
        let original = StoreSecretRequest {
            name: "n".to_string(),
            secret: sample_secret(),
            labels: Default::default(),
        };
        let mut value = serde_json::to_value(&original).unwrap();
        value.as_object_mut().unwrap().remove("labels");
        let restored: StoreSecretRequest = serde_json::from_value(value).unwrap();
        assert!(restored.labels.is_empty());
    }

    #[test]
    fn up_to_eight_well_formed_labels_are_accepted() {
        assert_eq!(validate_labels(&labels(&[])), Ok(()));
        assert_eq!(validate_labels(&labels(&[("kind", "token")])), Ok(()));
        let eight: Vec<(String, String)> =
            (0..8).map(|i| (format!("k{i}"), "v".to_string())).collect();
        let eight: std::collections::BTreeMap<_, _> = eight.into_iter().collect();
        assert_eq!(validate_labels(&eight), Ok(()));
    }

    #[test]
    fn more_than_eight_labels_are_refused() {
        let nine: std::collections::BTreeMap<_, _> =
            (0..9).map(|i| (format!("k{i}"), "v".to_string())).collect();
        assert!(validate_labels(&nine).is_err());
    }

    #[test]
    fn a_key_is_a_plain_cedar_attribute_name() {
        let ok_32 = "a".repeat(32);
        assert_eq!(validate_labels(&labels(&[(&ok_32, "v")])), Ok(()));
        assert_eq!(validate_labels(&labels(&[("a_b2", "v")])), Ok(()));
        for key in [
            "",
            "Kind",
            "1kind",
            "_kind",
            "kind-x",
            "kind x",
            "kind.x",
            "kínd",
            &"a".repeat(33),
        ] {
            assert!(
                validate_labels(&labels(&[(key, "v")])).is_err(),
                "{key:?} was accepted"
            );
        }
    }

    #[test]
    fn a_value_is_printable_ascii_of_one_to_128_characters() {
        let ok_128 = "x".repeat(128);
        assert_eq!(validate_labels(&labels(&[("k", &ok_128)])), Ok(()));
        assert_eq!(
            validate_labels(&labels(&[("k", "a value, with: punctuation/_-.")])),
            Ok(())
        );
        for value in [
            "",
            "tab\there",
            "new\nline",
            "del\u{7f}",
            "caf\u{e9}",
            &"x".repeat(129),
        ] {
            assert!(
                validate_labels(&labels(&[("k", value)])).is_err(),
                "{value:?} was accepted"
            );
        }
    }

    #[test]
    fn the_keys_a_policy_reserves_cannot_be_labels() {
        for key in ["owner", "namespace", "frozen"] {
            assert!(
                validate_labels(&labels(&[(key, "v")])).is_err(),
                "{key} was accepted"
            );
        }
    }

    #[test]
    fn a_problem_names_the_key_and_never_repeats_a_value() {
        let problem =
            validate_labels(&labels(&[("Kind", "super-secret-looking-value")])).unwrap_err();
        assert!(problem.contains("Kind"), "{problem}");
        assert!(!problem.contains("super-secret-looking-value"), "{problem}");
    }

    #[test]
    fn a_bad_value_is_refused_naming_the_key_and_never_repeating_the_value() {
        let problem = validate_labels(&labels(&[("k", "caf\u{e9}-SECRET-looking")])).unwrap_err();
        assert!(problem.contains("\"k\""), "{problem}");
        assert!(!problem.contains("SECRET"), "{problem}");
    }

    #[test]
    fn the_namespace_of_a_name_is_what_comes_before_the_first_slash() {
        assert_eq!(
            namespace_of("git-repo-observer/github"),
            Some("git-repo-observer")
        );
        assert_eq!(namespace_of("a.b.c/d/e"), Some("a.b.c"));
        assert_eq!(namespace_of("a/b"), Some("a"));
    }

    #[test]
    fn a_name_with_nothing_on_either_side_of_the_slash_has_no_namespace() {
        for name in [
            "github",
            "",
            "/github",
            "a/",
            "/",
            "agent-escrow-keypair:x.y",
        ] {
            assert_eq!(namespace_of(name), None, "{name:?}");
        }
    }

    #[test]
    fn the_new_refusal_reasons_round_trip_and_keep_the_old_ones_first() {
        for reason in [
            StoreRefusalReason::InvalidSecretName,
            StoreRefusalReason::InvalidLabels,
            StoreRefusalReason::LabelsChanged,
            StoreRefusalReason::RequesterCertificateNotFound,
            StoreRefusalReason::PolicyDenied,
        ] {
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&reason)
                .unwrap()
                .to_vec();
            assert_eq!(
                rkyv::from_bytes::<StoreRefusalReason, rkyv::rancor::Error>(&bytes).unwrap(),
                reason
            );
        }
    }

    #[test]
    fn store_secret_request_round_trips_through_serde_json() {
        let original = StoreSecretRequest {
            name: "gitlab-token".to_string(),
            secret: sample_secret(),
            labels: Default::default(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: StoreSecretRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn store_secret_request_round_trips_through_rkyv() {
        let original = StoreSecretRequest {
            name: "gitlab-token".to_string(),
            secret: sample_secret(),
            labels: Default::default(),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: StoreSecretRequest =
            rkyv::from_bytes::<StoreSecretRequest, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn store_secret_reply_stored_round_trips_through_both() {
        let original = StoreSecretReply::Stored;

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: StoreSecretReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: StoreSecretReply =
            rkyv::from_bytes::<StoreSecretReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored_rkyv);
    }

    #[test]
    fn store_secret_reply_refused_round_trips_with_correct_reason() {
        let original = StoreSecretReply::Refused {
            reason: StoreRefusalReason::WrongEscrowKey,
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: StoreSecretReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: StoreSecretReply =
            rkyv::from_bytes::<StoreSecretReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored_rkyv);
    }
}
