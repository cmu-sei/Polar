//! The environment an agent built by `AgentBuilder` reads, and the check of it as a whole.
//!
//! Every variable the builder reads is declared once here, with `polar-env`, and so are
//! the ones its components read (the health actor's, in `polar-health`). [`check_environment`]
//! runs every component's section against an environment and reports every problem, so one
//! function judges the environment wherever it is called:
//!
//! - `AgentBuilder::build()` calls it with the process environment, before anything is
//!   spawned, so a bad pod fails with every problem named in one message;
//! - `polar-config-check` calls it with the environment a workload will be given, before
//!   the pod exists, so the same rules fail a render instead of a rollout;
//! - [`describe`] lists what is read, for documentation and for the checker's `--describe`.
//!
//! What this does not know is what a particular agent's code needs: the least time it needs
//! to drain, the capabilities it requires. Those are the agent's own declarations, handed to
//! the builder, and the builder checks them against what is provided when it starts.
//!
//! Each capability brings the sections of the components it uses ([`sections_for`]): the
//! Cassini client's and the graph's today. A section is added there as another component
//! (the Kubernetes API read) moves onto `polar-env`.

use crate::capability::Capability;
use polar_env::{Described, Env, Kind, Mutability, Optional, Section, VarDesc};

/// The capabilities the deployment provides.
pub const CAPABILITIES: Optional<Vec<String>> = Optional::list(
    "POLAR_CAPABILITIES",
    "the capabilities this deployment provides, comma-separated; the agent refuses to start if its code needs one that is missing",
);

/// The most the agent may take to drain once told to shut down.
pub const SHUTDOWN_BUDGET_SECS: Optional<u64> = Optional::new(
    "POLAR_SHUTDOWN_BUDGET_SECS",
    "the most seconds the agent may take to drain once told to shut down; also sizes the pod's termination grace period",
    "whole number of seconds, at least 1",
    parse_budget,
);

fn parse_budget(raw: &str) -> Result<u64, String> {
    raw.trim()
        .parse::<u64>()
        .ok()
        .filter(|secs| *secs >= 1)
        .ok_or_else(|| "must be a whole number of seconds, at least 1".to_string())
}

static VARIABLES: &[&dyn Described] = &[&CAPABILITIES, &SHUTDOWN_BUDGET_SECS];

/// The variables the builder itself reads.
pub static BUILDER: Section = Section {
    name: "builder",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

/// The sections every agent built by the builder has, whatever it does.
pub fn base_sections() -> [&'static Section; 2] {
    [&BUILDER, &polar_health::vars::SECTION]
}

/// The sections of the components a capability brings. A capability whose components are not
/// yet on `polar-env` (the Kubernetes API read) brings none, and is listed in
/// `remaining-work.md` with the port.
pub fn sections_for(capabilities: &[Capability]) -> Vec<&'static Section> {
    capabilities
        .iter()
        .filter_map(|capability| match capability {
            Capability::Cassini => Some(&cassini_client::environment::SECTION),
            Capability::GraphWrite => Some(&polar::graph::environment::SECTION),
            Capability::Encryption => Some(&polar_crypto::environment::SECTION),
            Capability::KubeApiRead => None,
        })
        .collect()
}

/// Every section there is, each with the capability that brings it (`None` for the ones
/// every agent has), for describing what an agent can read.
fn tagged_sections() -> Vec<(Option<Capability>, &'static Section)> {
    base_sections()
        .into_iter()
        .map(|section| (None, section))
        .chain(Capability::ALL.into_iter().flat_map(|capability| {
            sections_for(&[capability])
                .into_iter()
                .map(move |section| (Some(capability), section))
        }))
        .collect()
}

/// Every section there is, for describing what an agent can read.
pub fn all_sections() -> Vec<&'static Section> {
    tagged_sections().into_iter().map(|(_, s)| s).collect()
}

/// The sections that apply to a deployment that provides what `env` says it provides:
/// the base ones, and those of each provided capability. A deployment that does not say
/// (an empty `POLAR_CAPABILITIES`, as outside the catalog) is asked only for the base.
pub fn sections_provided_by(env: &dyn Env) -> Vec<&'static Section> {
    let provided = env
        .var(crate::capability::PROVIDED_ENV)
        .map(|raw| Capability::parse_list(&raw))
        .unwrap_or_default();
    base_sections()
        .into_iter()
        .chain(sections_for(&provided))
        .collect()
}

/// Every problem with `env`, said so that someone can act on it. All of them, not the first.
///
/// The environment of a pod whose deployment provides a capability must hold what that
/// capability's components read, so this judges those sections too. What the code itself
/// requires is checked when it starts, in `startup::prepare`, with the same sections.
pub fn check_environment(env: &dyn Env) -> Vec<String> {
    sections_provided_by(env)
        .iter()
        .flat_map(|section| section.check(env))
        .collect()
}

/// What is read, by section.
pub fn describe() -> Vec<(&'static str, Vec<VarDesc>)> {
    all_sections()
        .iter()
        .map(|section| (section.name, section.describe()))
        .collect()
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Required => "required",
        Kind::Optional => "optional",
        Kind::Default => "default",
    }
}

fn mutability_name(mutability: Mutability) -> &'static str {
    match mutability {
        Mutability::Static => "static",
        Mutability::Dynamic => "dynamic",
    }
}

/// [`describe`] as JSON, for tools: scaffolding a workload entry or a schedule, and validating
/// a schedule against what an agent declares it may change.
///
/// `{"sections": [{"name", "capability", "variables": [{"name", "kind", "default", "secret",
/// "mutability", "type", "doc"}]}]}`. `capability` is the name in `POLAR_CAPABILITIES` that
/// brings the section, or null for the sections every agent has; `kind` is `required`,
/// `optional` or `default`; `mutability` is `static` or `dynamic` (only a dynamic variable
/// accepts a scheduled override). It describes declarations and never an environment, so no
/// value, secret or otherwise, can appear in it.
pub fn describe_json() -> serde_json::Value {
    let sections: Vec<_> = tagged_sections()
        .into_iter()
        .map(|(capability, section)| {
            let variables: Vec<_> = section
                .describe()
                .into_iter()
                .map(|v| {
                    serde_json::json!({
                        "name": v.name,
                        "kind": kind_name(v.kind),
                        "default": v.default,
                        "secret": v.secret,
                        "mutability": mutability_name(v.mutability),
                        "type": v.type_name,
                        "doc": v.doc,
                    })
                })
                .collect();
            serde_json::json!({
                "name": section.name,
                "capability": capability.map(Capability::as_str),
                "variables": variables,
            })
        })
        .collect();
    serde_json::json!({ "sections": sections })
}

/// What an environment resolves to: a snapshot of the variables declared by the sections
/// that apply to a deployment providing what `env` says it provides (the same sections
/// [`check_environment`] judges), with the value each takes, and where that came from.
pub fn effective(env: &dyn Env) -> polar_env::Config {
    let mut builder = polar_env::Config::builder(env);
    for section in sections_provided_by(env) {
        builder = builder.register(section);
    }
    builder.build()
}

/// [`effective`]'s report as text: each section, then one line per variable with its value
/// (a secret is shown as `<hidden>`), its source and whether a scheduled override may change it.
pub fn effective_text(report: &[polar_env::Resolved]) -> String {
    let mut text = String::new();
    let mut section = "";
    for variable in report {
        if variable.section != section {
            section = variable.section;
            text.push_str(&format!("{section}\n"));
        }
        let value = variable.value.as_deref().unwrap_or("(not set)");
        text.push_str(&format!(
            "  {} = {value}  [{}, {}]\n",
            variable.name,
            variable.source.as_str(),
            mutability_name(variable.mutability)
        ));
    }
    text
}

/// [`effective`]'s report as JSON: `{"sections": [{"name", "variables": [{"name", "value",
/// "source", "mutability", "secret"}]}]}`. `value` is null when unset and `"<hidden>"` for
/// a secret that is set.
pub fn effective_json(report: &[polar_env::Resolved]) -> serde_json::Value {
    let mut sections: Vec<(&str, Vec<serde_json::Value>)> = Vec::new();
    for variable in report {
        if sections.last().map(|(name, _)| *name) != Some(variable.section) {
            sections.push((variable.section, Vec::new()));
        }
        sections
            .last_mut()
            .expect("just pushed")
            .1
            .push(serde_json::json!({
                "name": variable.name,
                "value": variable.value,
                "source": variable.source.as_str(),
                "mutability": mutability_name(variable.mutability),
                "secret": variable.secret,
            }));
    }
    serde_json::json!({
        "sections": sections
            .into_iter()
            .map(|(name, variables)| serde_json::json!({ "name": name, "variables": variables }))
            .collect::<Vec<_>>()
    })
}

/// [`describe`] as text, one variable per line under its section.
pub fn describe_text() -> String {
    let mut text = String::new();
    for (section, variables) in describe() {
        text.push_str(&format!("{section}\n"));
        for variable in variables {
            let kind = match variable.kind {
                Kind::Default => format!("default {}", variable.default.unwrap_or("")),
                other => kind_name(other).to_string(),
            };
            let secret = if variable.secret { ", secret" } else { "" };
            text.push_str(&format!(
                "  {}  [{kind}{secret}]  {}  {}\n",
                variable.name, variable.type_name, variable.doc
            ));
        }
    }
    text
}

/// What a Cassini client and a graph controller are given in a sound deployment, for tests
/// in this crate that need an environment an agent would start in.
#[cfg(test)]
pub(crate) mod fixtures {
    pub const CASSINI_ENV: &[(&str, &str)] = &[
        ("TLS_CLIENT_CERT", "/c/cert.pem"),
        ("TLS_CLIENT_KEY", "/c/key.pem"),
        ("TLS_CA_CERT", "/c/ca.pem"),
        ("BROKER_ADDR", "cassini:8080"),
        ("CASSINI_SERVER_NAME", "cassini.local"),
    ];

    pub const GRAPH_ENV: &[(&str, &str)] = &[
        ("GRAPH_DB", "neo4j"),
        ("GRAPH_USER", "neo4j"),
        ("GRAPH_PASSWORD", "pw"),
        ("GRAPH_ENDPOINT", "bolt+s://db:7687"),
    ];
}

#[cfg(test)]
mod tests {
    use super::fixtures::{CASSINI_ENV, GRAPH_ENV};
    use super::*;
    use polar_env::Env;
    use polar_env::Source;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn an_empty_environment_has_no_problems() {
        // Nothing here is required: the deployment may say nothing and the agent runs on
        // its defaults, which is how it runs outside the cluster.
        assert_eq!(check_environment(&env(&[])), Vec::<String>::new());
    }

    #[test]
    fn a_sound_environment_has_no_problems() {
        let mut sound = [CASSINI_ENV, GRAPH_ENV].concat();
        sound.extend_from_slice(&[
            ("POLAR_CAPABILITIES", "cassini,graph-write"),
            ("POLAR_SHUTDOWN_BUDGET_SECS", "45"),
            ("POLAR_HEALTH_TICK_SECS", "30"),
            ("POLAR_HEALTH_REJUVENATION_SECS", "300"),
        ]);
        assert_eq!(check_environment(&env(&sound)), Vec::<String>::new());
    }

    #[test]
    fn every_section_is_checked_and_every_problem_is_reported() {
        let problems = check_environment(&env(&[
            ("POLAR_SHUTDOWN_BUDGET_SECS", "0"),
            ("POLAR_HEALTH_TICK_SECS", "soon"),
        ]));
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems
                .iter()
                .any(|p| p.starts_with("POLAR_SHUTDOWN_BUDGET_SECS"))
        );
        assert!(
            problems
                .iter()
                .any(|p| p.starts_with("POLAR_HEALTH_TICK_SECS"))
        );
    }

    #[test]
    fn a_relation_between_health_variables_is_reported() {
        let problems = check_environment(&env(&[("POLAR_HEALTH_REJUVENATION_SECS", "100")]));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("hard expiry floor"), "{problems:?}");
    }

    #[test]
    fn the_budget_must_be_at_least_a_second() {
        for bad in ["0", "-1", "soon", "1.5"] {
            let problems = check_environment(&env(&[("POLAR_SHUTDOWN_BUDGET_SECS", bad)]));
            assert_eq!(problems.len(), 1, "{bad}: {problems:?}");
            assert!(problems[0].contains("at least 1"), "{bad}: {problems:?}");
        }
        assert!(check_environment(&env(&[("POLAR_SHUTDOWN_BUDGET_SECS", "1")])).is_empty());
    }

    #[test]
    fn a_problem_says_what_the_variable_is_for() {
        let problems = check_environment(&env(&[("POLAR_SHUTDOWN_BUDGET_SECS", "soon")]));
        assert!(problems[0].contains("drain"), "{problems:?}");
    }

    #[test]
    fn each_capability_brings_the_sections_its_components_read() {
        let names = |caps: &[Capability]| -> Vec<&'static str> {
            sections_for(caps).iter().map(|s| s.name).collect()
        };
        assert_eq!(names(&[Capability::Cassini]), ["cassini-client"]);
        assert_eq!(names(&[Capability::GraphWrite]), ["graph"]);
        assert_eq!(
            names(&[Capability::Cassini, Capability::GraphWrite]),
            ["cassini-client", "graph"]
        );
        assert_eq!(names(&[Capability::Encryption]), ["crypto"]);
        // Its component is not on polar-env yet; recorded in remaining-work.md.
        assert!(names(&[Capability::KubeApiRead]).is_empty());
    }

    #[test]
    fn a_provided_capability_demands_the_environment_its_components_read() {
        let problems = check_environment(&env(&[("POLAR_CAPABILITIES", "cassini")]));
        assert_eq!(problems.len(), 5, "{problems:?}");
        assert!(problems.iter().any(|p| p.starts_with("TLS_CLIENT_CERT")));
        assert!(problems.iter().any(|p| p.starts_with("BROKER_ADDR")));

        let mut sound = CASSINI_ENV.to_vec();
        sound.push(("POLAR_CAPABILITIES", "cassini"));
        assert_eq!(check_environment(&env(&sound)), Vec::<String>::new());
    }

    #[test]
    fn graph_write_demands_the_graph_environment_and_its_rule() {
        let mut vars = CASSINI_ENV.to_vec();
        vars.push(("POLAR_CAPABILITIES", "cassini,graph-write"));
        let missing = check_environment(&env(&vars));
        assert_eq!(missing.len(), 4, "{missing:?}");

        vars.extend_from_slice(GRAPH_ENV);
        assert!(check_environment(&env(&vars)).is_empty());

        vars.push(("GRAPH_CLIENT_CERT", "/c.pem"));
        let partial = check_environment(&env(&vars));
        assert_eq!(partial.len(), 1, "{partial:?}");
        assert!(partial[0].contains("incomplete"), "{partial:?}");
    }

    #[test]
    fn nothing_is_demanded_of_a_capability_the_deployment_does_not_provide() {
        let mut vars = CASSINI_ENV.to_vec();
        vars.push(("POLAR_CAPABILITIES", "cassini"));
        // graph-write is not provided, so its variables are not required.
        assert!(check_environment(&env(&vars)).is_empty());
    }

    #[test]
    fn the_environment_describes_itself_by_section() {
        let described = describe();
        let sections: Vec<_> = described.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            sections,
            ["builder", "health", "cassini-client", "graph", "crypto"]
        );

        let names: Vec<_> = described
            .iter()
            .flat_map(|(_, vars)| vars.iter().map(|v| v.name))
            .collect();
        for expected in [
            "POLAR_CAPABILITIES",
            "POLAR_SHUTDOWN_BUDGET_SECS",
            "POLAR_HEALTH_TICK_SECS",
            "POLAR_HEALTH_FILE",
            "BROKER_ADDR",
            "GRAPH_ENDPOINT",
        ] {
            assert!(
                names.contains(&expected),
                "{expected} missing from {names:?}"
            );
        }
    }

    fn json() -> serde_json::Value {
        describe_json()
    }

    fn variable<'a>(json: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
        json["sections"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["variables"].as_array().unwrap())
            .find(|v| v["name"] == name)
            .unwrap_or_else(|| panic!("{name} is not described"))
    }

    #[test]
    fn the_json_description_has_every_variable_the_text_one_has() {
        let json = json();
        let in_json: usize = json["sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["variables"].as_array().unwrap().len())
            .sum();
        let in_text: usize = describe().iter().map(|(_, vars)| vars.len()).sum();
        assert_eq!(in_json, in_text);
        assert!(in_json > 10);
    }

    #[test]
    fn a_variable_says_what_a_scaffold_needs_to_know() {
        let json = json();
        let tick = variable(&json, "POLAR_HEALTH_TICK_SECS");
        assert_eq!(tick["kind"], "default");
        assert_eq!(tick["default"], "30");
        assert_eq!(tick["secret"], false);
        assert_eq!(tick["mutability"], "static");
        assert!(tick["type"].as_str().unwrap().contains("seconds"));
        assert!(
            tick["doc"]
                .as_str()
                .unwrap()
                .contains("between health ticks")
        );

        let broker = variable(&json, "BROKER_ADDR");
        assert_eq!(broker["kind"], "required");
        assert!(broker["default"].is_null());

        let queue = variable(&json, "CASSINI_QUEUE_PATH");
        assert_eq!(queue["kind"], "optional");
    }

    #[test]
    fn a_secret_is_marked_and_no_value_appears_anywhere() {
        let json = json();
        assert_eq!(variable(&json, "GRAPH_PASSWORD")["secret"], true);
        // It describes declarations, never an environment, so no value can be in it.
        assert!(!json.to_string().contains("hunter2"));
    }

    #[test]
    fn a_section_says_which_capability_brings_it() {
        let json = json();
        let capability = |name: &str| {
            json["sections"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["name"] == name)
                .unwrap_or_else(|| panic!("no section {name}"))["capability"]
                .clone()
        };
        assert!(capability("builder").is_null(), "every agent has it");
        assert!(capability("health").is_null());
        assert_eq!(capability("cassini-client"), "cassini");
        assert_eq!(capability("graph"), "graph-write");
        assert_eq!(capability("crypto"), "encryption");
    }

    fn resolved<'a>(report: &'a [polar_env::Resolved], name: &str) -> &'a polar_env::Resolved {
        report
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("{name} is not in the report"))
    }

    #[test]
    fn the_effective_view_says_where_each_value_came_from() {
        let mut vars = CASSINI_ENV.to_vec();
        vars.extend_from_slice(&[
            ("POLAR_CAPABILITIES", "cassini"),
            ("POLAR_HEALTH_TICK_SECS", "15"),
        ]);
        let report = effective(&env(&vars)).report();

        let tick = resolved(&report, "POLAR_HEALTH_TICK_SECS");
        assert_eq!(
            (tick.value.as_deref(), tick.source),
            (Some("15"), Source::Env)
        );
        let stale = resolved(&report, "POLAR_HEALTH_STALE_SECS");
        assert_eq!(
            (stale.value.as_deref(), stale.source),
            (Some("90"), Source::Default)
        );
        let queue = resolved(&report, "CASSINI_QUEUE_PATH");
        assert_eq!(
            (queue.value.as_deref(), queue.source),
            (None, Source::Unset)
        );
    }

    #[test]
    fn the_effective_view_covers_what_the_deployment_provides_and_no_more() {
        let mut vars = CASSINI_ENV.to_vec();
        vars.push(("POLAR_CAPABILITIES", "cassini"));
        let report = effective(&env(&vars)).report();
        assert!(report.iter().any(|r| r.name == "BROKER_ADDR"));
        assert!(!report.iter().any(|r| r.name == "GRAPH_ENDPOINT"));

        vars.push(("POLAR_CAPABILITIES", "cassini,graph-write"));
        let report = effective(&env(&vars)).report();
        assert!(report.iter().any(|r| r.name == "GRAPH_ENDPOINT"));
    }

    #[test]
    fn the_effective_view_never_shows_a_secret() {
        let mut vars = CASSINI_ENV.to_vec();
        vars.extend_from_slice(GRAPH_ENV);
        vars.push(("POLAR_CAPABILITIES", "cassini,graph-write"));
        vars.push(("GRAPH_PASSWORD", "hunter2"));
        let config = effective(&env(&vars));
        let report = config.report();
        assert_eq!(
            resolved(&report, "GRAPH_PASSWORD").value.as_deref(),
            Some("<hidden>")
        );
        assert!(!effective_text(&report).contains("hunter2"));
        assert!(!effective_json(&report).to_string().contains("hunter2"));
    }

    #[test]
    fn the_effective_text_is_one_line_per_variable_under_its_section() {
        let text = effective_text(&effective(&env(&[("POLAR_HEALTH_TICK_SECS", "15")])).report());
        assert!(
            text.starts_with("health\n") || text.contains("\nhealth\n"),
            "{text}"
        );
        assert!(
            text.contains("POLAR_HEALTH_TICK_SECS = 15  [env, static]"),
            "{text}"
        );
        assert!(
            text.contains("POLAR_HEALTH_STALE_SECS = 90  [default, static]"),
            "{text}"
        );
        assert!(
            text.contains("CASSINI_QUEUE_PATH") == false,
            "not provided: {text}"
        );
    }

    #[test]
    fn the_effective_json_has_the_same_variables() {
        let report = effective(&env(&[("POLAR_HEALTH_TICK_SECS", "15")])).report();
        let json = effective_json(&report);
        let tick = json["sections"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["variables"].as_array().unwrap())
            .find(|v| v["name"] == "POLAR_HEALTH_TICK_SECS")
            .unwrap();
        assert_eq!(tick["value"], "15");
        assert_eq!(tick["source"], "env");
        assert_eq!(tick["mutability"], "static");
        assert_eq!(tick["secret"], false);
    }

    #[test]
    fn every_default_parses() {
        for section in all_sections() {
            assert_eq!(
                section.defaults_problems(),
                Vec::<String>::new(),
                "{}",
                section.name
            );
        }
    }

    #[test]
    fn the_names_the_rest_of_the_crate_uses_are_the_declared_ones() {
        assert_eq!(crate::capability::PROVIDED_ENV, "POLAR_CAPABILITIES");
        assert_eq!(
            crate::shutdown::SHUTDOWN_BUDGET_ENV,
            "POLAR_SHUTDOWN_BUDGET_SECS"
        );
    }

    // keeps the import honest
    #[allow(dead_code)]
    fn _env(_: &dyn Env) {}
}
