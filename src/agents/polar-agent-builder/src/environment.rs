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
//! (encryption, the Kubernetes API read) moves onto `polar-env`.

use crate::capability::Capability;
use polar_env::{Described, Env, Kind, Optional, Section, VarDesc};

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
/// yet on `polar-env` (encryption, the Kubernetes API read) brings none, and is listed in
/// `remaining-work.md` with the port.
pub fn sections_for(capabilities: &[Capability]) -> Vec<&'static Section> {
    capabilities
        .iter()
        .filter_map(|capability| match capability {
            Capability::Cassini => Some(&cassini_client::environment::SECTION),
            Capability::GraphWrite => Some(&polar::graph::environment::SECTION),
            Capability::Encryption | Capability::KubeApiRead => None,
        })
        .collect()
}

/// Every section there is, for describing what an agent can read.
pub fn all_sections() -> Vec<&'static Section> {
    base_sections()
        .into_iter()
        .chain(sections_for(&Capability::ALL))
        .collect()
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

/// [`describe`] as text, one variable per line under its section.
pub fn describe_text() -> String {
    let mut text = String::new();
    for (section, variables) in describe() {
        text.push_str(&format!("{section}\n"));
        for variable in variables {
            let kind = match variable.kind {
                Kind::Required => "required".to_string(),
                Kind::Optional => "optional".to_string(),
                Kind::Default => format!("default {}", variable.default.unwrap_or("")),
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
        // Their components are not on polar-env yet; recorded in remaining-work.md.
        assert!(names(&[Capability::Encryption, Capability::KubeApiRead]).is_empty());
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
        assert_eq!(sections, ["builder", "health", "cassini-client", "graph"]);

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
