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

//! The escrow public-key query -- core-to-core only, no observer
//! involvement, unlike issuance and identity-normalization. mTLS to
//! Cassini on this specific topic pair is the authentication; no
//! separate token check is needed, matching how CredAgentCon->>CertIssuer
//! calls elsewhere in this codebase are already trusted without one.
//!
//! Named distinctly from identity.rs's own IdentityNormalizationRequest
//! deliberately -- "Identity" was already claimed by that flow (what
//! SAN should my CSR carry), and this answers a genuinely different
//! question (what's your current escrow public key). The load-bearing
//! reason this exists at all: any service that wants to escrow a
//! secret to this agent must encrypt it to this key first, which
//! means it needs a way to ask for the key before it can do that.
//!
//! No failure mode to represent -- the escrow keypair always exists
//! once this agent is running (EscrowKeypair::load_or_generate
//! guarantees it at startup), so unlike issuance or
//! identity-normalization, there's no refusal variant on the reply.

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// Genuinely empty -- nothing to validate, nothing to check. Any
/// authenticated core service asking on this topic gets the key.
#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub struct EscrowKeyRequest;

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub struct EscrowKeyReply {
    pub public_key_pem: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escrow_key_request_round_trips_through_serde_json() {
        let original = EscrowKeyRequest;
        let json = serde_json::to_string(&original).unwrap();
        let restored: EscrowKeyRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn escrow_key_request_round_trips_through_rkyv() {
        let original = EscrowKeyRequest;
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: EscrowKeyRequest =
            rkyv::from_bytes::<EscrowKeyRequest, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn escrow_key_reply_round_trips_through_both() {
        let original = EscrowKeyReply {
            public_key_pem: "-----BEGIN PUBLIC KEY-----\nfake\n-----END PUBLIC KEY-----\n"
                .to_string(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: EscrowKeyReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: EscrowKeyReply =
            rkyv::from_bytes::<EscrowKeyReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored_rkyv);
    }
}
