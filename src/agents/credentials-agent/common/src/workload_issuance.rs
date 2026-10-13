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

//! A workload asking the credentials agent for its pod-instance certificates before it has any certificate:
//! the one place a service-account token must cross the pod network, and so the one place it must not cross in
//! the clear (docs/architecture/cert-issuer-isolation.md, "Scope: polar-init through the observer").
//!
//! It is an ephemeral session, not TLS and not another certificate:
//!
//! 1. The workload asks for a session ([`WorkloadIssuanceRequest::OpenSession`]). The credentials agent makes a
//!    fresh X25519 keypair, keeps the secret half for a short time, and answers with the public half
//!    ([`SessionOffer`]).
//! 2. The workload seals its service-account token to that public half with
//!    [`EphemeralEncryptedPayload::encrypt`](crate::ephemeral_exchange::EphemeralEncryptedPayload::encrypt) and
//!    sends the sealed request ([`SealedIssuanceRequest`]) naming the session.
//! 3. The credentials agent opens the session once, takes the token out, and goes on exactly as it does for an
//!    unsealed request ([`IssuanceRequest`](crate::issuance::IssuanceRequest)); the reply is the usual one.
//!
//! The observer relays both messages and understands neither: it never holds the token. The session ends at the
//! credentials agent's consumer. Everything that is public stays outside the seal (the CSR, the type, the names),
//! so the observer can still turn away a CSR that is not a CSR. This protects the token from anyone who can only
//! watch the network; it does not authenticate the credentials agent to the workload, which is a separate
//! question this does not claim to settle.

use crate::ephemeral_exchange::EphemeralEncryptedPayload;
use crate::issuance::{IssuanceReply, WorkloadCertType};
use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// What the observer relays on `credentials-workload-issuance-requests`, inside an `Envelope`.
#[derive(Debug, Clone, Deserialize, Serialize, Archive)]
pub enum WorkloadIssuanceRequest {
    /// Open a session to seal a token to.
    OpenSession,
    /// Ask for a certificate with the token sealed to an open session.
    Issue(SealedIssuanceRequest),
}

/// A request for a certificate whose credential is sealed to a session the credentials agent opened.
///
/// `deny_unknown_fields`: this is external, untrusted input with a known, finite shape.
#[derive(Debug, Clone, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
#[serde(deny_unknown_fields)]
pub struct SealedIssuanceRequest {
    /// The session the credential is sealed to, from [`SessionOffer`]. Single use.
    pub session_id: String,
    pub csr_pem: String,
    #[serde(default)]
    pub cert_type: WorkloadCertType,
    #[serde(default)]
    pub names: Vec<String>,
    /// The service-account token, sealed to the session's public key.
    pub sealed_credential: EphemeralEncryptedPayload,
}

/// A session the credentials agent has opened: the public half of a fresh X25519 keypair, to seal to, and the
/// name to send back with the sealed request.
#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub struct SessionOffer {
    pub session_id: String,
    pub public_key: [u8; 32],
    /// How long the session is kept, in seconds. A request that arrives later is told the session is unknown.
    pub valid_for_secs: u64,
}

/// What comes back on `credentials-workload-issuance-replies`, inside an `Envelope`.
#[derive(Debug, Clone, Deserialize, Serialize, Archive)]
pub enum WorkloadIssuanceReply {
    Offer(SessionOffer),
    /// The answer to a sealed request: the usual issuance reply.
    Issuance(IssuanceReply),
    /// The session is not there: expired, already used, or never opened. The workload opens another.
    UnknownSession,
    /// Too many sessions are open at once. The workload tries again shortly.
    Busy,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ephemeral_exchange::EphemeralSession;
    use crate::issuance::IssuanceRefusalReason;

    fn sealed_request() -> SealedIssuanceRequest {
        let session = EphemeralSession::open();
        SealedIssuanceRequest {
            session_id: "abc".to_string(),
            csr_pem: "-----BEGIN CERTIFICATE REQUEST-----\n...\n-----END CERTIFICATE REQUEST-----"
                .to_string(),
            cert_type: WorkloadCertType::Server,
            names: vec!["cassini-ip-svc.polar.svc.cluster.local".to_string()],
            sealed_credential: EphemeralEncryptedPayload::encrypt(b"token", &session.public_key())
                .unwrap(),
        }
    }

    #[test]
    fn a_sealed_request_survives_json_and_rkyv() {
        let original = sealed_request();
        let json = serde_json::to_string(&original).unwrap();
        let from_json: SealedIssuanceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(from_json.session_id, "abc");
        assert_eq!(from_json.cert_type, WorkloadCertType::Server);
        assert_eq!(from_json.sealed_credential, original.sealed_credential);

        let request = WorkloadIssuanceRequest::Issue(original.clone());
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request)
            .unwrap()
            .to_vec();
        let back =
            rkyv::from_bytes::<WorkloadIssuanceRequest, rkyv::rancor::Error>(&bytes).unwrap();
        match back {
            WorkloadIssuanceRequest::Issue(r) => {
                assert_eq!(r.sealed_credential, original.sealed_credential);
                assert_eq!(r.names, original.names);
            }
            other => panic!("expected Issue, got {other:?}"),
        }
    }

    #[test]
    fn a_sealed_request_never_carries_the_token_in_the_clear() {
        let json = serde_json::to_string(&sealed_request()).unwrap();
        assert!(!json.contains("token\""), "{json}");
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&sealed_request()).unwrap();
        assert!(!bytes.windows(5).any(|w| w == b"token"));
    }

    #[test]
    fn a_sealed_request_refuses_unknown_fields_and_the_escrow_type() {
        let mut value = serde_json::to_value(sealed_request()).unwrap();
        value["credential"] = serde_json::json!("a clear token, smuggled in beside the sealed one");
        assert!(serde_json::from_value::<SealedIssuanceRequest>(value).is_err());
        let mut value = serde_json::to_value(sealed_request()).unwrap();
        value["cert_type"] = serde_json::json!("service");
        assert!(serde_json::from_value::<SealedIssuanceRequest>(value).is_err());
        // The type and names may be left out, as on the unsealed request: a client certificate.
        let mut value = serde_json::to_value(sealed_request()).unwrap();
        value.as_object_mut().unwrap().remove("cert_type");
        value.as_object_mut().unwrap().remove("names");
        let r: SealedIssuanceRequest = serde_json::from_value(value).unwrap();
        assert_eq!(r.cert_type, WorkloadCertType::Client);
        assert!(r.names.is_empty());
    }

    #[test]
    fn an_offer_and_every_reply_round_trip() {
        let offer = SessionOffer {
            session_id: "abc".to_string(),
            public_key: EphemeralSession::open().public_key(),
            valid_for_secs: 60,
        };
        let json = serde_json::to_string(&offer).unwrap();
        assert_eq!(serde_json::from_str::<SessionOffer>(&json).unwrap(), offer);

        for reply in [
            WorkloadIssuanceReply::Offer(offer.clone()),
            WorkloadIssuanceReply::Issuance(IssuanceReply::Refused {
                reason: IssuanceRefusalReason::TokenInvalid,
            }),
            WorkloadIssuanceReply::UnknownSession,
            WorkloadIssuanceReply::Busy,
        ] {
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&reply)
                .unwrap()
                .to_vec();
            let back =
                rkyv::from_bytes::<WorkloadIssuanceReply, rkyv::rancor::Error>(&bytes).unwrap();
            assert_eq!(format!("{back:?}"), format!("{reply:?}"));
        }
    }

    #[test]
    fn a_requester_can_seal_to_an_offer_and_the_session_opens_it() {
        // The whole exchange at the type level, as the two ends will do it.
        let session = EphemeralSession::open();
        let offer = SessionOffer {
            session_id: "s1".to_string(),
            public_key: session.public_key(),
            valid_for_secs: 60,
        };
        let sealed = EphemeralEncryptedPayload::encrypt(b"the token", &offer.public_key).unwrap();
        assert_eq!(session.unseal(&sealed).unwrap().as_slice(), b"the token");
    }
}
