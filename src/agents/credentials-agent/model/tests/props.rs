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

//! The authority model's invariants, over the sandbox's real model and generated sets of allowed requests
//! (docs/architecture/authority-model.md, slice 5).

use credentials_agent_model::{Allowed, Broken, Model, Pair, allowed_under, compare, evaluate};
use proptest::prelude::*;
use std::collections::BTreeMap;

const FIXTURE: &str = include_str!("fixtures/sandbox-model.json");

fn model() -> Model {
    Model::parse(FIXTURE).unwrap()
}

/// Everything the floor allows when no policy speaks, in a fixed order, for the generators to take from.
fn everything(m: &Model) -> Vec<(String, String, polar_agent_builder::Access)> {
    let no_policy = Pair {
        policy: None,
        memberships: BTreeMap::new(),
        hierarchy: Default::default(),
    };
    allowed_under(m, &no_policy, &[])
        .unwrap()
        .into_iter()
        .collect()
}

/// What is taken away: some requests by position, and, often, the observer's renewal request, so that the agents the
/// credentials agent issues stop living and the paths through them stop holding.
#[derive(Debug, Clone)]
struct Removed {
    at: Vec<usize>,
    renewal: bool,
}

/// A subset of `all`: each kept unless taken away.
fn without(all: &[(String, String, polar_agent_builder::Access)], removed: &Removed) -> Allowed {
    let renewal = (
        "credentials-agent-observer.polar.serviceaccount.cluster.local".to_string(),
        credentials_agent_common::CREDENTIALS_WORKLOAD_ISSUANCE_REQUESTS_TOPIC.to_string(),
        polar_agent_builder::Access::Publish,
    );
    all.iter()
        .enumerate()
        .filter(|(i, _)| !removed.at.iter().any(|r| r % all.len() == *i))
        .filter(|(_, a)| !(removed.renewal && **a == renewal))
        .map(|(_, a)| a.clone())
        .collect()
}

fn removals() -> impl Strategy<Value = Removed> {
    (
        prop::collection::vec(any::<usize>(), 0..12),
        prop::bool::weighted(0.4),
    )
        .prop_map(|(at, renewal)| Removed { at, renewal })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Allowing more never breaks a goal that held: the rules are monotone in what is allowed.
    #[test]
    fn allowing_more_never_breaks_a_goal_that_held(first in removals(), more in removals()) {
        let m = model();
        let all = everything(&m);
        let larger = without(&all, &first);
        let mut both = first.clone();
        both.at.extend(more.at);
        both.renewal |= more.renewal;
        let smaller = without(&all, &both);
        let small = evaluate(&m, &smaller);
        let large = evaluate(&m, &larger);
        prop_assert_eq!(small.goals.len(), large.goals.len());
        for (s, l) in small.goals.iter().zip(&large.goals) {
            prop_assert_eq!(&s.goal, &l.goal);
            prop_assert!(!s.held || l.held, "{:?} held with less and not with more", s.goal);
        }
    }

    /// Every witness is a path of edges: from where its goal starts, each step an edge of the evaluation that held it.
    #[test]
    fn every_witness_is_a_path_of_edges(removed in removals()) {
        let m = model();
        let e = evaluate(&m, &without(&everything(&m), &removed));
        for goal in &e.goals {
            match (&goal.witness, goal.held) {
                (Some(path), true) => {
                    prop_assert!(path.len() >= 2);
                    for step in path.windows(2) {
                        prop_assert!(e.held.contains(&(step[0].clone(), step[1].clone())), "{:?} is no held edge", step);
                    }
                }
                (None, true) => prop_assert!(false, "{:?} held with no witness", goal.goal),
                (_, false) => {}
            }
        }
        if let Some(path) = &e.renewal_witness {
            for step in path.windows(2) {
                prop_assert!(e.edges.contains(&(step[0].clone(), step[1].clone())));
            }
        }
    }

    /// The comparison refuses exactly when a goal held under the current pair and not under the proposed one, and each
    /// reason names a request the current pair allows and the proposed one does not.
    #[test]
    fn the_comparison_refuses_exactly_when_a_goal_is_lost(now in removals(), then in removals()) {
        let m = model();
        let all = everything(&m);
        let (current, proposed) = (without(&all, &now), without(&all, &then));
        let (c, p) = (evaluate(&m, &current), evaluate(&m, &proposed));
        let j = compare(&m, &c, &p, &current, &proposed);
        let lost = c.goals.iter().zip(&p.goals).any(|(a, b)| a.held && !b.held);
        prop_assert_eq!(j.refuses(), lost);
        for loss in &j.losses {
            let mut broken = &loss.broken;
            while let Broken::NotRenewed { because, .. } = broken {
                broken = because;
            }
            if let Broken::Forbidden { identity, topic, access } = broken {
                let request = (identity.clone(), topic.clone(), *access);
                prop_assert!(current.contains(&request) && !proposed.contains(&request), "{:?}", request);
            }
        }
    }

    /// The evaluation is a function of the facts, not of the order the model lists them in.
    #[test]
    fn the_order_of_the_facts_changes_nothing(seed in any::<u64>(), removed in removals()) {
        let m = model();
        let allowed = without(&everything(&m), &removed);
        let mut shuffled = m.clone();
        let mut s = seed;
        let mut next = || { s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (s >> 33) as usize };
        for i in (1..shuffled.workloads.len()).rev() { let j = next() % (i + 1); shuffled.workloads.swap(i, j); }
        for w in &mut shuffled.workloads {
            for i in (1..w.topics.len()).rev() { let j = next() % (i + 1); w.topics.swap(i, j); }
        }
        let a = evaluate(&m, &allowed);
        let b = evaluate(&shuffled, &allowed);
        prop_assert_eq!(a.goals, b.goals);
        prop_assert_eq!(a.renewal_witness, b.renewal_witness);
    }
}
