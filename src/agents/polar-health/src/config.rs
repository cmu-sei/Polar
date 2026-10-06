//! The health actor's settings, merged from the environment and the caller's
//! `HealthCheckArgs` in one place so the merge and its validation can be tested
//! without spawning an actor or touching the process environment.

use crate::{
    DEFAULT_EXPIRY_THRESHOLD_SECS, DEFAULT_GRAPH_DEADLINE_SECS, DEFAULT_TICK_SECS, DepCertEndpoint,
    HealthCheckArgs, POLAR_HEALTH_CERTS_ENV, POLAR_HEALTH_DEP_ENDPOINTS_ENV,
    POLAR_HEALTH_EXPIRY_SECS_ENV, POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV,
    POLAR_HEALTH_REJUVENATION_SECS_ENV, POLAR_HEALTH_TICK_SECS_ENV,
};

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
    /// The agent's own client certificates.
    pub cert_paths: Vec<String>,
    /// Dependency endpoints whose certificates must be healthy before rejuvenating.
    pub dep_cert_endpoints: Vec<DepCertEndpoint>,
}

impl HealthConfig {
    /// Merges the process environment over `args`, then validates.
    pub fn load(args: HealthCheckArgs) -> Result<Self, String> {
        Self::from_lookup(args, |name| std::env::var(name).ok())
    }

    /// As [`load`](Self::load), reading variables through `lookup`.
    ///
    /// The environment wins over `args` for the dependency endpoints and the
    /// rejuvenation threshold, so a deployment can set them without a code change.
    /// A numeric variable that does not parse falls back to the default without
    /// complaint.
    pub fn from_lookup(
        args: HealthCheckArgs,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let cert_paths = lookup(POLAR_HEALTH_CERTS_ENV)
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();

        let dep_cert_endpoints = match lookup(POLAR_HEALTH_DEP_ENDPOINTS_ENV) {
            Some(raw) if !raw.trim().is_empty() => raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(DepCertEndpoint::parse)
                .collect::<Result<Vec<_>, _>>()?,
            _ => args.dep_cert_endpoints,
        };

        let config = Self {
            expects_graph: args.expects_graph,
            rejuvenation_threshold_secs: lookup(POLAR_HEALTH_REJUVENATION_SECS_ENV)
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(args.rejuvenation_threshold_secs),
            expiry_threshold_secs: lookup(POLAR_HEALTH_EXPIRY_SECS_ENV)
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(DEFAULT_EXPIRY_THRESHOLD_SECS),
            tick_secs: lookup(POLAR_HEALTH_TICK_SECS_ENV)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(DEFAULT_TICK_SECS),
            graph_deadline_secs: lookup(POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(DEFAULT_GRAPH_DEADLINE_SECS),
            cert_paths,
            dep_cert_endpoints,
        };
        config.validate()?;
        Ok(config)
    }

    /// Rejects settings the actor cannot work with.
    pub fn validate(&self) -> Result<(), String> {
        if self.expiry_threshold_secs <= 0 {
            return Err(format!(
                "POLAR_HEALTH_EXPIRY_SECS must be positive, got {}",
                self.expiry_threshold_secs
            ));
        }
        if self.rejuvenation_threshold_secs <= self.expiry_threshold_secs {
            return Err(format!(
                "rejuvenation_threshold_secs ({}) must be greater than expiry_threshold_secs ({})",
                self.rejuvenation_threshold_secs, self.expiry_threshold_secs
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn args() -> HealthCheckArgs {
        HealthCheckArgs {
            expects_graph: true,
            rejuvenation_threshold_secs: 300,
            dep_cert_endpoints: vec![DepCertEndpoint::parse("from-args.example:443:100").unwrap()],
        }
    }

    fn load(vars: &[(&str, &str)]) -> Result<HealthConfig, String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        HealthConfig::from_lookup(args(), |name| map.get(name).cloned())
    }

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let c = load(&[]).unwrap();
        assert!(c.expects_graph);
        assert_eq!(c.rejuvenation_threshold_secs, 300);
        assert_eq!(c.expiry_threshold_secs, DEFAULT_EXPIRY_THRESHOLD_SECS);
        assert_eq!(c.tick_secs, DEFAULT_TICK_SECS);
        assert_eq!(c.graph_deadline_secs, DEFAULT_GRAPH_DEADLINE_SECS);
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
        ])
        .unwrap();
        assert_eq!(c.rejuvenation_threshold_secs, 900);
        assert_eq!(c.expiry_threshold_secs, 120);
        assert_eq!(c.tick_secs, 5);
        assert_eq!(c.graph_deadline_secs, 40);
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

    /// Today's behaviour, kept on purpose by the refactor: an unparseable number
    /// is silently replaced by the default. The startup consistency check
    /// (health rework S2) is where this becomes an error.
    #[test]
    fn unparseable_numbers_fall_back_to_the_defaults_silently() {
        let c = load(&[
            (POLAR_HEALTH_TICK_SECS_ENV, "soon"),
            (POLAR_HEALTH_EXPIRY_SECS_ENV, "x"),
            (POLAR_HEALTH_REJUVENATION_SECS_ENV, "1.5"),
            (POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV, "-3"),
        ])
        .unwrap();
        assert_eq!(c.tick_secs, DEFAULT_TICK_SECS);
        assert_eq!(c.expiry_threshold_secs, DEFAULT_EXPIRY_THRESHOLD_SECS);
        assert_eq!(c.rejuvenation_threshold_secs, 300);
        assert_eq!(c.graph_deadline_secs, DEFAULT_GRAPH_DEADLINE_SECS);
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
                (POLAR_HEALTH_REJUVENATION_SECS_ENV, "61"),
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
}
