//! The Kubernetes liveness probe, as a library.
//!
//! `polar-healthcheck` is a thin wrapper around [`run`]. The logic lives here so
//! it can be tested against real files and a chosen clock, without a process
//! boundary.

use std::fs;

use crate::{
    DEFAULT_EXPIRY_THRESHOLD_SECS, DEFAULT_STALE_THRESHOLD_SECS, HealthState, HealthStatus,
    POLAR_HEALTH_CERTS_ENV, POLAR_HEALTH_EXPIRY_SECS_ENV, POLAR_HEALTH_FILE_DEFAULT,
    POLAR_HEALTH_FILE_ENV, POLAR_HEALTH_STALE_SECS_ENV, certs,
};

/// What the probe is configured with.
#[derive(Debug, Clone)]
pub struct ProbeConfig {
    /// Certificates to check; empty means skip the certificate check.
    pub cert_paths: Vec<String>,
    /// Fail if a certificate has less than this much life left.
    pub expiry_threshold_secs: i64,
    /// The health file the agent's actor writes.
    pub health_file: String,
    /// Fail if the health file is older than this.
    pub stale_threshold_secs: u64,
    /// The raw value of `POLAR_HEALTH_CERTS`, kept to tell "unset" from "set but empty".
    certs_env_empty: bool,
}

impl ProbeConfig {
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// As [`from_env`](Self::from_env), reading variables through `lookup`.
    /// A numeric variable that does not parse falls back to the default.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let certs_env = lookup(POLAR_HEALTH_CERTS_ENV).unwrap_or_default();
        Self {
            certs_env_empty: certs_env.is_empty(),
            cert_paths: certs_env
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            expiry_threshold_secs: lookup(POLAR_HEALTH_EXPIRY_SECS_ENV)
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_EXPIRY_THRESHOLD_SECS),
            health_file: lookup(POLAR_HEALTH_FILE_ENV)
                .unwrap_or_else(|| POLAR_HEALTH_FILE_DEFAULT.to_string()),
            stale_threshold_secs: lookup(POLAR_HEALTH_STALE_SECS_ENV)
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_STALE_THRESHOLD_SECS),
        }
    }
}

/// The probe's verdict and what it would print to stderr.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeReport {
    /// `true` means exit 0.
    pub ok: bool,
    pub lines: Vec<String>,
}

fn check_certs(config: &ProbeConfig, now: i64, lines: &mut Vec<String>) -> bool {
    if config.certs_env_empty {
        lines.push(format!(
            "WARN: {POLAR_HEALTH_CERTS_ENV} not set, skipping cert checks"
        ));
        return true;
    }

    let mut all_ok = true;
    for path in &config.cert_paths {
        match certs::ttl_secs(path, now) {
            Err(e) => {
                lines.push(format!("ERROR: {e}"));
                all_ok = false;
            }
            Ok(secs_remaining) if secs_remaining < config.expiry_threshold_secs => {
                lines.push(format!(
                    "ERROR: cert {path} expires in {secs_remaining}s (threshold: {}s)",
                    config.expiry_threshold_secs
                ));
                all_ok = false;
            }
            Ok(secs_remaining) => {
                lines.push(format!("OK: cert {path} valid for {secs_remaining}s"));
            }
        }
    }
    all_ok
}

fn check_health_file(config: &ProbeConfig, now: u64, lines: &mut Vec<String>) -> bool {
    let path = &config.health_file;

    let contents = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            lines.push(format!("ERROR: failed to read health file {path}: {e}"));
            return false;
        }
    };
    let state = match serde_json::from_str::<HealthState>(&contents) {
        Ok(s) => s,
        Err(e) => {
            lines.push(format!("ERROR: failed to parse health file {path}: {e}"));
            return false;
        }
    };

    let age = now.saturating_sub(state.timestamp);
    if age > config.stale_threshold_secs {
        lines.push(format!(
            "ERROR: health file is {age}s old (threshold: {}s)",
            config.stale_threshold_secs
        ));
        return false;
    }

    // Printed whether or not the agent is healthy; the wording is wrong for the
    // healthy case and the probe rework (S4) removes it.
    lines.push(format!(
        "ERROR: agent status is '{}' (cassini={}, graph={})",
        state.status, state.cassini_connected, state.graph_connected
    ));
    state.status == HealthStatus::Healthy
}

/// Runs both checks at `now` (Unix seconds). Neither short-circuits the other.
pub fn run(config: &ProbeConfig, now: u64) -> ProbeReport {
    let mut lines = Vec::new();
    let certs_ok = check_certs(config, now as i64, &mut lines);
    let health_ok = check_health_file(config, now, &mut lines);
    ProbeReport {
        ok: certs_ok && health_ok,
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::cert_file;

    const NOW: u64 = 2_000_000;

    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }

        fn health_file(&self) -> String {
            self.dir.path().join("health.json").to_string_lossy().into()
        }

        fn write_state(&self, status: HealthStatus, timestamp: u64) {
            let state = HealthState::at(status, timestamp, true, true);
            fs::write(self.health_file(), serde_json::to_string(&state).unwrap()).unwrap();
        }

        fn config(&self, certs: &[String]) -> ProbeConfig {
            let joined = certs.join(",");
            let file = self.health_file();
            ProbeConfig::from_lookup(|name| match name {
                POLAR_HEALTH_CERTS_ENV => Some(joined.clone()),
                POLAR_HEALTH_FILE_ENV => Some(file.clone()),
                _ => None,
            })
        }
    }

    #[test]
    fn a_fresh_healthy_file_passes() {
        let f = Fixture::new();
        f.write_state(HealthStatus::Healthy, NOW - 10);
        assert!(run(&f.config(&[]), NOW).ok);
    }

    #[test]
    fn a_stale_file_fails() {
        let f = Fixture::new();
        f.write_state(HealthStatus::Healthy, NOW - 91);
        let report = run(&f.config(&[]), NOW);
        assert!(!report.ok);
        assert!(
            report
                .lines
                .iter()
                .any(|l| l.contains("health file is 91s old")),
            "{:?}",
            report.lines
        );
    }

    #[test]
    fn the_stale_threshold_itself_is_still_fresh() {
        let f = Fixture::new();
        f.write_state(HealthStatus::Healthy, NOW - 90);
        assert!(run(&f.config(&[]), NOW).ok);
    }

    #[test]
    fn an_unhealthy_status_fails_today() {
        // The probe re-judges the status the actor already decided. The rework
        // (S4) makes the probe freshness-only and flips this.
        let f = Fixture::new();
        f.write_state(HealthStatus::Unhealthy, NOW - 10);
        assert!(!run(&f.config(&[]), NOW).ok);
    }

    #[test]
    fn a_missing_file_fails() {
        let f = Fixture::new();
        let report = run(&f.config(&[]), NOW);
        assert!(!report.ok);
        assert!(
            report
                .lines
                .iter()
                .any(|l| l.contains("failed to read health file"))
        );
    }

    #[test]
    fn an_unparseable_file_fails() {
        let f = Fixture::new();
        fs::write(f.health_file(), "{not json").unwrap();
        let report = run(&f.config(&[]), NOW);
        assert!(!report.ok);
        assert!(
            report
                .lines
                .iter()
                .any(|l| l.contains("failed to parse health file"))
        );
    }

    #[test]
    fn a_future_timestamp_counts_as_fresh() {
        let f = Fixture::new();
        f.write_state(HealthStatus::Healthy, NOW + 500);
        assert!(run(&f.config(&[]), NOW).ok);
    }

    #[test]
    fn an_unset_cert_variable_skips_the_cert_check() {
        let f = Fixture::new();
        f.write_state(HealthStatus::Healthy, NOW);
        let file = f.health_file();
        let config =
            ProbeConfig::from_lookup(|name| (name == POLAR_HEALTH_FILE_ENV).then(|| file.clone()));
        let report = run(&config, NOW);
        assert!(report.ok);
        assert!(report.lines[0].starts_with("WARN:"), "{:?}", report.lines);
    }

    #[test]
    fn a_short_lived_cert_fails_even_with_a_healthy_file() {
        let f = Fixture::new();
        f.write_state(HealthStatus::Healthy, crate::HealthState::now());
        let cert = cert_file(&f.dir, "a.pem", 30);
        let now = crate::HealthState::now();
        let report = run(&f.config(&[cert]), now);
        assert!(!report.ok);
        assert!(report.lines.iter().any(|l| l.contains("threshold: 60s")));
    }

    #[test]
    fn a_missing_cert_fails() {
        let f = Fixture::new();
        f.write_state(HealthStatus::Healthy, NOW);
        let report = run(
            &f.config(&["/nonexistent/polar-probe-test.pem".into()]),
            NOW,
        );
        assert!(!report.ok);
    }

    #[test]
    fn every_cert_is_reported_not_just_the_first_failure() {
        let f = Fixture::new();
        f.write_state(HealthStatus::Healthy, crate::HealthState::now());
        let short = cert_file(&f.dir, "short.pem", 30);
        let long = cert_file(&f.dir, "long.pem", 3600);
        let report = run(&f.config(&[short, long]), crate::HealthState::now());
        assert!(!report.ok);
        assert!(report.lines.iter().any(|l| l.starts_with("ERROR: cert")));
        assert!(report.lines.iter().any(|l| l.starts_with("OK: cert")));
    }

    #[test]
    fn a_bad_certificate_does_not_hide_a_stale_file() {
        let f = Fixture::new();
        f.write_state(HealthStatus::Healthy, NOW - 500);
        let report = run(
            &f.config(&["/nonexistent/polar-probe-test.pem".into()]),
            NOW,
        );
        assert!(!report.ok);
        assert!(
            report
                .lines
                .iter()
                .any(|l| l.contains("failed to read cert"))
        );
        assert!(report.lines.iter().any(|l| l.contains("health file is")));
    }

    #[test]
    fn settings_come_from_the_environment_with_defaults() {
        let d = ProbeConfig::from_lookup(|_| None);
        assert_eq!(d.expiry_threshold_secs, 60);
        assert_eq!(d.stale_threshold_secs, 90);
        assert_eq!(d.health_file, "/tmp/polar-health.json");

        let c = ProbeConfig::from_lookup(|name| match name {
            POLAR_HEALTH_EXPIRY_SECS_ENV => Some("120".into()),
            POLAR_HEALTH_STALE_SECS_ENV => Some("45".into()),
            POLAR_HEALTH_FILE_ENV => Some("/run/h.json".into()),
            _ => None,
        });
        assert_eq!(c.expiry_threshold_secs, 120);
        assert_eq!(c.stale_threshold_secs, 45);
        assert_eq!(c.health_file, "/run/h.json");
    }

    /// Cosmetic defect pinned as it is: a healthy agent still prints an
    /// "ERROR: agent status is" line. The probe rework removes it.
    #[test]
    fn pinned_defect_a_healthy_agent_still_prints_an_error_line() {
        let f = Fixture::new();
        f.write_state(HealthStatus::Healthy, NOW);
        let report = run(&f.config(&[]), NOW);
        assert!(report.ok);
        assert!(
            report
                .lines
                .iter()
                .any(|l| l.starts_with("ERROR: agent status is 'healthy'")),
            "{:?}",
            report.lines
        );
    }
}
