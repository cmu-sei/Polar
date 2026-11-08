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

//! Who the writer asks: the workloads expected to answer, each by its one identity. A signed runtime section of the
//! writer, `agents` (docs/architecture/soft-state.md, "Revised by the maintainer's review": "the set of agents asked is
//! the state agent's signed configuration"), read with `ConfigurationHandle::runtime` and replaced whole by each
//! version. Its first version is seeded from the catalog's workloads by `infra/seed.nu`, as the reader's `schema` is;
//! later ones are signed by an author holding `c:update-runtime`. Never an environment variable.

use polar_agent_builder::RuntimeSection;
use polar_agent_builder::runtime::SectionAuthority;
use serde::{Deserialize, Serialize};

/// What the section is called in a document.
pub const SECTION_NAME: &str = "agents";

/// The most workloads one version may name.
pub const MAX_WORKLOADS: usize = 500;

pub static AGENTS: RuntimeSection = RuntimeSection {
    authority: SectionAuthority::Runtime,
    name: SECTION_NAME,
    doc: "The workloads the state agent's writer asks about their health, by each one's identity: a workload named \
          here and outside the family it was deployed with is reported unreachable. Replaced whole by each version.",
    body: r#"{"workloads": {"list_of": {"workload": {"type": "workload name", "required": true}, "identity": {"type": "<workload>.<namespace>.serviceaccount.<domain>", "required": true, "doc": "the identity the broker names the workload by; only an answer it stamped with it counts"}}}}"#,
    validate,
};

fn validate(body: &serde_json::Value) -> Result<(), Vec<String>> {
    Expected::from_body(body).map(|_| ())
}

/// One expected workload.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub workload: String,
    /// `<workload>.<namespace>.serviceaccount.<domain>`, as the broker names it.
    pub identity: String,
}

/// The workloads expected to answer: a version of the `agents` section.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    pub workloads: Vec<Entry>,
}

impl Expected {
    /// The expected workloads a section body describes, or every problem with it: a body that is not one, more than
    /// [`MAX_WORKLOADS`], an identity that names another workload, a workload or an identity named twice. Names only.
    pub fn from_body(body: &serde_json::Value) -> Result<Self, Vec<String>> {
        let expected: Expected = serde_json::from_value(body.clone())
            .map_err(|e| vec![format!("the body is not a list of agents: {e}")])?;
        let mut problems = Vec::new();
        if expected.workloads.len() > MAX_WORKLOADS {
            problems.push(format!(
                "`workloads` has {} entries, and at most {MAX_WORKLOADS} are taken",
                expected.workloads.len()
            ));
        }
        let mut workloads = std::collections::BTreeSet::new();
        let mut identities = std::collections::BTreeSet::new();
        for (i, entry) in expected.workloads.iter().enumerate() {
            if cert_issuer_common::identity::workload_of(&entry.identity) != Some(&entry.workload) {
                problems.push(format!(
                    "entry {i}: the identity of {} names another workload",
                    entry.workload
                ));
            }
            if !workloads.insert(entry.workload.as_str()) {
                problems.push(format!("entry {i}: {} is named twice", entry.workload));
            }
            if !identities.insert(entry.identity.as_str()) {
                problems.push(format!("entry {i}: {} is named twice", entry.identity));
            }
        }
        if problems.is_empty() {
            Ok(expected)
        } else {
            Err(problems)
        }
    }

    /// The entry whose identity is `identity`, exactly.
    pub fn by_identity(&self, identity: &str) -> Option<&Entry> {
        self.workloads.iter().find(|e| e.identity == identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_body_names_each_workload_by_its_identity() {
        let expected = Expected::from_body(&json!({ "workloads": [
            { "workload": "jira-observer", "identity": "jira-observer.polar.serviceaccount.cluster.local" },
            { "workload": "kube-observer", "identity": "kube-observer.polar.serviceaccount.cluster.local" }
        ] }))
        .unwrap();
        assert_eq!(expected.workloads.len(), 2);
        assert_eq!(
            expected
                .by_identity("kube-observer.polar.serviceaccount.cluster.local")
                .map(|e| e.workload.as_str()),
            Some("kube-observer")
        );
        assert!(
            expected
                .by_identity("kube-observer.elsewhere.serviceaccount.cluster.local")
                .is_none()
        );
        assert!(serde_json::from_str::<serde_json::Value>(AGENTS.body).is_ok());
    }

    #[test]
    fn a_body_that_is_not_one_is_refused_with_every_problem_named() {
        for (bad, why) in [
            (json!("text"), "not a list of agents"),
            (
                json!({ "workloads": [], "extra": 1 }),
                "not a list of agents",
            ),
            (
                json!({ "workloads": [{ "workload": "a", "identity": "b.polar.serviceaccount.x" }] }),
                "names another workload",
            ),
            (
                json!({ "workloads": [{ "workload": "a", "identity": "a.p.serviceaccount.x" },
                                    { "workload": "a", "identity": "a.q.serviceaccount.x" }] }),
                "named twice",
            ),
        ] {
            let problems = Expected::from_body(&bad).unwrap_err();
            assert!(
                problems.iter().any(|p| p.contains(why)),
                "{bad}: {problems:?}"
            );
            assert!((AGENTS.validate)(&bad).is_err());
        }
    }
}
