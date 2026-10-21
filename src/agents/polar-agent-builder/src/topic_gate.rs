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

//! The check each publish and subscribe passes before it leaves the agent: the agent's own policy first, then its
//! topic set, the same rule the broker applies (docs/architecture/policy-enforcement.md, "Decisions"). A `permit`
//! admits a topic outside the set, a `forbid` refuses one inside it, and where no policy speaks, or there is none
//! yet, the set decides. A policy that cannot be consulted refuses: nothing here fails open. The Cassini client is
//! handed it as a function (`cassini_client::facade::TopicGate`) and decides nothing itself.

use crate::authority::{LookupError, PolicyLookup};
use crate::policy_authority::entity;
use crate::topics::{Access, TopicSet};
use cassini_client::facade::{TopicGate, TopicUse};
use polar_policy::Decision;
use std::sync::Arc;

/// What the agent's policy said about one use of a topic, as the gate reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PolicySaid {
    /// No policy actor is running yet.
    NotRunning,
    /// It answered.
    Decided(Decision),
    /// It is running and has no rules.
    NoPolicyLoaded,
    /// It could not be consulted.
    Unavailable(String),
}

/// The rule, on its own: whether an agent may use `topic` this way, given what policy said and whether the topic
/// is in the agent's set.
pub(crate) fn rule(
    said: &PolicySaid,
    in_set: bool,
    topic: &str,
    access: Access,
) -> Result<(), String> {
    let floor = || {
        if in_set {
            Ok(())
        } else {
            Err(format!(
                "this agent does not {} on `{topic}`: it is not among its declared or implied topics, and no policy \
                 permits it",
                access.as_str()
            ))
        }
    };
    match said {
        PolicySaid::Decided(Decision::Allow) => Ok(()),
        PolicySaid::Decided(Decision::Deny) => Err(format!(
            "policy forbids this agent to {} on `{topic}`",
            access.as_str()
        )),
        PolicySaid::Decided(Decision::NotDetermined)
        | PolicySaid::NoPolicyLoaded
        | PolicySaid::NotRunning => floor(),
        PolicySaid::Unavailable(why) => Err(format!(
            "policy could not be consulted about `{topic}` ({why}), so nothing is sent"
        )),
    }
}

/// Asks the agent's policy, as `identity`, about using `topic` this way: `Action::"publish"` or
/// `Action::"subscribe"` on `Topic::"<topic>"`.
pub(crate) async fn ask(
    lookup: &PolicyLookup,
    identity: &str,
    certificate_pem: Option<String>,
    topic: &str,
    access: Access,
    facts: polar_policy::TopicFacts,
) -> PolicySaid {
    if lookup.actor().is_none() {
        return PolicySaid::NotRunning;
    }
    let (Some(action), Some(resource)) =
        (entity("Action", access.as_str()), entity("Topic", topic))
    else {
        return PolicySaid::Unavailable(format!(
            "`{topic}` is not a name policy can be asked about"
        ));
    };
    match lookup
        .decide(
            identity,
            certificate_pem,
            &action,
            &resource,
            polar_policy::Facts {
                topic: Some(facts),
                ..Default::default()
            },
        )
        .await
    {
        Ok(decision) => PolicySaid::Decided(decision),
        Err(LookupError::NoPolicyLoaded) => PolicySaid::NoPolicyLoaded,
        Err(LookupError::Unavailable(why)) => PolicySaid::Unavailable(why),
    }
}

/// The gate for an agent named `service`, whose certificate's identity is `identity` and whose topics are `set`.
/// `certificate_pem` is its own certificate, read when it started, so the groups it carries count (its groups do not
/// change when it is renewed: they are the catalog's).
pub(crate) fn gate(
    service: &str,
    identity: String,
    certificate_pem: Option<String>,
    set: Arc<TopicSet>,
) -> TopicGate {
    let lookup = PolicyLookup::for_service(service);
    Arc::new(move |use_, topic: String| {
        let lookup = lookup.clone();
        let identity = identity.clone();
        let certificate_pem = certificate_pem.clone();
        let set = set.clone();
        Box::pin(async move {
            let access = match use_ {
                TopicUse::Publish => Access::Publish,
                TopicUse::Subscribe => Access::Subscribe,
            };
            let said = ask(
                &lookup,
                &identity,
                certificate_pem,
                &topic,
                access,
                set.facts(&topic, access),
            )
            .await;
            rule(&said, set.allows(&topic, access), &topic, access)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topics::{Facts, TopicDecl};
    use proptest::prelude::*;
    use ractor::Actor;

    const IDENTITY: &str = "kube-observer.polar.serviceaccount.cluster.local";

    fn set() -> Arc<TopicSet> {
        Arc::new(TopicSet::compute(
            &Facts {
                name: "kube-observer",
                cassini_client: true,
                self_test: true,
                encryption: false,
            },
            &[TopicDecl::publish(
                "kubernetes.cluster.consumer.Pod",
                "what it sees",
            )],
        ))
    }

    /// A policy actor registered for `service`, holding `source` (no rules when it is empty).
    async fn policy_for(service: &str, source: &str) {
        let (permissions, _) = Actor::spawn(None, polar_permissions::PermissionsActor, ())
            .await
            .unwrap();
        let (policy, _) = Actor::spawn(
            Some(format!("{service}.policy")),
            polar_policy::PolicyActor,
            polar_policy::PolicyActorArgs {
                permissions_actor: permissions,
            },
        )
        .await
        .unwrap();
        if !source.is_empty() {
            ractor::call!(policy, |reply| polar_policy::PolicyMessage::UpdatePolicy {
                policy_source: source.to_string(),
                reply,
            })
            .unwrap()
            .unwrap();
        }
    }

    async fn uses(service: &str, use_: TopicUse, topic: &str) -> Result<(), String> {
        gate(service, IDENTITY.to_string(), None, set())(use_, topic.to_string()).await
    }

    #[tokio::test]
    async fn with_no_policy_yet_the_set_decides() {
        let service = format!("gate-none-{}", rand::random::<u64>());
        assert!(
            uses(
                &service,
                TopicUse::Publish,
                "kubernetes.cluster.consumer.Pod"
            )
            .await
            .is_ok()
        );
        let said = uses(&service, TopicUse::Publish, "somebody.elses.topic")
            .await
            .unwrap_err();
        assert!(
            said.contains("not among its declared or implied topics"),
            "{said}"
        );
        // Declared to publish is not declared to subscribe.
        assert!(
            uses(
                &service,
                TopicUse::Subscribe,
                "kubernetes.cluster.consumer.Pod"
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn a_policy_with_no_rules_or_silent_on_topics_leaves_it_to_the_set() {
        let empty = format!("gate-empty-{}", rand::random::<u64>());
        policy_for(&empty, "").await;
        assert!(
            uses(&empty, TopicUse::Publish, "kubernetes.cluster.consumer.Pod")
                .await
                .is_ok()
        );
        assert!(
            uses(&empty, TopicUse::Publish, "somebody.elses.topic")
                .await
                .is_err()
        );

        // The seeded policy speaks only of retrieving secrets.
        let seeded = format!("gate-seeded-{}", rand::random::<u64>());
        policy_for(
            &seeded,
            include_str!("../../../../infra/seeds/signed/policy.cedar"),
        )
        .await;
        assert!(
            uses(
                &seeded,
                TopicUse::Publish,
                "kubernetes.cluster.consumer.Pod"
            )
            .await
            .is_ok()
        );
        assert!(
            uses(&seeded, TopicUse::Publish, "somebody.elses.topic")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn policy_is_told_whether_the_topic_is_the_agent_s_own() {
        // A policy can keep every agent to what it declared, whatever else it permits.
        let service = format!("gate-facts-{}", rand::random::<u64>());
        policy_for(
            &service,
            r#"forbid (principal, action == Action::"publish", resource) unless { context.declared || context.implied };"#,
        )
        .await;
        assert!(
            uses(
                &service,
                TopicUse::Publish,
                "kubernetes.cluster.consumer.Pod"
            )
            .await
            .is_ok()
        );
        let said = uses(&service, TopicUse::Publish, "somebody.elses.topic")
            .await
            .unwrap_err();
        assert!(said.contains("policy forbids"), "{said}");
    }

    #[tokio::test]
    async fn the_groups_the_agent_s_certificate_carries_count_in_its_own_decisions() {
        let service = format!("gate-groups-{}", rand::random::<u64>());
        policy_for(
            &service,
            r#"forbid (principal, action == Action::"publish", resource) unless { principal in Group::"cassini-client" };"#,
        )
        .await;
        let certificate = crate::own_name::tests::certificate_with(&[IDENTITY, "g:cassini-client"]);
        let with = gate(&service, IDENTITY.to_string(), Some(certificate), set());
        assert!(
            with(TopicUse::Publish, "kubernetes.cluster.consumer.Pod".into())
                .await
                .is_ok()
        );
        let without = gate(&service, IDENTITY.to_string(), None, set());
        assert!(
            without(TopicUse::Publish, "kubernetes.cluster.consumer.Pod".into())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn policy_overrides_the_set_both_ways() {
        let service = format!("gate-policy-{}", rand::random::<u64>());
        policy_for(
            &service,
            &format!(
                r#"permit (principal == Identity::"{IDENTITY}", action == Action::"publish", resource == Topic::"agent.new-workload.configuration");
                   forbid (principal == Identity::"{IDENTITY}", action == Action::"publish", resource == Topic::"kubernetes.cluster.consumer.Pod");"#
            ),
        )
        .await;
        // A permit admits what the set does not hold, and a forbid refuses what it does.
        assert!(
            uses(
                &service,
                TopicUse::Publish,
                "agent.new-workload.configuration"
            )
            .await
            .is_ok()
        );
        let said = uses(
            &service,
            TopicUse::Publish,
            "kubernetes.cluster.consumer.Pod",
        )
        .await
        .unwrap_err();
        assert!(said.contains("policy forbids"), "{said}");
        // Where it says nothing, the set decides.
        assert!(
            uses(&service, TopicUse::Subscribe, "polar-authority-updates")
                .await
                .is_ok()
        );
        assert!(
            uses(&service, TopicUse::Publish, "somebody.elses.topic")
                .await
                .is_err()
        );
    }

    fn said() -> impl Strategy<Value = PolicySaid> {
        prop_oneof![
            Just(PolicySaid::NotRunning),
            Just(PolicySaid::NoPolicyLoaded),
            Just(PolicySaid::Decided(Decision::Allow)),
            Just(PolicySaid::Decided(Decision::Deny)),
            Just(PolicySaid::Decided(Decision::NotDetermined)),
            "[a-z ]{0,12}".prop_map(PolicySaid::Unavailable),
        ]
    }

    proptest! {
        /// Policy first, then the set: an answer decides, silence or no policy leaves it to the set, and no answer
        /// at all refuses, whatever the topic and whichever way it is used.
        #[test]
        fn the_decision_follows_the_rule(
            said in said(),
            in_set in any::<bool>(),
            topic in "[a-z.:-]{1,20}",
            publish in any::<bool>(),
        ) {
            let access = if publish { Access::Publish } else { Access::Subscribe };
            let admitted = rule(&said, in_set, &topic, access).is_ok();
            let expected = match &said {
                PolicySaid::Decided(Decision::Allow) => true,
                PolicySaid::Decided(Decision::Deny) | PolicySaid::Unavailable(_) => false,
                _ => in_set,
            };
            prop_assert_eq!(admitted, expected);
            if let Err(reason) = rule(&said, in_set, &topic, access) {
                prop_assert!(reason.contains(&topic));
                // The client re-registers on this phrase from the broker; a refusal must never carry it.
                prop_assert!(!reason.contains("session mismatch"));
            }
        }
    }
}
