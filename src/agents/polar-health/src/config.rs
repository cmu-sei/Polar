//! The health actor's settings, merged from the environment and the caller's
//! `HealthCheckArgs` in one place so the merge and its validation can be tested
//! without spawning an actor or touching the process environment.

use crate::vars::{
    CASSINI_DEADLINE_SECS, CERTS, DEP_ENDPOINTS, EXPIRY_SECS, FILE, GRAPH_DEADLINE_SECS,
    POLAR_HEALTH_CASSINI_DEADLINE_SECS_ENV, POLAR_HEALTH_EXPIRY_SECS_ENV,
    POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV, POLAR_HEALTH_REJUVENATION_SECS_ENV,
    POLAR_HEALTH_STALE_SECS_ENV, POLAR_HEALTH_TICK_SECS_ENV, REJUVENATION_SECS, STALE_SECS,
    TICK_SECS,
};
use crate::{DEFAULT_JITTER_WINDOW_SECS, DepCertEndpoint, HealthCheckArgs};
use polar_env::{Env, ProcessEnv};

/// Everything the health actor is configured with.
#[derive(Debug, Clone)]
pub struct HealthConfig {
    /// Does this agent connect to a graph database?
    pub expects_graph: bool,
    /// Seconds before own cert expiry to enter rejuvenation mode.
    pub rejuvenation_threshold_secs: i64,
    /// Own cert lifetime below which the agent is failed outright.
    pub expiry_threshold_secs: i64,
    pub tick_secs: u64,
    /// How long an `expects_graph` agent may go without a real graph success.
    pub graph_deadline_secs: u64,
    /// How long the agent may go without a Cassini connection before the actor
    /// gives a verdict.
    pub cassini_deadline_secs: u64,
    /// How stale the liveness probe lets the health file get. The probe reads it
    /// from the same pod environment; the actor reads it only to check that its own
    /// tick is fast enough for the probe.
    pub probe_stale_secs: u64,
    /// Where the health file is written, which is where the liveness probe reads it.
    pub file_path: String,
    /// The agent's own client certificates.
    pub cert_paths: Vec<String>,
    /// Dependency endpoints whose certificates must be healthy before rejuvenating.
    pub dep_cert_endpoints: Vec<DepCertEndpoint>,
}

/// The value of a read, or its problem recorded and a placeholder put in its place so the
/// rest can still be read and every problem reported together.
macro_rules! take {
    ($problems:ident, $read:expr, $placeholder:expr) => {
        match $read {
            Ok(value) => value,
            Err(error) => {
                $problems.push(error.to_string());
                $placeholder
            }
        }
    };
}

impl HealthConfig {
    /// Merges the process environment over `args`, then validates.
    pub fn load(args: HealthCheckArgs) -> Result<Self, String> {
        Self::from_env(args, &ProcessEnv)
    }

    /// As [`load`](Self::load), reading variables from `env`.
    ///
    /// The environment wins over `args` for the dependency endpoints and the
    /// rejuvenation threshold, so a deployment can set them without a code change.
    /// A variable that is set but does not parse is an error, since a typo would
    /// otherwise quietly run the agent on settings nobody chose; a blank one means the
    /// default. Every such problem is reported together, so one restart shows them all.
    pub fn from_env(args: HealthCheckArgs, env: &dyn Env) -> Result<Self, String> {
        let mut problems: Vec<String> = Vec::new();

        let file_path = take!(problems, FILE.read(env), String::new());
        let cert_paths = take!(problems, CERTS.read(env), None);
        let dep_cert_endpoints = take!(problems, DEP_ENDPOINTS.read(env), None);
        let rejuvenation = take!(problems, REJUVENATION_SECS.read(env), None);
        let expiry_threshold_secs = take!(problems, EXPIRY_SECS.read(env), 0);
        let tick_secs = take!(problems, TICK_SECS.read(env), 0);
        let graph_deadline_secs = take!(problems, GRAPH_DEADLINE_SECS.read(env), 0);
        let cassini_deadline_secs = take!(problems, CASSINI_DEADLINE_SECS.read(env), 0);
        let probe_stale_secs = take!(problems, STALE_SECS.read(env), 0);

        if !problems.is_empty() {
            return Err(problems.join("; "));
        }

        let config = Self {
            expects_graph: args.expects_graph,
            rejuvenation_threshold_secs: rejuvenation.unwrap_or(args.rejuvenation_threshold_secs),
            expiry_threshold_secs,
            tick_secs,
            graph_deadline_secs,
            cassini_deadline_secs,
            probe_stale_secs,
            file_path,
            cert_paths: cert_paths.unwrap_or_default(),
            dep_cert_endpoints: dep_cert_endpoints.unwrap_or(args.dep_cert_endpoints),
        };
        config.validate()?;
        Ok(config)
    }

    /// Rejects settings the actor cannot work with, or that would make the health
    /// system contradict itself.
    pub fn validate(&self) -> Result<(), String> {
        if self.expiry_threshold_secs <= 0 {
            return Err(format!(
                "{POLAR_HEALTH_EXPIRY_SECS_ENV} must be positive, got {}",
                self.expiry_threshold_secs
            ));
        }
        if self.rejuvenation_threshold_secs <= self.expiry_threshold_secs {
            return Err(format!(
                "{POLAR_HEALTH_REJUVENATION_SECS_ENV} ({}) must be greater than \
                 {POLAR_HEALTH_EXPIRY_SECS_ENV} ({})",
                self.rejuvenation_threshold_secs, self.expiry_threshold_secs
            ));
        }
        for (name, value) in [
            (POLAR_HEALTH_TICK_SECS_ENV, self.tick_secs),
            (
                POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV,
                self.graph_deadline_secs,
            ),
            (
                POLAR_HEALTH_CASSINI_DEADLINE_SECS_ENV,
                self.cassini_deadline_secs,
            ),
        ] {
            if value < 1 {
                return Err(format!("{name} must be at least 1, got {value}"));
            }
        }

        // A rejuvenation has to finish before the certificate reaches the hard
        // floor. The slowest case is a tick to notice the window and check the
        // dependencies, the longest jitter, and a tick to act on it.
        let needed = 2 * self.tick_secs + DEFAULT_JITTER_WINDOW_SECS;
        let room = (self.rejuvenation_threshold_secs - self.expiry_threshold_secs) as u64;
        if room < needed {
            return Err(format!(
                "{POLAR_HEALTH_REJUVENATION_SECS_ENV} ({}) minus {POLAR_HEALTH_EXPIRY_SECS_ENV} ({}) \
                 leaves {room}s, but a rejuvenation can take {needed}s (two ticks of \
                 {POLAR_HEALTH_TICK_SECS_ENV} = {}s plus the {}s jitter window), so the \
                 certificate could reach its hard expiry floor before the agent could rejuvenate",
                self.rejuvenation_threshold_secs,
                self.expiry_threshold_secs,
                self.tick_secs,
                DEFAULT_JITTER_WINDOW_SECS
            ));
        }

        // The probe must tolerate one slow tick, or it calls a healthy agent dead.
        if self.probe_stale_secs <= 2 * self.tick_secs {
            return Err(format!(
                "{POLAR_HEALTH_STALE_SECS_ENV} ({}) must exceed twice {POLAR_HEALTH_TICK_SECS_ENV} \
                 ({}), or the liveness probe would call the agent dead after a single slow tick",
                self.probe_stale_secs, self.tick_secs
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vars::{
        DEFAULT_CASSINI_DEADLINE_SECS, DEFAULT_EXPIRY_THRESHOLD_SECS, DEFAULT_GRAPH_DEADLINE_SECS,
        DEFAULT_TICK_SECS,
    };
    use crate::vars::{
        POLAR_HEALTH_CERTS_ENV, POLAR_HEALTH_DEP_ENDPOINTS_ENV, POLAR_HEALTH_EXPIRY_SECS_ENV,
        POLAR_HEALTH_FILE_ENV, POLAR_HEALTH_REJUVENATION_SECS_ENV,
    };
    use std::collections::HashMap;

    fn args() -> HealthCheckArgs {
        HealthCheckArgs::new(
            true,
            300,
            vec![DepCertEndpoint::parse("from-args.example:443:100").unwrap()],
        )
    }

    fn load(vars: &[(&str, &str)]) -> Result<HealthConfig, String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        HealthConfig::from_env(args(), &map)
    }

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let c = load(&[]).unwrap();
        assert!(c.expects_graph);
        assert_eq!(c.rejuvenation_threshold_secs, 300);
        assert_eq!(c.expiry_threshold_secs, DEFAULT_EXPIRY_THRESHOLD_SECS);
        assert_eq!(c.tick_secs, DEFAULT_TICK_SECS);
        assert_eq!(c.graph_deadline_secs, DEFAULT_GRAPH_DEADLINE_SECS);
        assert_eq!(c.cassini_deadline_secs, DEFAULT_CASSINI_DEADLINE_SECS);
        assert!(c.cert_paths.is_empty());
        assert_eq!(c.dep_cert_endpoints.len(), 1);
        assert_eq!(c.dep_cert_endpoints[0].host, "from-args.example");
    }

    #[test]
    fn environment_overrides_every_setting() {
        let c = load(&[
            (POLAR_HEALTH_REJUVENATION_SECS_ENV, "900"),
            (POLAR_HEALTH_EXPIRY_SECS_ENV, "120"),
            (POLAR_HEALTH_TICK_SECS_ENV, "5"),
            (POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV, "40"),
            (POLAR_HEALTH_CASSINI_DEADLINE_SECS_ENV, "90"),
        ])
        .unwrap();
        assert_eq!(c.cassini_deadline_secs, 90);
        assert_eq!(c.rejuvenation_threshold_secs, 900);
        assert_eq!(c.expiry_threshold_secs, 120);
        assert_eq!(c.tick_secs, 5);
        assert_eq!(c.graph_deadline_secs, 40);
    }

    #[test]
    fn the_health_file_is_where_the_probe_looks_by_default_and_where_the_deployment_says() {
        assert_eq!(
            load(&[]).unwrap().file_path,
            crate::POLAR_HEALTH_FILE_DEFAULT
        );
        assert_eq!(
            load(&[(POLAR_HEALTH_FILE_ENV, "/run/h.json")])
                .unwrap()
                .file_path,
            "/run/h.json"
        );
    }

    #[test]
    fn dependency_endpoints_from_the_environment_replace_those_from_args() {
        let c = load(&[(
            POLAR_HEALTH_DEP_ENDPOINTS_ENV,
            "a.example:443:300, b.example:8080:60",
        )])
        .unwrap();
        let hosts: Vec<_> = c
            .dep_cert_endpoints
            .iter()
            .map(|e| e.host.as_str())
            .collect();
        assert_eq!(hosts, ["a.example", "b.example"]);
    }

    #[test]
    fn blank_dependency_environment_falls_back_to_args() {
        let c = load(&[(POLAR_HEALTH_DEP_ENDPOINTS_ENV, "   ")]).unwrap();
        assert_eq!(c.dep_cert_endpoints[0].host, "from-args.example");
    }

    #[test]
    fn a_malformed_dependency_endpoint_is_an_error() {
        let err = load(&[(POLAR_HEALTH_DEP_ENDPOINTS_ENV, "a.example:443")]).unwrap_err();
        assert!(err.contains("expected host:port:min_ttl_seconds"), "{err}");
    }

    #[test]
    fn cert_paths_are_split_trimmed_and_blanks_dropped() {
        let c = load(&[(POLAR_HEALTH_CERTS_ENV, " /a.pem, ,/b.pem ,")]).unwrap();
        assert_eq!(c.cert_paths, ["/a.pem", "/b.pem"]);
    }

    #[test]
    fn an_unparseable_number_is_an_error_that_names_the_variable() {
        for name in [
            POLAR_HEALTH_TICK_SECS_ENV,
            POLAR_HEALTH_EXPIRY_SECS_ENV,
            POLAR_HEALTH_REJUVENATION_SECS_ENV,
            POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV,
            POLAR_HEALTH_CASSINI_DEADLINE_SECS_ENV,
            POLAR_HEALTH_STALE_SECS_ENV,
        ] {
            for bad in ["soon", "1.5", "30s"] {
                let err = load(&[(name, bad)]).unwrap_err();
                assert!(err.contains(name), "{name}={bad}: {err}");
                assert!(err.contains(bad), "{name}={bad}: {err}");
            }
        }
    }

    #[test]
    fn a_negative_number_is_not_a_number_for_the_unsigned_settings() {
        // The two certificate thresholds are signed and reach the ordering rules
        // instead; the others cannot be negative at all.
        for name in [
            POLAR_HEALTH_TICK_SECS_ENV,
            POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV,
            POLAR_HEALTH_CASSINI_DEADLINE_SECS_ENV,
            POLAR_HEALTH_STALE_SECS_ENV,
        ] {
            let err = load(&[(name, "-3")]).unwrap_err();
            assert!(err.contains("whole number of seconds"), "{name}: {err}");
        }
    }

    #[test]
    fn a_blank_number_means_the_default() {
        let c = load(&[
            (POLAR_HEALTH_TICK_SECS_ENV, "  "),
            (POLAR_HEALTH_EXPIRY_SECS_ENV, ""),
        ])
        .unwrap();
        assert_eq!(c.tick_secs, DEFAULT_TICK_SECS);
        assert_eq!(c.expiry_threshold_secs, DEFAULT_EXPIRY_THRESHOLD_SECS);
    }

    #[test]
    fn the_tick_must_be_at_least_one_second() {
        let err = load(&[(POLAR_HEALTH_TICK_SECS_ENV, "0")]).unwrap_err();
        assert!(err.contains(POLAR_HEALTH_TICK_SECS_ENV), "{err}");
        assert!(err.contains("at least 1"), "{err}");
    }

    #[test]
    fn the_graph_and_cassini_deadlines_must_be_at_least_one_second() {
        for name in [
            POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV,
            POLAR_HEALTH_CASSINI_DEADLINE_SECS_ENV,
        ] {
            let err = load(&[(name, "0")]).unwrap_err();
            assert!(err.contains(name) && err.contains("at least 1"), "{err}");
        }
    }

    #[test]
    fn a_rejuvenation_must_have_room_to_finish_before_the_hard_floor() {
        // Worst case: a tick to notice the window, the jitter window, a tick to
        // act. With the defaults that is 2 x 30 + 180 = 240, and 300 - 60 is
        // exactly 240: it fits, with nothing to spare.
        assert!(load(&[]).is_ok());
        assert!(
            load(&[
                (POLAR_HEALTH_REJUVENATION_SECS_ENV, "300"),
                (POLAR_HEALTH_EXPIRY_SECS_ENV, "60"),
            ])
            .is_ok()
        );

        // One second too little.
        let err = load(&[(POLAR_HEALTH_REJUVENATION_SECS_ENV, "299")]).unwrap_err();
        assert!(err.contains("hard expiry"), "{err}");
        assert!(err.contains("240"), "the room needed is stated: {err}");

        // A longer tick needs more room.
        assert!(load(&[(POLAR_HEALTH_TICK_SECS_ENV, "31")]).is_err());
        assert!(
            load(&[
                (POLAR_HEALTH_TICK_SECS_ENV, "31"),
                (POLAR_HEALTH_REJUVENATION_SECS_ENV, "302"),
                (POLAR_HEALTH_STALE_SECS_ENV, "90"),
            ])
            .is_ok()
        );
    }

    #[test]
    fn the_probe_must_tolerate_a_missed_tick() {
        // Stale limit 60 with a 30 s tick: one slow tick and the probe calls the
        // agent dead.
        let err = load(&[(POLAR_HEALTH_STALE_SECS_ENV, "60")]).unwrap_err();
        assert!(err.contains(POLAR_HEALTH_STALE_SECS_ENV), "{err}");
        assert!(load(&[(POLAR_HEALTH_STALE_SECS_ENV, "61")]).is_ok());
        assert!(load(&[(POLAR_HEALTH_STALE_SECS_ENV, "90")]).is_ok());
    }

    #[test]
    fn a_forced_rejuvenation_threshold_far_above_the_cert_lifetime_is_fine() {
        // How the cluster tests force a rejuvenation.
        assert!(load(&[(POLAR_HEALTH_REJUVENATION_SECS_ENV, "7200")]).is_ok());
    }

    #[test]
    fn expiry_must_be_positive() {
        let err = load(&[(POLAR_HEALTH_EXPIRY_SECS_ENV, "0")]).unwrap_err();
        assert!(err.contains("must be positive"), "{err}");
    }

    #[test]
    fn rejuvenation_must_exceed_expiry() {
        let err = load(&[
            (POLAR_HEALTH_EXPIRY_SECS_ENV, "60"),
            (POLAR_HEALTH_REJUVENATION_SECS_ENV, "60"),
        ])
        .unwrap_err();
        assert!(err.contains("must be greater than"), "{err}");
        assert!(
            load(&[
                (POLAR_HEALTH_EXPIRY_SECS_ENV, "60"),
                (POLAR_HEALTH_REJUVENATION_SECS_ENV, "301"),
            ])
            .is_ok()
        );
    }

    #[test]
    fn a_bad_endpoint_is_reported_before_a_bad_threshold() {
        // The order today's pre_start checks in; kept so error messages do not move.
        let err = load(&[
            (POLAR_HEALTH_DEP_ENDPOINTS_ENV, "nope"),
            (POLAR_HEALTH_EXPIRY_SECS_ENV, "0"),
        ])
        .unwrap_err();
        assert!(err.contains("expected host:port"), "{err}");
    }

    // -- the declarations ---------------------------------------------------------------

    #[test]
    fn every_problem_with_the_variables_is_reported_at_once() {
        // The old code stopped at the first one, so fixing a pod meant a restart per typo.
        let err = load(&[
            (POLAR_HEALTH_TICK_SECS_ENV, "soon"),
            (POLAR_HEALTH_EXPIRY_SECS_ENV, "x"),
            (POLAR_HEALTH_STALE_SECS_ENV, "-1"),
        ])
        .unwrap_err();
        for name in [
            POLAR_HEALTH_TICK_SECS_ENV,
            POLAR_HEALTH_EXPIRY_SECS_ENV,
            POLAR_HEALTH_STALE_SECS_ENV,
        ] {
            assert!(err.contains(name), "{name} missing from: {err}");
        }
    }

    #[test]
    fn an_error_names_the_variable_and_what_it_is_for() {
        let err = load(&[(POLAR_HEALTH_TICK_SECS_ENV, "soon")]).unwrap_err();
        assert!(err.contains("between health ticks"), "{err}");
    }

    #[test]
    fn the_section_describes_every_variable_the_actor_reads() {
        let mut names: Vec<_> = crate::vars::SECTION
            .describe()
            .iter()
            .map(|d| d.name)
            .collect();
        names.sort();
        let mut expected = vec![
            POLAR_HEALTH_CASSINI_DEADLINE_SECS_ENV,
            POLAR_HEALTH_CERTS_ENV,
            POLAR_HEALTH_DEP_ENDPOINTS_ENV,
            POLAR_HEALTH_EXPIRY_SECS_ENV,
            POLAR_HEALTH_FILE_ENV,
            POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV,
            POLAR_HEALTH_REJUVENATION_SECS_ENV,
            POLAR_HEALTH_STALE_SECS_ENV,
            POLAR_HEALTH_TICK_SECS_ENV,
        ];
        expected.sort();
        assert_eq!(names, expected);
    }

    #[test]
    fn the_defaults_in_the_declarations_are_the_defaults_in_the_constants() {
        let defaults: HashMap<_, _> = crate::vars::SECTION
            .describe()
            .into_iter()
            .map(|d| (d.name, d.default))
            .collect();
        assert_eq!(defaults[POLAR_HEALTH_TICK_SECS_ENV], Some("30"));
        assert_eq!(defaults[POLAR_HEALTH_EXPIRY_SECS_ENV], Some("60"));
        assert_eq!(defaults[POLAR_HEALTH_STALE_SECS_ENV], Some("90"));
        assert_eq!(defaults[POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV], Some("120"));
        assert_eq!(defaults[POLAR_HEALTH_CASSINI_DEADLINE_SECS_ENV], Some("60"));
        assert_eq!(
            defaults[POLAR_HEALTH_FILE_ENV],
            Some(crate::POLAR_HEALTH_FILE_DEFAULT)
        );
        // These two take their value from the agent when the deployment says nothing.
        assert_eq!(defaults[POLAR_HEALTH_CERTS_ENV], None);
        assert_eq!(defaults[POLAR_HEALTH_REJUVENATION_SECS_ENV], None);
    }

    #[test]
    fn every_declared_default_parses() {
        assert_eq!(
            crate::vars::SECTION.defaults_problems(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_empty_environment_is_acceptable_because_everything_has_a_default() {
        let nothing: HashMap<String, String> = HashMap::new();
        assert_eq!(crate::vars::SECTION.check(&nothing), Vec::<String>::new());
    }

    #[test]
    fn the_section_runs_the_relations_when_the_variables_are_sound() {
        let env: HashMap<String, String> = [(
            POLAR_HEALTH_REJUVENATION_SECS_ENV.to_string(),
            "100".to_string(),
        )]
        .into();
        let problems = crate::vars::SECTION.check(&env);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("hard expiry floor"), "{problems:?}");
    }

    #[test]
    fn the_section_does_not_run_the_relations_on_variables_that_do_not_parse() {
        let env: HashMap<String, String> = [
            (
                POLAR_HEALTH_REJUVENATION_SECS_ENV.to_string(),
                "100".to_string(),
            ),
            (POLAR_HEALTH_TICK_SECS_ENV.to_string(), "soon".to_string()),
        ]
        .into();
        let problems = crate::vars::SECTION.check(&env);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].starts_with(POLAR_HEALTH_TICK_SECS_ENV),
            "{problems:?}"
        );
    }

    #[test]
    fn a_relation_problem_speaks_of_the_variables_the_deployer_sets() {
        // Whoever fixes a deployment has the variable names in front of them, not the
        // names of the fields they are read into.
        for (set, mentions) in [
            (
                vec![
                    (POLAR_HEALTH_REJUVENATION_SECS_ENV, "60"),
                    (POLAR_HEALTH_EXPIRY_SECS_ENV, "60"),
                ],
                vec![
                    POLAR_HEALTH_REJUVENATION_SECS_ENV,
                    POLAR_HEALTH_EXPIRY_SECS_ENV,
                ],
            ),
            (
                vec![(POLAR_HEALTH_REJUVENATION_SECS_ENV, "250")],
                vec![
                    POLAR_HEALTH_REJUVENATION_SECS_ENV,
                    POLAR_HEALTH_EXPIRY_SECS_ENV,
                    POLAR_HEALTH_TICK_SECS_ENV,
                ],
            ),
        ] {
            let err = load(&set).unwrap_err();
            for name in mentions {
                assert!(err.contains(name), "{name} missing from: {err}");
            }
            assert!(
                !err.contains("_threshold_secs"),
                "a field name leaked into: {err}"
            );
        }
    }
}
