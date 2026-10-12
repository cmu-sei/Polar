//! Signed configuration documents, as the scheduler processor meets them.
//!
//! The processor does not verify these: it cannot know who may sign for whom, and the receiving agent checks
//! for itself. It reads three fields to know where a document is going and what to call it in the graph, and
//! forwards the document untouched. A document it cannot route is still written to the graph, so that it is
//! seen, and goes nowhere.

use serde_json::Value;

/// Where a document is going and what it is called, read from its payload without trusting any of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routing {
    pub id: String,
    pub agent: String,
    pub version: u64,
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
    Ok(Routing {
        id: text(&payload["id"], "id")?,
        agent: text(&payload["target"]["agent"], "target.agent")?,
        version: payload["version"]
            .as_u64()
            .ok_or_else(|| "the payload has no whole-number `version`".to_string())?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_document_is_told_from_an_unsigned_schedule_by_its_two_fields() {
        let signed = json!({"payload": {"id": "c", "target": {"agent": "a"}, "version": 3}, "signature": {}});
        assert_eq!(
            classify(&signed.to_string()),
            Shape::Document(Ok(Routing {
                id: "c".into(),
                agent: "a".into(),
                version: 3
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
    fn a_document_that_cannot_be_routed_says_why() {
        for (payload, wanted) in [
            (json!({"target": {"agent": "a"}, "version": 1}), "id"),
            (json!({"id": "c", "version": 1}), "target.agent"),
            (
                json!({"id": "c", "target": {"agent": "a"}, "version": "1"}),
                "version",
            ),
            (json!([1]), "id"),
        ] {
            let doc = json!({"payload": payload, "signature": {}});
            match classify(&doc.to_string()) {
                Shape::Document(Err(why)) => assert!(why.contains(wanted), "{why}"),
                other => panic!("{other:?}"),
            }
        }
    }
}
