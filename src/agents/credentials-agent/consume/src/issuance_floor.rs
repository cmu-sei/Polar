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

//! The credentials agent's issuance floor: for each workload it issues, the topics its certificates may carry as
//! `p:pub:` and `p:sub:` names (docs/architecture/policy-enforcement.md, stages 2 and 3). The render writes it from
//! the catalog, each workload's declared and family topics as its scaffold prints them (implied topics follow from the
//! identity and are not written), and applying the catalog is the administrator's sign-off of it. It is read once, at
//! start, and a document that cannot be read stops the agent.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One workload's entry: the workload as its token names it (`system:serviceaccount:<namespace>:<name>`), and its
/// declared topics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadTopics {
    pub workload: String,
    #[serde(default)]
    pub publish: Vec<String>,
    #[serde(default)]
    pub subscribe: Vec<String>,
}

/// The floor, by workload.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IssuanceFloor {
    by_workload: BTreeMap<String, WorkloadTopics>,
}

impl IssuanceFloor {
    /// Reads the floor as the render writes it: a JSON list of [`WorkloadTopics`]. Every problem is reported
    /// together: text that is not such a list, a workload with no name or listed twice, a topic that is not a name.
    pub fn parse(text: &str) -> Result<Self, Vec<String>> {
        let entries: Vec<WorkloadTopics> = serde_json::from_str(text).map_err(|e| {
            vec![format!(
                "the issuance floor is not a list of workloads and their topics: {e}"
            )]
        })?;
        let mut problems = Vec::new();
        let mut by_workload = BTreeMap::new();
        for entry in entries {
            if entry.workload.trim().is_empty() {
                problems.push("the issuance floor has an entry with no workload".to_string());
                continue;
            }
            for topic in entry.publish.iter().chain(&entry.subscribe) {
                if topic.trim().is_empty() || topic.contains(char::is_whitespace) {
                    problems.push(format!(
                        "the issuance floor gives `{}` `{topic}`, which is not a topic name",
                        entry.workload
                    ));
                }
            }
            let workload = entry.workload.clone();
            if by_workload.insert(workload.clone(), entry).is_some() {
                problems.push(format!(
                    "the issuance floor lists `{workload}` more than once"
                ));
            }
        }
        if problems.is_empty() {
            Ok(Self { by_workload })
        } else {
            Err(problems)
        }
    }

    /// The topics a workload's certificates may carry; none for a workload the floor does not name.
    pub fn topics_for(&self, workload: &str) -> Option<&WorkloadTopics> {
        self.by_workload.get(workload)
    }

    pub fn len(&self) -> usize {
        self.by_workload.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_workload.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn the_render_s_form_is_read_and_each_workload_found_by_its_token_name() {
        let floor = IssuanceFloor::parse(
            r#"[{"workload": "system:serviceaccount:polar:kube-observer", "publish": ["kubernetes.cluster.consumer.Pod"]},
                {"workload": "system:serviceaccount:polar:kube-consumer", "subscribe": ["kubernetes.cluster.consumer.Pod"], "publish": []}]"#,
        )
        .unwrap();
        assert_eq!(floor.len(), 2);
        let observer = floor
            .topics_for("system:serviceaccount:polar:kube-observer")
            .unwrap();
        assert_eq!(observer.publish, ["kubernetes.cluster.consumer.Pod"]);
        assert!(observer.subscribe.is_empty());
        assert!(
            floor
                .topics_for("system:serviceaccount:polar:somebody")
                .is_none()
        );
        assert!(IssuanceFloor::parse("[]").unwrap().is_empty());
    }

    #[test]
    fn a_floor_that_is_wrong_is_refused_with_every_problem() {
        assert!(IssuanceFloor::parse("{}").is_err());
        assert!(IssuanceFloor::parse(r#"[{"workload": "w", "extra": 1}]"#).is_err());
        let said = IssuanceFloor::parse(
            r#"[{"workload": ""}, {"workload": "w", "publish": ["", "two words"]}, {"workload": "w"}]"#,
        )
        .unwrap_err()
        .join("\n");
        assert!(said.contains("an entry with no workload"), "{said}");
        assert!(said.contains("gives `w` ``"), "{said}");
        assert!(said.contains("gives `w` `two words`"), "{said}");
        assert!(said.contains("lists `w` more than once"), "{said}");
    }

    fn name() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9.:-]{0,20}"
    }

    proptest! {
        /// What the render writes is read back as written, for any workloads with any exact topic names.
        #[test]
        fn a_written_floor_reads_back_as_written(
            entries in prop::collection::btree_map(name(), (prop::collection::vec(name(), 0..4), prop::collection::vec(name(), 0..4)), 0..6)
        ) {
            let written: Vec<WorkloadTopics> = entries
                .iter()
                .map(|(w, (p, s))| WorkloadTopics { workload: w.clone(), publish: p.clone(), subscribe: s.clone() })
                .collect();
            let floor = IssuanceFloor::parse(&serde_json::to_string(&written).unwrap()).unwrap();
            prop_assert_eq!(floor.len(), written.len());
            for w in &written {
                prop_assert_eq!(floor.topics_for(&w.workload), Some(w));
            }
        }
    }
}
