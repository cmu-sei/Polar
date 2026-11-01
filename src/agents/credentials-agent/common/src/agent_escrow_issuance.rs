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

//! The third issuance lifecycle -- automated agent escrow-identity
//! issuance, per credentials-agent-architecture.md's own "Automated
//! agent escrow-identity issuance" section. Genuinely distinct from
//! IssuanceRequest/IssuanceReply (issuance.rs), not a reuse of that
//! shape: this flow only ever accepts a Kubernetes credential (an
//! agent's own service identity, never a human's IdP token), issues
//! a CertType::Service certificate (not CertType::Client), and its
//! own refusal reasons reflect that -- no token-introspection
//! failure mode exists here at all, matching how the existing
//! Kubernetes-credential path in issuance.rs already skips
//! introspection entirely.

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// Published (wrapped in `polar::request_reply::Envelope`) by a custodian to
/// `credentials-agent-escrow-issuance-requests`, for its workload, when nothing is escrowed for it yet: the escrow key
/// is made in the custody daemon, and its CSR names the workload. Answered only to the workload's custodian, within its
/// window (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian"). There is no token: the custodian's
/// certificate is what its pod's token bought.
#[derive(Debug, Clone, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
#[serde(deny_unknown_fields)]
pub struct AgentEscrowIssuanceRequest {
    pub csr_pem: String,
}

/// Published to `credentials-agent-escrow-issuance-replies`, wrapped
/// in an `Envelope` carrying the original request's `correlation_id`.
#[derive(Debug, Clone, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
pub enum AgentEscrowIssuanceReply {
    Issued {
        certificate_pem: String,
        ca_chain_pem: String,
        /// The Credentials Agent's current escrow public key --
        /// bundled automatically for the same reason
        /// IssuanceReply::Issued already does this: the agent
        /// receiving this certificate is about to immediately escrow
        /// its own, newly-generated private key (the
        /// issue-then-escrow bootstrap sequence), and genuinely needs
        /// this key to do so, without a separate round-trip.
        escrow_public_key_pem: String,
    },
    Refused {
        reason: AgentEscrowIssuanceRefusalReason,
    },
}

/// Why a *well-formed* agent escrow-identity issuance request was
/// refused. Deliberately its own enum, not a reuse of
/// IssuanceRefusalReason -- this flow's own failure modes are
/// genuinely different, not a subset or superset of the human-issuance
/// ones.
#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub enum AgentEscrowIssuanceRefusalReason {
    /// The Kubernetes credential itself failed validation (signature,
    /// issuer, audience, expiry -- the existing, already-tested
    /// multi-issuer oidc.rs logic).
    CredentialInvalid,
    /// This flow only ever accepts a genuine Kubernetes credential --
    /// an agent's own service identity, never a human's IdP token.
    /// Receiving anything classified otherwise here is a real
    /// anomaly worth a clear, explicit refusal, not a silent
    /// misroute or a fallback attempt to treat it as one anyway.
    NotAKubernetesCredential,
    /// This identity already holds a currently valid, unrevoked
    /// service certificate. Scoped to identity alone, not
    /// (identity, instance_binding) the way the human-issuance
    /// renewal-prevention check is -- a service certificate isn't
    /// pod-scoped at all; it's meant to be one durable identity for
    /// the service as a whole, so a second, competing service
    /// certificate for the same identity would break the "at most
    /// one non-revoked entry per identity" property this system's
    /// certificate-lookup query already relies on.
    AlreadyHasValidServiceCertificate,
    /// The requester is not a custodian the ledger holds a live certificate for, or its certificate does not name the
    /// escrow key.
    NotACustodian,
    /// A custodian's request after its window: its authority is spent.
    CustodianWindowClosed,
    /// Everything up through credential validation succeeded, but
    /// cert-issuer's own /issue call itself failed (CA unavailable,
    /// an internal signing error).
    SigningFailed,
    /// Another pod of the workload is being issued its escrow key's certificate, within its custodian's window: the first
    /// escrow issuance of an identity is serialized, one key per workload shared by its instances
    /// (docs/architecture/authority-model.md, "Multi-instance workloads"). Not final: the custodian waits and asks for
    /// the key again, which it retrieves once the other pod has escrowed it. Last, so no earlier reason's tag moves.
    BeingIssued,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> AgentEscrowIssuanceRequest {
        AgentEscrowIssuanceRequest {
            csr_pem: "-----BEGIN CERTIFICATE REQUEST-----\n...\n-----END CERTIFICATE REQUEST-----"
                .to_string(),
        }
    }

    #[test]
    fn agent_escrow_issuance_request_round_trips_through_serde_json() {
        let original = sample_request();
        let json = serde_json::to_string(&original).unwrap();
        let restored: AgentEscrowIssuanceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(original.csr_pem, restored.csr_pem);
    }

    #[test]
    fn agent_escrow_issuance_request_round_trips_through_rkyv() {
        let original = sample_request();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: AgentEscrowIssuanceRequest =
            rkyv::from_bytes::<AgentEscrowIssuanceRequest, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original.csr_pem, restored.csr_pem);
    }

    #[test]
    fn agent_escrow_issuance_request_rejects_unknown_fields() {
        let json = r#"{
            "csr_pem": "...",
            "unexpected_field": "should not be allowed"
        }"#;
        let result: Result<AgentEscrowIssuanceRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn agent_escrow_issuance_request_rejects_missing_fields() {
        let json = r#"{}"#;
        let result: Result<AgentEscrowIssuanceRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn agent_escrow_issuance_reply_issued_round_trips_through_both() {
        let original = AgentEscrowIssuanceReply::Issued {
            certificate_pem: "cert".to_string(),
            ca_chain_pem: "chain".to_string(),
            escrow_public_key_pem: "escrow-key".to_string(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: AgentEscrowIssuanceReply = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            restored_json,
            AgentEscrowIssuanceReply::Issued { .. }
        ));

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: AgentEscrowIssuanceReply =
            rkyv::from_bytes::<AgentEscrowIssuanceReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert!(matches!(
            restored_rkyv,
            AgentEscrowIssuanceReply::Issued { .. }
        ));
    }

    #[test]
    fn agent_escrow_issuance_reply_refused_round_trips_with_correct_reason() {
        let original = AgentEscrowIssuanceReply::Refused {
            reason: AgentEscrowIssuanceRefusalReason::AlreadyHasValidServiceCertificate,
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored: AgentEscrowIssuanceReply = serde_json::from_str(&json).unwrap();
        match restored {
            AgentEscrowIssuanceReply::Refused { reason } => {
                assert_eq!(
                    reason,
                    AgentEscrowIssuanceRefusalReason::AlreadyHasValidServiceCertificate
                );
            }
            _ => panic!("expected Refused"),
        }
    }

    #[test]
    fn every_refusal_reason_round_trips_through_rkyv() {
        for reason in [
            AgentEscrowIssuanceRefusalReason::CredentialInvalid,
            AgentEscrowIssuanceRefusalReason::NotAKubernetesCredential,
            AgentEscrowIssuanceRefusalReason::AlreadyHasValidServiceCertificate,
            AgentEscrowIssuanceRefusalReason::SigningFailed,
            AgentEscrowIssuanceRefusalReason::NotACustodian,
            AgentEscrowIssuanceRefusalReason::CustodianWindowClosed,
            AgentEscrowIssuanceRefusalReason::BeingIssued,
        ] {
            let original = AgentEscrowIssuanceReply::Refused { reason };
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
                .unwrap()
                .to_vec();
            let restored: AgentEscrowIssuanceReply =
                rkyv::from_bytes::<AgentEscrowIssuanceReply, rkyv::rancor::Error>(&bytes).unwrap();
            assert!(matches!(restored, AgentEscrowIssuanceReply::Refused { .. }));
        }
    }
}
