//! Signed configuration documents, as the scheduler processor meets them.
//!
//! The processor does not verify these: it cannot know who may sign for whom, and the receiving agent checks
//! for itself. It reads three fields to know where a document is going and what to call it in the graph, and
//! forwards the document untouched. A document it cannot route is still written to the graph, so that it is
//! seen, and goes nowhere.

use polar::graph::controller::{GraphController, GraphControllerMsg};
use polar_scheduler_common::document_type;
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
    // Only configuration is handled so far. A command or an admission is still written to the graph, as one
    // that cannot be routed, so that it is seen.
    let kind = text(&payload["type"], "type")?;
    if kind != document_type::CONFIGURATION {
        return Err(format!(
            "a `{kind}` document is not handled by the scheduler yet"
        ));
    }
    Ok(Routing {
        id: text(&payload["id"], "id")?,
        agent: text(&payload["target"]["agent"], "target.agent")?,
        version: payload["version"]
            .as_u64()
            .ok_or_else(|| "the payload has no whole-number `version`".to_string())?,
    })
}

/// A signed document the processor holds for an agent: the latest of one item, as it was received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDocument {
    pub id: String,
    pub agent: String,
    pub version: u64,
    /// The document exactly as it was read from the repository.
    pub document: String,
}

/// The documents the graph holds, read when the processor starts so that what it knows does not depend on
/// having been up when they arrived: an agent that starts after a restart of the processor is still sent
/// them, and an older document pushed later cannot pass for the latest. Nodes that could not be routed have
/// no target or version, and are not documents to send anywhere.
pub async fn load_stored_documents(
    controller: &GraphController,
) -> Result<Vec<StoredDocument>, String> {
    let query = neo4rs::Query::new(STORED_DOCUMENTS_QUERY.to_string());
    let rows = match ractor::rpc::call(
        controller,
        |reply| GraphControllerMsg::Query(query, reply),
        None,
    )
    .await
    {
        Ok(ractor::rpc::CallResult::Success(Ok(rows))) => rows,
        Ok(ractor::rpc::CallResult::Success(Err(e))) => return Err(e),
        other => return Err(format!("the graph controller did not answer: {other:?}")),
    };
    documents_from_rows(&rows)
}

/// What [`load_stored_documents`] asks the graph.
pub const STORED_DOCUMENTS_QUERY: &str = "MATCH (c:Schedule:Configuration) \
     WHERE c.target IS NOT NULL AND c.version IS NOT NULL AND c.document IS NOT NULL \
     RETURN c.id AS id, c.target AS target, c.version AS version, c.document AS document";

/// The documents in the rows [`STORED_DOCUMENTS_QUERY`] returned.
pub fn documents_from_rows(rows: &[neo4rs::Row]) -> Result<Vec<StoredDocument>, String> {
    rows.iter()
        .map(|row| {
            let get = |e: neo4rs::DeError| e.to_string();
            let version: i64 = row.get("version").map_err(get)?;
            Ok(StoredDocument {
                id: row.get("id").map_err(get)?,
                agent: row.get("target").map_err(get)?,
                version: u64::try_from(version).map_err(|e| e.to_string())?,
                document: row.get("document").map_err(get)?,
            })
        })
        .collect()
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
