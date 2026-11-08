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

//! The state agent's floor: who owns and who reads which graph label, by each workload's one identity. It is a signed
//! runtime section of the state agent, `schema` (docs/architecture/soft-state.md, "Revised by the maintainer's
//! review"), read with `ConfigurationHandle::runtime` and replaced whole by each version, so a change reaches the
//! running agent without a render or a rollout. Its first version is seeded from the agents' scaffolds by
//! `infra/seed.nu`, as the credentials agent's `policy` is; later ones are signed by an author holding
//! `c:update-runtime`.

use polar_agent_builder::RuntimeSection;
use polar_agent_builder::graph_schema::is_identifier;
use polar_agent_builder::runtime::SectionAuthority;
use serde::{Deserialize, Serialize};

/// What the section is called in a document.
pub const SECTION_NAME: &str = "schema";

/// The most workloads one version may name.
pub const MAX_WORKLOADS: usize = 500;

pub static SCHEMA: RuntimeSection = RuntimeSection {
    authority: SectionAuthority::Runtime,
    name: SECTION_NAME,
    doc: "Who owns and who reads which graph label, by each workload's identity: whom the state agent answers about \
          what. Replaced whole by each version.",
    body: r#"{"workloads": {"list_of": {"workload": {"type": "workload name", "required": true}, "identity": {"type": "<workload>.<namespace>.serviceaccount.<domain>", "required": true, "doc": "the identity the broker names the workload by; the entry is matched on it exactly"}, "owns": {"type": "list of graph labels", "required": false, "doc": "the labels the workload writes"}, "reads": {"type": "list of graph labels", "required": false, "doc": "the labels the state agent answers the workload about"}}}}"#,
    validate,
};

fn validate(body: &serde_json::Value) -> Result<(), Vec<String>> {
    Floor::from_body(body).map(|_| ())
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

/// Who may read which label: a version of the `schema` section.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Floor {
    pub workloads: Vec<Entry>,
}

impl Floor {
    /// The floor a section body describes, or every problem with it: a body that is not one, more than
    /// [`MAX_WORKLOADS`] entries, an identity that names another workload, one named twice, a label that is not a plain
    /// identifier. Names only, never a value of anything else.
    pub fn from_body(body: &serde_json::Value) -> Result<Self, Vec<String>> {
        let floor: Floor = serde_json::from_value(body.clone())
            .map_err(|e| vec![format!("the body is not a schema: {e}")])?;
        let mut problems = Vec::new();
        if floor.workloads.len() > MAX_WORKLOADS {
            problems.push(format!(
                "`workloads` has {} entries, and at most {MAX_WORKLOADS} are taken",
                floor.workloads.len()
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (i, entry) in floor.workloads.iter().enumerate() {
            if cert_issuer_common::identity::workload_of(&entry.identity) != Some(&entry.workload) {
                problems.push(format!(
                    "entry {i}: the identity of {} names another workload",
                    entry.workload
                ));
            }
            if !seen.insert(entry.identity.as_str()) {
                problems.push(format!("entry {i}: {} is named twice", entry.identity));
            }
            for label in entry.owns.iter().chain(entry.reads.iter()) {
                if !is_identifier(label) {
                    problems.push(format!(
                        "entry {i}: a label of {} is not a plain identifier",
                        entry.workload
                    ));
                }
            }
        }
        if problems.is_empty() {
            Ok(floor)
        } else {
            Err(problems)
        }
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
    use serde_json::json;

    const ID: &str = "jira-observer.polar.serviceaccount.cluster.local";

    fn floor() -> Floor {
        Floor::from_body(&json!({ "workloads": [
            { "workload": "jira-observer", "identity": ID, "reads": ["JiraIssue"] },
            { "workload": "jira-processor", "identity": "jira-processor.polar.serviceaccount.cluster.local",
              "owns": ["JiraIssue", "JiraUser"] }
        ] }))
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
    fn a_body_that_is_not_a_schema_is_refused_with_every_problem_named() {
        assert!(
            Floor::from_body(&json!({ "workloads": [] }))
                .unwrap()
                .workloads
                .is_empty()
        );
        for (bad, why) in [
            (json!("text"), "not a schema"),
            (json!({ "workloads": [], "extra": 1 }), "not a schema"),
            (
                json!({ "workloads": [{ "workload": "a", "identity": "b.polar.serviceaccount.x" }] }),
                "names another workload",
            ),
            (
                json!({ "workloads": [{ "workload": "a", "identity": "a.p.serviceaccount.x" },
                                    { "workload": "a", "identity": "a.p.serviceaccount.x" }] }),
                "named twice",
            ),
            (
                json!({ "workloads": [{ "workload": "a", "identity": "a.p.serviceaccount.x", "reads": ["Jira Issue"] }] }),
                "not a plain identifier",
            ),
        ] {
            let problems = Floor::from_body(&bad).unwrap_err();
            assert!(
                problems.iter().any(|p| p.contains(why)),
                "{bad}: {problems:?}"
            );
            assert!((SCHEMA.validate)(&bad).is_err());
        }
        assert!(serde_json::from_str::<serde_json::Value>(SCHEMA.body).is_ok());
    }
}
