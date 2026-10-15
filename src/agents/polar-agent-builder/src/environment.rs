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

//! The environment an agent built by `AgentBuilder` reads: the variables the builder itself
//! declares, and which sections apply.
//!
//! Every variable the builder reads is declared once here, with `polar-env`, and so are the ones
//! its components read (the health actor's, in `polar-health`). `startup::prepare` runs the
//! sections the agent's code asks for against the environment (or the document) it is given and
//! reports every problem, before anything is spawned. `PolarAgent`'s `--scaffold` and `--check`
//! use the same sections, so what an agent prints about itself and what it judges are one set.
//!
//! What this does not know is what a particular agent's code needs: the least time it needs to
//! drain, the capabilities it requires. Those are the agent's own declarations, and the builder
//! checks them against what is provided when it starts.
//!
//! Each capability brings the sections of the components it uses ([`sections_for`]): the Cassini
//! client's and the graph's today. A section is added there as another component (the
//! Kubernetes API read) moves onto `polar-env`.

use crate::capability::Capability;
use polar_env::{Defaulted, Described, Optional, Section};

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

/// The identity the broker gives the credentials agent: the only publisher whose policy and permissions
/// updates this agent listens to. The default is its service account in the `polar` namespace; a deployment in
/// another namespace names it.
pub static AUTHORITY_PUBLISHER: Defaulted<String> = Defaulted::text(
    "POLAR_AUTHORITY_PUBLISHER",
    "the identity the broker gives the credentials agent (its service account), the only publisher this agent believes policy and permissions updates from",
    "credentials-agent.polar.serviceaccount.cluster.local",
);

fn parse_budget(raw: &str) -> Result<u64, String> {
    raw.trim()
        .parse::<u64>()
        .ok()
        .filter(|secs| *secs >= 1)
        .ok_or_else(|| "must be a whole number of seconds, at least 1".to_string())
}

static VARIABLES: &[&dyn Described] = &[&CAPABILITIES, &SHUTDOWN_BUDGET_SECS, &AUTHORITY_PUBLISHER];

/// The variables the builder itself reads.
pub static BUILDER: Section = Section {
    name: "builder",
    vars: VARIABLES,
    relations: |_| Vec::new(),
};

/// The sections every agent built by the builder has, whatever it does: the builder's, the
/// health actor's, and how it logs and traces.
pub fn base_sections() -> [&'static Section; 3] {
    [
        &BUILDER,
        &polar_health::vars::SECTION,
        &polar::logging::SECTION,
    ]
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

    /// Every section there is.
    fn all_sections() -> Vec<&'static Section> {
        base_sections()
            .into_iter()
            .chain(Capability::ALL.into_iter().flat_map(|c| sections_for(&[c])))
            .collect()
    }

    /// The sections that apply to a deployment that provides what `env` says it provides: the
    /// base ones, and those of each provided capability.
    fn sections_provided_by(env: &dyn Env) -> Vec<&'static Section> {
        let provided = env
            .var(crate::capability::PROVIDED_ENV)
            .map(|raw| Capability::parse_list(&raw))
            .unwrap_or_default();
        base_sections()
            .into_iter()
            .chain(sections_for(&provided))
            .collect()
    }

    /// Every problem the sections find in `env`, all of them. (`startup::prepare` does this with
    /// the sections the agent's own code asks for; these tests are about the rules in the sections.)
    fn check_environment(env: &dyn Env) -> Vec<String> {
        sections_provided_by(env)
            .iter()
            .flat_map(|section| section.check(env))
            .collect()
    }

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_credentials_agents_identity_defaults_to_its_service_account_and_can_be_named() {
        use polar_env::Config;
        let default = Config::builder(&env(&[])).register(&BUILDER).build();
        assert_eq!(
            default.read(&AUTHORITY_PUBLISHER).unwrap(),
            "credentials-agent.polar.serviceaccount.cluster.local"
        );
        let named = Config::builder(&env(&[(
            "POLAR_AUTHORITY_PUBLISHER",
            "credentials-agent.other.serviceaccount.cluster.local",
        )]))
        .register(&BUILDER)
        .build();
        assert_eq!(
            named.read(&AUTHORITY_PUBLISHER).unwrap(),
            "credentials-agent.other.serviceaccount.cluster.local"
        );
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
