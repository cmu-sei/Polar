//! Everything `AgentBuilder::build` decides from the environment, as one function.
//!
//! The builder takes one snapshot of the process environment (a [`Config`]), holding only
//! the variables that some registered section declares: the builder's own, the components'
//! (health), and any the agent declared for itself with `with_environment`. Everything
//! after that reads the snapshot, so:
//!
//! - an undeclared variable that happens to be set, a secret included, never enters the
//!   agent, and a change to the process environment after startup is not seen;
//! - the configuration that was checked is the configuration the agent runs on;
//! - components are handed their settings parsed and never see an environment, so their
//!   tests need no process-wide state.
//!
//! This is a function of an [`Env`], not of the process, so the whole startup decision is
//! tested here with a map; `build()` only supplies the real one.

use crate::capability::{self, Capability, CapabilityCheck};
use crate::{environment, shutdown};
use polar_env::{Config, Env, Section, Source};
use polar_health::{DEFAULT_REJUVENATION_THRESHOLD_SECS, HealthCheckArgs, HealthConfig};
use std::sync::Arc;
use std::time::Duration;

/// What the environment decided.
pub(crate) struct Startup {
    /// The snapshot. The infrastructure supervisor holds it for the life of the agent.
    pub config: Arc<Config>,
    /// The health actor's settings, parsed from the snapshot.
    pub health: HealthConfig,
    /// How long the agent may take to drain once told to shut down.
    pub shutdown_budget: Duration,
}

/// Why an environment was refused.
pub(crate) enum PrepareError {
    /// Variables that are missing or malformed, every one of them.
    Unsound(Vec<String>),
    /// The environment is sound but does not suit this agent: a capability it needs is not
    /// provided, or the shutdown budget is below what the code needs.
    Mismatch(String),
}

impl PrepareError {
    /// Each problem on its own.
    pub(crate) fn problems(&self) -> Vec<String> {
        match self {
            PrepareError::Unsound(problems) => problems.clone(),
            PrepareError::Mismatch(message) => vec![message.clone()],
        }
    }

    /// The one message `AgentBuilder::build` fails with.
    pub(crate) fn message(&self) -> String {
        match self {
            PrepareError::Unsound(problems) => {
                format!(
                    "this agent's environment is not sound: {}",
                    problems.join("; ")
                )
            }
            PrepareError::Mismatch(message) => message.clone(),
        }
    }
}

/// Takes the snapshot of `env` and judges it, or says everything that is wrong with it.
///
/// `agent_sections` are the ones the agent declared for itself; `required` the capabilities
/// its code needs; `declared_budget` the least time its code needs to drain.
pub(crate) fn prepare(
    env: &dyn Env,
    agent_sections: &[&'static Section],
    required: &[Capability],
    declared_budget: Option<Duration>,
) -> Result<Startup, String> {
    prepare_from(env, Source::Env, agent_sections, required, declared_budget)
        .map_err(|error| error.message())
}

/// As [`prepare`], for values that come from `source` (the process environment, or a
/// configuration document), and with each problem kept apart.
pub(crate) fn prepare_from(
    env: &dyn Env,
    source: Source,
    agent_sections: &[&'static Section],
    required: &[Capability],
    declared_budget: Option<Duration>,
) -> Result<Startup, PrepareError> {
    let mut builder = Config::builder(env).source(source);
    for section in environment::base_sections()
        .into_iter()
        .chain(environment::sections_for(required))
        .chain(agent_sections.iter().copied())
    {
        builder = builder.register(section);
    }
    let config = builder.build();

    // The environment as a whole, before anything else: every variable the builder, its
    // components and the agent read must be sound, and every problem is reported together.
    let problems = config.check();
    if !problems.is_empty() {
        return Err(PrepareError::Unsound(problems));
    }

    // Checked before anything is spawned, so a mismatch with the deployment is one clear,
    // immediate, non-zero exit instead of a half-started agent.
    let provided = config.var(capability::PROVIDED_ENV);
    match capability::check(required, provided.as_deref()) {
        CapabilityCheck::Satisfied => {}
        CapabilityCheck::NotDeclared => {
            tracing::warn!(
                "{} is not set: the deployment did not declare its capabilities, so this \
                 agent's requirements ({}) cannot be checked",
                capability::PROVIDED_ENV,
                required
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        CapabilityCheck::Missing(missing) => {
            return Err(PrepareError::Mismatch(capability::missing_message(
                &missing,
                provided.as_deref(),
            )));
        }
    }

    let shutdown_budget =
        shutdown::budget(declared_budget, &config).map_err(PrepareError::Mismatch)?;

    // Whether to watch the graph follows from the capabilities. The dependency endpoints
    // come from POLAR_HEALTH_DEP_ENDPOINTS, which the deployment sets from the same
    // `needs`, so there is no endpoint list to keep in sync with the manifest here.
    let health = HealthConfig::from_env(
        HealthCheckArgs::new(
            required.contains(&Capability::GraphWrite),
            DEFAULT_REJUVENATION_THRESHOLD_SECS,
            Vec::new(),
        ),
        &config,
    )
    .map_err(PrepareError::Mismatch)?;

    Ok(Startup {
        config: Arc::new(config),
        health,
        shutdown_budget,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::fixtures::{CASSINI_ENV, GRAPH_ENV};
    use polar_env::{Described, Required};
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// A deployment that provides what the agent needs: the client's and, when the agent
    /// writes to the graph, the graph's variables, plus `pairs`.
    fn deployment(pairs: &[(&str, &str)], required: &[Capability]) -> HashMap<String, String> {
        let mut vars = CASSINI_ENV.to_vec();
        if required.contains(&Capability::GraphWrite) {
            vars.extend_from_slice(GRAPH_ENV);
        }
        vars.extend_from_slice(pairs);
        env(&vars)
    }

    fn prepared(pairs: &[(&str, &str)], required: &[Capability]) -> Result<Startup, String> {
        prepare(&deployment(pairs, required), &[], required, None)
    }

    // An agent's own section: one variable only this agent reads.
    static BUCKET: Required<String> =
        Required::text("AGENT_BUCKET", "the bucket this agent writes to");
    static AGENT_VARS: &[&dyn Described] = &[&BUCKET];
    static AGENT: Section = Section {
        name: "agent",
        vars: AGENT_VARS,
        relations: |_| Vec::new(),
    };

    #[test]
    fn an_empty_environment_starts_on_the_defaults() {
        let s = prepared(&[], &[Capability::Cassini]).unwrap();
        assert_eq!(s.shutdown_budget, shutdown::DEFAULT_SHUTDOWN_BUDGET);
        assert_eq!(s.health.file_path, polar_health::POLAR_HEALTH_FILE_DEFAULT);
        assert!(!s.health.expects_graph);
    }

    #[test]
    fn an_agent_that_needs_the_broker_is_refused_without_its_coordinates() {
        let err = prepare(&env(&[]), &[], &[Capability::Cassini], None)
            .err()
            .unwrap();
        for name in ["TLS_CLIENT_CERT", "BROKER_ADDR", "CASSINI_SERVER_NAME"] {
            assert!(err.contains(name), "{name}: {err}");
        }
    }

    #[test]
    fn an_agent_that_writes_to_the_graph_is_refused_without_its_graph_settings() {
        let err = prepare(
            &env(CASSINI_ENV),
            &[],
            &[Capability::Cassini, Capability::GraphWrite],
            None,
        )
        .err()
        .unwrap();
        assert!(err.contains("GRAPH_ENDPOINT"), "{err}");
        assert!(!err.contains("hunter2"));
    }

    #[test]
    fn what_a_capability_does_not_need_is_not_demanded() {
        // No graph capability, so no graph variables, and none that happen to be set get in.
        let s = prepare(
            &env(&[CASSINI_ENV, &[("GRAPH_PASSWORD", "hunter2")]].concat()),
            &[],
            &[Capability::Cassini],
            None,
        )
        .unwrap();
        assert_eq!(s.config.var("GRAPH_PASSWORD"), None);
    }

    #[test]
    fn an_agent_that_encrypts_takes_the_crypto_settings_from_the_snapshot() {
        let s = prepared(
            &[("KUBERNETES_SA_TOKEN", "/workspace/token")],
            &[Capability::Cassini, Capability::Encryption],
        )
        .unwrap();
        let settings = polar_crypto::environment::CryptoSettings::from_env(&*s.config).unwrap();
        assert_eq!(settings.service_account_token_path, "/workspace/token");
        assert_eq!(settings.own_certificate_path, "/c/cert.pem");
    }

    #[test]
    fn an_agent_that_does_not_encrypt_does_not_carry_the_token_path() {
        let s = prepared(
            &[("KUBERNETES_SA_TOKEN", "/workspace/token")],
            &[Capability::Cassini],
        )
        .unwrap();
        assert_eq!(s.config.var("KUBERNETES_SA_TOKEN"), None);
    }

    #[test]
    fn every_problem_in_the_environment_is_reported_together() {
        let err = prepared(
            &[
                ("POLAR_SHUTDOWN_BUDGET_SECS", "0"),
                ("POLAR_HEALTH_TICK_SECS", "soon"),
            ],
            &[Capability::Cassini],
        )
        .err()
        .unwrap();
        assert!(err.contains("not sound"), "{err}");
        assert!(err.contains("POLAR_SHUTDOWN_BUDGET_SECS"), "{err}");
        assert!(err.contains("POLAR_HEALTH_TICK_SECS"), "{err}");
    }

    #[test]
    fn a_variable_nothing_declared_never_enters_the_snapshot() {
        let s = prepared(
            &[
                ("SOME_UNRELATED_SECRET", "hunter2"),
                ("POLAR_SHUTDOWN_BUDGET_SECS", "30"),
            ],
            &[Capability::Cassini],
        )
        .unwrap();
        assert_eq!(s.config.var("SOME_UNRELATED_SECRET"), None);
        assert_eq!(
            s.config.var("POLAR_SHUTDOWN_BUDGET_SECS").as_deref(),
            Some("30")
        );
    }

    #[test]
    fn a_section_the_agent_declared_is_checked_and_readable() {
        let missing = prepare(
            &deployment(&[], &[]),
            &[&AGENT],
            &[Capability::Cassini],
            None,
        )
        .err()
        .unwrap();
        assert!(missing.contains("AGENT_BUCKET"), "{missing}");

        let s = prepare(
            &deployment(&[("AGENT_BUCKET", "b1")], &[]),
            &[&AGENT],
            &[Capability::Cassini],
            None,
        )
        .unwrap();
        assert_eq!(s.config.read(&BUCKET).unwrap(), "b1");
    }

    #[test]
    fn a_capability_the_deployment_does_not_provide_stops_the_agent() {
        let err = prepared(
            &[("POLAR_CAPABILITIES", "cassini")],
            &[Capability::Cassini, Capability::GraphWrite],
        )
        .err()
        .unwrap();
        assert!(err.contains("graph-write"), "{err}");
    }

    #[test]
    fn capabilities_the_deployment_does_not_declare_are_not_checked() {
        // Outside the cluster nothing sets them; the builder warns and carries on.
        assert!(prepared(&[], &[Capability::Cassini, Capability::GraphWrite]).is_ok());
    }

    #[test]
    fn writing_to_the_graph_makes_the_health_actor_watch_it() {
        let s = prepared(
            &[("POLAR_CAPABILITIES", "cassini,graph-write")],
            &[Capability::Cassini, Capability::GraphWrite],
        )
        .unwrap();
        assert!(s.health.expects_graph);
    }

    #[test]
    fn the_health_settings_come_from_the_snapshot() {
        let s = prepared(
            &[
                ("POLAR_HEALTH_FILE", "/run/h.json"),
                ("POLAR_HEALTH_TICK_SECS", "15"),
            ],
            &[Capability::Cassini],
        )
        .unwrap();
        assert_eq!(s.health.file_path, "/run/h.json");
        assert_eq!(s.health.tick_secs, 15);
    }

    #[test]
    fn the_budget_the_deployment_gives_must_cover_what_the_code_declares() {
        let short = prepare(
            &deployment(&[("POLAR_SHUTDOWN_BUDGET_SECS", "10")], &[]),
            &[],
            &[Capability::Cassini],
            Some(Duration::from_secs(40)),
        )
        .err()
        .unwrap();
        assert!(short.contains("POLAR_SHUTDOWN_BUDGET_SECS"), "{short}");

        let enough = prepare(
            &deployment(&[("POLAR_SHUTDOWN_BUDGET_SECS", "50")], &[]),
            &[],
            &[Capability::Cassini],
            Some(Duration::from_secs(40)),
        )
        .unwrap();
        assert_eq!(enough.shutdown_budget, Duration::from_secs(50));
    }
}
