//! Checking the body an author edited against the description the agent gave of the section, before anything
//! is signed. What it can judge from the description: that the body is an object, that no field is a stranger,
//! that required fields are there, that lists are lists of objects, and that a value is the kind its type text
//! promises (a whole number, a list, text...). The rest of the type text is prose, and the agent's own
//! validation judges it when the document arrives.
//!
//! Problems name the field and never repeat a value: a body may hold a secret.

use crate::scaffold::{Field, fields_of, is_kind, kind_of};
use serde_json::Value;

/// Every problem with `body` against `description`, each naming where it is (`credentials[1].kind`).
pub fn check_body(description: &Value, body: &Value) -> Vec<String> {
    let fields = match fields_of(description) {
        Ok(fields) => fields,
        Err(e) => {
            return vec![format!(
                "the agent's description of this section cannot be read: {e}"
            )];
        }
    };
    let mut problems = Vec::new();
    check_object(&fields, body, "", &mut problems);
    problems
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{path}.{name}")
    }
}

fn check_object(fields: &[(String, Field)], value: &Value, path: &str, problems: &mut Vec<String>) {
    let Some(object) = value.as_object() else {
        let at = if path.is_empty() { "the body" } else { path };
        problems.push(format!("{at} must be an object"));
        return;
    };
    for key in object.keys() {
        if !fields.iter().any(|(name, _)| name == key) {
            problems.push(format!("`{}` is not a field here", join(path, key)));
        }
    }
    for (name, field) in fields {
        let at = join(path, name);
        let present = object.get(name).filter(|v| !v.is_null());
        let Some(value) = present else {
            let has_default = matches!(
                field,
                Field::Leaf {
                    default: Some(_),
                    ..
                }
            );
            if field.required() && !has_default {
                problems.push(format!("`{at}` is required"));
            }
            continue;
        };
        match field {
            Field::Leaf {
                type_name, secret, ..
            } => {
                if *secret && !value.is_string() {
                    problems.push(format!("`{at}` is a secret and must be text"));
                } else if let Some(kind) = kind_of(type_name)
                    && !is_kind(kind, value)
                {
                    problems.push(format!("`{at}` must be {} ({type_name})", describe(kind)));
                }
            }
            Field::List { entries, .. } => match value.as_array() {
                None => problems.push(format!("`{at}` must be a list")),
                Some(list) => {
                    for (i, item) in list.iter().enumerate() {
                        check_object(entries, item, &format!("{at}[{i}]"), problems);
                    }
                }
            },
        }
    }
}

fn describe(kind: crate::scaffold::Kind) -> &'static str {
    use crate::scaffold::Kind;
    match kind {
        Kind::Number => "a whole number",
        Kind::Flag => "true or false",
        Kind::List => "a list",
        Kind::Map => "an object",
        Kind::Text => "text",
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
                "kind": {"type": "token | user-password", "required": true},
                "port": {"type": "whole number", "required": false},
                "secret": {"type": "ciphertext", "required": true, "secret": true},
                "mode": {"type": "text", "required": true, "default": "token"}
            }},
            "limit": {"type": "whole number", "required": false}
        })
    }

    fn entry() -> Value {
        json!({"name": "gh", "kind": "token", "secret": "x"})
    }

    fn problems(body: Value) -> Vec<String> {
        check_body(&description(), &body)
    }

    #[test]
    fn a_body_that_fits_has_no_problems() {
        assert!(problems(json!({"credentials": [entry()]})).is_empty());
        assert!(problems(json!({"credentials": [entry(), entry()], "limit": 5})).is_empty());
        assert!(problems(json!({"credentials": []})).is_empty());
    }

    #[test]
    fn a_required_field_that_is_missing_or_null_is_named() {
        let p = problems(json!({}));
        assert_eq!(p, vec!["`credentials` is required".to_string()]);
        let mut e = entry();
        e.as_object_mut().unwrap().remove("kind");
        e["secret"] = json!(null);
        let p = problems(json!({"credentials": [e]}));
        assert!(
            p.contains(&"`credentials[0].kind` is required".to_string()),
            "{p:?}"
        );
        assert!(
            p.contains(&"`credentials[0].secret` is required".to_string()),
            "{p:?}"
        );
    }

    #[test]
    fn a_required_field_with_a_default_may_be_left_out() {
        // `mode` is required but has a default, and the entry does not carry it
        assert!(problems(json!({"credentials": [entry()]})).is_empty());
    }

    #[test]
    fn a_field_the_section_does_not_have_is_named() {
        let p = problems(json!({"credentials": [entry()], "extra": 1}));
        assert_eq!(p, vec!["`extra` is not a field here".to_string()]);
        let mut e = entry();
        e["stranger"] = json!(true);
        let p = problems(json!({"credentials": [e]}));
        assert_eq!(
            p,
            vec!["`credentials[0].stranger` is not a field here".to_string()]
        );
    }

    #[test]
    fn values_of_the_wrong_kind_are_named_with_the_kind_they_should_be() {
        let mut e = entry();
        e["port"] = json!("80");
        e["name"] = json!(7);
        let p = problems(json!({"credentials": [e], "limit": true}));
        assert!(
            p.contains(&"`credentials[0].port` must be a whole number (whole number)".to_string()),
            "{p:?}"
        );
        assert!(
            p.contains(&"`credentials[0].name` must be text (text)".to_string()),
            "{p:?}"
        );
        assert!(
            p.contains(&"`limit` must be a whole number (whole number)".to_string()),
            "{p:?}"
        );
    }

    #[test]
    fn a_list_must_be_a_list_of_objects() {
        assert_eq!(
            problems(json!({"credentials": {"name": "gh"}})),
            vec!["`credentials` must be a list".to_string()]
        );
        assert_eq!(
            problems(json!({"credentials": ["gh"]})),
            vec!["credentials[0] must be an object".to_string()]
        );
        assert_eq!(
            problems(json!([])),
            vec!["the body must be an object".to_string()]
        );
    }

    #[test]
    fn a_secret_that_is_not_text_is_named_and_a_problem_never_repeats_a_value() {
        let mut e = entry();
        e["secret"] = json!(12345);
        e["name"] = json!("PLAINTEXT-CANARY");
        e["kind"] = json!(["PLAINTEXT-CANARY"]);
        let p = problems(json!({"credentials": [e]}));
        assert!(
            p.iter()
                .any(|m| m.contains("`credentials[0].secret` is a secret and must be text"))
        );
        assert!(
            p.iter()
                .all(|m| !m.contains("CANARY") && !m.contains("12345")),
            "{p:?}"
        );
    }

    #[test]
    fn a_type_that_promises_no_kind_accepts_any_value() {
        let d = json!({"source": {"type": "Cedar policy text", "required": true}});
        for value in [json!("x"), json!(1), json!({"a": 1})] {
            assert!(check_body(&d, &json!({"source": value})).is_empty());
        }
    }

    #[test]
    fn a_description_that_cannot_be_read_is_reported_not_trusted() {
        let p = check_body(&json!({"x": {"required": true}}), &json!({}));
        assert!(p[0].contains("cannot be read"), "{p:?}");
    }
}
