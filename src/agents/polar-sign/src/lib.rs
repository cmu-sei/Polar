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

#![deny(clippy::disallowed_methods)]
//! `polar-sign`: makes and checks signed configuration documents.
//!
//! A user writes what they want changed, this signs it with their certificate and key, and the
//! result is one Dhall file to commit to the schedules repository (design:
//! `docs/architecture/signed-configuration.md`). The signing and the checking are
//! `polar-signing`'s; this crate puts a payload together, writes the file in the form the
//! scheduler's observer reads, and reads one back.

pub mod author;
pub mod check;
pub mod fetch;
pub mod flow;
pub mod lenient;
pub mod push;
pub mod scaffold;
pub mod seal;
pub mod template;

use serde_json::{Map, Value, json};

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DhallError {
    #[error(
        "`{0}` cannot be written in Dhall here: only text, whole numbers, true and false, lists and records"
    )]
    Unsupported(&'static str),
    #[error("`{0}` is not a name a record field can have here")]
    FieldName(String),
    /// A list whose elements have no one type: Dhall would refuse it (`InvalidListElement`), so it is refused here,
    /// before anything is signed, by the path of the values that differ (`[]` for any element of a list).
    #[error(
        "the values at `{path}` are not all of one type, as the elements of a list must be: {why}"
    )]
    Mismatch { path: String, why: String },
}

/// A value's Dhall type, as far as the value says: an empty list's elements are `Unknown` until a sibling in the same
/// list says what they are.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Type {
    Unknown,
    Bool,
    Natural,
    Integer,
    Text,
    List(Box<Type>),
    Record(std::collections::BTreeMap<String, Type>),
}

impl Type {
    fn name(&self) -> &'static str {
        match self {
            Type::Unknown => "an empty list's element",
            Type::Bool => "Bool",
            Type::Natural => "Natural",
            Type::Integer => "Integer",
            Type::Text => "Text",
            Type::List(_) => "a list",
            Type::Record(_) => "a record",
        }
    }

    /// The type as Dhall writes it; an element nothing says anything about is the empty record type, `{}`.
    fn dhall(&self) -> Result<String, DhallError> {
        Ok(match self {
            Type::Unknown => "{}".to_string(),
            Type::Bool => "Bool".to_string(),
            Type::Natural => "Natural".to_string(),
            Type::Integer => "Integer".to_string(),
            Type::Text => "Text".to_string(),
            Type::List(element) => format!("List {}", element.dhall_atom()?),
            Type::Record(fields) if fields.is_empty() => "{}".to_string(),
            Type::Record(fields) => {
                let rendered = fields
                    .iter()
                    .map(|(name, t)| Ok(format!("{} : {}", field_name(name)?, t.dhall()?)))
                    .collect::<Result<Vec<_>, DhallError>>()?;
                format!("{{ {} }}", rendered.join(", "))
            }
        })
    }

    /// As [`dhall`](Self::dhall), parenthesised where it is an argument (`List (List Text)`).
    fn dhall_atom(&self) -> Result<String, DhallError> {
        let written = self.dhall()?;
        Ok(match self {
            Type::List(_) => format!("({written})"),
            _ => written,
        })
    }
}

/// The type of `value`, at `path`: every list's elements unified into one type.
fn type_of(value: &Value, path: &str) -> Result<Type, DhallError> {
    Ok(match value {
        Value::Null => return Err(DhallError::Unsupported("null")),
        Value::Bool(_) => Type::Bool,
        Value::Number(n) => match (n.as_u64(), n.as_i64()) {
            (Some(_), _) => Type::Natural,
            (None, Some(_)) => Type::Integer,
            _ => {
                return Err(DhallError::Unsupported(
                    "a number that is not a whole number",
                ));
            }
        },
        Value::String(_) => Type::Text,
        Value::Array(items) => {
            let mut element = Type::Unknown;
            for (i, item) in items.iter().enumerate() {
                let here = format!("{path}[{i}]");
                element = unify(&element, &type_of(item, &here)?, &format!("{path}[]"))?;
            }
            Type::List(Box::new(element))
        }
        Value::Object(fields) => Type::Record(
            fields
                .iter()
                .map(|(name, v)| Ok((name.clone(), type_of(v, &format!("{path}.{name}"))?)))
                .collect::<Result<_, DhallError>>()?,
        ),
    })
}

/// The one type `a` and `b`, two values at `path` that must be of one type, both are: an empty list's element takes
/// its type from the other; lists unify their elements, and records field by field, with the same fields.
fn unify(a: &Type, b: &Type, path: &str) -> Result<Type, DhallError> {
    let mismatch = |why: String| DhallError::Mismatch {
        path: path.to_string(),
        why,
    };
    Ok(match (a, b) {
        (Type::Unknown, other) | (other, Type::Unknown) => other.clone(),
        (Type::List(x), Type::List(y)) => Type::List(Box::new(unify(x, y, &format!("{path}[]"))?)),
        (Type::Record(x), Type::Record(y)) => {
            let fields_x: Vec<&String> = x.keys().collect();
            let fields_y: Vec<&String> = y.keys().collect();
            if fields_x != fields_y {
                return Err(mismatch(format!(
                    "records with different fields ({} and {})",
                    fields_x
                        .iter()
                        .map(|f| f.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    fields_y
                        .iter()
                        .map(|f| f.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            Type::Record(
                x.iter()
                    .map(|(name, t)| {
                        Ok((name.clone(), unify(t, &y[name], &format!("{path}.{name}"))?))
                    })
                    .collect::<Result<_, DhallError>>()?,
            )
        }
        (x, y) if x == y => x.clone(),
        (x, y) => return Err(mismatch(format!("{} against {}", x.name(), y.name()))),
    })
}

/// `value`, of type `ty` (from [`type_of`]), as a Dhall expression: an empty list annotated with its element's type.
fn render(value: &Value, ty: &Type) -> Result<String, DhallError> {
    match (value, ty) {
        (Value::Null, _) => Err(DhallError::Unsupported("null")),
        (Value::Bool(true), _) => Ok("True".to_string()),
        (Value::Bool(false), _) => Ok("False".to_string()),
        (Value::Number(n), _) => match (n.as_u64(), n.as_i64()) {
            (Some(natural), _) => Ok(natural.to_string()),
            (None, Some(integer)) => Ok(format!("{integer}")),
            _ => Err(DhallError::Unsupported(
                "a number that is not a whole number",
            )),
        },
        (Value::String(s), _) => Ok(text(s)),
        (Value::Array(items), Type::List(element)) => {
            // Dhall wants a type on an empty list; once evaluated it is only an empty list, whatever it was
            // annotated as, which is how a version that withdraws everything (`[]`) is written.
            if items.is_empty() {
                return Ok(format!("([] : List {})", element.dhall_atom()?));
            }
            let rendered = items
                .iter()
                .map(|item| render(item, element))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("[ {} ]", rendered.join(", ")))
        }
        (Value::Object(fields), Type::Record(types)) => {
            if fields.is_empty() {
                return Ok("{=}".to_string());
            }
            let rendered = fields
                .iter()
                .map(|(name, v)| {
                    let t = types.get(name).unwrap_or(&Type::Unknown);
                    Ok(format!("{} = {}", field_name(name)?, render(v, t)?))
                })
                .collect::<Result<Vec<_>, DhallError>>()?;
            Ok(format!("{{ {} }}", rendered.join(", ")))
        }
        // Unreachable: the type is the value's own.
        (_, _) => Err(DhallError::Mismatch {
            path: String::new(),
            why: "a value that is not of its own type".to_string(),
        }),
    }
}

fn text(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // `${` would start an interpolation.
            '$' => out.push_str("\\$"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn field_name(name: &str) -> Result<String, DhallError> {
    let plain = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '/');
    if plain {
        Ok(name.to_string())
    } else if name.chars().all(|c| !c.is_control() && c != '`') && !name.is_empty() {
        Ok(format!("`{name}`"))
    } else {
        Err(DhallError::FieldName(name.to_string()))
    }
}

/// `value` as a Dhall expression: what the scheduler's observer evaluates back to the same JSON. Text, whole numbers,
/// booleans, lists and records; nothing else is in a document. Each list's elements are of one type, as Dhall wants:
/// an empty list is annotated with the element type its siblings give it (`([] : List Text)`, beside `[ "a" ]`), and
/// with `{}` when nothing says; a list whose elements cannot be of one type is refused, by its path.
pub fn dhall(value: &Value) -> Result<String, DhallError> {
    let ty = type_of(value, "$")?;
    render(value, &ty)
}

/// The payload of a configuration update: `{ type, id, target: { agent }, version, changes }`. See
/// the design for what each field is for.
pub fn configuration_payload(
    agent: &str,
    version: u64,
    id: &str,
    changes: &[(String, String)],
) -> Value {
    json!({
        "type": "configuration",
        "id": id,
        "target": { "agent": agent },
        "version": version,
        "changes": changes
            .iter()
            .map(|(name, value)| json!({ "name": name, "value": value }))
            .collect::<Vec<_>>(),
    })
}

/// The payload of a runtime document: `{ type, id, target: { agent }, section, version, body }`. The body is
/// whatever the agent declared for that section; the receiver validates it.
pub fn runtime_payload(agent: &str, section: &str, version: u64, id: &str, body: Value) -> Value {
    json!({
        "type": "runtime",
        "id": id,
        "target": { "agent": agent },
        "section": section,
        "version": version,
        "body": body,
    })
}

/// A body as given on a command line or in a file: JSON, and an object, since a section's body is a set of
/// named fields.
/// The runtime section that carries Cedar policy (the credentials agent's), whose `source` is checked before it is
/// signed, whether or not the agent's scaffold is at hand.
pub const POLICY_SECTION: &str = "policy";

/// What is wrong with the body of a runtime document for `section`, that would make every agent refuse it: for the
/// policy section, a `source` that is not a policy Polar can load.
pub fn runtime_problems(section: &str, body: &Value) -> Result<(), String> {
    if section != POLICY_SECTION {
        return Ok(());
    }
    let source = body
        .get("source")
        .and_then(Value::as_str)
        .ok_or("the policy section's body must hold the policy text in `source`")?;
    polar_policy::validate_source(source)
        .map_err(|why| format!("`source` is not a policy Polar can load: {why}"))
}

pub fn parse_body(text: &str) -> Result<Value, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| format!("the body is not JSON: {e}"))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err("the body must be a JSON object".to_string())
    }
}

/// `NAME=VALUE`, as given on a command line.
pub fn parse_change(arg: &str) -> Result<(String, String), String> {
    match arg.split_once('=') {
        Some((name, value)) if !name.trim().is_empty() => {
            Ok((name.trim().to_string(), value.to_string()))
        }
        _ => Err(format!("`{arg}` is not NAME=VALUE")),
    }
}

/// The certificates in a PEM file, each as its own PEM string.
pub fn split_pem(pem: &str) -> Vec<String> {
    const END: &str = "-----END CERTIFICATE-----";
    let mut out = Vec::new();
    let mut rest = pem;
    while let Some(end) = rest.find(END) {
        let (block, tail) = rest.split_at(end + END.len());
        if let Some(begin) = block.find("-----BEGIN CERTIFICATE-----") {
            out.push(format!("{}\n", block[begin..].trim_end()));
        }
        rest = tail;
    }
    out
}

/// A user's way of signing: their certificate, the certificates that link it to the CA, and their key.
pub struct Identity {
    /// The signer's own certificate first, then whatever links it to the CA.
    pub chain: Vec<String>,
    pub key_pem: String,
}

/// Reads `<user>.crt`, `<user>.key` and, if there is one, `<user>.chain.pem` (the CA certificates)
/// from `dir`, where `just admin-claim` puts them.
pub fn load_identity(dir: &std::path::Path, user: &str) -> Result<Identity, String> {
    let read = |name: String| {
        let path = dir.join(&name);
        std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))
    };
    let leaf = split_pem(&read(format!("{user}.crt"))?);
    let [leaf] = leaf.as_slice() else {
        return Err(format!(
            "{user}.crt holds {} certificates, not one",
            leaf.len()
        ));
    };
    let mut chain = vec![leaf.clone()];
    if let Ok(more) = read(format!("{user}.chain.pem")) {
        chain.extend(split_pem(&more));
    }
    Ok(Identity {
        chain,
        key_pem: read(format!("{user}.key"))?,
    })
}

/// A payload signed by `identity`, written as the Dhall file to commit.
pub fn sign_to_dhall(payload: Value, identity: &Identity) -> Result<String, String> {
    let document = polar_signing::sign(payload, &identity.chain, &identity.key_pem)
        .map_err(|e| e.to_string())?;
    document_dhall(&document).map_err(|e| e.to_string())
}

/// What a signed Dhall file evaluates to, as JSON. The observer evaluates it the same way: a file with an import is
/// refused, before anything it names is read or asked for.
pub fn evaluate(source: &str) -> Result<Value, String> {
    let simple: serde_dhall::SimpleValue = serde_dhall::from_str(source)
        .imports(false)
        .parse()
        .map_err(|e| format!("not valid Dhall: {e}"))?;
    serde_json::to_value(&simple).map_err(|e| e.to_string())
}

/// Checks a signed document the way a receiver does the signature part of it: the layout, the chain
/// to `trust_pem`, the signer's certificate and the signature. Returns who signed and the payload.
pub fn verify_source(
    source: &str,
    trust_pem: &[String],
    now: i64,
) -> Result<(polar_signing::SignerInfo, Value), String> {
    let value = evaluate(source)?;
    let document = polar_signing::Document::from_value(&value).map_err(|e| e.to_string())?;
    let trust = polar_signing::Trust::from_pem(trust_pem).map_err(|e| e.to_string())?;
    let signer = polar_signing::verify(&document, &trust, now).map_err(|e| e.to_string())?;
    Ok((signer, document.payload))
}

/// A document as a Dhall file: what is committed.
pub fn document_dhall(document: &polar_signing::Document) -> Result<String, DhallError> {
    let value = document.to_value();
    let mut fields: Map<String, Value> = Map::new();
    if let Value::Object(map) = value {
        fields = map;
    }
    Ok(format!(
        "-- Signed by `polar-sign`. Do not edit: any change invalidates the signature.\n{}\n",
        dhall(&Value::Object(fields))?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(value: Value) {
        let source = dhall(&value).expect("it renders");
        let back: serde_dhall::SimpleValue = serde_dhall::from_str(&source)
            .parse()
            .unwrap_or_else(|e| panic!("`{source}` does not evaluate: {e}"));
        assert_eq!(serde_json::to_value(&back).unwrap(), value, "{source}");
    }

    #[test]
    fn what_a_document_holds_evaluates_back_to_the_same_json() {
        roundtrip(json!({ "a": "text", "n": 7, "t": true, "f": false }));
        roundtrip(json!({ "nested": { "list": [ "x", "y" ] } }));
        roundtrip(json!({ "records": [ { "name": "RUST_LOG", "value": "debug" } ] }));
        roundtrip(configuration_payload(
            "credentials-agent",
            3,
            "11111111-2222-4333-8444-555555555555",
            &[("RUST_LOG".to_string(), "info,polar=debug".to_string())],
        ));
    }

    #[test]
    fn text_with_quotes_slashes_newlines_and_dollars_survives() {
        roundtrip(json!({ "v": "say \"hi\" \\ then\nnext\tline $HOME ${not} é" }));
        roundtrip(
            json!({ "pem": "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n" }),
        );
    }

    #[test]
    fn a_field_name_that_is_not_plain_is_quoted() {
        roundtrip(json!({ "a b": 1, "c.d": 2 }));
    }

    /// A file to verify may have come from anyone: what it names is not read.
    #[test]
    fn a_file_with_an_import_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let held = dir.path().join("held.txt");
        std::fs::write(&held, "HELD-IN-THE-FILE").unwrap();
        let held = held.display();
        for source in [
            format!("{{ a = {held} as Text }}"),
            format!("if False then {held} as Text else \"unreached\""),
            "env:HOME as Text".to_string(),
            "missing".to_string(),
        ] {
            let why = evaluate(&source).unwrap_err();
            assert!(!why.contains("HELD-IN-THE-FILE"), "`{source}`: {why}");
        }
        assert_eq!(evaluate("{ a = 1 }").unwrap(), json!({ "a": 1 }));
        // Dhall's fallback takes its other side: still nothing is read.
        assert_eq!(
            evaluate(&format!("{held} as Text ? \"other\"")).unwrap(),
            json!("other")
        );
    }

    /// What the scheduler's observer does with a committed file (polar-scheduler/observer/src/watcher.rs,
    /// `evaluate_dhall_file_to_json`): the file, read by serde_dhall, into JSON.
    fn as_the_scheduler_reads_it(source: &str) -> Value {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("document.dhall");
        std::fs::write(&file, source).unwrap();
        let simple: serde_dhall::SimpleValue = serde_dhall::from_file(&file)
            .imports(false)
            .parse()
            .unwrap_or_else(|e| panic!("`{source}` does not evaluate: {e}"));
        serde_json::to_value(&simple).unwrap()
    }

    /// The state agent's first schema, as the seed signed it on u7c (2026-10-31): one workload owns nothing, the other
    /// reads nothing, and an empty list beside a full one in the same field.
    fn state_schema() -> Value {
        json!({ "workloads": [
            { "workload": "jira-observer", "identity": "jira-observer.polar.serviceaccount.cluster.local",
              "owns": [], "reads": ["JiraIssue"] },
            { "workload": "jira-processor", "identity": "jira-processor.polar.serviceaccount.cluster.local",
              "owns": ["JiraGroup", "JiraIssue", "JiraIssueChangeLog", "JiraIssueType", "JiraProject", "JiraUser"],
              "reads": [] }
        ] })
    }

    #[test]
    fn an_empty_list_beside_a_full_one_takes_its_siblings_type_and_the_scheduler_reads_it_back() {
        let source = dhall(&state_schema()).expect("it renders");
        assert!(source.contains("([] : List Text)"), "{source}");
        assert_eq!(as_the_scheduler_reads_it(&source), state_schema());
        // Nested: a list of lists, and records in records.
        for value in [
            json!({ "x": [[], ["a"]] }),
            json!({ "x": [{ "y": { "z": [] } }, { "y": { "z": [1, 2] } }] }),
            json!({ "x": [{ "y": [] }, { "y": [] }] }),
            json!({ "x": [[[]], [], [["deep"]]] }),
        ] {
            let source = dhall(&value).expect("it renders");
            assert_eq!(as_the_scheduler_reads_it(&source), value, "{source}");
        }
    }

    #[test]
    fn a_list_whose_elements_cannot_be_of_one_type_is_refused_by_its_path() {
        for (value, path, why) in [
            (
                json!({ "w": [{ "a": 1 }, { "b": 1 }] }),
                "$.w[]",
                "different fields",
            ),
            (json!({ "w": ["x", 1] }), "$.w[]", "Text against Natural"),
            (
                json!({ "w": [{ "l": ["x"] }, { "l": [true] }] }),
                "$.w[].l[]",
                "Text against Bool",
            ),
            (json!({ "w": [[1], "x"] }), "$.w[]", "a list against Text"),
            (
                json!({ "w": [{ "a": { "b": 1 } }, { "a": { "b": "x" } }] }),
                "$.w[].a.b",
                "Natural against Text",
            ),
        ] {
            match dhall(&value) {
                Err(DhallError::Mismatch { path: p, why: w }) => {
                    assert_eq!(p, path, "{value}");
                    assert!(w.contains(why), "{value}: {w}");
                }
                other => panic!("{value}: {other:?}"),
            }
        }
    }

    #[test]
    fn the_state_agents_seeded_schema_is_signed_read_by_the_scheduler_and_verified() {
        let ca = polar_signing::testing::root("Polar Internal CA");
        let payload = runtime_payload(
            "soft-state-observer",
            "schema",
            1,
            "11111111-2222-4333-8444-555555555555",
            state_schema(),
        );
        let file = sign_to_dhall(payload.clone(), &identity_for(&ca)).unwrap();
        // What is signed is the canonical JSON payload, not the Dhall text: read back as the scheduler reads it, the
        // document verifies, and its payload is the one signed.
        let read = as_the_scheduler_reads_it(&file);
        let document = polar_signing::Document::from_value(&read).unwrap();
        assert_eq!(document.payload, payload);
        let (_, verified) = verify_source(&file, std::slice::from_ref(&ca.pem), now()).unwrap();
        assert_eq!(verified, payload);
    }

    fn shape() -> impl proptest::strategy::Strategy<Value = Shape> {
        use proptest::prelude::*;
        let leaf = prop_oneof![Just(Shape::Text), Just(Shape::Natural), Just(Shape::Bool)];
        leaf.prop_recursive(4, 24, 4, |inner| {
            prop_oneof![
                inner.clone().prop_map(|s| Shape::List(Box::new(s))),
                prop::collection::btree_map("[a-c]{1,2}", inner, 0..3).prop_map(Shape::Record),
            ]
        })
    }

    /// The shape of a value: every element of a list is of the list's one shape.
    #[derive(Debug, Clone)]
    enum Shape {
        Text,
        Natural,
        Bool,
        List(Box<Shape>),
        Record(std::collections::BTreeMap<String, Shape>),
    }

    fn of_shape(shape: Shape) -> proptest::strategy::BoxedStrategy<Value> {
        use proptest::prelude::*;
        match shape {
            Shape::Text => "\\PC{0,6}".prop_map(Value::String).boxed(),
            Shape::Natural => any::<u32>().prop_map(|n| json!(n)).boxed(),
            Shape::Bool => any::<bool>().prop_map(Value::Bool).boxed(),
            Shape::List(element) => prop::collection::vec(of_shape(*element), 0..3)
                .prop_map(Value::Array)
                .boxed(),
            Shape::Record(fields) => {
                let names: Vec<String> = fields.keys().cloned().collect();
                let values: Vec<BoxedStrategy<Value>> =
                    fields.into_values().map(of_shape).collect();
                values
                    .prop_map(move |vs| Value::Object(names.iter().cloned().zip(vs).collect()))
                    .boxed()
            }
        }
    }

    proptest::proptest! {
        /// Any tree of records, lists, text, naturals and booleans whose list elements share a shape renders to Dhall
        /// the scheduler reads back as the same JSON, empty lists included, wherever they are.
        #[test]
        fn a_tree_whose_lists_share_a_shape_reads_back_as_itself(
            value in proptest::strategy::Strategy::prop_flat_map(shape(), |s| of_shape(Shape::Record([("root".to_string(), s)].into()))),
        ) {
            let source = dhall(&value).expect("it renders");
            let back: serde_dhall::SimpleValue = serde_dhall::from_str(&source)
                .parse()
                .unwrap_or_else(|e| panic!("`{source}` does not evaluate: {e}"));
            proptest::prop_assert_eq!(serde_json::to_value(&back).unwrap(), value);
        }
    }

    #[test]
    fn what_dhall_cannot_hold_here_is_refused_not_guessed() {
        assert_eq!(dhall(&json!(null)), Err(DhallError::Unsupported("null")));
        roundtrip(json!([]));
        roundtrip(json!({ "credentials": [] }));
        roundtrip(json!({ "body": { "repositories": [], "n": 1 } }));
        assert!(dhall(&json!({ "x": 1.5 })).is_err());
    }

    #[test]
    fn a_change_is_name_equals_value_and_the_value_may_hold_equals() {
        assert_eq!(
            parse_change("RUST_LOG=debug"),
            Ok(("RUST_LOG".into(), "debug".into()))
        );
        assert_eq!(parse_change("A=b=c"), Ok(("A".into(), "b=c".into())));
        assert_eq!(parse_change("A="), Ok(("A".into(), "".into())));
        assert!(parse_change("nothing").is_err());
        assert!(parse_change("=x").is_err());
    }

    #[test]
    fn a_pem_file_with_two_certificates_is_two_strings() {
        let pem = "junk\n-----BEGIN CERTIFICATE-----\nAAA\n-----END CERTIFICATE-----\n\n-----BEGIN CERTIFICATE-----\nBBB\n-----END CERTIFICATE-----\n";
        let parts = split_pem(pem);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains("AAA") && !parts[0].contains("BBB"));
        assert!(parts[1].starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(split_pem("nothing here").is_empty());
    }

    #[test]
    fn a_configuration_payload_has_the_fields_the_receiver_reads() {
        let p = configuration_payload("agent", 2, "id-1", &[("A".into(), "b".into())]);
        assert_eq!(p["type"], "configuration");
        assert_eq!(p["target"]["agent"], "agent");
        assert_eq!(p["version"], 2);
        assert_eq!(p["changes"][0]["name"], "A");
        assert_eq!(p["changes"][0]["value"], "b");
        assert_eq!(
            p.as_object().unwrap().len(),
            5,
            "no field the receiver does not know"
        );
    }

    use polar_signing::testing::{Spec, now, root, signer};

    fn identity_for(ca: &polar_signing::testing::Authority) -> Identity {
        let s = signer(ca, Spec::default());
        Identity {
            chain: vec![s.cert_pem],
            key_pem: s.key_pem,
        }
    }

    fn payload() -> Value {
        configuration_payload("agent", 1, "id-1", &[("RUST_LOG".into(), "debug".into())])
    }

    #[test]
    fn what_is_signed_verifies_back_from_the_file_that_is_committed() {
        let ca = root("Polar Internal CA");
        let file = sign_to_dhall(payload(), &identity_for(&ca)).unwrap();
        assert!(file.starts_with("-- Signed by"));
        let (who, back) = verify_source(&file, std::slice::from_ref(&ca.pem), now()).unwrap();
        assert_eq!(who.username.as_deref(), Some("alice"));
        assert_eq!(back, payload());
    }

    #[test]
    fn a_file_edited_after_signing_is_refused() {
        let ca = root("Polar Internal CA");
        let file = sign_to_dhall(payload(), &identity_for(&ca)).unwrap();
        let edited = file.replace("\"debug\"", "\"trace\"");
        assert_ne!(edited, file);
        let refused = verify_source(&edited, std::slice::from_ref(&ca.pem), now()).unwrap_err();
        assert!(refused.contains("signature"), "{refused}");
    }

    #[test]
    fn a_signature_from_a_ca_that_is_not_trusted_is_refused() {
        let ca = root("Polar Internal CA");
        let file = sign_to_dhall(payload(), &identity_for(&ca)).unwrap();
        let other = root("Someone Else");
        assert!(verify_source(&file, &[other.pem], now()).is_err());
    }

    #[test]
    fn a_user_directory_gives_the_certificate_the_key_and_the_ca() {
        let ca = root("Polar Internal CA");
        let s = signer(&ca, Spec::default());
        let dir = std::env::temp_dir().join(format!("polar-sign-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("alice.crt"), &s.cert_pem).unwrap();
        std::fs::write(dir.join("alice.key"), &s.key_pem).unwrap();
        std::fs::write(dir.join("alice.chain.pem"), &ca.pem).unwrap();
        let identity = load_identity(&dir, "alice").unwrap();
        assert_eq!(identity.chain.len(), 2, "the certificate, then the CA");
        assert!(load_identity(&dir, "nobody").is_err());
        let file = sign_to_dhall(payload(), &identity).unwrap();
        // a chain that carries the pinned root is trusted by the pin
        assert!(verify_source(&file, std::slice::from_ref(&ca.pem), now()).is_ok());
    }

    #[test]
    fn a_runtime_payload_has_the_fields_the_receiver_reads() {
        let body = json!({ "source": "permit (principal, action, resource);" });
        let p = runtime_payload("credentials-agent", "policy", 3, "id-1", body.clone());
        assert_eq!(p["type"], "runtime");
        assert_eq!(p["target"]["agent"], "credentials-agent");
        assert_eq!(p["section"], "policy");
        assert_eq!(p["version"], 3);
        assert_eq!(p["body"], body);
        assert_eq!(
            p.as_object().unwrap().len(),
            6,
            "no field the receiver does not know"
        );
    }

    #[test]
    fn a_policy_is_checked_before_it_is_signed_and_other_sections_are_not() {
        let seed = include_str!("../../../../infra/seeds/signed/policy.cedar");
        assert!(runtime_problems("policy", &json!({ "source": seed })).is_ok());
        let wrong =
            json!({ "source": r#"permit (principal, action == Action::"reap", resource);"# });
        assert!(
            runtime_problems("policy", &wrong)
                .unwrap_err()
                .contains("schema")
        );
        assert!(runtime_problems("policy", &json!({ "rules": seed })).is_err());
        // Another section's body is the agent's to judge.
        assert!(runtime_problems("repositories", &wrong).is_ok());
    }

    #[test]
    fn a_body_is_a_json_object_and_nothing_else() {
        assert_eq!(parse_body(r#"{"a": 1}"#).unwrap(), json!({ "a": 1 }));
        for bad in ["", "[1]", "\"text\"", "7", "null", "{not json"] {
            assert!(parse_body(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_first_policy_signs_and_verifies_back_word_for_word() {
        let source = include_str!("../../../../infra/seeds/signed/policy.cedar");
        let ca = root("Polar Internal CA");
        let payload = runtime_payload(
            "credentials-agent",
            "policy",
            1,
            "id-1",
            json!({ "source": source }),
        );
        let file = sign_to_dhall(payload.clone(), &identity_for(&ca)).unwrap();
        let (_, back) = verify_source(&file, std::slice::from_ref(&ca.pem), now()).unwrap();
        assert_eq!(back, payload);
        assert_eq!(back["body"]["source"], source);
    }
}
