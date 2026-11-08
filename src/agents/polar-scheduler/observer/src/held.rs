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

//! The signed documents in the checkout, by file, as the observer has read them: what an agent that has
//! just started is sent. The checkout is the repository as last pulled, so this is the repository's word,
//! not a copy kept anywhere else; it is rebuilt from the checkout whenever the observer starts.

use polar_scheduler_common::routing::{Routing, Shape, classify};
use std::collections::HashMap;

struct Held {
    routing: Routing,
    json: String,
}

#[derive(Default)]
pub struct HeldDocuments {
    by_path: HashMap<String, Held>,
}

impl HeldDocuments {
    /// A file was created or changed. A signed document that can be routed is held; anything else (an
    /// older schedule, a document with no target) is not, and drops whatever the file held before.
    pub fn record(&mut self, path: &str, json: &str) {
        match classify(json) {
            Shape::Document(Ok(routing)) => {
                self.by_path.insert(
                    path.to_string(),
                    Held {
                        routing,
                        json: json.to_string(),
                    },
                );
            }
            _ => {
                self.by_path.remove(path);
            }
        }
    }

    /// A file was deleted.
    pub fn remove(&mut self, path: &str) {
        self.by_path.remove(path);
    }

    /// The documents held for `agent`, as they were read, oldest version first within each section so that
    /// an agent that applies versions in order ends at the latest.
    pub fn for_agent(&self, agent: &str) -> Vec<String> {
        let mut held: Vec<(&String, &Held)> = self
            .by_path
            .iter()
            .filter(|(_, h)| h.routing.agent == agent)
            .collect();
        // Versions are per section, so a section's documents stay together, each in its own order.
        held.sort_by(|(pa, a), (pb, b)| {
            (&a.routing.section, a.routing.version, &a.routing.id, pa).cmp(&(
                &b.routing.section,
                b.routing.version,
                &b.routing.id,
                pb,
            ))
        });
        held.into_iter().map(|(_, h)| h.json.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document(id: &str, agent: &str, version: u64) -> String {
        json!({
            "payload": {"type": "configuration", "id": id, "target": {"agent": agent}, "version": version, "changes": []},
            "signature": {}
        })
        .to_string()
    }

    #[test]
    fn a_document_is_held_for_the_agent_it_names_and_no_other() {
        let mut held = HeldDocuments::default();
        let a = document("one", "agent-a", 1);
        held.record("permanent/a.dhall", &a);
        held.record("permanent/b.dhall", &document("two", "agent-b", 1));
        assert_eq!(held.for_agent("agent-a"), [a]);
        assert!(held.for_agent("agent-z").is_empty());
    }

    #[test]
    fn what_a_file_holds_now_replaces_what_it_held() {
        let mut held = HeldDocuments::default();
        held.record("permanent/a.dhall", &document("one", "agent-a", 1));
        let later = document("one", "agent-a", 2);
        held.record("permanent/a.dhall", &later);
        assert_eq!(held.for_agent("agent-a"), [later]);
    }

    #[test]
    fn a_file_that_stops_being_a_routable_document_holds_nothing() {
        let mut held = HeldDocuments::default();
        held.record("permanent/a.dhall", &document("one", "agent-a", 1));
        held.record("permanent/a.dhall", r#"{"id": "x", "kind": "Permanent"}"#);
        assert!(held.for_agent("agent-a").is_empty());

        held.record("permanent/b.dhall", &document("two", "agent-b", 1));
        let unroutable =
            json!({"payload": {"type": "configuration", "id": "c", "version": 1}, "signature": {}});
        held.record("permanent/b.dhall", &unroutable.to_string());
        assert!(held.for_agent("agent-b").is_empty());
    }

    #[test]
    fn a_deleted_file_holds_nothing() {
        let mut held = HeldDocuments::default();
        held.record("permanent/a.dhall", &document("one", "agent-a", 1));
        held.remove("permanent/a.dhall");
        assert!(held.for_agent("agent-a").is_empty());
        held.remove("permanent/never-there.dhall"); // harmless
    }

    fn runtime(id: &str, agent: &str, section: &str, version: u64) -> String {
        json!({
            "payload": {"type": "runtime", "id": id, "target": {"agent": agent}, "section": section,
                        "version": version, "body": {}},
            "signature": {}
        })
        .to_string()
    }

    #[test]
    fn runtime_documents_are_ordered_by_version_within_their_own_section() {
        let mut held = HeldDocuments::default();
        // Versions are per section: sorted by version alone these would interleave (1, 2, 3, 4) and split each
        // section's documents; they belong together, each in its own order.
        let (a5, a9, b1, b2) = (
            runtime("a1", "agent-a", "alpha", 1),
            runtime("a3", "agent-a", "alpha", 3),
            runtime("b2", "agent-a", "beta", 2),
            runtime("b4", "agent-a", "beta", 4),
        );
        for (path, doc) in [("1", &a9), ("2", &b2), ("3", &a5), ("4", &b1)] {
            held.record(path, doc);
        }
        let sent = held.for_agent("agent-a");
        let position = |doc: &String| sent.iter().position(|d| d == doc).unwrap();
        assert_eq!(sent.len(), 4);
        assert!(position(&a5) < position(&a9), "alpha in version order");
        assert!(position(&b1) < position(&b2), "beta in version order");
        // And the sections stay together, so no section's documents are split by the other's.
        assert!(
            (position(&a5).abs_diff(position(&a9)) == 1)
                && (position(&b1).abs_diff(position(&b2)) == 1),
            "{sent:?}"
        );
    }

    #[test]
    fn several_documents_for_one_agent_come_oldest_version_first() {
        let mut held = HeldDocuments::default();
        let (v3, v1, v2) = (
            document("c", "agent-a", 3),
            document("a", "agent-a", 1),
            document("b", "agent-a", 2),
        );
        held.record("z.dhall", &v3);
        held.record("a.dhall", &v1);
        held.record("m.dhall", &v2);
        assert_eq!(held.for_agent("agent-a"), [v1, v2, v3]);
    }
}
