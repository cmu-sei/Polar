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

//! The agent escrow-key query -- the third query in
//! credentials-agent-architecture.md's own "Escrow-key and
//! certificate-lookup queries" section. Genuinely distinct from
//! escrow_key.rs's own EscrowKeyRequest/Reply: that answers "what's
//! the Credentials Agent's own escrow public key" (a singleton, no
//! parameters needed at all); this answers "what's a *specific
//! agent's* own escrow public key" (an identity-keyed lookup that
//! might genuinely not exist yet, if that agent has never completed
//! automated agent escrow-identity issuance). Shaped like
//! certificate.rs's CertificateRequest/Reply for exactly that reason
//! -- both are identity-keyed lookups with a real "not found" case,
//! unlike the Credentials Agent's own, always-present escrow key.
//!
//! This is the key CryptoActor itself queries (and caches) on behalf
//! of the "encrypt and send" flow.

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
pub struct AgentEscrowKeyRequest {
    pub identity: String,
}

/// Deliberately not distinguishing "never issued" from "revoked" --
/// see CertificateReply::NotFound's own doc comment for the identical
/// reasoning; a caller only needs to know whether there's a usable
/// key to encrypt to right now.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
pub enum AgentEscrowKeyReply {
    Found { public_key_pem: String },
    NotFound,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_escrow_key_request_round_trips_through_serde_json() {
        let original = AgentEscrowKeyRequest {
            identity: "git-observer".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: AgentEscrowKeyRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn agent_escrow_key_request_round_trips_through_rkyv() {
        let original = AgentEscrowKeyRequest {
            identity: "git-observer".to_string(),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: AgentEscrowKeyRequest =
            rkyv::from_bytes::<AgentEscrowKeyRequest, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn agent_escrow_key_reply_found_round_trips_through_both() {
        let original = AgentEscrowKeyReply::Found {
            public_key_pem: "-----BEGIN PUBLIC KEY-----\nfake\n-----END PUBLIC KEY-----\n"
                .to_string(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: AgentEscrowKeyReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: AgentEscrowKeyReply =
            rkyv::from_bytes::<AgentEscrowKeyReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored_rkyv);
    }

    #[test]
    fn agent_escrow_key_reply_not_found_round_trips_through_both() {
        let original = AgentEscrowKeyReply::NotFound;

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: AgentEscrowKeyReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: AgentEscrowKeyReply =
            rkyv::from_bytes::<AgentEscrowKeyReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored_rkyv);
    }
}
