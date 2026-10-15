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

//! The certificate-lookup query -- core-to-core only, no observer
//! involvement, matching escrow_key.rs's own trust model: mTLS to
//! Cassini on this specific topic pair is the authentication, no
//! separate token check needed.
//!
//! The load-bearing reason this exists: an agent that wants to
//! encrypt a payload directly to another agent (rather than relying
//! on the Credentials Agent's own escrow relay) needs that agent's
//! current, real public certificate first -- this is how it gets one,
//! without needing its own out-of-band copy.
//!
//! Unlike escrow_key.rs's own request/reply pair, a real failure mode
//! exists here -- the identity asked about may never have been issued
//! a certificate at all, or its only one may have been revoked. Found
//! carries the actual signed PEM, sourced from
//! CredentialsState::certificate_for (see state.rs), which already
//! guarantees at most one non-revoked entry can exist per identity.

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub struct CertificateRequest {
    pub identity: String,
}

#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub enum CertificateReply {
    Found {
        certificate_pem: String,
    },
    /// Never issued, or its only certificate has since been revoked
    /// -- these two cases are deliberately not distinguished here.
    /// The caller only ever needs "do I have a usable cert to encrypt
    /// to right now", not the specific reason one isn't available;
    /// that distinction already lives in the event log and graph for
    /// anyone actually auditing why.
    NotFound,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_request_round_trips_through_serde_json() {
        let original = CertificateRequest {
            identity: "jdoe.polar.keycloak-issuer".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: CertificateRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn certificate_request_round_trips_through_rkyv() {
        let original = CertificateRequest {
            identity: "jdoe.polar.keycloak-issuer".to_string(),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: CertificateRequest =
            rkyv::from_bytes::<CertificateRequest, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn certificate_reply_found_round_trips_through_both() {
        let original = CertificateReply::Found {
            certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                .to_string(),
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: CertificateReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: CertificateReply =
            rkyv::from_bytes::<CertificateReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored_rkyv);
    }

    #[test]
    fn certificate_reply_not_found_round_trips_through_both() {
        let original = CertificateReply::NotFound;

        let json = serde_json::to_string(&original).unwrap();
        let restored_json: CertificateReply = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored_json);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored_rkyv: CertificateReply =
            rkyv::from_bytes::<CertificateReply, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored_rkyv);
    }
}
