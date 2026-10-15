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

//! The configuration document: what an agent is given instead of an environment.
//!
//! A scaffold is the declarations written out (every variable, its type, default, secrecy,
//! mutability and documentation) with every value empty. Someone fills the values in, and the
//! same document, populated, is what the agent reads at runtime, through [`FileEnv`]. Because it
//! is one shape, the schema, the empty form and the filled form need no conversion, and
//! nothing about the agent is written down anywhere but in its code.
//!
//! A value is a literal, or a reference to a variable of the process environment
//! (`{"env": "NAME"}`). A secret is never a literal: the document is meant to sit in a
//! ConfigMap, and the real value arrives through the environment from a Secret.

use crate::{Env, Kind, Mutability, Section};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;

/// The version of the document format this code reads and writes.
pub const FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub format: u32,
    pub sections: Vec<DocSection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocSection {
    pub name: String,
    /// The capability whose need brought this section in; none for a section every agent has.
    #[serde(default)]
    pub capability: Option<String>,
    pub variables: Vec<DocVariable>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocVariable {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    /// The document must give a value (or the environment must, through a reference).
    pub required: bool,
    pub secret: bool,
    pub default: Option<String>,
    /// `static` or `dynamic`.
    pub mutability: String,
    pub doc: String,
    #[serde(default)]
    pub value: Option<DocValue>,
    /// Where the value came from (`file`, `default`, `unset`), in a document printed for a
    /// person or a tool. Not read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// What the variable resolves to now, a secret as `<hidden>`. Not read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
}

/// What fills a variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DocValue {
    Literal(String),
    /// The value of this variable of the process environment.
    Reference {
        env: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentError {
    Io { path: String, reason: String },
    Parse(String),
    Format { found: u32 },
    Invalid(Vec<String>),
}

impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DocumentError::Io { path, reason } => write!(f, "cannot read {path}: {reason}"),
            DocumentError::Parse(reason) => {
                write!(f, "the configuration document is not valid: {reason}")
            }
            DocumentError::Format { found } => write!(
                f,
                "the configuration document is format {found}; this agent reads format {FORMAT}"
            ),
            DocumentError::Invalid(problems) => {
                write!(
                    f,
                    "the configuration document has problems: {}",
                    problems.join("; ")
                )
            }
        }
    }
}

impl std::error::Error for DocumentError {}

impl Document {
    /// The declarations written out, every value empty. Each section is paired with the
    /// capability that brought it in, if any.
    pub fn scaffold(sections: &[(&Section, Option<&str>)]) -> Document {
        Document {
            format: FORMAT,
            sections: sections
                .iter()
                .map(|(section, capability)| DocSection {
                    name: section.name.to_string(),
                    capability: capability.map(str::to_string),
                    variables: section
                        .describe()
                        .into_iter()
                        .map(|desc| DocVariable {
                            name: desc.name.to_string(),
                            type_name: desc.type_name.to_string(),
                            required: desc.kind == Kind::Required,
                            secret: desc.secret,
                            default: desc.default.map(str::to_string),
                            mutability: match desc.mutability {
                                Mutability::Static => "static",
                                Mutability::Dynamic => "dynamic",
                            }
                            .to_string(),
                            doc: desc.doc.to_string(),
                            value: None,
                            source: None,
                            resolved: None,
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    pub fn to_json(&self) -> String {
        // A document holds only strings, booleans and lists of them: it always serialises.
        serde_json::to_string_pretty(self).expect("a configuration document serialises")
    }

    pub fn from_json(text: &str) -> Result<Document, DocumentError> {
        // Look at the version before the rest, so a document of another format is told so
        // and not reported as malformed because its shape differs.
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| DocumentError::Parse(e.to_string()))?;
        if let Some(found) = value.get("format").and_then(|f| f.as_u64())
            && found != u64::from(FORMAT)
        {
            return Err(DocumentError::Format {
                found: found as u32,
            });
        }
        serde_json::from_value(value).map_err(|e| DocumentError::Parse(e.to_string()))
    }

    /// What is wrong with the document itself, apart from whether an agent likes its values.
    /// A secret's value is never repeated.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for variable in self.sections.iter().flat_map(|s| s.variables.iter()) {
            *seen.entry(variable.name.as_str()).or_default() += 1;
            match &variable.value {
                Some(DocValue::Literal(_)) if variable.secret => problems.push(format!(
                    "{} is a secret and is given as a literal; give it as a reference to an \
                     environment variable, {{\"env\": NAME}}",
                    variable.name
                )),
                Some(DocValue::Reference { env }) if env.trim().is_empty() => {
                    problems.push(format!(
                        "{} refers to an environment variable and names no variable",
                        variable.name
                    ))
                }
                _ => {}
            }
        }
        let mut repeated: Vec<_> = seen
            .into_iter()
            .filter(|(_, n)| *n > 1)
            .map(|(name, _)| name)
            .collect();
        repeated.sort();
        for name in repeated {
            problems.push(format!("{name} is declared more than once"));
        }
        problems
    }
}

/// A document as an environment: the values it holds, with references resolved against the
/// process environment. A name the document does not hold is not set.
pub struct FileEnv {
    values: HashMap<String, DocValue>,
    process: Box<dyn Env>,
}

impl FileEnv {
    pub fn new(document: &Document, process: impl Env + 'static) -> FileEnv {
        let values = document
            .sections
            .iter()
            .flat_map(|section| section.variables.iter())
            .filter_map(|variable| Some((variable.name.clone(), variable.value.clone()?)))
            .collect();
        FileEnv {
            values,
            process: Box::new(process),
        }
    }

    /// Reads and checks the document at `path`.
    pub fn open(path: &Path, process: impl Env + 'static) -> Result<FileEnv, DocumentError> {
        let text = std::fs::read_to_string(path).map_err(|e| DocumentError::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        let document = Document::from_json(&text)?;
        let problems = document.problems();
        if !problems.is_empty() {
            return Err(DocumentError::Invalid(problems));
        }
        Ok(FileEnv::new(&document, process))
    }
}

/// The variable that names the configuration document. It is the one variable read before the
/// document is, so it is not declared in a section: it says where the declarations' values are.
pub const CONFIG_PATH_VARIABLE: &str = "POLAR_CONFIG";

/// Where an agent's values come from: the configuration document named by `POLAR_CONFIG` if
/// that is set (references in it resolved against `process`), and otherwise `process` itself.
/// Says which, so a report can.
pub fn from_environment(
    process: impl Env + 'static,
) -> Result<(Box<dyn Env>, crate::Source), DocumentError> {
    let path = process
        .var(CONFIG_PATH_VARIABLE)
        .filter(|path| !path.trim().is_empty());
    match path {
        None => Ok((Box::new(process), crate::Source::Env)),
        Some(path) => {
            let file = FileEnv::open(Path::new(path.trim()), process)?;
            Ok((Box::new(file), crate::Source::File))
        }
    }
}

impl Env for FileEnv {
    fn var(&self, name: &str) -> Option<String> {
        match self.values.get(name)? {
            DocValue::Literal(value) => Some(value.clone()),
            DocValue::Reference { env } => self.process.var(env),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Defaulted, Optional, Required, Source};

    static ENDPOINT: Required<String> = Required::text("DOC_ENDPOINT", "the address of the thing");
    static PASSWORD: Required<String> =
        Required::text("DOC_PASSWORD", "the password for the thing").secret();
    static TICK: Defaulted<u64> =
        Defaulted::seconds("DOC_TICK_SECS", "seconds between ticks", "30");
    static NOTE: Optional<String> = Optional::text("DOC_NOTE", "a note, if wanted");

    static THING: Section = Section {
        name: "thing",
        vars: &[&ENDPOINT, &PASSWORD, &TICK, &NOTE],
        relations: |_| Vec::new(),
    };

    fn no_process() -> impl Env + 'static {
        |_: &str| None::<String>
    }

    fn variable<'a>(doc: &'a Document, name: &str) -> &'a DocVariable {
        doc.sections
            .iter()
            .flat_map(|s| s.variables.iter())
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("{name} is not in the document"))
    }

    #[test]
    fn a_scaffold_lists_every_variable_with_its_schema_and_no_value() {
        let doc = Document::scaffold(&[(&THING, Some("thing-cap"))]);
        assert_eq!(doc.format, FORMAT);
        assert_eq!(doc.sections.len(), 1);
        assert_eq!(doc.sections[0].name, "thing");
        assert_eq!(doc.sections[0].capability.as_deref(), Some("thing-cap"));

        let endpoint = variable(&doc, "DOC_ENDPOINT");
        assert!(endpoint.required && !endpoint.secret);
        assert_eq!(endpoint.default, None);
        assert_eq!(endpoint.value, None);
        assert!(endpoint.doc.contains("address"));

        assert!(variable(&doc, "DOC_PASSWORD").secret);

        let tick = variable(&doc, "DOC_TICK_SECS");
        assert!(!tick.required, "a defaulted variable need not be given");
        assert_eq!(tick.default.as_deref(), Some("30"));
        assert_eq!(tick.value, None, "the default is the default, not a value");
        assert_eq!(tick.mutability, "static");

        let note = variable(&doc, "DOC_NOTE");
        assert!(!note.required);
        assert_eq!(note.default, None);
    }

    #[test]
    fn a_document_survives_a_trip_through_json() {
        let mut doc = Document::scaffold(&[(&THING, None)]);
        doc.sections[0].variables[0].value = Some(DocValue::Literal("db:7687".into()));
        doc.sections[0].variables[1].value = Some(DocValue::Reference {
            env: "THE_SECRET".into(),
        });
        let text = doc.to_json();
        assert_eq!(Document::from_json(&text).unwrap(), doc);
    }

    #[test]
    fn a_value_is_a_string_or_an_environment_reference_in_the_json() {
        let text = r#"{ "format": 1, "sections": [ { "name": "thing", "variables": [
            { "name": "DOC_ENDPOINT", "type": "text", "required": true, "secret": false,
              "default": null, "mutability": "static", "doc": "d", "value": "db:7687" },
            { "name": "DOC_PASSWORD", "type": "text", "required": true, "secret": true,
              "default": null, "mutability": "static", "doc": "d", "value": { "env": "THE_SECRET" } }
        ] } ] }"#;
        let doc = Document::from_json(text).unwrap();
        assert_eq!(
            variable(&doc, "DOC_ENDPOINT").value,
            Some(DocValue::Literal("db:7687".into()))
        );
        assert_eq!(
            variable(&doc, "DOC_PASSWORD").value,
            Some(DocValue::Reference {
                env: "THE_SECRET".into()
            })
        );
    }

    #[test]
    fn a_document_of_another_format_is_refused_saying_which() {
        let error = Document::from_json(r#"{ "format": 7, "sections": [] }"#).unwrap_err();
        assert_eq!(error, DocumentError::Format { found: 7 });
        assert!(error.to_string().contains("format 7"));
    }

    #[test]
    fn text_that_is_not_a_document_is_a_parse_error() {
        assert!(matches!(
            Document::from_json("nonsense"),
            Err(DocumentError::Parse(_))
        ));
    }

    #[test]
    fn a_secret_given_as_a_literal_is_a_problem() {
        let mut doc = Document::scaffold(&[(&THING, None)]);
        doc.sections[0].variables[1].value = Some(DocValue::Literal("hunter2".into()));
        let problems = doc.problems();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("DOC_PASSWORD"));
        assert!(
            !problems[0].contains("hunter2"),
            "a secret is not repeated: {problems:?}"
        );
    }

    #[test]
    fn a_secret_given_as_a_reference_is_fine() {
        let mut doc = Document::scaffold(&[(&THING, None)]);
        doc.sections[0].variables[1].value = Some(DocValue::Reference {
            env: "THE_SECRET".into(),
        });
        assert_eq!(doc.problems(), Vec::<String>::new());
    }

    #[test]
    fn a_variable_in_two_places_and_an_empty_reference_are_problems() {
        let mut doc = Document::scaffold(&[(&THING, None), (&THING, None)]);
        doc.sections[0].variables[0].value = Some(DocValue::Reference { env: "".into() });
        let problems = doc.problems().join(" | ");
        assert!(problems.contains("more than once"), "{problems}");
        assert!(problems.contains("DOC_ENDPOINT"), "{problems}");
        assert!(problems.contains("names no variable"), "{problems}");
    }

    #[test]
    fn a_file_env_serves_literals_and_nothing_it_does_not_hold() {
        let mut doc = Document::scaffold(&[(&THING, None)]);
        doc.sections[0].variables[0].value = Some(DocValue::Literal("db:7687".into()));
        let env = FileEnv::new(&doc, no_process());
        assert_eq!(env.var("DOC_ENDPOINT"), Some("db:7687".to_string()));
        assert_eq!(env.var("DOC_NOTE"), None, "no value");
        assert_eq!(env.var("SOMETHING_ELSE"), None, "not in the document");
    }

    #[test]
    fn a_reference_is_resolved_from_the_process_environment() {
        let mut doc = Document::scaffold(&[(&THING, None)]);
        doc.sections[0].variables[1].value = Some(DocValue::Reference {
            env: "THE_SECRET".into(),
        });
        let process = |name: &str| (name == "THE_SECRET").then(|| "hunter2".to_string());
        let env = FileEnv::new(&doc, process);
        assert_eq!(env.var("DOC_PASSWORD"), Some("hunter2".to_string()));
    }

    #[test]
    fn a_reference_to_a_variable_that_is_not_set_is_not_set() {
        let mut doc = Document::scaffold(&[(&THING, None)]);
        doc.sections[0].variables[1].value = Some(DocValue::Reference {
            env: "THE_SECRET".into(),
        });
        let env = FileEnv::new(&doc, no_process());
        assert_eq!(env.var("DOC_PASSWORD"), None);
    }

    #[test]
    fn a_file_is_opened_and_checked() {
        let dir = std::env::temp_dir().join(format!("polar-env-doc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut doc = Document::scaffold(&[(&THING, None)]);
        doc.sections[0].variables[0].value = Some(DocValue::Literal("db:7687".into()));
        let good = dir.join("good.json");
        std::fs::write(&good, doc.to_json()).unwrap();
        let env = FileEnv::open(&good, no_process()).unwrap();
        assert_eq!(env.var("DOC_ENDPOINT"), Some("db:7687".to_string()));

        doc.sections[0].variables[1].value = Some(DocValue::Literal("hunter2".into()));
        let bad = dir.join("bad.json");
        std::fs::write(&bad, doc.to_json()).unwrap();
        assert!(matches!(
            FileEnv::open(&bad, no_process()),
            Err(DocumentError::Invalid(_))
        ));

        assert!(matches!(
            FileEnv::open(&dir.join("missing.json"), no_process()),
            Err(DocumentError::Io { .. })
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_config_built_on_a_document_reads_typed_values_the_same_way_and_says_file() {
        let mut doc = Document::scaffold(&[(&THING, None)]);
        doc.sections[0].variables[0].value = Some(DocValue::Literal("db:7687".into()));
        doc.sections[0].variables[2].value = Some(DocValue::Literal("15".into()));
        let env = FileEnv::new(&doc, no_process());

        let config = Config::builder(&env)
            .register(&THING)
            .source(Source::File)
            .build();
        assert_eq!(config.read(&ENDPOINT).unwrap(), "db:7687");
        assert_eq!(config.read(&TICK).unwrap(), 15);

        let report = config.report();
        let source_of = |name: &str| report.iter().find(|r| r.name == name).unwrap().source;
        assert_eq!(source_of("DOC_ENDPOINT"), Source::File);
        assert_eq!(source_of("DOC_TICK_SECS"), Source::File);
        assert_eq!(source_of("DOC_NOTE"), Source::Unset);
        assert_eq!(Source::File.as_str(), "file");
    }

    #[test]
    fn the_defaults_still_apply_to_what_the_document_does_not_give() {
        let doc = Document::scaffold(&[(&THING, None)]);
        let env = FileEnv::new(&doc, no_process());
        let config = Config::builder(&env)
            .register(&THING)
            .source(Source::File)
            .build();
        assert_eq!(config.read(&TICK).unwrap(), 30);
        let report = config.report();
        assert_eq!(
            report
                .iter()
                .find(|r| r.name == "DOC_TICK_SECS")
                .unwrap()
                .source,
            Source::Default
        );
    }

    #[test]
    fn a_missing_required_value_is_a_problem_the_config_reports() {
        let doc = Document::scaffold(&[(&THING, None)]);
        let env = FileEnv::new(&doc, no_process());
        let config = Config::builder(&env)
            .register(&THING)
            .source(Source::File)
            .build();
        let problems = config.check().join(" | ");
        assert!(problems.contains("DOC_ENDPOINT"), "{problems}");
        assert!(problems.contains("DOC_PASSWORD"), "{problems}");
    }

    fn document_file(name: &str, document: &Document) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("polar-env-doc-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, document.to_json()).unwrap();
        path
    }

    #[test]
    fn without_a_document_the_process_environment_is_the_source() {
        let process = |name: &str| (name == "DOC_ENDPOINT").then(|| "from-env".to_string());
        let (env, source) = from_environment(process).unwrap();
        assert_eq!(source, Source::Env);
        assert_eq!(env.var("DOC_ENDPOINT"), Some("from-env".to_string()));
    }

    #[test]
    fn a_blank_path_is_no_document() {
        let process = |name: &str| (name == CONFIG_PATH_VARIABLE).then(|| "  ".to_string());
        let (_, source) = from_environment(process).unwrap();
        assert_eq!(source, Source::Env);
    }

    #[test]
    fn a_named_document_is_the_source_and_the_environment_is_not() {
        let mut doc = Document::scaffold(&[(&THING, None)]);
        doc.sections[0].variables[0].value = Some(DocValue::Literal("from-file".into()));
        doc.sections[0].variables[1].value = Some(DocValue::Reference {
            env: "THE_SECRET".into(),
        });
        let path = document_file("named", &doc);
        let path_text = path.display().to_string();

        let process = move |name: &str| match name {
            CONFIG_PATH_VARIABLE => Some(path_text.clone()),
            "THE_SECRET" => Some("hunter2".to_string()),
            // The document does not mention it, so the agent must not see it.
            "DOC_NOTE" => Some("leaked from the environment".to_string()),
            _ => None,
        };
        let (env, source) = from_environment(process).unwrap();
        assert_eq!(source, Source::File);
        assert_eq!(env.var("DOC_ENDPOINT"), Some("from-file".to_string()));
        assert_eq!(
            env.var("DOC_PASSWORD"),
            Some("hunter2".to_string()),
            "the reference"
        );
        assert_eq!(
            env.var("DOC_NOTE"),
            None,
            "the environment is not a second source"
        );
    }

    #[test]
    fn a_named_document_that_cannot_be_read_is_an_error_not_a_fall_back() {
        let process =
            |name: &str| (name == CONFIG_PATH_VARIABLE).then(|| "/no/such/file.json".to_string());
        assert!(matches!(
            from_environment(process),
            Err(DocumentError::Io { .. })
        ));
    }
}
