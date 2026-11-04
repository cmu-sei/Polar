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

//! The `mappings` runtime section: which repositories are built, and with what.
//!
//! A signed document, authored with `polar-sign` and carried by the scheduler, replaces it whole. Nothing
//! secret is in it: a mapping names the credentials its builds may be given (by name), and the orchestrator
//! decides from this, never from a build request, which a build receives.

use crate::config::RepoMapping;
use polar_agent_builder::RuntimeSection;
use polar_agent_builder::runtime::SectionAuthority;
use serde_json::{Map, Value};

pub const SECTION_NAME: &str = "mappings";
pub const MAX_ENTRIES: usize = 500;
const FIELDS: [&str; 4] = ["url", "pipeline_image", "command", "credentials"];

pub static MAPPINGS: RuntimeSection = RuntimeSection {
    authority: SectionAuthority::Runtime,
    name: SECTION_NAME,
    doc: "The repositories build-orchestrator builds, and with what. Replaced whole by each version.",
    body: r#"{"repositories": {"list_of": {"url": {"type": "https URL of the repository", "required": true, "doc": "the key of the entry; it must be the URL the git observer reports"}, "pipeline_image": {"type": "image reference pinned by digest", "required": false, "doc": "what the build runs in; left out, the build fails cleanly"}, "command": {"type": "list of text", "required": false, "doc": "the pipeline's command, in place of the configuration file's"}, "credentials": {"type": "list of credential names", "required": false, "doc": "the credentials a build of this repository may be given, by name; none unless named"}}}}"#,
    validate,
};

fn validate(body: &Value) -> Result<(), Vec<String>> {
    parse(body).map(|_| ())
}

pub fn parse(body: &Value) -> Result<Vec<RepoMapping>, Vec<String>> {
    let Some(object) = body.as_object() else {
        return Err(vec!["the body must be an object".to_string()]);
    };
    let mut problems: Vec<String> = object
        .keys()
        .filter(|k| k.as_str() != "repositories")
        .map(|k| format!("`{k}` is not a field of this section"))
        .collect();
    let Some(list) = object.get("repositories").and_then(Value::as_array) else {
        problems.push("`repositories` must be a list of entries".to_string());
        return Err(problems);
    };
    if list.len() > MAX_ENTRIES {
        problems.push(format!(
            "`repositories` has {} entries, and at most {MAX_ENTRIES} are taken",
            list.len()
        ));
    }
    let mut mappings: Vec<RepoMapping> = Vec::new();
    for (i, item) in list.iter().enumerate() {
        match entry(i, item) {
            Ok(mapping) => {
                if mappings.iter().any(|m| m.repo_url == mapping.repo_url) {
                    problems.push(format!("entry {i}: the repository is named more than once"));
                }
                mappings.push(mapping);
            }
            Err(mut more) => problems.append(&mut more),
        }
    }
    if problems.is_empty() {
        Ok(mappings)
    } else {
        Err(problems)
    }
}

fn text_list(
    object: &Map<String, Value>,
    label: &str,
    key: &str,
    problems: &mut Vec<String>,
) -> Vec<String> {
    match object.get(key) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| match v.as_str() {
                Some(s) if !s.is_empty() => Some(s.to_string()),
                _ => {
                    problems.push(format!("{label}: `{key}` must be a list of non-empty text"));
                    None
                }
            })
            .collect(),
        Some(_) => {
            problems.push(format!("{label}: `{key}` must be a list of text"));
            Vec::new()
        }
    }
}

fn entry(i: usize, item: &Value) -> Result<RepoMapping, Vec<String>> {
    let label = format!("entry {i}");
    let Some(object) = item.as_object() else {
        return Err(vec![format!("{label}: must be an object")]);
    };
    let mut problems: Vec<String> = object
        .keys()
        .filter(|k| !FIELDS.contains(&k.as_str()))
        .map(|k| format!("{label}: `{k}` is not a field of an entry"))
        .collect();

    let repo_url = match object.get("url").and_then(Value::as_str) {
        Some(url) if url.starts_with("https://") && !url.contains('@') => url.to_string(),
        Some(_) => {
            problems.push(format!(
                "{label}: `url` must be an https URL with no login in it"
            ));
            String::new()
        }
        None => {
            problems.push(format!("{label}: `url` is required"));
            String::new()
        }
    };
    let pipeline_image = match object.get("pipeline_image") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(_) => {
            problems.push(format!("{label}: `pipeline_image` must be text"));
            None
        }
    };
    let command = text_list(object, &label, "command", &mut problems);
    let credentials = text_list(object, &label, "credentials", &mut problems);

    if problems.is_empty() {
        Ok(RepoMapping {
            repo_url,
            pipeline_image,
            command: (!command.is_empty()).then_some(command),
            credentials,
        })
    } else {
        Err(problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_mapping_reads() {
        let parsed = parse(&json!({"repositories": [
            {"url": "https://git.example.org/a/b.git", "pipeline_image": "reg/x@sha256:1", "credentials": ["git-a"]}
        ]}))
        .unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].credentials, ["git-a"]);
        assert!(parsed[0].command.is_none());
    }

    #[test]
    fn every_problem_is_named_and_none_is_a_value() {
        let problems = parse(&json!({"repositories": [
            {"url": "http://x/y", "extra": 1},
            {"url": "https://u:p@x/y"},
            {"url": "https://x/y", "credentials": "git-a"},
            {"url": "https://x/y"}, {"url": "https://x/y"},
        ], "other": 1}))
        .unwrap_err();
        for expected in [
            "`other`",
            "entry 0: `extra`",
            "entry 0: `url` must be",
            "entry 1",
            "entry 2: `credentials`",
            "more than once",
        ] {
            assert!(
                problems.iter().any(|p| p.contains(expected)),
                "{expected} in {problems:?}"
            );
        }
        assert!(
            !problems.iter().any(|p| p.contains("u:p")),
            "a login must never be echoed"
        );
    }

    #[test]
    fn an_empty_list_withdraws_every_mapping() {
        assert!(parse(&json!({"repositories": []})).unwrap().is_empty());
    }
}
