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

//! The state agent's floor: every workload of the target by its one identity, with the labels its scaffold declares it
//! owns and reads (`PolarAgent::GRAPH`). The render writes it into `SOFT_STATE_FLOOR` from the scaffolds
//! (`infra/graph.nu`, `state_floor`), as it writes the credentials agent's authority model, so applying the catalog is
//! its sign-off (docs/architecture/policy-enforcement.md, "Decisions"). Read once, at start.

use polar_env::{Defaulted, Described, Env, Section};
use serde::{Deserialize, Serialize};

/// The one format the agent reads.
pub const FORMAT: u32 = 1;

/// The floor, as JSON.
pub static FLOOR: Defaulted<String> = Defaulted::text(
    "SOFT_STATE_FLOOR",
    "who may read which graph label: every workload of the target by its identity, with the labels it owns and \
     reads, as JSON {format: 1, workloads: [{workload, identity, owns, reads}]}. The render writes it from the \
     agents' scaffolds (docs/architecture/soft-state.md); empty, no one may read anything unless policy permits it. \
     Read once, at start",
    "",
);

static VARIABLES: &[&dyn Described] = &[&FLOOR];

/// The section the state agent reads.
pub static SECTION: Section = Section {
    name: "soft-state",
    vars: VARIABLES,
    relations,
};

/// A floor that cannot be read is a configuration problem, named before the agent starts.
fn relations(env: &dyn Env) -> Vec<String> {
    match env.var(FLOOR.name()) {
        Some(text) => match Floor::parse(&text) {
            Ok(_) => Vec::new(),
            Err(why) => vec![format!("{}: {why}", FLOOR.name())],
        },
        None => Vec::new(),
    }
}

/// One workload of the floor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub workload: String,
    /// `<workload>.<namespace>.serviceaccount.<domain>`, as the broker names it.
    pub identity: String,
    #[serde(default)]
    pub owns: Vec<String>,
    #[serde(default)]
    pub reads: Vec<String>,
}

/// Who may read which label.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Floor {
    pub format: u32,
    pub workloads: Vec<Entry>,
}

impl Floor {
    /// The floor in `text`: empty text is the empty floor; otherwise JSON of [`FORMAT`], whose entries each name the
    /// workload their identity names, once.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.trim().is_empty() {
            return Ok(Self {
                format: FORMAT,
                workloads: Vec::new(),
            });
        }
        let floor: Floor = serde_json::from_str(text).map_err(|e| format!("not a floor: {e}"))?;
        if floor.format != FORMAT {
            return Err(format!(
                "format {} is not the format this agent reads ({FORMAT})",
                floor.format
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &floor.workloads {
            if cert_issuer_common::identity::workload_of(&entry.identity) != Some(&entry.workload) {
                return Err(format!(
                    "the entry for {} has the identity {}, which names another workload",
                    entry.workload, entry.identity
                ));
            }
            if !seen.insert(entry.identity.as_str()) {
                return Err(format!("{} is in the floor twice", entry.identity));
            }
        }
        Ok(floor)
    }

    /// The entry of the asker whose identity is `identity`, exactly: a workload of the same name in another namespace
    /// or domain is not it.
    pub fn entry(&self, identity: &str) -> Option<&Entry> {
        self.workloads.iter().find(|e| e.identity == identity)
    }

    /// Whether the floor says the asker `identity` reads `label`.
    pub fn declares_read(&self, identity: &str, label: &str) -> bool {
        self.entry(identity)
            .is_some_and(|e| e.reads.iter().any(|l| l == label))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "jira-observer.polar.serviceaccount.cluster.local";

    fn floor() -> Floor {
        Floor::parse(&format!(
            r#"{{"format":1,"workloads":[
                {{"workload":"jira-observer","identity":"{ID}","reads":["JiraIssue"]}},
                {{"workload":"jira-processor","identity":"jira-processor.polar.serviceaccount.cluster.local",
                  "owns":["JiraIssue","JiraUser"]}}]}}"#
        ))
        .unwrap()
    }

    #[test]
    fn a_read_is_the_floor_s_only_for_the_identity_that_declares_it() {
        let floor = floor();
        assert!(floor.declares_read(ID, "JiraIssue"));
        assert!(!floor.declares_read(ID, "JiraUser"));
        // Owning is not reading.
        assert!(!floor.declares_read(
            "jira-processor.polar.serviceaccount.cluster.local",
            "JiraIssue"
        ));
        // The same workload name in another namespace is another identity.
        assert!(!floor.declares_read(
            "jira-observer.elsewhere.serviceaccount.cluster.local",
            "JiraIssue"
        ));
        assert!(!floor.declares_read("nobody", "JiraIssue"));
    }

    #[test]
    fn an_empty_floor_admits_no_one_and_a_bad_one_is_refused() {
        assert!(Floor::parse("").unwrap().workloads.is_empty());
        assert!(Floor::parse("  ").unwrap().workloads.is_empty());
        for (bad, why) in [
            ("{", "not a floor"),
            (r#"{"format":2,"workloads":[]}"#, "format 2"),
            (r#"{"format":1,"workloads":[],"extra":1}"#, "not a floor"),
            (
                r#"{"format":1,"workloads":[{"workload":"a","identity":"b.polar.serviceaccount.cluster.local"}]}"#,
                "names another workload",
            ),
            (
                r#"{"format":1,"workloads":[{"workload":"a","identity":"a.polar.serviceaccount.x"},
                                            {"workload":"a","identity":"a.polar.serviceaccount.x"}]}"#,
                "twice",
            ),
        ] {
            let error = Floor::parse(bad).unwrap_err();
            assert!(error.contains(why), "{bad}: {error}");
        }
    }

    #[test]
    fn a_floor_that_cannot_be_read_is_a_configuration_problem() {
        let env = |name: &str| (name == "SOFT_STATE_FLOOR").then(|| "{".to_string());
        let problems = relations(&env);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("SOFT_STATE_FLOOR"), "{problems:?}");
        assert!(relations(&|_: &str| None).is_empty());
    }
}
