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

//! Reading, without trusting, where a signed document is going.
//!
//! The polar scheduler's observer and processor both need to know which agent a document is for and what
//! it is called. Neither verifies it: it cannot know who may sign for whom, and the receiving agent checks
//! for itself. They read three fields and pass the document on untouched. A document that cannot be routed
//! is still written to the graph, so that it is seen, and goes nowhere.

use crate::document_type;
use serde_json::Value;

/// Where a document is going and what it is called, read from its payload without trusting any of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routing {
    pub id: String,
    pub agent: String,
    pub version: u64,
    /// The section of the agent's runtime configuration a `runtime` document is for; none for a
    /// `configuration` document, whose versions are the agent's own.
    pub section: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// Not a signed document: the older, unsigned schedule shape.
    NotADocument,
    /// Shaped like one. `Err` says why it cannot be routed.
    Document(Result<Routing, String>),
}

/// A signed document is an object with a `payload` and a `signature`.
pub fn classify(json: &str) -> Shape {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Shape::NotADocument;
    };
    let Some(object) = value.as_object() else {
        return Shape::NotADocument;
    };
    if !(object.contains_key("payload") && object.contains_key("signature")) {
        return Shape::NotADocument;
    }
    Shape::Document(routing(&object["payload"]))
}

fn routing(payload: &Value) -> Result<Routing, String> {
    let text = |value: &Value, what: &str| {
        value
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("the payload has no text `{what}`"))
    };
    // A command or an admission is still written to the graph, as one that cannot be routed, so that it
    // is seen.
    let kind = text(&payload["type"], "type")?;
    let section = match kind.as_str() {
        document_type::CONFIGURATION => None,
        document_type::RUNTIME => Some(text(&payload["section"], "section")?),
        other => {
            return Err(format!(
                "a `{other}` document is not handled by the scheduler yet"
            ));
        }
    };
    Ok(Routing {
        id: text(&payload["id"], "id")?,
        agent: text(&payload["target"]["agent"], "target.agent")?,
        version: payload["version"]
            .as_u64()
            .ok_or_else(|| "the payload has no whole-number `version`".to_string())?,
        section,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_document_is_told_from_an_unsigned_schedule_by_its_two_fields() {
        let signed = json!({"payload": {"type": "configuration", "id": "c", "target": {"agent": "a"}, "version": 3}, "signature": {}});
        assert_eq!(
            classify(&signed.to_string()),
            Shape::Document(Ok(Routing {
                id: "c".into(),
                agent: "a".into(),
                version: 3,
                section: None,
            }))
        );
        let old = json!({"id": "x", "kind": "Permanent", "version": 1});
        assert_eq!(classify(&old.to_string()), Shape::NotADocument);
        assert_eq!(classify("not json"), Shape::NotADocument);
        assert_eq!(classify("[1]"), Shape::NotADocument);
        // Only one of the two fields is not a document.
        assert_eq!(classify(r#"{"payload": {}}"#), Shape::NotADocument);
    }

    #[test]
    fn a_runtime_document_is_routed_by_its_agent_and_carries_its_section() {
        let doc = json!({
            "payload": {"type": "runtime", "id": "repos-0007", "target": {"agent": "git-repo-controller"},
                        "section": "repositories", "version": 7, "body": {}},
            "signature": {}
        });
        assert_eq!(
            classify(&doc.to_string()),
            Shape::Document(Ok(Routing {
                id: "repos-0007".into(),
                agent: "git-repo-controller".into(),
                version: 7,
                section: Some("repositories".into()),
            }))
        );
        // A configuration document has no section.
        let configuration = json!({"payload": {"type": "configuration", "id": "c", "target": {"agent": "a"}, "version": 3}, "signature": {}});
        match classify(&configuration.to_string()) {
            Shape::Document(Ok(routing)) => assert_eq!(routing.section, None),
            other => panic!("{other:?}"),
        }
        // A runtime document that names no section cannot be routed to one.
        let no_section = json!({
            "payload": {"type": "runtime", "id": "r", "target": {"agent": "a"}, "version": 1, "body": {}},
            "signature": {}
        });
        match classify(&no_section.to_string()) {
            Shape::Document(Err(why)) => assert!(why.contains("section"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_document_that_cannot_be_routed_says_why() {
        for (payload, wanted) in [
            (
                json!({"type": "configuration", "target": {"agent": "a"}, "version": 1}),
                "id",
            ),
            (
                json!({"type": "configuration", "id": "c", "version": 1}),
                "target.agent",
            ),
            (
                json!({"type": "configuration", "id": "c", "target": {"agent": "a"}, "version": "1"}),
                "version",
            ),
            (json!([1]), "type"),
            (
                json!({"id": "c", "target": {"agent": "a"}, "version": 1}),
                "type",
            ),
            (
                json!({"type": "command", "id": "c", "target": {"agent": "a"}, "version": 1}),
                "not handled",
            ),
        ] {
            let doc = json!({"payload": payload, "signature": {}});
            match classify(&doc.to_string()) {
                Shape::Document(Err(why)) => assert!(why.contains(wanted), "{why}"),
                other => panic!("{other:?}"),
            }
        }
    }
}
