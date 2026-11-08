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

//! The file an author is given to fill in: `polar-sign template <agent> <section>` for a runtime section and
//! `polar-sign template <agent> --settings` for the settings a signed document may change. Generated from the
//! agent's scaffold, so it is always what the agent will accept: every field, its type, whether it is
//! required, its default, what it is for, and which values are secret. It is JSON with `//` comment lines
//! (see [`crate::lenient`]).

use crate::scaffold::{Field, Kind, Scaffold, fields_of, kind_of};
use serde_json::{Value, json};

const INDENT: &str = "  ";

/// The value a field starts with: its default, or an empty one of the kind its type promises.
fn starting_value(type_name: &str, default: &Option<Value>) -> Value {
    if let Some(default) = default {
        return default.clone();
    }
    match kind_of(type_name) {
        Some(Kind::Number) => json!(0),
        Some(Kind::Flag) => json!(false),
        Some(Kind::List) => json!([]),
        Some(Kind::Map) => json!({}),
        Some(Kind::Text) | None => json!(""),
    }
}

fn comment(out: &mut Vec<String>, depth: usize, text: &str) {
    for line in text.lines() {
        out.push(format!("{}// {line}", INDENT.repeat(depth)));
    }
}

/// The lines of one object's fields, at `depth`, with commas where JSON needs them. A field that is optional
/// and has no default is written as a comment only: delete the slashes to use it.
fn object_lines(fields: &[(String, Field)], depth: usize, agent: &str) -> Vec<String> {
    // Each field gives comment lines and, unless it is left as a comment, the lines of its value.
    struct Part {
        comments: Vec<String>,
        value: Vec<String>,
    }
    let mut parts: Vec<Part> = Vec::new();
    // What the author must fill in comes before what they may, each group in the order the description gave.
    let mut ordered: Vec<&(String, Field)> = fields.iter().collect();
    ordered.sort_by_key(|(_, field)| !field.required());
    for (name, field) in ordered {
        let mut comments = Vec::new();
        let pad = INDENT.repeat(depth);
        match field {
            Field::Leaf {
                type_name,
                required,
                secret,
                default,
                doc,
            } => {
                let mut head = format!(
                    "{} - {type_name}",
                    if *required { "required" } else { "optional" }
                );
                if let Some(d) = default {
                    head.push_str(&format!(" - default {d}"));
                }
                comment(&mut comments, depth, &head);
                if !doc.is_empty() {
                    comment(&mut comments, depth, doc);
                }
                if *secret {
                    comment(
                        &mut comments,
                        depth,
                        &format!(
                            "SECRET: type the value here. `polar-sign sign` encrypts it so that only {agent} can \
                             open it, before anything is signed or written. \"-\" asks for it on standard input."
                        ),
                    );
                }
                let value = starting_value(type_name, default);
                let line = format!("{pad}\"{name}\": {value}");
                if *required || default.is_some() || *secret {
                    parts.push(Part {
                        comments,
                        value: vec![line],
                    });
                } else {
                    // With the comma it would need, so that taking the slashes off leaves a valid line wherever it
                    // is (a trailing comma is forgiven by the reader, so it also does at the end).
                    comments.push(format!("{pad}// {},", line.trim_start()));
                    parts.push(Part {
                        comments,
                        value: Vec::new(),
                    });
                }
            }
            Field::List {
                entries,
                required,
                doc,
            } => {
                comment(
                    &mut comments,
                    depth,
                    &format!(
                        "{} - a list; copy the entry below for each one",
                        if *required { "required" } else { "optional" }
                    ),
                );
                if !doc.is_empty() {
                    comment(&mut comments, depth, doc);
                }
                let mut value = vec![format!("{pad}\"{name}\": [")];
                value.push(format!("{pad}{INDENT}{{"));
                value.extend(object_lines(entries, depth + 2, agent));
                value.push(format!("{pad}{INDENT}}}"));
                value.push(format!("{pad}]"));
                parts.push(Part { comments, value });
            }
        }
    }
    let mut lines = Vec::new();
    for part in parts {
        lines.extend(part.comments);
        let n = part.value.len();
        // A comma after every value, the last included: the reader forgives the last, and an author who takes the
        // slashes off a commented field below it needs no comma of their own.
        for (j, line) in part.value.into_iter().enumerate() {
            lines.push(if j + 1 == n { format!("{line},") } else { line });
        }
    }
    lines
}

/// The template for a runtime section of the agent in `scaffold`.
pub fn runtime_template(scaffold: &Scaffold, section: &str) -> Result<String, String> {
    let decl = scaffold.section(section).ok_or_else(|| {
        let known: Vec<&str> = scaffold.runtime.iter().map(|r| r.name.as_str()).collect();
        if known.is_empty() {
            format!("{} takes no runtime sections", scaffold.agent)
        } else {
            format!(
                "{} has no runtime section `{section}`; it has: {}",
                scaffold.agent,
                known.join(", ")
            )
        }
    })?;
    let fields = fields_of(&decl.body)?;
    let mut out = Vec::new();
    comment(
        &mut out,
        0,
        &format!(
            "polar-sign template: {}, section `{}`.",
            scaffold.agent, decl.name
        ),
    );
    if !decl.doc.is_empty() {
        comment(&mut out, 0, &decl.doc);
    }
    comment(
        &mut out,
        0,
        "Edit it, then sign it: polar-sign sign <this file> [--push]. Lines that start with // are ignored.",
    );
    out.push("{".to_string());
    out.push(format!("{INDENT}\"agent\": \"{}\",", scaffold.agent));
    out.push(format!("{INDENT}\"section\": \"{}\",", decl.name));
    comment(
        &mut out,
        1,
        "Must be higher than the last version applied to this section; a document with an older one is refused.",
    );
    out.push(format!("{INDENT}\"version\": 1,"));
    out.push(format!("{INDENT}\"body\": {{"));
    out.extend(object_lines(&fields, 2, &scaffold.agent));
    out.push(format!("{INDENT}}}"));
    out.push("}".to_string());
    Ok(out.join("\n") + "\n")
}

/// The template for the settings of the agent in `scaffold` that a signed document may change. Every one is
/// commented out: uncomment those to change. A secret setting is listed and cannot be set this way yet.
pub fn settings_template(scaffold: &Scaffold) -> Result<String, String> {
    let dynamic: Vec<_> = scaffold.dynamic_variables().collect();
    if dynamic.is_empty() {
        return Err(format!(
            "{} has no setting a signed document may change",
            scaffold.agent
        ));
    }
    let mut out = Vec::new();
    comment(
        &mut out,
        0,
        &format!(
            "polar-sign template: {}, the settings a signed document may change while it runs.",
            scaffold.agent
        ),
    );
    comment(
        &mut out,
        0,
        "Uncomment the ones to change (delete the slashes before the name) and give each a value, as text.",
    );
    comment(
        &mut out,
        0,
        "Then sign it: polar-sign sign <this file> [--push]. Lines that start with // are ignored.",
    );
    out.push("{".to_string());
    out.push(format!("{INDENT}\"agent\": \"{}\",", scaffold.agent));
    comment(
        &mut out,
        1,
        "Must be higher than the last version applied to this agent's settings.",
    );
    out.push(format!("{INDENT}\"version\": 1,"));
    out.push(format!("{INDENT}\"settings\": {{"));
    for v in dynamic {
        let mut head = v.type_name.clone();
        if let Some(d) = &v.default {
            head.push_str(&format!(" - default {d}"));
        }
        comment(&mut out, 2, &format!("{} ({head})", v.name));
        if !v.doc.is_empty() {
            comment(&mut out, 2, &v.doc);
        }
        if v.secret {
            comment(
                &mut out,
                2,
                "SECRET: a secret setting cannot be changed by a signed document yet.",
            );
        } else {
            let value = v.default.clone().unwrap_or(json!(""));
            comment(&mut out, 2, &format!("\"{}\": {value},", v.name));
        }
    }
    out.push(format!("{INDENT}}}"));
    out.push("}".to_string());
    Ok(out.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lenient;

    fn scaffold(runtime: Value, variables: Value) -> Scaffold {
        Scaffold::parse(
            &json!({
                "agent": {"name": "git-repo-observer"},
                "sections": [{"name": "builder", "variables": variables}],
                "runtime": runtime
            })
            .to_string(),
        )
        .unwrap()
    }

    fn observer() -> Scaffold {
        scaffold(
            json!([{"name": "credentials", "doc": "Credentials the observer may use.", "body": {
                "credentials": {"list_of": {
                    "name": {"type": "text of lowercase letters, digits and hyphens", "required": true,
                             "doc": "what a repository calls the credential"},
                    "host_pattern": {"type": "host pattern", "required": true},
                    "kind": {"type": "token | user-password", "required": true},
                    "username": {"type": "text", "required": false, "doc": "for a token the default is oauth2"},
                    "secret": {"type": "ciphertext", "required": true, "secret": true}
                }}
            }}]),
            json!([]),
        )
    }

    /// The file as the signer reads it (comment lines gone, a trailing comma forgiven). A comma missing between two
    /// values, or a value left out, is an error here.
    fn strict(text: &str) -> Value {
        lenient::parse(text).unwrap_or_else(|e| panic!("{e}\n{text}"))
    }

    #[test]
    fn a_runtime_template_is_a_file_the_signer_can_read_back() {
        let text = runtime_template(&observer(), "credentials").unwrap();
        let value = strict(&text);
        assert_eq!(value["agent"], "git-repo-observer");
        assert_eq!(value["section"], "credentials");
        assert_eq!(value["version"], 1);
        let entry = &value["body"]["credentials"][0];
        for field in ["name", "host_pattern", "kind", "secret"] {
            assert!(entry.get(field).is_some(), "{field} in {text}");
        }
        assert!(
            entry.get("username").is_none(),
            "an optional field with no default is only a comment"
        );
    }

    #[test]
    fn what_must_be_filled_in_comes_before_what_may_be() {
        let text = runtime_template(&observer(), "credentials").unwrap();
        let position = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle} in {text}"))
        };
        let optional = position("// \"username\"");
        for required in ["\"host_pattern\":", "\"kind\":", "\"name\":", "\"secret\":"] {
            assert!(position(required) < optional, "{required}");
        }
    }

    #[test]
    fn every_field_is_explained_where_it_is() {
        let text = runtime_template(&observer(), "credentials").unwrap();
        assert!(text.contains("// Credentials the observer may use."));
        assert!(text.contains("// required - host pattern"));
        assert!(text.contains("// optional - text"));
        assert!(text.contains("// for a token the default is oauth2"));
        assert!(text.contains("// what a repository calls the credential"));
        // the optional field is offered, commented out
        assert!(text.contains("// \"username\": \"\""));
    }

    #[test]
    fn a_secret_field_says_it_is_one_and_who_can_open_it() {
        let text = runtime_template(&observer(), "credentials").unwrap();
        assert!(text.contains("SECRET: type the value here"));
        assert!(text.contains("only git-repo-observer can open it"));
        assert!(text.contains("\"-\" asks for it on standard input"));
    }

    #[test]
    fn defaults_are_filled_in_and_empty_values_have_the_kind_the_type_promises() {
        let s = scaffold(
            json!([{"name": "repositories", "doc": "", "body": {
                "repositories": {"list_of": {
                    "url": {"type": "https URL of a host name", "required": true},
                    "frequency_secs": {"type": "whole number, 60 to 604800", "required": false, "default": 900},
                    "max_depth": {"type": "whole number", "required": true},
                    "shallow": {"type": "true or false", "required": true},
                    "refs": {"type": "list of full ref names", "required": true},
                    "other": {"type": "map from a to b", "required": true},
                    "odd": {"type": "Cedar policy text", "required": true}
                }}
            }}]),
            json!([]),
        );
        let value = lenient::parse(&runtime_template(&s, "repositories").unwrap()).unwrap();
        let entry = &value["body"]["repositories"][0];
        assert_eq!(entry["url"], "");
        assert_eq!(entry["frequency_secs"], 900);
        assert_eq!(entry["max_depth"], 0);
        assert_eq!(entry["shallow"], false);
        assert_eq!(entry["refs"], json!([]));
        assert_eq!(entry["other"], json!({}));
        assert_eq!(entry["odd"], "");
    }

    #[test]
    fn commas_are_right_whichever_fields_are_commented_out() {
        // the last field is the one that is only a comment: the one before it must not end in a comma
        let s = scaffold(
            json!([{"name": "s", "doc": "", "body": {
                "a": {"type": "text", "required": true},
                "b": {"type": "text", "required": false}
            }}]),
            json!([]),
        );
        let text = runtime_template(&s, "s").unwrap();
        assert_eq!(strict(&text)["body"], json!({"a": ""}));
        // and a body of only commented fields is an empty object
        let only = scaffold(
            json!([{"name": "s", "doc": "", "body": {"b": {"type": "text", "required": false}}}]),
            json!([]),
        );
        assert_eq!(
            strict(&runtime_template(&only, "s").unwrap())["body"],
            json!({})
        );
    }

    #[test]
    fn a_section_the_agent_does_not_have_is_refused_with_the_ones_it_has() {
        let err = runtime_template(&observer(), "nope").unwrap_err();
        assert!(
            err.contains("no runtime section `nope`") && err.contains("credentials"),
            "{err}"
        );
        let none = scaffold(json!([]), json!([]));
        assert!(
            runtime_template(&none, "x")
                .unwrap_err()
                .contains("takes no runtime sections")
        );
    }

    fn with_settings() -> Scaffold {
        scaffold(
            json!([]),
            json!([
                {"name": "RUST_LOG", "type": "text", "required": false, "secret": false, "default": "info",
                 "mutability": "dynamic", "doc": "log filter"},
                {"name": "POLAR_SHUTDOWN_BUDGET_SECS", "type": "whole number of seconds", "required": false,
                 "secret": false, "default": "40", "mutability": "static", "doc": "drain time"},
                {"name": "API_TOKEN", "type": "text", "required": false, "secret": true, "default": null,
                 "mutability": "dynamic", "doc": "a token"}
            ]),
        )
    }

    #[test]
    fn the_settings_template_offers_only_the_dynamic_settings_all_commented_out() {
        let text = settings_template(&with_settings()).unwrap();
        assert!(text.contains("// RUST_LOG (text - default \"info\")"));
        assert!(text.contains("// \"RUST_LOG\": \"info\","));
        assert!(
            !text.contains("POLAR_SHUTDOWN_BUDGET_SECS"),
            "a static setting is not offered"
        );
        assert!(
            text.contains("SECRET: a secret setting cannot be changed by a signed document yet.")
        );
        assert!(
            !text.contains("// \"API_TOKEN\""),
            "a secret setting is not offered a value"
        );
        let value = lenient::parse(&text).unwrap();
        assert_eq!(value["settings"], json!({}));
        assert_eq!(value["agent"], "git-repo-observer");
        assert_eq!(value["version"], 1);
    }

    #[test]
    fn uncommenting_a_setting_gives_a_file_that_still_reads() {
        let text = settings_template(&with_settings()).unwrap();
        let edited = text.replace("// \"RUST_LOG\": \"info\",", "\"RUST_LOG\": \"debug\",");
        assert_eq!(
            lenient::parse(&edited).unwrap()["settings"],
            json!({"RUST_LOG": "debug"})
        );
    }

    #[test]
    fn taking_the_slashes_off_an_optional_field_in_the_middle_leaves_a_file_that_reads() {
        let s = scaffold(
            json!([{"name": "s", "doc": "", "body": {
                "a": {"type": "text", "required": true},
                "b": {"type": "text", "required": false},
                "c": {"type": "whole number", "required": false},
                "d": {"type": "text", "required": true}
            }}]),
            json!([]),
        );
        let text = runtime_template(&s, "s").unwrap();
        let edited = text
            .replace("// \"b\": \"\",", "\"b\": \"x\",")
            .replace("// \"c\": 0,", "\"c\": 7,");
        assert_eq!(
            lenient::parse(&edited).unwrap()["body"],
            json!({"a": "", "d": "", "b": "x", "c": 7})
        );
    }

    #[test]
    fn an_agent_with_nothing_to_change_says_so() {
        let s = scaffold(
            json!([]),
            json!([{"name": "X", "type": "text", "mutability": "static"}]),
        );
        assert!(
            settings_template(&s)
                .unwrap_err()
                .contains("no setting a signed document may change")
        );
    }
}
