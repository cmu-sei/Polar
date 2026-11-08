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

//! Turning the secrets an author typed into the ciphertext that goes into the document. The fields to seal are
//! the ones the section's description marks `secret`; each plaintext value is encrypted to the agent that will
//! use it, bound to the agent, section, version, the entry's `name` and the field
//! (`polar_escrow::binding_context`), so a ciphertext cannot be moved to another entry or version.
//!
//! A value that is already ciphertext is left as it is, so a file can be signed again. Nothing here prints a
//! value, and the encryption itself (which needs the agent's certificate) is a function the caller passes in,
//! called only when there is a plaintext to seal, so a body with no secrets needs no network at all.

use crate::scaffold::{Field, fields_of};
use polar_escrow::{EscrowedSecret, binding_context};
use serde_json::Value;

/// What was done.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// Plaintext values replaced by ciphertext.
    pub encrypted: usize,
    /// Values that were already ciphertext.
    pub already: usize,
}

/// Where a secret comes from when the author wrote `-`.
pub type ReadSecret<'a> = dyn FnMut() -> Result<String, String> + 'a;
/// Encrypts a plaintext under a binding context; gives the text to put in the document.
pub type Encrypt<'a> = dyn FnMut(&[u8], &[u8]) -> Result<String, String> + 'a;

/// Replaces every plaintext secret in `body` (as `description` marks them) with ciphertext. On any problem
/// nothing is reported as done: the caller must not sign a body that comes back as an error.
pub fn seal_body(
    description: &Value,
    body: &mut Value,
    agent: &str,
    section: &str,
    version: u64,
    encrypt: &mut Encrypt<'_>,
    read_secret: &mut ReadSecret<'_>,
) -> Result<Sealed, Vec<String>> {
    let fields = fields_of(description).map_err(|e| vec![e])?;
    let mut sealed = Sealed::default();
    let mut problems = Vec::new();
    let mut stdin_used = false;
    let mut ctx = Ctx {
        agent,
        section,
        version,
        encrypt,
        read_secret,
        stdin_used: &mut stdin_used,
    };
    seal_object(
        &fields,
        body,
        "",
        None,
        &mut ctx,
        &mut sealed,
        &mut problems,
    );
    if problems.is_empty() {
        Ok(sealed)
    } else {
        Err(problems)
    }
}

struct Ctx<'a, 'b> {
    agent: &'a str,
    section: &'a str,
    version: u64,
    encrypt: &'a mut Encrypt<'b>,
    read_secret: &'a mut ReadSecret<'b>,
    stdin_used: &'a mut bool,
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{path}.{name}")
    }
}

fn seal_object(
    fields: &[(String, Field)],
    value: &mut Value,
    path: &str,
    entry_name: Option<&str>,
    ctx: &mut Ctx<'_, '_>,
    sealed: &mut Sealed,
    problems: &mut Vec<String>,
) {
    let Some(object) = value.as_object_mut() else {
        return; // the structure check has already said so
    };
    // The entry's own `name`, which is what its secrets are bound to.
    let own_name = object
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| entry_name.map(str::to_string));
    for (name, field) in fields {
        let at = join(path, name);
        match field {
            Field::Leaf { secret: true, .. } => {
                let Some(current) = object.get(name).and_then(Value::as_str).map(str::to_string)
                else {
                    continue; // absent, or not text: the structure check judged it
                };
                if EscrowedSecret::from_document_text(&current).is_ok() {
                    sealed.already += 1;
                    continue;
                }
                let bound_to = own_name.as_deref().unwrap_or(name.as_str());
                if own_name.is_none() && !path.is_empty() {
                    problems.push(format!(
                        "`{at}` is a secret in an entry with no `name`, and a secret is bound to its entry's name"
                    ));
                    continue;
                }
                let plaintext = if current == "-" {
                    if *ctx.stdin_used {
                        problems.push(format!(
                            "`{at}`: only one secret can be read from standard input"
                        ));
                        continue;
                    }
                    *ctx.stdin_used = true;
                    match (ctx.read_secret)() {
                        Ok(text) => text,
                        Err(e) => {
                            problems.push(format!("`{at}`: {e}"));
                            continue;
                        }
                    }
                } else {
                    current
                };
                if plaintext.is_empty() {
                    problems.push(format!("`{at}` is a secret and is empty"));
                    continue;
                }
                let context =
                    match binding_context(ctx.agent, ctx.section, ctx.version, bound_to, name) {
                        Ok(context) => context,
                        Err(e) => {
                            problems.push(format!("`{at}`: {e}"));
                            continue;
                        }
                    };
                match (ctx.encrypt)(plaintext.as_bytes(), &context) {
                    Ok(text) => {
                        object.insert(name.clone(), Value::String(text));
                        sealed.encrypted += 1;
                    }
                    Err(e) => problems.push(format!("`{at}`: {e}")),
                }
            }
            Field::List { entries, .. } => {
                if let Some(list) = object.get_mut(name).and_then(Value::as_array_mut) {
                    for (i, item) in list.iter_mut().enumerate() {
                        seal_object(
                            entries,
                            item,
                            &format!("{at}[{i}]"),
                            None,
                            ctx,
                            sealed,
                            problems,
                        );
                    }
                }
            }
            Field::Leaf { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn description() -> Value {
        json!({
            "credentials": {"list_of": {
                "name": {"type": "text", "required": true},
                "secret": {"type": "ciphertext", "required": true, "secret": true}
            }}
        })
    }

    /// An "encryption" that shows what it was given, without any key.
    fn fake(
        calls: &mut Vec<(String, String)>,
    ) -> impl FnMut(&[u8], &[u8]) -> Result<String, String> + '_ {
        move |plaintext, context| {
            calls.push((
                String::from_utf8(plaintext.to_vec()).unwrap(),
                String::from_utf8(context.to_vec()).unwrap(),
            ));
            Ok(format!("SEALED-{}", calls.len()))
        }
    }

    fn no_stdin() -> impl FnMut() -> Result<String, String> {
        || Err("no standard input".to_string())
    }

    fn real_ciphertext() -> String {
        use rsa::pkcs8::EncodePublicKey;
        let mut rng = rand::rng();
        let key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pem = key
            .to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();
        EscrowedSecret::encrypt_bound(b"x", &pem, b"ctx")
            .unwrap()
            .to_document_text()
            .unwrap()
    }

    #[test]
    fn each_plaintext_secret_is_encrypted_under_the_context_of_its_entry() {
        let mut body = json!({"credentials": [
            {"name": "gh", "secret": "ghp-one"},
            {"name": "gl", "secret": "glpat-two"}
        ]});
        let mut calls = Vec::new();
        let done = seal_body(
            &description(),
            &mut body,
            "git-repo-observer",
            "credentials",
            3,
            &mut fake(&mut calls),
            &mut no_stdin(),
        )
        .unwrap();
        assert_eq!(
            done,
            Sealed {
                encrypted: 2,
                already: 0
            }
        );
        assert_eq!(
            calls,
            vec![
                (
                    "ghp-one".to_string(),
                    "git-repo-observer|credentials|3|gh|secret".to_string()
                ),
                (
                    "glpat-two".to_string(),
                    "git-repo-observer|credentials|3|gl|secret".to_string()
                ),
            ]
        );
        assert_eq!(body["credentials"][0]["secret"], "SEALED-1");
        assert_eq!(body["credentials"][1]["secret"], "SEALED-2");
        assert_eq!(body["credentials"][0]["name"], "gh");
    }

    #[test]
    fn a_value_that_is_already_ciphertext_is_left_and_nothing_is_encrypted() {
        let ciphertext = real_ciphertext();
        let mut body = json!({"credentials": [{"name": "gh", "secret": ciphertext}]});
        let mut calls = Vec::new();
        let done = seal_body(
            &description(),
            &mut body,
            "a",
            "credentials",
            1,
            &mut fake(&mut calls),
            &mut no_stdin(),
        )
        .unwrap();
        assert_eq!(
            done,
            Sealed {
                encrypted: 0,
                already: 1
            }
        );
        assert!(calls.is_empty());
        assert_eq!(body["credentials"][0]["secret"], json!(ciphertext));
    }

    #[test]
    fn a_dash_is_read_from_standard_input_once() {
        let mut body = json!({"credentials": [{"name": "gh", "secret": "-"}]});
        let mut calls = Vec::new();
        let mut reads = 0;
        let mut stdin = || {
            reads += 1;
            Ok("from-stdin".to_string())
        };
        seal_body(
            &description(),
            &mut body,
            "a",
            "credentials",
            1,
            &mut fake(&mut calls),
            &mut stdin,
        )
        .unwrap();
        assert_eq!(calls[0].0, "from-stdin");
        assert_eq!(reads, 1);

        let mut two = json!({"credentials": [
            {"name": "gh", "secret": "-"}, {"name": "gl", "secret": "-"}]});
        let mut calls = Vec::new();
        let mut stdin = || Ok("s".to_string());
        let problems = seal_body(
            &description(),
            &mut two,
            "a",
            "credentials",
            1,
            &mut fake(&mut calls),
            &mut stdin,
        )
        .unwrap_err();
        assert!(
            problems[0].contains("only one secret can be read from standard input"),
            "{problems:?}"
        );
    }

    #[test]
    fn standard_input_that_cannot_be_read_is_a_problem() {
        let mut body = json!({"credentials": [{"name": "gh", "secret": "-"}]});
        let mut calls = Vec::new();
        let problems = seal_body(
            &description(),
            &mut body,
            "a",
            "credentials",
            1,
            &mut fake(&mut calls),
            &mut no_stdin(),
        )
        .unwrap_err();
        assert!(problems[0].contains("no standard input"));
        assert!(calls.is_empty());
    }

    #[test]
    fn an_empty_secret_and_an_entry_with_no_name_are_refused() {
        let mut calls = Vec::new();
        let mut empty = json!({"credentials": [{"name": "gh", "secret": ""}]});
        let p = seal_body(
            &description(),
            &mut empty,
            "a",
            "credentials",
            1,
            &mut fake(&mut calls),
            &mut no_stdin(),
        )
        .unwrap_err();
        assert!(p[0].contains("is a secret and is empty"), "{p:?}");

        let mut nameless = json!({"credentials": [{"secret": "tok"}]});
        let p = seal_body(
            &description(),
            &mut nameless,
            "a",
            "credentials",
            1,
            &mut fake(&mut calls),
            &mut no_stdin(),
        )
        .unwrap_err();
        assert!(p[0].contains("no `name`"), "{p:?}");
        assert!(calls.is_empty());
    }

    #[test]
    fn a_name_that_cannot_be_part_of_a_context_is_refused_by_what_it_is() {
        let mut calls = Vec::new();
        let mut body = json!({"credentials": [{"name": "Has Space", "secret": "tok"}]});
        let p = seal_body(
            &description(),
            &mut body,
            "a",
            "credentials",
            1,
            &mut fake(&mut calls),
            &mut no_stdin(),
        )
        .unwrap_err();
        assert!(
            p[0].contains("`name` cannot be part of a binding context"),
            "{p:?}"
        );
        assert!(calls.is_empty());
    }

    #[test]
    fn a_failure_to_encrypt_is_reported_by_field_and_never_repeats_the_secret() {
        let mut body = json!({"credentials": [{"name": "gh", "secret": "TOP-SECRET-VALUE"}]});
        let mut failing = |_: &[u8], _: &[u8]| {
            Err::<String, String>("the certificate is not trusted".to_string())
        };
        let p = seal_body(
            &description(),
            &mut body,
            "a",
            "credentials",
            1,
            &mut failing,
            &mut no_stdin(),
        )
        .unwrap_err();
        assert_eq!(
            p,
            vec!["`credentials[0].secret`: the certificate is not trusted".to_string()]
        );
        assert!(p.iter().all(|m| !m.contains("TOP-SECRET")));
    }

    #[test]
    fn a_top_level_secret_is_bound_to_its_own_field_name() {
        let d = json!({"token": {"type": "ciphertext", "required": true, "secret": true}});
        let mut body = json!({"token": "tok"});
        let mut calls = Vec::new();
        seal_body(
            &d,
            &mut body,
            "agent-x",
            "sec",
            2,
            &mut fake(&mut calls),
            &mut no_stdin(),
        )
        .unwrap();
        assert_eq!(calls[0].1, "agent-x|sec|2|token|token");
    }

    #[test]
    fn a_body_with_no_secret_fields_never_asks_for_encryption() {
        let d = json!({"a": {"type": "text", "required": true}});
        let mut body = json!({"a": "plain"});
        let mut calls = Vec::new();
        let done = seal_body(
            &d,
            &mut body,
            "a",
            "s",
            1,
            &mut fake(&mut calls),
            &mut no_stdin(),
        )
        .unwrap();
        assert_eq!(done, Sealed::default());
        assert!(calls.is_empty());
        assert_eq!(body, json!({"a": "plain"}));
    }
}
