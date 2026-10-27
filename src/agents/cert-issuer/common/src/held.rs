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

//! Held requests (docs/architecture/core-restarts.md, "3 in detail"): a bootstrap service's token-only request that
//! the floor admits and the floor state does not is held by cert-issuer, offered to the credentials agent, and signed
//! if it vouches. These are the words both sides share, over Cassini and over mTLS alike.
//!
//! What the credentials agent is told is what cert-issuer validated, never the token.

use crate::CertType;
use serde::{Deserialize, Serialize};

/// cert-issuer offers each held request on this topic, to the credentials agent.
pub const HELD_REQUESTS_TOPIC: &str = "cert-issuer-held-requests";
/// The credentials agent answers on this topic; cert-issuer takes an answer only the broker says it published.
pub const HELD_REPLIES_TOPIC: &str = "cert-issuer-held-replies";
/// The route the credentials agent lists held requests on over mTLS while its Cassini session is lost.
pub const HELD_ROUTE: &str = "/internal/held";

/// A held request, as the credentials agent is told of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldOffer {
    /// The name to answer it by.
    pub id: String,
    /// The workload as its token names it (`system:serviceaccount:<namespace>:<name>`).
    pub workload: String,
    /// The identity its certificate would carry, in DNS form.
    pub identity: String,
    /// The pod its token is bound to.
    pub instance_binding: Option<String>,
    pub cert_type: CertType,
    /// The names it asked for beyond its identity.
    pub names: Vec<String>,
    pub csr_pem: String,
}

/// The credentials agent's answer to one held request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum HeldDecision {
    /// Sign it.
    Vouch,
    /// Do not, ever: the request is refused, and asking again gets the refusal.
    Refuse { reason: String },
}

/// An answer, naming the held request it is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldReply {
    pub id: String,
    pub decision: HeldDecision,
}

/// The held requests the credentials agent has not answered, over mTLS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldList {
    pub held: Vec<HeldOffer>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offer_and_an_answer_survive_the_json_they_travel_as_and_refuse_what_they_do_not_know() {
        let offer = HeldOffer {
            id: "h1".to_string(),
            workload: "system:serviceaccount:polar:cassini".to_string(),
            identity: "cassini.polar.serviceaccount.cluster.local".to_string(),
            instance_binding: Some("pod-1".to_string()),
            cert_type: CertType::Server,
            names: vec!["cassini-ip-svc.polar.svc.cluster.local".to_string()],
            csr_pem: "csr".to_string(),
        };
        let back: HeldOffer =
            serde_json::from_str(&serde_json::to_string(&offer).unwrap()).unwrap();
        assert_eq!(back, offer);
        for decision in [
            HeldDecision::Vouch,
            HeldDecision::Refuse {
                reason: "already holds one".to_string(),
            },
        ] {
            let reply = HeldReply {
                id: "h1".to_string(),
                decision,
            };
            let back: HeldReply =
                serde_json::from_str(&serde_json::to_string(&reply).unwrap()).unwrap();
            assert_eq!(back, reply);
        }
        let mut value = serde_json::to_value(&offer).unwrap();
        value["token"] = serde_json::json!("never carried");
        assert!(serde_json::from_value::<HeldOffer>(value).is_err());
    }
}
