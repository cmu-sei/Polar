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

//! Signed admin actions. The signature covers `payload` only: the
//! rkyv bytes of an `AdminActionBody`. The consumer stamps the
//! verified author, the time and (for provisioning) the account UUID
//! when it records the event; none of them are ever caller-supplied.

use crate::events::{GrantReason, RevocationReason};
use cert_issuer_common::capability::Capability;
use cert_issuer_common::permission::Permission;
use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use sha2::{Digest, Sha256};

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub enum AdminAction {
    ProvisionAccount {
        username: String,
        email: String,
        permissions: Vec<Permission>,
        capabilities: Vec<Capability>,
        reason: GrantReason,
        given_name: Option<String>,
        surname: Option<String>,
        title: Option<String>,
        telephone_number: Option<String>,
    },
    Grant {
        target_identity: String,
        permissions: Vec<Permission>,
        reason: GrantReason,
    },
    Revoke {
        serial: String,
        reason: RevocationReason,
    },
    /// Cedar policy text. The credentials agent parses and validates it.
    PolicyUpdate { document: String },
}

/// What is actually signed. The nonce makes two otherwise-identical
/// actions (grant, revoke, grant again) distinct, so hash-based replay
/// refusal never blocks a legitimate repeat.
#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub struct AdminActionBody {
    pub nonce: String,
    pub action: AdminAction,
}

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub struct SignedAdminAction {
    /// rkyv bytes of an `AdminActionBody`. Kept as raw bytes so the
    /// signature never depends on re-serialization.
    pub payload: Vec<u8>,
    /// Serial of the author's certificate, looked up in the ledger.
    pub author_serial: String,
    pub signature: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum AdminActionError {
    #[error("failed to encode admin action: {0}")]
    Encode(String),
    #[error("failed to decode admin action payload: {0}")]
    Decode(String),
}

impl AdminActionBody {
    pub fn to_payload(&self) -> Result<Vec<u8>, AdminActionError> {
        rkyv::to_bytes::<rkyv::rancor::Error>(self)
            .map(|b| b.to_vec())
            .map_err(|e| AdminActionError::Encode(e.to_string()))
    }
}

impl SignedAdminAction {
    pub fn decode_body(&self) -> Result<AdminActionBody, AdminActionError> {
        rkyv::from_bytes::<AdminActionBody, rkyv::rancor::Error>(&self.payload)
            .map_err(|e| AdminActionError::Decode(e.to_string()))
    }

    /// Hex SHA-256 of the payload bytes: what the ledger records to
    /// refuse a replay.
    pub fn payload_hash(&self) -> String {
        Sha256::digest(&self.payload)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

impl AdminAction {
    /// The one capability an author must hold for this action.
    pub fn required_capability(&self) -> Capability {
        match self {
            AdminAction::ProvisionAccount { .. } => Capability::ProvisionAccount,
            AdminAction::Grant { .. } => Capability::Grant,
            AdminAction::Revoke { .. } => Capability::Revoke,
            AdminAction::PolicyUpdate { .. } => Capability::UpdatePolicy,
        }
    }
}

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub enum AdminActionReply {
    /// Verified and recorded on the ledger.
    Recorded,
    Refused {
        reason: AdminActionRefusal,
    },
    /// The action was valid, but a real infrastructure failure (the
    /// ledger append) prevented recording it. Retrying may succeed.
    Failed {
        detail: String,
    },
}

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub enum AdminActionRefusal {
    /// No account certificate with the claimed serial is on the ledger.
    UnknownAuthor,
    AuthorRevoked,
    BadSignature,
    MalformedPayload,
    MissingCapability {
        required: Capability,
    },
    /// This exact signed payload was already recorded.
    Replay,
    /// The username already belongs to an account with a valid certificate.
    AccountAlreadyExists,
    /// The username is provisioned and waiting to be claimed.
    AccountAlreadyPending,
    /// Recorded actions of this kind have no effect yet (policy updates).
    NotYetSupported,
    /// The username belongs to the system (the bootstrap account) and can
    /// never be provisioned by an admin action.
    ReservedUsername,
    /// The bootstrap certificate has already authorised its one
    /// provisioning. Further admins must be provisioned by that admin.
    BootstrapCertificateSpent,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::GrantReason;

    fn body(nonce: &str) -> AdminActionBody {
        AdminActionBody {
            nonce: nonce.to_string(),
            action: AdminAction::Revoke {
                serial: "abc123".to_string(),
                reason: RevocationReason::Compromised,
            },
        }
    }

    fn signed(body: &AdminActionBody) -> SignedAdminAction {
        SignedAdminAction {
            payload: body.to_payload().unwrap(),
            author_serial: "01".to_string(),
            signature: vec![1, 2, 3],
        }
    }

    #[test]
    fn body_round_trips_through_the_signed_payload() {
        let original = body("n1");
        assert_eq!(signed(&original).decode_body().unwrap(), original);
    }

    #[test]
    fn signed_action_round_trips_through_rkyv_and_json() {
        let original = signed(&body("n1"));
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let from_rkyv = rkyv::from_bytes::<SignedAdminAction, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(from_rkyv, original);
        let from_json: SignedAdminAction =
            serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
        assert_eq!(from_json, original);
    }

    #[test]
    fn identical_actions_with_different_nonces_have_different_hashes() {
        assert_ne!(
            signed(&body("n1")).payload_hash(),
            signed(&body("n2")).payload_hash()
        );
    }

    #[test]
    fn same_payload_hashes_the_same() {
        assert_eq!(
            signed(&body("n1")).payload_hash(),
            signed(&body("n1")).payload_hash()
        );
    }

    #[test]
    fn garbage_payload_is_a_decode_error_not_a_panic() {
        let bad = SignedAdminAction {
            payload: vec![9, 9, 9],
            author_serial: String::new(),
            signature: vec![],
        };
        assert!(bad.decode_body().is_err());
    }

    #[test]
    fn every_action_requires_its_own_capability() {
        assert_eq!(
            AdminAction::Revoke {
                serial: "s".to_string(),
                reason: RevocationReason::Compromised
            }
            .required_capability(),
            Capability::Revoke
        );
        assert_eq!(
            AdminAction::PolicyUpdate {
                document: String::new()
            }
            .required_capability(),
            Capability::UpdatePolicy
        );
        assert_eq!(
            AdminAction::Grant {
                target_identity: "x".to_string(),
                permissions: vec![],
                reason: GrantReason::UserOnboarding
            }
            .required_capability(),
            Capability::Grant
        );
    }
}
