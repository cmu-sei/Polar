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

/// `value` as a Dhall expression: what the scheduler's observer evaluates back to the same JSON.
/// Text, whole numbers, booleans, lists and records; nothing else is in a document.
pub fn dhall(value: &Value) -> Result<String, DhallError> {
    match value {
        Value::Null => Err(DhallError::Unsupported("null")),
        Value::Bool(true) => Ok("True".to_string()),
        Value::Bool(false) => Ok("False".to_string()),
        Value::Number(n) => match (n.as_u64(), n.as_i64()) {
            (Some(natural), _) => Ok(natural.to_string()),
            (None, Some(integer)) => Ok(format!("{integer}")),
            _ => Err(DhallError::Unsupported(
                "a number that is not a whole number",
            )),
        },
        Value::String(s) => Ok(text(s)),
        Value::Array(items) => {
            // Dhall wants a type on an empty list; once evaluated it is only an empty list, whatever it was
            // annotated as, which is how a version that withdraws everything (`[]`) is written.
            if items.is_empty() {
                return Ok("([] : List {})".to_string());
            }
            let rendered = items.iter().map(dhall).collect::<Result<Vec<_>, _>>()?;
            Ok(format!("[ {} ]", rendered.join(", ")))
        }
        Value::Object(fields) => {
            if fields.is_empty() {
                return Ok("{=}".to_string());
            }
            let rendered = fields
                .iter()
                .map(|(name, v)| Ok(format!("{} = {}", field_name(name)?, dhall(v)?)))
                .collect::<Result<Vec<_>, DhallError>>()?;
            Ok(format!("{{ {} }}", rendered.join(", ")))
        }
    }
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

/// What a Dhall source evaluates to, as JSON (the observer evaluates it the same way).
pub fn evaluate(source: &str) -> Result<Value, String> {
    let simple: serde_dhall::SimpleValue = serde_dhall::from_str(source)
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
            "credentials-agent-consumer",
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
        let p = runtime_payload(
            "credentials-agent-consumer",
            "policy",
            3,
            "id-1",
            body.clone(),
        );
        assert_eq!(p["type"], "runtime");
        assert_eq!(p["target"]["agent"], "credentials-agent-consumer");
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
            "credentials-agent-consumer",
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
