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

//! Requests to the credentials agent (docs/architecture/authority-model.md, slice 9): every request on one of its
//! request topics is judged by Cedar, `Action::"request"` on `Request::"<topic>"`, and where policy is silent the floor
//! admits the topic's own publishers, by any origin. The rule is here once, for the credentials agent's request gate and
//! for the judge, so the two cannot disagree.

use crate::facts::Model;
use polar_agent_builder::topic_gate::PolicySaid;
use polar_agent_builder::topics::{Access, Origin};
use polar_policy::TopicFacts;

/// What the authority judges a requester for: the three Cedar actions on a topic or a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    Publish,
    Subscribe,
    Request,
}

impl Action {
    /// Its name in Cedar, `Action::"<name>"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Publish => "publish",
            Action::Subscribe => "subscribe",
            Action::Request => "request",
        }
    }

    /// The entity type of what it acts on: `Topic`, or `Request`.
    pub fn resource_type(self) -> &'static str {
        match self {
            Action::Publish | Action::Subscribe => "Topic",
            Action::Request => "Request",
        }
    }
}

impl From<Access> for Action {
    fn from(access: Access) -> Self {
        match access {
            Access::Publish => Action::Publish,
            Access::Subscribe => Action::Subscribe,
        }
    }
}

/// Whether `topic` is one the credentials agent serves requests on.
pub fn is_request(topic: &str) -> bool {
    credentials_agent_common::REQUEST_TOPICS.contains(&topic)
}

/// How `identity` publishes `topic` in `model`: its workload's origin for it, or none (a person, or a workload that does
/// not publish it).
pub fn publishes<'a>(model: &'a Model, identity: &str, topic: &str) -> Option<&'a Origin> {
    model
        .workloads
        .iter()
        .find(|w| w.identity == identity)?
        .topics
        .iter()
        .find(|t| t.name == topic && t.access == Access::Publish)
        .map(|t| &t.origin)
}

/// Whether `identity` is a custodian and `topic` one it publishes: a custodian is no catalog workload, and what it may
/// publish is its identity's (`credentials_agent_common::CUSTODIAN_PUBLISHES`; docs/architecture/key-custody.md,
/// "Part A's bootstrap: the custodian"). Its requests are judged as implied ones.
pub fn custodian_publishes(identity: &str, topic: &str) -> bool {
    cert_issuer_common::custody::custodian_of(identity).is_some()
        && credentials_agent_common::CUSTODIAN_PUBLISHES.contains(&topic)
}

/// What Cedar is told of an origin: `context.declared` (family topics included) and `context.implied`.
pub fn facts_of(origin: Option<&Origin>) -> TopicFacts {
    TopicFacts {
        declared: matches!(
            origin,
            Some(Origin::Declared { .. } | Origin::Family { .. })
        ),
        implied: matches!(origin, Some(Origin::Implied { .. })),
    }
}

/// Whether a request on `topic` is admitted, given what policy said and whether the requester publishes the topic:
/// Cedar first; where it is silent, or no policy is loaded, the floor, which admits the topic's own publishers; and a
/// policy that cannot be consulted refuses.
pub fn admitted(said: &PolicySaid, publishes: bool, topic: &str) -> Result<(), String> {
    let floor = || {
        if publishes {
            Ok(())
        } else {
            Err(format!(
                "the requester does not publish `{topic}` in the catalog, and no policy permits its request"
            ))
        }
    };
    match said {
        PolicySaid::Decided(polar_policy::Decision::Allow) => Ok(()),
        PolicySaid::Decided(polar_policy::Decision::Deny) => Err(format!(
            "policy forbids the requester's request on `{topic}`"
        )),
        PolicySaid::Decided(polar_policy::Decision::NotDetermined)
        | PolicySaid::NoPolicyLoaded
        | PolicySaid::NotRunning => floor(),
        PolicySaid::Unavailable(why) => Err(format!(
            "policy could not be consulted about a request on `{topic}` ({why}), so it is refused"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar_policy::Decision;

    fn every_answer() -> Vec<PolicySaid> {
        vec![
            PolicySaid::NotRunning,
            PolicySaid::Decided(Decision::Allow),
            PolicySaid::Decided(Decision::Deny),
            PolicySaid::Decided(Decision::NotDetermined),
            PolicySaid::NoPolicyLoaded,
            PolicySaid::Unavailable("down".to_string()),
        ]
    }

    /// Every answer policy can give, with the requester a publisher of the topic or not: Cedar decides when it speaks,
    /// the floor (the topic's publishers) when it is silent, and no answer is never a grant.
    #[test]
    fn a_request_is_cedar_s_when_it_speaks_the_floor_s_when_it_is_silent_and_refused_when_it_cannot_be_asked()
     {
        for said in every_answer() {
            for publishes in [true, false] {
                let expected = match &said {
                    PolicySaid::Decided(Decision::Allow) => true,
                    PolicySaid::Decided(Decision::Deny) | PolicySaid::Unavailable(_) => false,
                    PolicySaid::Decided(Decision::NotDetermined)
                    | PolicySaid::NoPolicyLoaded
                    | PolicySaid::NotRunning => publishes,
                };
                let got = admitted(&said, publishes, "credentials-issuance-requests");
                assert_eq!(
                    got.is_ok(),
                    expected,
                    "{said:?}, publishes {publishes}: {got:?}"
                );
                if let Err(why) = got {
                    assert!(why.contains("credentials-issuance-requests"), "{why}");
                }
            }
        }
    }

    #[test]
    fn a_refusal_says_why() {
        let silent = PolicySaid::Decided(Decision::NotDetermined);
        assert!(
            admitted(&silent, false, "t")
                .unwrap_err()
                .contains("does not publish")
        );
        let deny = PolicySaid::Decided(Decision::Deny);
        assert!(
            admitted(&deny, true, "t")
                .unwrap_err()
                .contains("policy forbids")
        );
        let down = PolicySaid::Unavailable("down".to_string());
        assert!(
            admitted(&down, true, "t")
                .unwrap_err()
                .contains("could not be consulted")
        );
    }

    #[test]
    fn the_actions_are_cedar_s_and_act_on_their_own_resources() {
        assert_eq!(Action::from(Access::Publish), Action::Publish);
        assert_eq!(Action::from(Access::Subscribe), Action::Subscribe);
        let named: Vec<_> = [Action::Publish, Action::Subscribe, Action::Request]
            .iter()
            .map(|a| (a.as_str(), a.resource_type()))
            .collect();
        assert_eq!(
            named,
            [
                ("publish", "Topic"),
                ("subscribe", "Topic"),
                ("request", "Request")
            ]
        );
    }

    /// Over the sandbox's real model: how each requester publishes a request's topic, and what Cedar is told of it.
    #[test]
    fn a_requester_publishes_a_topic_by_its_workload_s_own_origin_and_cedar_is_told_which() {
        let model = Model::parse(include_str!("../tests/fixtures/sandbox-model.json")).unwrap();
        let id = |w: &str| format!("{w}.polar.serviceaccount.cluster.local");
        let workload_issuance =
            credentials_agent_common::CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC;
        let store = credentials_agent_common::CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC;
        let replies = credentials_agent_common::CREDENTIALS_WORKLOAD_ISSUANCE_REPLIES_TOPIC;

        let declared = publishes(&model, &id("credentials-agent-observer"), workload_issuance);
        assert!(
            matches!(declared, Some(Origin::Declared { .. })),
            "{declared:?}"
        );
        assert_eq!(
            facts_of(declared),
            TopicFacts {
                declared: true,
                implied: false
            }
        );

        let implied = publishes(&model, &id("kube-consumer"), store);
        assert!(
            matches!(implied, Some(Origin::Implied { .. })),
            "{implied:?}"
        );
        assert_eq!(
            facts_of(implied),
            TopicFacts {
                declared: false,
                implied: true
            }
        );

        // It publishes neither the other's topic nor what it only hears.
        assert_eq!(
            publishes(&model, &id("credentials-agent-observer"), store),
            None
        );
        assert_eq!(
            publishes(&model, &id("credentials-agent-observer"), replies),
            None
        );
        // Not a workload: a person, or a name that is no one's.
        assert_eq!(
            publishes(
                &model,
                "4f0c1d7e-0000-4000-8000-000000000001",
                workload_issuance
            ),
            None
        );
        assert_eq!(facts_of(None), TopicFacts::default());
    }

    #[test]
    fn the_request_topics_are_the_credentials_agent_s() {
        for topic in credentials_agent_common::REQUEST_TOPICS {
            assert!(is_request(topic), "{topic}");
        }
        assert!(!is_request(
            credentials_agent_common::CREDENTIALS_ISSUANCE_REPLIES_TOPIC
        ));
        assert!(!is_request("polar-authority-updates"));
    }
}
