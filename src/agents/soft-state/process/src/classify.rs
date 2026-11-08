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

//! What a round found, for each expected workload (docs/architecture/soft-state.md, "Revised by the maintainer's
//! review"): answering, with its report; unresponsive, asked and silent until the round closed (the observable the
//! Reaper needs); or unreachable, named by the signed configuration but outside the family the writer was deployed
//! with, so it cannot be asked at all (drift, reported).

use crate::expected::{Entry, Expected};
use polar_health::HealthReport;
use std::collections::BTreeMap;

/// What a round found for one workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    Answering(HealthReport),
    Unresponsive,
    Unreachable,
}

impl Observation {
    /// The state a transition records: the report's status for an answer, else what the round found.
    pub fn state(&self) -> &'static str {
        match self {
            Observation::Answering(report) => report.status.as_str(),
            Observation::Unresponsive => "unresponsive",
            Observation::Unreachable => "unreachable",
        }
    }

    /// The reason a transition records: the report's, for an answer.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Observation::Answering(report) => report.reason.map(|r| r.as_str()),
            _ => None,
        }
    }
}

/// The workloads a round asks: each expected one whose question topic is in the deployed family (`reachable`).
pub fn asked(expected: &Expected, reachable: impl Fn(&str) -> bool) -> Vec<&Entry> {
    expected
        .workloads
        .iter()
        .filter(|entry| reachable(&entry.workload))
        .collect()
}

/// What a round found for each expected workload, in the configuration's order: unreachable when it is outside the
/// deployed family; answering when an answer came stamped with its identity exactly; unresponsive otherwise. An answer
/// from any other identity counts for nothing.
pub fn classify(
    expected: &Expected,
    reachable: impl Fn(&str) -> bool,
    answers: &BTreeMap<String, HealthReport>,
) -> Vec<(Entry, Observation)> {
    expected
        .workloads
        .iter()
        .map(|entry| {
            let observation = if !reachable(&entry.workload) {
                Observation::Unreachable
            } else {
                match answers.get(&entry.identity) {
                    Some(report) => Observation::Answering(report.clone()),
                    None => Observation::Unresponsive,
                }
            };
            (entry.clone(), observation)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar_health::{HealthCounters, HealthPhase, ReportStatus};
    use proptest::prelude::*;
    use std::collections::BTreeSet;

    fn id(workload: &str) -> String {
        format!("{workload}.polar.serviceaccount.cluster.local")
    }

    fn entry(workload: &str) -> Entry {
        Entry {
            workload: workload.to_string(),
            identity: id(workload),
        }
    }

    fn report(status: ReportStatus) -> HealthReport {
        HealthReport {
            status,
            phase: HealthPhase::Normal,
            reason: None,
            message: None,
            cassini_connected: true,
            graph_connected: false,
            expects_graph: false,
            counters: HealthCounters::default(),
            uptime_secs: 5,
        }
    }

    #[test]
    fn each_expected_workload_is_answering_unresponsive_or_unreachable() {
        let expected = Expected {
            workloads: vec![entry("a"), entry("b"), entry("gone")],
        };
        let reachable = |w: &str| w != "gone";
        let answers = BTreeMap::from([
            (id("a"), report(ReportStatus::Healthy)),
            // Another namespace's workload of the same name is not it.
            (
                "b.elsewhere.serviceaccount.cluster.local".to_string(),
                report(ReportStatus::Healthy),
            ),
        ]);
        let found = classify(&expected, reachable, &answers);
        assert_eq!(
            found
                .iter()
                .map(|(e, o)| (e.workload.as_str(), o.state()))
                .collect::<Vec<_>>(),
            [
                ("a", "healthy"),
                ("b", "unresponsive"),
                ("gone", "unreachable")
            ]
        );
        assert_eq!(
            asked(&expected, reachable)
                .iter()
                .map(|e| e.workload.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    /// Up to eight workloads, each expected or not, reachable or not, answering (with a status) or not, and some
    /// answers from identities nobody expects.
    fn round() -> impl Strategy<Value = (Expected, BTreeSet<String>, BTreeMap<String, HealthReport>)>
    {
        let status = prop_oneof![
            Just(ReportStatus::Starting),
            Just(ReportStatus::Healthy),
            Just(ReportStatus::Degraded),
            Just(ReportStatus::Unhealthy),
            Just(ReportStatus::Draining),
        ];
        (
            proptest::collection::btree_set("[a-z]{1,6}", 0..8),
            proptest::collection::vec(any::<bool>(), 8),
            proptest::collection::vec(proptest::option::of(status.clone()), 8),
            proptest::collection::vec(("[a-z]{1,6}", status), 0..4),
        )
            .prop_map(|(names, reachable, answering, strangers)| {
                let names: Vec<String> = names.into_iter().collect();
                let expected = Expected {
                    workloads: names.iter().map(|n| entry(n)).collect(),
                };
                let reachable: BTreeSet<String> = names
                    .iter()
                    .zip(&reachable)
                    .filter(|(_, r)| **r)
                    .map(|(n, _)| n.clone())
                    .collect();
                let mut answers: BTreeMap<String, HealthReport> = names
                    .iter()
                    .zip(&answering)
                    .filter_map(|(n, s)| s.map(|s| (id(n), report(s))))
                    .collect();
                for (name, s) in strangers {
                    answers
                        .entry(format!("{name}.other.serviceaccount.cluster.local"))
                        .or_insert(report(s));
                }
                (expected, reachable, answers)
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// For any round: every expected workload is classified once, in order; unreachable exactly when it is outside
        /// the deployed family (and then it was never asked); otherwise answering, with the very report stamped with
        /// its identity, or unresponsive when none was; and answers from identities nobody expects change nothing.
        #[test]
        fn the_classification_follows_its_rule((expected, reachable, answers) in round()) {
            let is_reachable = |w: &str| reachable.contains(w);
            let found = classify(&expected, is_reachable, &answers);
            prop_assert_eq!(
                found.iter().map(|(e, _)| e.clone()).collect::<Vec<_>>(),
                expected.workloads.clone()
            );
            let asked: Vec<&Entry> = asked(&expected, is_reachable);
            for (entry, observation) in &found {
                let was_asked = asked.contains(&entry);
                match observation {
                    Observation::Unreachable => prop_assert!(!was_asked),
                    Observation::Answering(report) => {
                        prop_assert!(was_asked);
                        prop_assert_eq!(Some(report), answers.get(&entry.identity));
                    }
                    Observation::Unresponsive => {
                        prop_assert!(was_asked);
                        prop_assert!(!answers.contains_key(&entry.identity));
                    }
                }
            }
            let known: BTreeMap<String, HealthReport> = answers
                .iter()
                .filter(|(identity, _)| expected.by_identity(identity).is_some())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            prop_assert_eq!(classify(&expected, is_reachable, &known), found);
        }
    }
}
