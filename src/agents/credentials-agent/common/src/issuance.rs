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

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// The observer's HTTP request body, and (wrapped in
/// `polar::request_reply::Envelope`) what it publishes to
/// `credentials-issuance-requests` after well-formedness checking.
///
/// Shared by both real callers of this shape -- user self-service
/// issuance (a real IdP access token) and admin bootstrap (a
/// kubectl-admin-backed Kubernetes credential) -- per "one mechanism,
/// both flows": the observer forwards either blindly, never
/// distinguishing which kind of credential it's holding. `credential`
/// is named generically rather than `token` for exactly this reason.
///
/// `deny_unknown_fields`: an unexpected field in the incoming JSON is
/// rejected outright at deserialization, not silently ignored --
/// this is external, untrusted input, and its exact expected shape is
/// known and finite.
#[derive(Debug, Clone, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
#[serde(deny_unknown_fields)]
pub struct IssuanceRequest {
    pub csr_pem: String,
    pub credential: String,
}

/// Published to `credentials-issuance-replies`, wrapped in an
/// `Envelope` carrying the original request's `correlation_id`.
#[derive(Debug, Clone, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
pub enum IssuanceReply {
    Issued {
        certificate_pem: String,
        ca_chain_pem: String,
        /// The Credentials Agent's current escrow public key, PEM
        /// (SubjectPublicKeyInfo) encoded -- bundled automatically so
        /// the caller doesn't need a separate identity-key round-trip
        /// for the common "just got my cert, also need this" case.
        escrow_public_key_pem: String,
    },
    Refused {
        reason: IssuanceRefusalReason,
    },
}

/// Why a *well-formed* issuance request was refused -- distinct from
/// `credentials_agent_common::observer::ObserverRejection`, which
/// covers requests the observer itself rejects synchronously, before
/// anything reaches Cassini or gets a `correlation_id` at all. Every
/// variant here corresponds to a real trust/authorization decision
/// made on the trusted side.
///
/// Shared by both user self-service issuance and admin bootstrap, per
/// credentials-agent-architecture.md's decision not to give these two
/// flows separate types despite one real asymmetry: `NoGrantFound` can
/// never fire for admin bootstrap, which has no grant lookup at all --
/// only whether the Kubernetes credential itself validates. Accepted
/// as-is rather than split into two enums for one context-dependent
/// variant.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
pub enum IssuanceRefusalReason {
    TokenInvalid,
    TokenIntrospectionFailed,
    NoGrantFound,
    /// The pod-cannot-request-its-own-renewal case -- deliberately its
    /// own variant, not folded into a generic "denied," given how much
    /// weight credentials-agent-architecture.md places on this being a
    /// specific, auditable, security-relevant property.
    AlreadyHasValidCertificate,
    /// Everything up through the grant/renewal checks succeeded, but
    /// cert-issuer's own /issue call itself failed (CA unavailable, an
    /// internal signing error). Distinct from the other variants,
    /// which all cover failures before ever reaching cert-issuer.
    SigningFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> IssuanceRequest {
        IssuanceRequest {
            csr_pem: "-----BEGIN CERTIFICATE REQUEST-----\n...\n-----END CERTIFICATE REQUEST-----".to_string(),
            credential: "some-token-or-k8s-credential".to_string(),
        }
    }

    #[test]
    fn issuance_request_round_trips_through_serde_json() {
        let original = sample_request();
        let json = serde_json::to_string(&original).unwrap();
        let restored: IssuanceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(original.csr_pem, restored.csr_pem);
        assert_eq!(original.credential, restored.credential);
    }

    #[test]
    fn issuance_request_round_trips_through_rkyv() {
        let original = sample_request();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: IssuanceRequest =
            rkyv::from_bytes::<IssuanceRequest, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original.csr_pem, restored.csr_pem);
        assert_eq!(original.credential, restored.credential);
    }

    #[test]
    fn issuance_request_rejects_unknown_fields() {
        // The real security property this exists for: an unexpected
        // field in external, untrusted input must be rejected outright,
        // not silently ignored.
        let json = r#"{
            "csr_pem": "...",
            "credential": "...",
            "unexpected_field": "should not be allowed"
        }"#;
        let result: Result<IssuanceRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn issuance_request_rejects_missing_fields() {
        let json = r#"{ "csr_pem": "only-one-field" }"#;
        let result: Result<IssuanceRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn issuance_reply_issued_round_trips_through_both() {
        let original = IssuanceReply::Issued {
            certificate_pem: "cert".to_string(),
            ca_chain_pem: "chain".to_string(),
            escrow_public_key_pem: "escrow-key".to_string(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: IssuanceReply = serde_json::from_str(&json).unwrap();
        assert!(matches!(restored_json, IssuanceReply::Issued { .. }));

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: IssuanceReply =
            rkyv::from_bytes::<IssuanceReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert!(matches!(restored_rkyv, IssuanceReply::Issued { .. }));
    }

    #[test]
    fn issuance_reply_refused_round_trips_with_correct_reason() {
        let original = IssuanceReply::Refused {
            reason: IssuanceRefusalReason::AlreadyHasValidCertificate,
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored: IssuanceReply = serde_json::from_str(&json).unwrap();
        match restored {
            IssuanceReply::Refused { reason } => {
                assert_eq!(reason, IssuanceRefusalReason::AlreadyHasValidCertificate);
            }
            _ => panic!("expected Refused"),
        }
    }
}
