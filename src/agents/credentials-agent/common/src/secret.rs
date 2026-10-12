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

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub struct StoreSecretRequest {
    /// The storing agent's own chosen identifier -- e.g. "gitlab-token".
    /// A second store under the same name supersedes the first; see
    /// CredentialsState's own folding of SecretStored, in state.rs.
    pub name: String,
    pub secret: EscrowedSecret,
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

    #[test]
    fn store_secret_request_round_trips_through_serde_json() {
        let original = StoreSecretRequest {
            name: "gitlab-token".to_string(),
            secret: sample_secret(),
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
