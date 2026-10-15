//! What an agent says about itself: the scaffold it prints for `<agent> --scaffold`
//! (docs/architecture/signed-configuration.md, "The author's flow"). From it the tool knows every setting an
//! agent lets a signed document change, every runtime section it takes, the fields of each, their defaults
//! and which are secret, so a template and the checks on the author's edits come from the agent and not from
//! a copy that can drift.

use serde_json::Value;

/// A setting (an environment variable the agent declared).
#[derive(Debug, Clone, PartialEq)]
pub struct Variable {
    pub name: String,
    pub type_name: String,
    pub required: bool,
    pub secret: bool,
    pub default: Option<Value>,
    /// `static` (read at start) or `dynamic` (a signed document may change it while the agent runs).
    pub mutability: String,
    pub doc: String,
}

/// A runtime section the agent declared, with the description of its body.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeDecl {
    pub name: String,
    pub doc: String,
    pub body: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scaffold {
    pub agent: String,
    pub variables: Vec<Variable>,
    pub runtime: Vec<RuntimeDecl>,
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

impl Scaffold {
    /// Reads the JSON an agent prints for `--scaffold`.
    pub fn parse(source: &str) -> Result<Scaffold, String> {
        let value: Value =
            serde_json::from_str(source).map_err(|e| format!("the scaffold is not JSON: {e}"))?;
        let agent = value
            .pointer("/agent/name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())
            .ok_or("the scaffold does not name its agent (`agent.name`)")?
            .to_string();

        let mut variables = Vec::new();
        for section in value
            .get("sections")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            for variable in section
                .get("variables")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let name = text(variable, "name");
                if name.is_empty() {
                    return Err("a variable in the scaffold has no name".to_string());
                }
                variables.push(Variable {
                    name,
                    type_name: text(variable, "type"),
                    required: variable
                        .get("required")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    secret: variable
                        .get("secret")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    default: variable.get("default").filter(|d| !d.is_null()).cloned(),
                    mutability: text(variable, "mutability"),
                    doc: text(variable, "doc"),
                });
            }
        }

        let mut runtime = Vec::new();
        for decl in value
            .get("runtime")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let name = text(decl, "name");
            let body = decl
                .get("body")
                .cloned()
                .ok_or_else(|| format!("the runtime section `{name}` has no body description"))?;
            if name.is_empty() {
                return Err("a runtime section in the scaffold has no name".to_string());
            }
            runtime.push(RuntimeDecl {
                name,
                doc: text(decl, "doc"),
                body,
            });
        }
        Ok(Scaffold {
            agent,
            variables,
            runtime,
        })
    }

    pub fn section(&self, name: &str) -> Option<&RuntimeDecl> {
        self.runtime.iter().find(|r| r.name == name)
    }

    pub fn variable(&self, name: &str) -> Option<&Variable> {
        self.variables.iter().find(|v| v.name == name)
    }

    /// The settings a signed document may change while the agent runs.
    pub fn dynamic_variables(&self) -> impl Iterator<Item = &Variable> {
        self.variables.iter().filter(|v| v.mutability == "dynamic")
    }
}

/// One field of a section's body, as the section describes it.
#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    /// A single value. `type_name` is prose; see [`Kind`] for the part of it that is checked.
    Leaf {
        type_name: String,
        required: bool,
        secret: bool,
        default: Option<Value>,
        doc: String,
    },
    /// A list of entries, each an object with these fields.
    List {
        entries: Vec<(String, Field)>,
        required: bool,
        doc: String,
    },
}

impl Field {
    pub fn required(&self) -> bool {
        match self {
            Field::Leaf { required, .. } | Field::List { required, .. } => *required,
        }
    }

    pub fn doc(&self) -> &str {
        match self {
            Field::Leaf { doc, .. } | Field::List { doc, .. } => doc,
        }
    }
}

/// The fields a body description names, in the order it names them. A description is an object from a field
/// name to a spec; a spec with `list_of` is a list of entries described the same way, and any other spec
/// is a single value with a `type`.
pub fn fields_of(description: &Value) -> Result<Vec<(String, Field)>, String> {
    let object = description
        .as_object()
        .ok_or("a body description must be an object of fields")?;
    let mut fields = Vec::new();
    for (name, spec) in object {
        let explicit = spec.get("required").and_then(Value::as_bool);
        let doc = text(spec, "doc");
        let field = if let Some(entries) = spec.get("list_of") {
            Field::List {
                entries: fields_of(entries).map_err(|e| format!("`{name}`: {e}"))?,
                // A section's body is nearly always its list, so a list is required unless it says it is not.
                required: explicit.unwrap_or(true),
                doc,
            }
        } else {
            let required = explicit.unwrap_or(false);
            let type_name = spec
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("the field `{name}` has no `type`"))?
                .to_string();
            Field::Leaf {
                type_name,
                required,
                secret: spec.get("secret").and_then(Value::as_bool).unwrap_or(false),
                default: spec.get("default").filter(|d| !d.is_null()).cloned(),
                doc,
            }
        };
        fields.push((name.clone(), field));
    }
    Ok(fields)
}

/// The kind of JSON value a field's `type` text promises, for the types whose text says so. The rest of the
/// text is prose for the author, and the agent's own validation judges it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Number,
    Flag,
    List,
    Map,
    Text,
}

/// What the `type` text of a field promises, or `None` when it promises nothing the tool can check. The
/// conventions agents' descriptions follow: `whole number…`, `true or false`, `list of…`, `map from…`, and a
/// handful of text forms (`text…`, `https URL…`, `host pattern`, `ciphertext`, `name of…`, `a | b` choices).
pub fn kind_of(type_name: &str) -> Option<Kind> {
    let t = type_name.trim();
    if t.starts_with("whole number") {
        Some(Kind::Number)
    } else if t.starts_with("true or false") {
        Some(Kind::Flag)
    } else if t.starts_with("list of") {
        Some(Kind::List)
    } else if t.starts_with("map from") {
        Some(Kind::Map)
    } else if t.starts_with("text")
        || t.starts_with("https URL")
        || t.starts_with("host pattern")
        || t.starts_with("ciphertext")
        || t.starts_with("name of")
        || t.contains(" | ")
    {
        Some(Kind::Text)
    } else {
        None
    }
}

/// Whether `value` is the kind of thing `kind` promises.
pub fn is_kind(kind: Kind, value: &Value) -> bool {
    match kind {
        Kind::Number => value.is_u64() || value.is_i64(),
        Kind::Flag => value.is_boolean(),
        Kind::List => value.is_array(),
        Kind::Map => value.is_object(),
        Kind::Text => value.is_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> String {
        json!({
            "agent": {"name": "git-repo-controller", "purpose": "p"},
            "sections": [
                {"name": "builder", "variables": [
                    {"name": "POLAR_SHUTDOWN_BUDGET_SECS", "type": "whole number of seconds", "required": false,
                     "secret": false, "default": "40", "mutability": "static", "doc": "how long to drain"},
                    {"name": "RUST_LOG", "type": "text", "required": false, "secret": false, "default": "info",
                     "mutability": "dynamic", "doc": "log filter"},
                    {"name": "API_TOKEN", "type": "text", "required": false, "secret": true, "default": null,
                     "mutability": "dynamic", "doc": "a token"}
                ]}
            ],
            "runtime": [
                {"name": "repositories", "doc": "the repositories", "body": {
                    "repositories": {"list_of": {
                        "url": {"type": "https URL of a host name", "required": true, "doc": "the key"},
                        "frequency_secs": {"type": "whole number, 60 to 604800", "required": false, "default": 900}
                    }}
                }}
            ]
        })
        .to_string()
    }

    #[test]
    fn a_scaffold_is_read_into_its_agent_variables_and_sections() {
        let s = Scaffold::parse(&sample()).unwrap();
        assert_eq!(s.agent, "git-repo-controller");
        assert_eq!(s.variables.len(), 3);
        assert_eq!(s.variable("RUST_LOG").unwrap().default, Some(json!("info")));
        assert_eq!(s.variable("API_TOKEN").unwrap().default, None);
        assert!(s.variable("API_TOKEN").unwrap().secret);
        assert!(s.section("repositories").is_some());
        assert!(s.section("nothing").is_none());
        assert!(s.variable("NOPE").is_none());
    }

    #[test]
    fn only_the_dynamic_settings_are_the_ones_a_document_may_change() {
        let s = Scaffold::parse(&sample()).unwrap();
        let names: Vec<&str> = s.dynamic_variables().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["RUST_LOG", "API_TOKEN"]);
    }

    #[test]
    fn a_scaffold_that_cannot_be_read_says_why() {
        assert!(
            Scaffold::parse("not json")
                .unwrap_err()
                .contains("not JSON")
        );
        assert!(
            Scaffold::parse("{}")
                .unwrap_err()
                .contains("does not name its agent")
        );
        let nameless =
            json!({"agent": {"name": "a"}, "runtime": [{"doc": "d", "body": {}}]}).to_string();
        assert!(Scaffold::parse(&nameless).is_err());
        let bodyless = json!({"agent": {"name": "a"}, "runtime": [{"name": "s"}]}).to_string();
        assert!(
            Scaffold::parse(&bodyless)
                .unwrap_err()
                .contains("no body description")
        );
        let unnamed_variable =
            json!({"agent": {"name": "a"}, "sections": [{"variables": [{"type": "text"}]}]})
                .to_string();
        assert!(Scaffold::parse(&unnamed_variable).is_err());
    }

    #[test]
    fn a_body_description_gives_its_fields_and_lists_of_entries() {
        let s = Scaffold::parse(&sample()).unwrap();
        let fields = fields_of(&s.section("repositories").unwrap().body).unwrap();
        assert_eq!(fields.len(), 1);
        let (name, Field::List { entries, .. }) = &fields[0] else {
            panic!("a list");
        };
        assert_eq!(name, "repositories");
        assert_eq!(entries.len(), 2);
        let by_name = |n: &str| &entries.iter().find(|(name, _)| name == n).unwrap().1;
        let Field::Leaf {
            required, default, ..
        } = by_name("url")
        else {
            panic!("a leaf");
        };
        assert!(*required);
        assert_eq!(*default, None);
        let Field::Leaf { default, .. } = by_name("frequency_secs") else {
            panic!("a leaf");
        };
        assert_eq!(*default, Some(json!(900)));
        assert!(
            fields[0].1.required(),
            "a list is required unless it says it is not"
        );
    }

    #[test]
    fn a_list_that_says_it_is_not_required_is_not() {
        let fields =
            fields_of(&json!({"l": {"required": false, "list_of": {"x": {"type": "text"}}}}))
                .unwrap();
        assert!(!fields[0].1.required());
    }

    #[test]
    fn a_description_that_is_not_one_is_refused_by_field() {
        assert!(fields_of(&json!([])).is_err());
        assert!(
            fields_of(&json!({"x": {"required": true}}))
                .unwrap_err()
                .contains("`x`")
        );
        let nested = fields_of(&json!({"l": {"list_of": {"y": {"doc": "no type"}}}})).unwrap_err();
        assert!(nested.contains("`l`") && nested.contains("`y`"), "{nested}");
    }

    #[test]
    fn the_type_text_promises_a_kind_only_when_it_says_so() {
        for (text, kind) in [
            ("whole number", Kind::Number),
            ("whole number, 60 to 604800", Kind::Number),
            ("whole number of seconds", Kind::Number),
            ("true or false", Kind::Flag),
            (
                "list of full ref names, such as refs/heads/main",
                Kind::List,
            ),
            ("map from an identity to the list of groups", Kind::Map),
            ("text", Kind::Text),
            ("text of lowercase letters, digits and hyphens", Kind::Text),
            ("https URL of a host name", Kind::Text),
            ("host pattern", Kind::Text),
            ("ciphertext", Kind::Text),
            ("name of a credential", Kind::Text),
            ("token | user-password", Kind::Text),
        ] {
            assert_eq!(kind_of(text), Some(kind), "{text}");
        }
        for prose in ["Cedar policy text", "comma-separated things", "anything"] {
            assert_eq!(kind_of(prose), None, "{prose}");
        }
    }

    #[test]
    fn each_kind_accepts_its_own_json_and_nothing_else() {
        let values = [
            json!(1),
            json!(true),
            json!([1]),
            json!({"a": 1}),
            json!("s"),
            json!(null),
            json!(1.5),
        ];
        for (kind, accepts) in [
            (Kind::Number, 0),
            (Kind::Flag, 1),
            (Kind::List, 2),
            (Kind::Map, 3),
            (Kind::Text, 4),
        ] {
            for (i, value) in values.iter().enumerate() {
                assert_eq!(is_kind(kind, value), i == accepts, "{kind:?} {value}");
            }
        }
    }
}
