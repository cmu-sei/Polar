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
    Broken, Invariant, Model, Node, Pair, allowed_under, evaluate, judge,
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
                    access: Access::Publish,
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
        Access::Publish
    )));
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
            access: Access::Publish,
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
            Access::Subscribe
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
