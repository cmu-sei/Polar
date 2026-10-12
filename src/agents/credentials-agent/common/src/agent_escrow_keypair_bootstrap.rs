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

//! The one, narrow bootstrap case ephemeral_exchange.rs's own module
//! doc names: an agent retrieving its own escrow keypair for the
//! very first time. Genuinely distinct from retrieve_secret.rs's own
//! RetrieveSecretRequest/Reply -- that re-encrypts to the requester's
//! own, already-stable mTLS certificate, which doesn't exist yet for
//! a service identity that hasn't completed automated agent
//! escrow-identity issuance for the first time. This flow uses the
//! ephemeral X25519 exchange instead, and needs no name field at
//! all -- like AgentEscrowKeyRequest, this is implicitly "retrieve
//! *my own* escrow keypair," derived entirely from the requester's
//! own broker-verified identity, never anything the request itself
//! specifies.
//!
//! An agent's own escrow private key is itself just another secret,
//! stored through the same general SecretStored mechanism
//! store_secret.rs already provides -- not a separate storage path.
//! It's stored under a well-known name derived from the storing
//! agent's own identity:
//!
//!   agent_escrow_keypair_secret_name(identity) ->
//!     format!("agent-escrow-keypair:{identity}")
//!
//! Both the storing side (CryptoActor's own bootstrap sequence,
//! still to come) and this flow's own retrieve handler must agree on
//! this exact convention -- named once, here, as the single source
//! of truth for it, rather than duplicated inline at each call site.

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// The well-known name an agent's own escrowed keypair is stored
/// under, via the general SecretStored mechanism -- see this
/// module's own doc comment for why this lives here, as shared,
/// canonical logic, rather than being reconstructed independently by
/// both the storing and retrieving sides.
pub fn agent_escrow_keypair_secret_name(identity: &str) -> String {
    format!("agent-escrow-keypair:{identity}")
}

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub struct RetrieveAgentEscrowKeypairRequest {
    /// This agent's own, throwaway X25519 public key, generated
    /// solely for this one exchange -- see ephemeral_exchange.rs's
    /// own EphemeralEncryptedPayload for the full mechanism.
    pub ephemeral_public_key: [u8; 32],
}

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub enum RetrieveAgentEscrowKeypairReply {
    Found {
        payload: crate::ephemeral_exchange::EphemeralEncryptedPayload,
    },
    /// No escrow keypair has ever been stored for this identity --
    /// deliberately not a Refused variant, mirroring
    /// RetrieveSecretReply::NotFound's own precedent: this is an
    /// absence (the identity hasn't completed automated agent
    /// escrow-identity issuance and its own escrow step yet), not a
    /// security or policy rejection.
    NotFound,
    Refused {
        reason: RetrieveAgentEscrowKeypairRefusalReason,
    },
    /// A real infrastructure failure, mirroring RetrieveSecretReply::
    /// Failed/StoreSecretReply::Failed's own distinction from a
    /// deterministic rejection.
    Failed { detail: String },
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
pub enum RetrieveAgentEscrowKeypairRefusalReason {
    /// The publish carrying this request opted out of sharing its
    /// own identity (TcpClientMessage::publish_anonymous) -- mirrors
    /// RetrieveRefusalReason::AnonymousPublisherNotAllowed's own
    /// reasoning: with no verified identity, there's no way to know
    /// whose escrow keypair is even being asked for.
    ///
    /// No RequesterCertificateNotFound-style variant here, unlike
    /// RetrieveRefusalReason -- this flow never looks up the
    /// requester's own mTLS certificate at all; the ephemeral
    /// exchange doesn't need one.
    AnonymousPublisherNotAllowed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ephemeral_exchange::EphemeralEncryptedPayload;
    use x25519_dalek::{EphemeralSecret, PublicKey};

    fn sample_payload() -> EphemeralEncryptedPayload {
        let requester_secret = EphemeralSecret::random();
        let requester_public_key = PublicKey::from(&requester_secret).to_bytes();
        EphemeralEncryptedPayload::encrypt(b"a real escrowed keypair", &requester_public_key)
            .unwrap()
    }

    #[test]
    fn agent_escrow_keypair_secret_name_is_deterministic_and_identity_scoped() {
        assert_eq!(
            agent_escrow_keypair_secret_name("git-observer"),
            agent_escrow_keypair_secret_name("git-observer")
        );
        assert_ne!(
            agent_escrow_keypair_secret_name("git-observer"),
            agent_escrow_keypair_secret_name("jira-connector")
        );
    }

    #[test]
    fn retrieve_agent_escrow_keypair_request_round_trips_through_serde_json() {
        let original = RetrieveAgentEscrowKeypairRequest {
            ephemeral_public_key: [7u8; 32],
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: RetrieveAgentEscrowKeypairRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn retrieve_agent_escrow_keypair_request_round_trips_through_rkyv() {
        let original = RetrieveAgentEscrowKeypairRequest {
            ephemeral_public_key: [7u8; 32],
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: RetrieveAgentEscrowKeypairRequest =
            rkyv::from_bytes::<RetrieveAgentEscrowKeypairRequest, rkyv::rancor::Error>(&bytes)
                .unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn retrieve_agent_escrow_keypair_reply_found_round_trips_through_both() {
        let original = RetrieveAgentEscrowKeypairReply::Found {
            payload: sample_payload(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: RetrieveAgentEscrowKeypairReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: RetrieveAgentEscrowKeypairReply =
            rkyv::from_bytes::<RetrieveAgentEscrowKeypairReply, rkyv::rancor::Error>(&bytes)
                .unwrap();
        assert_eq!(original, restored_rkyv);
    }

    #[test]
    fn retrieve_agent_escrow_keypair_reply_not_found_round_trips_through_both() {
        let original = RetrieveAgentEscrowKeypairReply::NotFound;

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: RetrieveAgentEscrowKeypairReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: RetrieveAgentEscrowKeypairReply =
            rkyv::from_bytes::<RetrieveAgentEscrowKeypairReply, rkyv::rancor::Error>(&bytes)
                .unwrap();
        assert_eq!(original, restored_rkyv);
    }

    #[test]
    fn retrieve_agent_escrow_keypair_reply_refused_round_trips_with_correct_reason() {
        let original = RetrieveAgentEscrowKeypairReply::Refused {
            reason: RetrieveAgentEscrowKeypairRefusalReason::AnonymousPublisherNotAllowed,
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: RetrieveAgentEscrowKeypairReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: RetrieveAgentEscrowKeypairReply =
            rkyv::from_bytes::<RetrieveAgentEscrowKeypairReply, rkyv::rancor::Error>(&bytes)
                .unwrap();
        assert_eq!(original, restored_rkyv);
    }

    #[test]
    fn retrieve_agent_escrow_keypair_reply_failed_round_trips_through_both() {
        let original = RetrieveAgentEscrowKeypairReply::Failed {
            detail: "ledger append failed".to_string(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: RetrieveAgentEscrowKeypairReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: RetrieveAgentEscrowKeypairReply =
            rkyv::from_bytes::<RetrieveAgentEscrowKeypairReply, rkyv::rancor::Error>(&bytes)
                .unwrap();
        assert_eq!(original, restored_rkyv);
    }
}
