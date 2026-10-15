//! The `repositories` runtime section: which repositories are observed, and how
//! (docs/architecture/git-controller.md). A pure reading of a body into the configurations the observer is
//! told of; nothing here touches a network, a credential or the clock.
//!
//! The controller never sees a credential. An entry names the one it needs, and the observer, which holds the
//! credentials, finds out whether it has it.

use git_agent_common::RepoObservationConfig;
use git_agent_common::remote::{credential_name_problem, https_host};
use polar::graph::nodes::git::RepoId;
use polar_agent_builder::RuntimeSection;
use polar_agent_builder::runtime::SectionAuthority;
use serde_json::Value;

/// What the section is called in a document.
pub const SECTION_NAME: &str = "repositories";
/// Most repositories one version may name.
pub const MAX_ENTRIES: usize = 500;
/// Longest repository URL.
pub const MAX_URL: usize = 2048;
/// Most refs one repository may list, and the longest one.
pub const MAX_REFS: usize = 64;
pub const MAX_REF: usize = 256;
/// The shortest time between two fetches of a repository, in seconds, and the longest.
pub const MIN_FREQUENCY_SECS: u64 = 60;
pub const MAX_FREQUENCY_SECS: u64 = 7 * 24 * 3600;
pub const DEFAULT_FREQUENCY_SECS: u64 = 900;
/// Commits walked per ref: the bounds and the default.
pub const MAX_DEPTH: u64 = 10_000;
pub const DEFAULT_MAX_DEPTH: u64 = 100;
/// The remote a repository is fetched from.
pub const REMOTE: &str = "origin";

pub static REPOSITORIES: RuntimeSection = RuntimeSection {
    authority: SectionAuthority::Runtime,
    name: SECTION_NAME,
    doc: "The repositories to observe, and how. Replaced whole by each version.",
    body: r#"{"repositories": {"list_of": {"url": {"type": "https URL of a host name", "required": true, "doc": "the key of the entry; no login in it, and no port other than digits"}, "refs": {"type": "list of full ref names, such as refs/heads/main", "required": false, "doc": "the branches to observe, by the remote's names; left out, the repository's default branch"}, "frequency_secs": {"type": "whole number, 60 to 604800", "required": false, "default": 900}, "max_depth": {"type": "whole number, 1 to 10000", "required": false, "default": 100, "doc": "commits walked per ref, a safety bound"}, "shallow_depth": {"type": "whole number, 1 to 10000", "required": false, "doc": "depth of a shallow fetch; unset is a full incremental fetch"}, "credential": {"type": "name of a credential", "required": false, "doc": "by name; the controller never sees its value"}}}}"#,
    validate,
};

fn validate(body: &Value) -> Result<(), Vec<String>> {
    parse(body).map(|_| ())
}

/// The configurations a body describes, or every problem with it.
pub fn parse(body: &Value) -> Result<Vec<RepoObservationConfig>, Vec<String>> {
    let Some(object) = body.as_object() else {
        return Err(vec!["the body must be an object".to_string()]);
    };
    let mut problems: Vec<String> = object
        .keys()
        .filter(|k| k.as_str() != SECTION_NAME)
        .map(|k| format!("`{k}` is not a field of this section"))
        .collect();
    let Some(list) = object.get(SECTION_NAME).and_then(Value::as_array) else {
        problems.push("`repositories` must be a list of entries".to_string());
        return Err(problems);
    };
    if list.len() > MAX_ENTRIES {
        problems.push(format!(
            "`repositories` has {} entries, and at most {MAX_ENTRIES} are taken",
            list.len()
        ));
    }
    let mut configs: Vec<RepoObservationConfig> = Vec::new();
    for (i, item) in list.iter().enumerate() {
        match entry(i, item) {
            Ok(config) => {
                if configs.iter().any(|c| c.repo_id == config.repo_id) {
                    problems.push(format!(
                        "entry {i}: the repository `{}` is named more than once",
                        config.repo_id
                    ));
                }
                configs.push(config);
            }
            Err(mut more) => problems.append(&mut more),
        }
    }
    if problems.is_empty() {
        Ok(configs)
    } else {
        Err(problems)
    }
}

/// A whole number field within bounds, or the problem with it.
fn number(
    object: &serde_json::Map<String, Value>,
    label: &str,
    key: &str,
    range: std::ops::RangeInclusive<u64>,
    problems: &mut Vec<String>,
) -> Option<u64> {
    match object.get(key) {
        None | Some(Value::Null) => None,
        Some(v) => match v.as_u64().filter(|n| range.contains(n)) {
            Some(n) => Some(n),
            None => {
                problems.push(format!(
                    "{label}: `{key}` must be a whole number from {} to {}",
                    range.start(),
                    range.end()
                ));
                None
            }
        },
    }
}

fn entry(i: usize, item: &Value) -> Result<RepoObservationConfig, Vec<String>> {
    let Some(object) = item.as_object() else {
        return Err(vec![format!("entry {i}: must be an object")]);
    };
    let mut problems = Vec::new();
    for key in object.keys() {
        if ![
            "url",
            "refs",
            "frequency_secs",
            "max_depth",
            "shallow_depth",
            "credential",
        ]
        .contains(&key.as_str())
        {
            problems.push(format!("entry {i}: `{key}` is not a field of an entry"));
        }
    }
    let label = format!("entry {i}");

    let url = match object.get("url").and_then(Value::as_str) {
        None => {
            problems.push(format!("{label}: `url` must be text"));
            String::new()
        }
        Some(url) => {
            if url.len() > MAX_URL {
                problems.push(format!(
                    "{label}: `url` is longer than {MAX_URL} characters"
                ));
            } else if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
                problems.push(format!("{label}: `url` has a space or a control character"));
            } else if let Err(why) = https_host(url) {
                problems.push(format!("{label}: `url`: {why}"));
            }
            url.to_string()
        }
    };
    let label = if problems.is_empty() || url.is_empty() {
        label
    } else {
        // The URL is not secret and names the entry better than its place in the list.
        format!(
            "entry {i} ({})",
            url.chars().take(80).collect::<String>().escape_debug()
        )
    };

    let mut refs = Vec::new();
    match object.get("refs") {
        None | Some(Value::Null) => {}
        Some(Value::Array(list)) if list.len() <= MAX_REFS => {
            for r in list {
                match r.as_str() {
                    Some(r)
                        if r.starts_with("refs/")
                            && r.len() <= MAX_REF
                            && !r.chars().any(|c| c.is_control() || c.is_whitespace()) =>
                    {
                        if !refs.contains(&r.to_string()) {
                            refs.push(r.to_string());
                        }
                    }
                    _ => problems.push(format!(
                        "{label}: each of `refs` must be a full ref name such as refs/heads/main (at most {MAX_REF} characters, no spaces)"
                    )),
                }
            }
        }
        Some(_) => problems.push(format!(
            "{label}: `refs` must be a list of at most {MAX_REFS} ref names"
        )),
    }

    let frequency = number(
        object,
        &label,
        "frequency_secs",
        MIN_FREQUENCY_SECS..=MAX_FREQUENCY_SECS,
        &mut problems,
    );
    let max_depth = number(object, &label, "max_depth", 1..=MAX_DEPTH, &mut problems);
    let shallow_depth = number(
        object,
        &label,
        "shallow_depth",
        1..=MAX_DEPTH,
        &mut problems,
    );

    let credential = match object.get("credential") {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) => match credential_name_problem(name) {
            None => Some(name.clone()),
            Some(why) => {
                problems.push(format!("{label}: `credential` {why}"));
                None
            }
        },
        Some(_) => {
            problems.push(format!("{label}: `credential` must be text"));
            None
        }
    };

    if !problems.is_empty() {
        return Err(problems);
    }
    let mut config = RepoObservationConfig::new(
        RepoId::from_url(&RepoId::normalize_repo_url(&url)),
        url,
        vec![REMOTE.to_string()],
        shallow_depth.map(|d| d as usize),
        refs,
    );
    config.frequency = frequency.unwrap_or(DEFAULT_FREQUENCY_SECS);
    config.max_depth = max_depth.unwrap_or(DEFAULT_MAX_DEPTH) as usize;
    config.credential = credential;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn problems_of(body: Value) -> Vec<String> {
        parse(&body).expect_err("the body should be refused")
    }

    fn one(entry: Value) -> Result<RepoObservationConfig, Vec<String>> {
        parse(&json!({"repositories": [entry]})).map(|mut v| v.remove(0))
    }

    fn with(entry: Value, key: &str, value: Value) -> Value {
        let mut entry = entry;
        entry[key] = value;
        entry
    }

    fn good() -> Value {
        json!({"url": "https://github.com/cmu-sei/Polar.git"})
    }

    #[test]
    fn the_section_is_declared_soundly() {
        assert!(polar_agent_builder::runtime::check_declarations(&[&REPOSITORIES]).is_empty());
    }

    #[test]
    fn the_description_a_scaffold_gives_has_defaults_of_the_kind_each_type_promises() {
        // polar-sign puts a default into a template as it is, so a default must be the value, not prose
        let body: Value = serde_json::from_str(REPOSITORIES.body).unwrap();
        let entry = &body["repositories"]["list_of"];
        for (name, spec) in entry.as_object().unwrap() {
            let Some(default) = spec.get("default") else {
                continue;
            };
            let kind = spec["type"].as_str().unwrap();
            if kind.starts_with("whole number") {
                assert!(default.is_u64(), "{name}");
            } else if kind.starts_with("list of") {
                assert!(default.is_array(), "{name}");
            }
        }
    }

    #[test]
    fn an_entry_with_only_a_url_gets_the_defaults() {
        let config = one(good()).unwrap();
        assert_eq!(config.repo_url, "https://github.com/cmu-sei/Polar.git");
        assert_eq!(config.remotes, vec!["origin".to_string()]);
        assert!(
            config.refs.is_empty(),
            "the observer finds the default branch"
        );
        assert_eq!(config.frequency, 900);
        assert_eq!(config.max_depth, 100);
        assert_eq!(config.shallow_depth, None);
        assert_eq!(config.credential, None);
    }

    #[test]
    fn every_field_is_carried_through() {
        let config = one(json!({
            "url": "https://git.example.org:8443/team/service.git",
            "refs": ["refs/heads/main", "refs/heads/dev", "refs/heads/main"],
            "frequency_secs": 3600,
            "max_depth": 500,
            "shallow_depth": 50,
            "credential": "github-sei"
        }))
        .unwrap();
        assert_eq!(
            config.refs,
            vec!["refs/heads/main", "refs/heads/dev"],
            "listed once each, in order"
        );
        assert_eq!(config.frequency, 3600);
        assert_eq!(config.max_depth, 500);
        assert_eq!(config.shallow_depth, Some(50));
        assert_eq!(config.credential.as_deref(), Some("github-sei"));
    }

    #[test]
    fn a_repository_is_identified_by_its_normalised_url_and_cloned_from_the_url_as_written() {
        let a = one(json!({"url": "https://GitHub.com/org/repo.git"})).unwrap();
        let b = one(json!({"url": "https://github.com/org/repo"})).unwrap();
        assert_eq!(a.repo_id, b.repo_id);
        assert_eq!(a.repo_url, "https://GitHub.com/org/repo.git");
        let c = one(json!({"url": "https://github.com/org/Repo"})).unwrap();
        assert_ne!(a.repo_id, c.repo_id, "the path is case-sensitive");
    }

    #[test]
    fn an_empty_list_is_a_version_that_observes_nothing() {
        assert!(parse(&json!({"repositories": []})).unwrap().is_empty());
    }

    #[test]
    fn the_body_itself_must_be_the_right_shape() {
        assert!(problems_of(json!([])).iter().any(|p| p.contains("object")));
        assert!(
            problems_of(json!({}))
                .iter()
                .any(|p| p.contains("must be a list"))
        );
        assert!(
            problems_of(json!({"repositories": {}}))
                .iter()
                .any(|p| p.contains("list"))
        );
        assert!(
            problems_of(json!({"repositories": [], "extra": 1}))
                .iter()
                .any(|p| p.contains("`extra`"))
        );
        let many: Vec<Value> = (0..=MAX_ENTRIES)
            .map(|i| json!({"url": format!("https://github.com/o/r{i}")}))
            .collect();
        assert!(
            problems_of(json!({"repositories": many}))
                .iter()
                .any(|p| p.contains("at most"))
        );
        assert!(
            problems_of(json!({"repositories": [3]}))
                .iter()
                .any(|p| p.contains("must be an object"))
        );
    }

    #[test]
    fn a_url_that_could_reach_somewhere_unintended_is_refused() {
        for url in [
            "http://github.com/o/r.git",
            "git://github.com/o/r.git",
            "ssh://git@github.com/o/r.git",
            "file:///srv/r.git",
            "ext::sh -c id",
            "/srv/r.git",
            "https://user@github.com/o/r.git",
            "https://user:pw@github.com/o/r.git",
            "https://127.0.0.1/o/r.git",
            "https://[::1]/o/r.git",
            "https://evil.example.org\\@github.com/o/r.git",
            "https://github.com/o/r.git extra",
            "https://github.com/o/r.git\n",
            "",
        ] {
            assert!(one(json!({"url": url})).is_err(), "{url:?}");
        }
        let long = format!("https://github.com/{}", "a".repeat(MAX_URL));
        assert!(one(json!({"url": long})).is_err());
        assert!(one(json!({"url": 5})).is_err());
        assert!(one(json!({})).is_err());
    }

    #[test]
    fn each_bad_field_is_reported_by_entry_and_field() {
        let cases: Vec<(Value, &str)> = vec![
            (
                with(good(), "refs", json!("refs/heads/main")),
                "`refs` must be a list",
            ),
            (with(good(), "refs", json!(["main"])), "full ref name"),
            (with(good(), "refs", json!([5])), "full ref name"),
            (
                with(good(), "refs", json!(["refs/heads/a b"])),
                "full ref name",
            ),
            (
                with(
                    good(),
                    "refs",
                    json!(["refs/".to_string() + &"a".repeat(MAX_REF)]),
                ),
                "full ref name",
            ),
            (
                with(good(), "frequency_secs", json!(59)),
                "`frequency_secs` must be a whole number from 60",
            ),
            (
                with(good(), "frequency_secs", json!(MAX_FREQUENCY_SECS + 1)),
                "`frequency_secs`",
            ),
            (
                with(good(), "frequency_secs", json!("900")),
                "`frequency_secs`",
            ),
            (
                with(good(), "frequency_secs", json!(-1)),
                "`frequency_secs`",
            ),
            (
                with(good(), "frequency_secs", json!(900.5)),
                "`frequency_secs`",
            ),
            (with(good(), "max_depth", json!(0)), "`max_depth`"),
            (
                with(good(), "max_depth", json!(MAX_DEPTH + 1)),
                "`max_depth`",
            ),
            (with(good(), "shallow_depth", json!(0)), "`shallow_depth`"),
            (
                with(good(), "credential", json!("Github")),
                "`credential` may have only",
            ),
            (
                with(good(), "credential", json!("")),
                "`credential` must be 1 to 64",
            ),
            (
                with(good(), "credential", json!(7)),
                "`credential` must be text",
            ),
            (
                with(good(), "stray", json!(1)),
                "`stray` is not a field of an entry",
            ),
        ];
        for (entry, expected) in cases {
            let problems = problems_of(json!({"repositories": [entry.clone()]}));
            assert!(
                problems.iter().any(|p| p.contains(expected)),
                "{entry}: wanted `{expected}`, got {problems:?}"
            );
        }
    }

    #[test]
    fn the_bounds_are_inclusive() {
        for (key, value) in [
            ("frequency_secs", MIN_FREQUENCY_SECS),
            ("frequency_secs", MAX_FREQUENCY_SECS),
            ("max_depth", 1),
            ("max_depth", MAX_DEPTH),
            ("shallow_depth", 1),
            ("shallow_depth", MAX_DEPTH),
        ] {
            assert!(
                one(with(good(), key, json!(value))).is_ok(),
                "{key}={value}"
            );
        }
    }

    #[test]
    fn at_most_so_many_refs_are_taken() {
        let refs = |n: usize| {
            (0..n)
                .map(|i| format!("refs/heads/b{i}"))
                .collect::<Vec<_>>()
        };
        assert!(one(with(good(), "refs", json!(refs(MAX_REFS)))).is_ok());
        let problems =
            problems_of(json!({"repositories": [with(good(), "refs", json!(refs(MAX_REFS + 1)))]}));
        assert!(
            problems.iter().any(|p| p.contains("at most 64 ref names")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_null_field_is_an_absent_one() {
        let config = one(
            json!({"url": "https://github.com/o/r", "refs": null, "frequency_secs": null,
            "max_depth": null, "shallow_depth": null, "credential": null}),
        )
        .unwrap();
        assert_eq!(config.frequency, DEFAULT_FREQUENCY_SECS);
        assert_eq!(config.credential, None);
    }

    #[test]
    fn a_repository_named_twice_is_refused_even_when_written_differently() {
        let problems = problems_of(json!({"repositories": [
            {"url": "https://github.com/o/r.git"},
            {"url": "https://GITHUB.com/o/r"},
        ]}));
        assert!(
            problems.iter().any(|p| p.contains("more than once")),
            "{problems:?}"
        );
    }

    #[test]
    fn every_problem_of_a_body_is_reported_at_once() {
        let body = json!({"repositories": [
            with(good(), "frequency_secs", json!(1)),
            json!({"url": "http://github.com/o/r"}),
        ], "extra": true});
        assert!(problems_of(body).len() >= 3);
    }

    #[test]
    fn the_validator_the_builder_runs_is_the_parser() {
        assert!((REPOSITORIES.validate)(&json!({"repositories": [good()]})).is_ok());
        assert!((REPOSITORIES.validate)(&json!({"repositories": [json!({"url": "x"})]})).is_err());
    }

    #[test]
    fn the_order_of_the_entries_is_kept() {
        let configs = parse(&json!({"repositories": [
            {"url": "https://github.com/o/b"}, {"url": "https://github.com/o/a"}, {"url": "https://github.com/o/c"}
        ]}))
        .unwrap();
        let urls: Vec<_> = configs.iter().map(|c| c.repo_url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://github.com/o/b",
                "https://github.com/o/a",
                "https://github.com/o/c"
            ]
        );
    }
}
