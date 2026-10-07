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
//! A section is added here for each capability's components as they are moved onto
//! `polar-env` (the Cassini client, the graph).

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

/// Every section that applies to an agent built by the builder.
pub fn sections() -> [&'static Section; 2] {
    [&BUILDER, &polar_health::vars::SECTION]
}

/// Every problem with `env`, said so that someone can act on it. All of them, not the first.
pub fn check_environment(env: &dyn Env) -> Vec<String> {
    sections()
        .iter()
        .flat_map(|section| section.check(env))
        .collect()
}

/// What is read, by section.
pub fn describe() -> Vec<(&'static str, Vec<VarDesc>)> {
    sections()
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

/// Stops the agent before anything is spawned if its environment is not sound.
pub fn ensure_environment_is_sound(env: &dyn Env) -> Result<(), ractor::ActorProcessingErr> {
    let problems = check_environment(env);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(ractor::ActorProcessingErr::from(format!(
            "this agent's environment is not sound: {}",
            problems.join("; ")
        )))
    }
}

#[cfg(test)]
mod tests {
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
        let sound = env(&[
            ("POLAR_CAPABILITIES", "cassini,graph-write"),
            ("POLAR_SHUTDOWN_BUDGET_SECS", "45"),
            ("POLAR_HEALTH_TICK_SECS", "30"),
            ("POLAR_HEALTH_REJUVENATION_SECS", "300"),
        ]);
        assert_eq!(check_environment(&sound), Vec::<String>::new());
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
    fn an_unsound_environment_stops_the_agent_with_every_problem_in_one_message() {
        let bad = env(&[
            ("POLAR_SHUTDOWN_BUDGET_SECS", "0"),
            ("POLAR_HEALTH_TICK_SECS", "soon"),
        ]);
        let err = ensure_environment_is_sound(&bad).unwrap_err().to_string();
        assert!(err.contains("POLAR_SHUTDOWN_BUDGET_SECS"), "{err}");
        assert!(err.contains("POLAR_HEALTH_TICK_SECS"), "{err}");
        assert!(ensure_environment_is_sound(&env(&[])).is_ok());
    }

    #[test]
    fn the_environment_describes_itself_by_section() {
        let described = describe();
        let sections: Vec<_> = described.iter().map(|(name, _)| *name).collect();
        assert_eq!(sections, ["builder", "health"]);

        let names: Vec<_> = described
            .iter()
            .flat_map(|(_, vars)| vars.iter().map(|v| v.name))
            .collect();
        for expected in [
            "POLAR_CAPABILITIES",
            "POLAR_SHUTDOWN_BUDGET_SECS",
            "POLAR_HEALTH_TICK_SECS",
            "POLAR_HEALTH_FILE",
        ] {
            assert!(
                names.contains(&expected),
                "{expected} missing from {names:?}"
            );
        }
    }

    #[test]
    fn every_default_parses() {
        for section in sections() {
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
