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

//! The client-driven identity-normalization query: given a raw
//! credential, what SAN should the CSR built from it carry, so it
//! matches exactly what cert-issuer's own /issue will check via
//! verify_identity. A separate, simpler request/reply pair from
//! issuance -- no introspection, no grant lookup, no signing, just a
//! thin wrapper around cert-issuer's /validate with
//! include_normalized_identity: true.

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// The observer's HTTP request body for `POST /identity`, and (wrapped
/// in `Envelope`) what it publishes to
/// `credentials-identity-normalization-requests`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityNormalizationRequest {
    pub credential: String,
}

/// Published to `credentials-identity-normalization-replies`, wrapped
/// in an `Envelope` carrying the original request's `correlation_id`.
#[derive(Debug, Clone, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
pub enum IdentityNormalizationReply {
    Normalized { identity: String },
    Refused {
        reason: IdentityNormalizationRefusalReason,
    },
}

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub enum IdentityNormalizationRefusalReason {
    /// cert-issuer's /validate rejected the credential outright, for
    /// any reason -- this flow does no introspection, grant lookup,
    /// or signing of its own to fail at a finer granularity than
    /// that.
    TokenInvalid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_normalization_request_round_trips_through_serde_json() {
        let original = IdentityNormalizationRequest {
            credential: "some-token".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: IdentityNormalizationRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn identity_normalization_request_round_trips_through_rkyv() {
        let original = IdentityNormalizationRequest {
            credential: "some-token".to_string(),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: IdentityNormalizationRequest =
            rkyv::from_bytes::<IdentityNormalizationRequest, rkyv::rancor::Error>(&bytes)
                .unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn identity_normalization_request_rejects_unknown_fields() {
        let json = r#"{"credential": "...", "unexpected_field": "..."}"#;
        let result: Result<IdentityNormalizationRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn normalized_reply_round_trips_through_both() {
        let original = IdentityNormalizationReply::Normalized {
            identity: "testuser.polar.keycloak-issuer".to_string(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: IdentityNormalizationReply = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            restored_json,
            IdentityNormalizationReply::Normalized { .. }
        ));

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: IdentityNormalizationReply =
            rkyv::from_bytes::<IdentityNormalizationReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert!(matches!(
            restored_rkyv,
            IdentityNormalizationReply::Normalized { .. }
        ));
    }

    #[test]
    fn refused_reply_round_trips_with_correct_reason() {
        let original = IdentityNormalizationReply::Refused {
            reason: IdentityNormalizationRefusalReason::TokenInvalid,
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: IdentityNormalizationReply = serde_json::from_str(&json).unwrap();
        match restored {
            IdentityNormalizationReply::Refused { reason } => {
                assert_eq!(reason, IdentityNormalizationRefusalReason::TokenInvalid);
            }
            _ => panic!("expected Refused"),
        }
    }
}
