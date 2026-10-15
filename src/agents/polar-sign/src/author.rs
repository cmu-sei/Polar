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

//! From the file an author edited to the payload that is signed: read it, check it against what the agent
//! declared, seal its secrets, and build the payload. Nothing here reaches a network or the clock; the one thing
//! that does, getting the agent's certificate, is a function the caller passes in, and it is called only when
//! there is a plaintext secret to seal.

use crate::check::check_body;
use crate::lenient;
use crate::scaffold::Scaffold;
use crate::seal::{ReadSecret, Sealed, seal_body};
use crate::{configuration_payload, runtime_payload};
use serde_json::Value;

/// What the author's file asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum Document {
    /// A runtime section's body.
    Runtime { section: String, body: Value },
    /// Changes to settings: the `settings` object as written.
    Settings { settings: Value },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub agent: String,
    pub version: u64,
    pub id: Option<String>,
    pub document: Document,
}

/// Reads the file an author edited (JSON with `//` comment lines): `agent`, `version`, and either `section` and
/// `body` or `settings`.
pub fn read_input(text: &str) -> Result<Input, String> {
    let value = lenient::parse(text)?;
    let object = value
        .as_object()
        .ok_or("the file must hold a JSON object")?;
    for key in object.keys() {
        if !["agent", "version", "id", "section", "body", "settings"].contains(&key.as_str()) {
            return Err(format!("`{key}` is not a field of this file"));
        }
    }
    let agent = object
        .get("agent")
        .and_then(Value::as_str)
        .filter(|a| !a.is_empty())
        .ok_or("`agent` must be the name of the agent, as text")?
        .to_string();
    let version = object
        .get("version")
        .and_then(Value::as_u64)
        .filter(|v| *v >= 1)
        .ok_or("`version` must be a whole number, 1 or more")?;
    let id = match object.get("id") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) if !id.is_empty() => Some(id.clone()),
        Some(_) => return Err("`id` must be text".to_string()),
    };
    let document = match (
        object.get("section"),
        object.get("body"),
        object.get("settings"),
    ) {
        (Some(Value::String(section)), Some(body), None) if !section.is_empty() => {
            Document::Runtime {
                section: section.clone(),
                body: body.clone(),
            }
        }
        (Some(_), None, None) => return Err("a file with a `section` needs a `body`".to_string()),
        (Some(_), _, Some(_)) => {
            return Err(
                "a file is either a section (`section` and `body`) or settings, not both"
                    .to_string(),
            );
        }
        (None, None, Some(settings)) => Document::Settings {
            settings: settings.clone(),
        },
        (None, Some(_), _) => return Err("a `body` needs the `section` it is for".to_string()),
        (Some(_), Some(_), None) => {
            return Err("`section` must be the name of the section, as text".to_string());
        }
        (None, None, None) => {
            return Err("the file needs a `section` and `body`, or `settings`".to_string());
        }
    };
    Ok(Input {
        agent,
        version,
        id,
        document,
    })
}

/// What is ready to sign.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub payload: Value,
    /// The section, or `None` for settings.
    pub section: Option<String>,
    pub sealed: Sealed,
}

/// A setting's value as the text a configuration change carries.
fn setting_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Checks `input` against `scaffold`, seals its secrets and builds the payload. Every problem comes back
/// together, naming the field, and none repeats a value.
pub fn prepare(
    input: Input,
    scaffold: &Scaffold,
    encrypt: &mut crate::seal::Encrypt<'_>,
    read_secret: &mut ReadSecret<'_>,
) -> Result<Prepared, Vec<String>> {
    if input.agent != scaffold.agent {
        return Err(vec![format!(
            "the file is for `{}` and the scaffold is {}'s",
            input.agent, scaffold.agent
        )]);
    }
    let id = input
        .id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    match input.document {
        Document::Runtime { section, mut body } => {
            let decl = scaffold.section(&section).ok_or_else(|| {
                let known: Vec<&str> = scaffold.runtime.iter().map(|r| r.name.as_str()).collect();
                vec![format!(
                    "{} has no runtime section `{section}`; it has: {}",
                    scaffold.agent,
                    if known.is_empty() {
                        "none".to_string()
                    } else {
                        known.join(", ")
                    }
                )]
            })?;
            let problems = check_body(&decl.body, &body);
            if !problems.is_empty() {
                return Err(problems);
            }
            let sealed = seal_body(
                &decl.body,
                &mut body,
                &scaffold.agent,
                &section,
                input.version,
                encrypt,
                read_secret,
            )?;
            Ok(Prepared {
                payload: runtime_payload(&scaffold.agent, &section, input.version, &id, body),
                section: Some(section),
                sealed,
            })
        }
        Document::Settings { settings } => {
            let Some(object) = settings.as_object() else {
                return Err(vec![
                    "`settings` must be an object of NAME: value".to_string(),
                ]);
            };
            if object.is_empty() {
                return Err(vec![
                    "`settings` is empty: there is nothing to change".to_string(),
                ]);
            }
            let mut problems = Vec::new();
            let mut changes = Vec::new();
            for (name, value) in object {
                match scaffold.variable(name) {
                    None => {
                        problems.push(format!("`{name}` is not a setting of {}", scaffold.agent))
                    }
                    Some(v) if v.mutability != "dynamic" => problems.push(format!(
                        "`{name}` cannot be changed while {} runs (it is {})",
                        scaffold.agent,
                        if v.mutability.is_empty() {
                            "not dynamic"
                        } else {
                            &v.mutability
                        }
                    )),
                    Some(v) if v.secret => problems.push(format!(
                        "`{name}` is a secret setting, and a signed document cannot set one yet"
                    )),
                    Some(_) => match setting_text(value) {
                        Some(text) => changes.push((name.clone(), text)),
                        None => problems
                            .push(format!("`{name}` must be text, a number or true or false")),
                    },
                }
            }
            if !problems.is_empty() {
                return Err(problems);
            }
            Ok(Prepared {
                payload: configuration_payload(&scaffold.agent, input.version, &id, &changes),
                section: None,
                sealed: Sealed::default(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scaffold() -> Scaffold {
        Scaffold::parse(
            &json!({
                "agent": {"name": "git-repo-observer"},
                "sections": [{"name": "b", "variables": [
                    {"name": "RUST_LOG", "type": "text", "mutability": "dynamic", "default": "info"},
                    {"name": "BUDGET", "type": "whole number of seconds", "mutability": "static"},
                    {"name": "TOKEN", "type": "text", "mutability": "dynamic", "secret": true},
                    {"name": "DEPTH", "type": "whole number", "mutability": "dynamic"},
                    {"name": "LOUD", "type": "true or false", "mutability": "dynamic"}
                ]}],
                "runtime": [{"name": "credentials", "doc": "d", "body": {
                    "credentials": {"list_of": {
                        "name": {"type": "text", "required": true},
                        "secret": {"type": "ciphertext", "required": true, "secret": true}
                    }}
                }}]
            })
            .to_string(),
        )
        .unwrap()
    }

    fn no_encrypt() -> impl FnMut(&[u8], &[u8]) -> Result<String, String> {
        |_, _| Err("no encryption in this test".to_string())
    }
    fn no_stdin() -> impl FnMut() -> Result<String, String> {
        || Err("no standard input".to_string())
    }

    fn input(text: &str) -> Input {
        read_input(text).unwrap_or_else(|e| panic!("{e}: {text}"))
    }

    fn settings(body: Value) -> Result<Prepared, Vec<String>> {
        let text =
            json!({"agent": "git-repo-observer", "version": 4, "settings": body}).to_string();
        prepare(
            input(&text),
            &scaffold(),
            &mut no_encrypt(),
            &mut no_stdin(),
        )
    }

    #[test]
    fn a_file_for_a_section_is_read_with_its_comments() {
        let i = input(
            "// a template\n{\n  \"agent\": \"git-repo-observer\",\n  // the version\n  \"version\": 3,\n  \"section\": \"credentials\",\n  \"body\": {\"credentials\": []},\n}\n",
        );
        assert_eq!(i.agent, "git-repo-observer");
        assert_eq!(i.version, 3);
        assert_eq!(i.id, None);
        assert_eq!(
            i.document,
            Document::Runtime {
                section: "credentials".into(),
                body: json!({"credentials": []})
            }
        );
    }

    #[test]
    fn a_file_that_is_not_one_is_refused_by_what_is_wrong() {
        for (text, wanted) in [
            ("[]", "must hold a JSON object"),
            (r#"{"version":1,"settings":{}}"#, "`agent`"),
            (r#"{"agent":"a","settings":{}}"#, "`version`"),
            (r#"{"agent":"a","version":0,"settings":{}}"#, "`version`"),
            (r#"{"agent":"a","version":"1","settings":{}}"#, "`version`"),
            (r#"{"agent":"a","version":1,"id":5,"settings":{}}"#, "`id`"),
            (
                r#"{"agent":"a","version":1}"#,
                "needs a `section` and `body`, or `settings`",
            ),
            (
                r#"{"agent":"a","version":1,"section":"s"}"#,
                "needs a `body`",
            ),
            (
                r#"{"agent":"a","version":1,"body":{}}"#,
                "needs the `section`",
            ),
            (
                r#"{"agent":"a","version":1,"section":"s","body":{},"settings":{}}"#,
                "not both",
            ),
            (
                r#"{"agent":"a","version":1,"section":3,"body":{}}"#,
                "`section` must be",
            ),
            (
                r#"{"agent":"a","version":1,"settings":{},"stray":1}"#,
                "`stray` is not a field",
            ),
        ] {
            let err = read_input(text).unwrap_err();
            assert!(err.contains(wanted), "{text}: {err}");
        }
    }

    #[test]
    fn settings_become_a_configuration_payload_with_each_value_as_text() {
        let done = settings(json!({"RUST_LOG": "debug", "DEPTH": 7, "LOUD": true})).unwrap();
        assert_eq!(done.section, None);
        assert_eq!(done.payload["type"], "configuration");
        assert_eq!(done.payload["target"]["agent"], "git-repo-observer");
        assert_eq!(done.payload["version"], 4);
        let mut changes: Vec<(String, String)> = done.payload["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["name"].as_str().unwrap().to_string(),
                    c["value"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        changes.sort();
        assert_eq!(
            changes,
            vec![
                ("DEPTH".to_string(), "7".to_string()),
                ("LOUD".to_string(), "true".to_string()),
                ("RUST_LOG".to_string(), "debug".to_string()),
            ]
        );
    }

    #[test]
    fn a_setting_that_cannot_be_changed_is_named_with_why() {
        let p = settings(json!({"NOPE": "x", "BUDGET": "50", "TOKEN": "hunter2", "DEPTH": [1]}))
            .unwrap_err();
        assert!(
            p.iter()
                .any(|m| m == "`NOPE` is not a setting of git-repo-observer"),
            "{p:?}"
        );
        assert!(
            p.iter().any(|m| m.contains(
                "`BUDGET` cannot be changed while git-repo-observer runs (it is static)"
            )),
            "{p:?}"
        );
        assert!(
            p.iter().any(|m| m.contains("`TOKEN` is a secret setting")),
            "{p:?}"
        );
        assert!(
            p.iter()
                .any(|m| m.contains("`DEPTH` must be text, a number or true or false")),
            "{p:?}"
        );
        assert!(p.iter().all(|m| !m.contains("hunter2")), "{p:?}");
    }

    #[test]
    fn nothing_to_change_is_refused() {
        assert!(settings(json!({})).unwrap_err()[0].contains("nothing to change"));
        assert!(settings(json!("x")).unwrap_err()[0].contains("must be an object"));
    }

    #[test]
    fn a_section_becomes_a_runtime_payload_with_its_secrets_sealed() {
        let text = json!({
            "agent": "git-repo-observer", "version": 2, "id": "my-id", "section": "credentials",
            "body": {"credentials": [{"name": "gh", "secret": "ghp-plain"}]}
        })
        .to_string();
        let mut seen = Vec::new();
        let mut encrypt = |plain: &[u8], context: &[u8]| {
            seen.push((
                String::from_utf8(plain.to_vec()).unwrap(),
                String::from_utf8(context.to_vec()).unwrap(),
            ));
            Ok("SEALED".to_string())
        };
        let done = prepare(input(&text), &scaffold(), &mut encrypt, &mut no_stdin()).unwrap();
        assert_eq!(done.section.as_deref(), Some("credentials"));
        assert_eq!(done.sealed.encrypted, 1);
        assert_eq!(done.payload["type"], "runtime");
        assert_eq!(done.payload["id"], "my-id");
        assert_eq!(done.payload["section"], "credentials");
        assert_eq!(done.payload["version"], 2);
        assert_eq!(done.payload["body"]["credentials"][0]["secret"], "SEALED");
        assert_eq!(
            seen,
            vec![(
                "ghp-plain".to_string(),
                "git-repo-observer|credentials|2|gh|secret".to_string()
            )]
        );
        assert!(!done.payload.to_string().contains("ghp-plain"));
    }

    #[test]
    fn an_id_is_made_when_the_author_gave_none() {
        let text = json!({"agent": "git-repo-observer", "version": 1, "section": "credentials",
            "body": {"credentials": []}})
        .to_string();
        let a = prepare(
            input(&text),
            &scaffold(),
            &mut no_encrypt(),
            &mut no_stdin(),
        )
        .unwrap();
        let b = prepare(
            input(&text),
            &scaffold(),
            &mut no_encrypt(),
            &mut no_stdin(),
        )
        .unwrap();
        assert!(!a.payload["id"].as_str().unwrap().is_empty());
        assert_ne!(a.payload["id"], b.payload["id"]);
    }

    #[test]
    fn a_body_that_does_not_fit_is_refused_before_anything_is_sealed() {
        let text = json!({"agent": "git-repo-observer", "version": 1, "section": "credentials",
            "body": {"credentials": [{"secret": "ghp-plain", "stray": 1}]}})
        .to_string();
        let mut encrypted = 0;
        let mut encrypt = |_: &[u8], _: &[u8]| {
            encrypted += 1;
            Ok("x".to_string())
        };
        let p = prepare(input(&text), &scaffold(), &mut encrypt, &mut no_stdin()).unwrap_err();
        assert!(
            p.iter()
                .any(|m| m.contains("`credentials[0].name` is required")),
            "{p:?}"
        );
        assert!(
            p.iter()
                .any(|m| m.contains("`credentials[0].stray` is not a field")),
            "{p:?}"
        );
        assert_eq!(encrypted, 0);
    }

    #[test]
    fn a_file_for_another_agent_or_a_section_it_has_not_is_refused() {
        let other =
            json!({"agent": "git-repo-controller", "version": 1, "settings": {"RUST_LOG": "x"}})
                .to_string();
        let p = prepare(
            input(&other),
            &scaffold(),
            &mut no_encrypt(),
            &mut no_stdin(),
        )
        .unwrap_err();
        assert!(p[0].contains("is for `git-repo-controller`"), "{p:?}");
        let nope =
            json!({"agent": "git-repo-observer", "version": 1, "section": "nope", "body": {}})
                .to_string();
        let p = prepare(
            input(&nope),
            &scaffold(),
            &mut no_encrypt(),
            &mut no_stdin(),
        )
        .unwrap_err();
        assert!(
            p[0].contains("no runtime section `nope`") && p[0].contains("credentials"),
            "{p:?}"
        );
    }
}
