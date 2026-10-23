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

//! The authority model judged over the sandbox's real model (`fixtures/sandbox-model.json`, made by
//! `fixtures/regenerate.nu` from this workspace's agents): docs/architecture/authority-model.md, slice 5.

use cert_issuer_common::hierarchy::Hierarchy;
use credentials_agent_common::authority_update::{
    AUTHORITY_SNAPSHOT_REPLIES_TOPIC, AUTHORITY_SNAPSHOT_REQUESTS_TOPIC,
};
use credentials_agent_common::{
    CREDENTIALS_WORKLOAD_ISSUANCE_REPLIES_TOPIC, CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC,
};
use credentials_agent_model::{
    Action, Broken, Invariant, Model, Node, Pair, Person, allowed_under, evaluate, judge,
};
use polar_agent_builder::Access;
use std::collections::BTreeMap;

const FIXTURE: &str = include_str!("fixtures/sandbox-model.json");

fn model() -> Model {
    Model::parse(FIXTURE).unwrap()
}

fn id(workload: &str) -> String {
    format!("{workload}.polar.serviceaccount.cluster.local")
}

fn pair(policy: Option<&str>) -> Pair {
    Pair {
        policy: policy.map(str::to_string),
        memberships: BTreeMap::new(),
        hierarchy: Hierarchy::default(),
    }
}

fn node_out(w: &str, t: &str) -> Node {
    Node::Out(w.to_string(), t.to_string())
}

fn node_in(w: &str, t: &str) -> Node {
    Node::In(w.to_string(), t.to_string())
}

#[test]
fn with_no_policy_every_invariant_holds_and_renewal_is_the_observer_s_round_trip() {
    let m = model();
    let e = evaluate(&m, &allowed_under(&m, &pair(None), &[]).unwrap());
    assert!(e.not_applicable.is_empty(), "{:?}", e.not_applicable);
    assert!(!e.goals.is_empty());
    for goal in &e.goals {
        assert!(goal.held, "{goal:?}");
    }
    assert_eq!(
        e.renewal_witness,
        Some(vec![
            node_out(
                "credentials-agent-observer",
                CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC
            ),
            node_in(
                "credentials-agent",
                CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC
            ),
            node_out(
                "credentials-agent",
                CREDENTIALS_WORKLOAD_ISSUANCE_REPLIES_TOPIC
            ),
            node_in(
                "credentials-agent-observer",
                CREDENTIALS_WORKLOAD_ISSUANCE_REPLIES_TOPIC
            ),
        ])
    );
    // Every workload lives, the ones the credentials agent issues because renewal closes.
    assert_eq!(e.lives.len(), m.workloads.len());
    // Each agent's snapshot round trip is a goal of hearing authority.
    assert!(
        e.goals
            .iter()
            .any(|g| g.invariant == Invariant::HearingAuthority
                && g.describe().contains(AUTHORITY_SNAPSHOT_REQUESTS_TOPIC))
    );
    let _ = AUTHORITY_SNAPSHOT_REPLIES_TOPIC;
}

const FORBID_THE_OBSERVER: &str = r#"forbid (principal == Identity::"credentials-agent-observer.polar.serviceaccount.cluster.local", action == Action::"publish", resource == Topic::"credentials-workload-issuance-requests");"#;

#[test]
fn cutting_renewal_is_refused_as_breaking_correction_with_the_request_that_cuts_it() {
    let m = model();
    let j = judge(&m, &pair(None), &pair(Some(FORBID_THE_OBSERVER)), &[]).unwrap();
    assert!(j.refuses());
    let correction = j
        .losses
        .iter()
        .find(|l| l.invariant == Invariant::Correction)
        .expect("correction is lost");
    match &correction.broken {
        Broken::NotRenewed { workload, because } => {
            assert!(
                [
                    "polar-git-server",
                    "polar-scheduler",
                    "polar-scheduler-observer"
                ]
                .contains(&workload.as_str()),
                "{workload}"
            );
            assert_eq!(
                **because,
                Broken::Forbidden {
                    identity: id("credentials-agent-observer"),
                    topic: CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC.to_string(),
                    action: Action::Publish,
                }
            );
        }
        other => panic!("not lost to renewal: {other:?}"),
    }
    let said = correction.to_string();
    assert!(
        said.contains("renewal") && said.contains("credentials-workload-issuance-requests"),
        "{said}"
    );
    // It names the rule that denies the request: the policy's only one, Cedar's `policy0`.
    assert_eq!(correction.rules, ["policy0"]);
    assert!(said.contains("denied by policy0"), "{said}");
    // The effect says what was taken.
    assert!(j.effect.lost.contains(&(
        id("credentials-agent-observer"),
        CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC.to_string(),
        Action::Publish
    )));
}

/// docs/architecture/authority-model.md, slice 9: the credentials agent's request gate is a step on the path as the
/// broker is, so forbidding the observer's request cuts renewal as forbidding its publish does.
#[test]
fn forbidding_the_observer_s_issuance_request_is_refused_as_breaking_correction() {
    let m = model();
    let policy = format!(
        r#"forbid (principal == Identity::"{}", action == Action::"request", resource == Request::"{CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC}");"#,
        id("credentials-agent-observer")
    );
    let j = judge(&m, &pair(None), &pair(Some(&policy)), &[]).unwrap();
    assert!(j.refuses());
    let correction = j
        .losses
        .iter()
        .find(|l| l.invariant == Invariant::Correction)
        .expect("correction is lost");
    match &correction.broken {
        Broken::NotRenewed { because, .. } => assert_eq!(
            **because,
            Broken::Forbidden {
                identity: id("credentials-agent-observer"),
                topic: CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC.to_string(),
                action: Action::Request,
            }
        ),
        other => panic!("not lost to renewal: {other:?}"),
    }
    assert_eq!(correction.rules, ["policy0"]);
    let said = correction.to_string();
    assert!(said.contains("may no longer request"), "{said}");
    // The publish itself is untouched: only the request is taken.
    assert_eq!(
        j.effect.lost,
        [(
            id("credentials-agent-observer"),
            CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC.to_string(),
            Action::Request
        )]
    );
}

/// The rules named as denying a lost request are found with the requester's own context, for a topic and for a request: a
/// rule that applies only to what a workload is implied, or only to what it declares, is named.
#[test]
fn the_rules_named_for_a_loss_are_decided_with_the_requester_s_own_context() {
    let m = model();
    // kube-observer is implied its snapshot request, and declares its pods topic before it.
    let snapshot = format!(
        r#"forbid (principal == Identity::"{}", action == Action::"publish", resource == Topic::"{AUTHORITY_SNAPSHOT_REQUESTS_TOPIC}") when {{ context.implied }};"#,
        id("kube-observer")
    );
    let j = judge(&m, &pair(None), &pair(Some(&snapshot)), &[]).unwrap();
    let loss = j
        .losses
        .iter()
        .find(|l| l.invariant == Invariant::HearingAuthority)
        .expect("kube-observer's snapshot round trip is lost");
    assert_eq!(loss.rules, ["policy0"], "{loss}");
    // The observer declares its issuance request: a request's context is how it publishes the request's topic.
    let request = format!(
        r#"forbid (principal == Identity::"{}", action == Action::"request", resource == Request::"{CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC}") when {{ context.declared }};"#,
        id("credentials-agent-observer")
    );
    let j = judge(&m, &pair(None), &pair(Some(&request)), &[]).unwrap();
    let loss = j
        .losses
        .iter()
        .find(|l| l.invariant == Invariant::Correction)
        .expect("correction is lost");
    assert_eq!(loss.rules, ["policy0"], "{loss}");
}

/// With no policy the floor admits a request on each of the credentials agent's topics from the workloads that publish
/// it, by any origin, and from no one else: no person, and no other workload.
#[test]
fn with_no_policy_each_request_topic_s_own_publishers_and_no_one_else_may_request() {
    let m = model();
    let alice = Person {
        identity: "4f0c1d7e-0000-4000-8000-000000000001".to_string(),
        groups: vec!["operators".to_string()],
        capabilities: Vec::new(),
    };
    let allowed = allowed_under(&m, &pair(None), &[alice]).unwrap();
    let requests: Vec<(String, String)> = allowed
        .iter()
        .filter(|(_, _, a)| *a == Action::Request)
        .map(|(i, t, _)| (i.clone(), t.clone()))
        .collect();
    let mut expected: Vec<(String, String)> = m
        .workloads
        .iter()
        .flat_map(|w| {
            w.topics
                .iter()
                .filter(|t| {
                    t.access == Access::Publish
                        && credentials_agent_common::REQUEST_TOPICS.contains(&t.name.as_str())
                })
                .map(|t| (w.identity.clone(), t.name.clone()))
        })
        .collect();
    expected.sort();
    expected.dedup();
    assert_eq!(requests, expected);
    // The observer's six relays and the encryption agents' escrow and secret requests are all among them.
    assert!(requests.contains(&(
        id("credentials-agent-observer"),
        CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC.to_string()
    )));
    assert!(requests.contains(&(
        id("kube-consumer"),
        "credentials-store-secret-requests".to_string()
    )));
}

/// A permit that lets a person request what no topic of theirs holds is accepted, and its effect says so.
#[test]
fn a_permit_opening_a_request_is_accepted_with_its_effect() {
    let m = model();
    let alice = Person {
        identity: "4f0c1d7e-0000-4000-8000-000000000001".to_string(),
        groups: vec!["operators".to_string()],
        capabilities: Vec::new(),
    };
    let policy = r#"permit (principal in Group::"operators", action == Action::"request", resource == Request::"credentials-certificate-requests");"#;
    let j = judge(
        &m,
        &pair(None),
        &pair(Some(policy)),
        std::slice::from_ref(&alice),
    )
    .unwrap();
    assert!(!j.refuses(), "{:?}", j.losses);
    assert_eq!(
        j.effect.gained,
        [(
            alice.identity.clone(),
            "credentials-certificate-requests".to_string(),
            Action::Request
        )]
    );
}

#[test]
fn forbidding_the_scheduler_s_forward_breaks_correction_and_not_renewal() {
    let m = model();
    let policy = format!(
        r#"forbid (principal == Identity::"{}", action == Action::"publish", resource == Topic::"agent.credentials-agent.configuration");"#,
        id("polar-scheduler")
    );
    let j = judge(&m, &pair(None), &pair(Some(&policy)), &[]).unwrap();
    assert!(j.refuses());
    assert!(
        j.losses
            .iter()
            .all(|l| l.invariant == Invariant::Correction),
        "{:?}",
        j.losses
    );
    assert_eq!(
        j.losses[0].broken,
        Broken::Forbidden {
            identity: id("polar-scheduler"),
            topic: "agent.credentials-agent.configuration".to_string(),
            action: Action::Publish,
        }
    );
}

#[test]
fn a_groups_document_that_moves_the_scheduler_into_a_forbidden_group_is_refused() {
    let m = model();
    let policy =
        r#"forbid (principal in Group::"retired", action == Action::"publish", resource);"#;
    let current = pair(Some(policy));
    let mut proposed = pair(Some(policy));
    proposed
        .memberships
        .insert(id("polar-scheduler"), vec!["retired".to_string()]);
    // The forbid alone takes nothing: no one is in the group.
    assert!(!judge(&m, &pair(None), &current, &[]).unwrap().refuses());
    let j = judge(&m, &current, &proposed, &[]).unwrap();
    assert!(j.refuses());
    assert!(
        j.losses
            .iter()
            .any(|l| l.invariant == Invariant::Correction)
    );
}

#[test]
fn a_permit_that_takes_nothing_away_is_accepted_and_its_effect_said() {
    let m = model();
    let policy = format!(
        r#"permit (principal == Identity::"{}", action == Action::"subscribe", resource == Topic::"{CREDENTIALS_WORKLOAD_ISSUANCE_REPLIES_TOPIC}");"#,
        id("harness-sink")
    );
    let j = judge(&m, &pair(None), &pair(Some(&policy)), &[]).unwrap();
    assert!(!j.refuses(), "{:?}", j.losses);
    assert_eq!(
        j.effect.gained,
        [(
            id("harness-sink"),
            CREDENTIALS_WORKLOAD_ISSUANCE_REPLIES_TOPIC.to_string(),
            Action::Subscribe
        )]
    );
    assert!(j.effect.lost.is_empty());
}

#[test]
fn a_groups_key_that_names_no_known_identity_is_reported_not_refused() {
    let m = model();
    let mut proposed = pair(None);
    let typo = "polar-scheduler.polar.serviceaccount.cluster.locl";
    proposed.memberships.insert(typo.to_string(), vec![]);
    let j = judge(&m, &pair(None), &proposed, &[]).unwrap();
    assert!(!j.refuses());
    assert_eq!(j.unknown_keys, [typo]);
}

#[test]
fn judging_a_document_over_the_real_model_is_quick() {
    let m = model();
    let started = std::time::Instant::now();
    let j = judge(&m, &pair(None), &pair(Some(FORBID_THE_OBSERVER)), &[]).unwrap();
    let took = started.elapsed();
    eprintln!("judged over {} workloads in {took:?}", m.workloads.len());
    assert!(j.refuses());
    assert!(took < std::time::Duration::from_secs(10), "{took:?}");
}

/// The names a credentials-agent-issued workload's certificates carry are its declared and family topics and its groups;
/// its implied topics are not written, since they follow from its identity.
#[test]
fn a_workload_the_credentials_agent_issues_carries_its_declared_and_family_topics_and_its_groups() {
    let m = model();
    let scheduler = m.workload("polar-scheduler").unwrap();
    let names = m
        .issued_by_the_credentials_agent(&id("polar-scheduler"))
        .expect("the credentials agent issues the scheduler");
    let own = |access: Access| -> Vec<String> {
        let mut topics: Vec<String> = scheduler
            .topics
            .iter()
            .filter(|t| {
                t.access == access
                    && !matches!(
                        t.origin,
                        polar_agent_builder::topics::Origin::Implied { .. }
                    )
            })
            .map(|t| t.name.clone())
            .collect();
        topics.sort();
        topics.dedup();
        topics
    };
    assert_eq!(names.publish, own(Access::Publish));
    assert_eq!(names.subscribe, own(Access::Subscribe));
    assert!(
        names
            .publish
            .iter()
            .any(|t| t == "agent.credentials-agent.configuration"),
        "a family topic: {names:?}"
    );
    assert_eq!(names.groups, ["cassini-client", "self-test"]);
    // A use of a topic that only a rule implies is not written; the same topic used another way may be.
    let implied: Vec<String> = scheduler
        .topics
        .iter()
        .filter(|t| {
            matches!(
                t.origin,
                polar_agent_builder::topics::Origin::Implied { .. }
            )
        })
        .filter(|t| !own(t.access).contains(&t.name))
        .map(|t| {
            let access = if t.access == Access::Publish {
                "pub"
            } else {
                "sub"
            };
            format!("p:{access}:{}", t.name)
        })
        .collect();
    assert!(!implied.is_empty());
    let sans = names.san_strings();
    for name in &implied {
        assert!(!sans.contains(name), "{name} is implied: {sans:?}");
    }
    assert!(
        sans.contains(&"p:pub:agent.credentials-agent.configuration".to_string()),
        "{sans:?}"
    );
    assert!(sans.contains(&"g:self-test".to_string()), "{sans:?}");
}

#[test]
fn a_bootstrap_service_or_an_identity_the_model_does_not_know_is_not_issued_by_the_credentials_agent()
 {
    let m = model();
    assert_eq!(
        m.issued_by_the_credentials_agent(&id("credentials-agent")),
        None
    );
    assert_eq!(m.issued_by_the_credentials_agent(&id("nobody")), None);
    // The token's subject is not an identity.
    assert_eq!(
        m.issued_by_the_credentials_agent("system:serviceaccount:polar:polar-scheduler"),
        None
    );
}

/// The credentials agent issues a workload whose issuer is the credentials agent and which is no bootstrap service: one
/// still asking cert-issuer directly is in neither floor, and a bootstrap service is cert-issuer's, whatever else is said.
#[test]
fn only_a_workload_the_credentials_agent_issues_and_that_is_no_bootstrap_service_is_issued_by_it() {
    let workload = |name: &str, issuer: &str, bootstrap: bool| {
        serde_json::json!({
            "workload": name, "identity": id(name), "issuer": issuer, "bootstrap": bootstrap, "groups": ["g"],
            "topics": [{"name": "news", "access": "publish", "from": "declared", "purpose": "p"}]
        })
    };
    let m = Model::parse(
        &serde_json::json!({"format": 1, "workloads": [
            workload("issued", "credentials-agent", false),
            workload("direct", "cert-issuer", false),
            workload("booted", "credentials-agent", true),
        ]})
        .to_string(),
    )
    .unwrap();
    let issued = m
        .issued_by_the_credentials_agent(&id("issued"))
        .expect("issued");
    assert_eq!(issued.san_strings(), ["p:pub:news", "g:g"]);
    assert_eq!(m.issued_by_the_credentials_agent(&id("direct")), None);
    assert_eq!(m.issued_by_the_credentials_agent(&id("booted")), None);
}
