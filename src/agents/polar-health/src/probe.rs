//! The Kubernetes liveness probe, as a library.
//!
//! The probe is the dead-man's switch for the one case the health actor cannot cover:
//! a process in which nothing runs any more. It asks one question, whether the actor
//! is still writing the health file, and answers it from the file's age alone.
//!
//! It deliberately does not re-judge anything the actor already decided. Health
//! status, connection state and certificate lifetimes are the actor's business: it
//! acts on them, in an orderly way, long before they matter. A probe that judged
//! them a second time could only disagree with the actor, and every disagreement
//! ends in the kubelet killing a pod the actor was in the middle of shutting down.
//!
//! `polar-healthcheck` is a thin wrapper around [`run`]. The logic lives here so it
//! can be tested against a chosen clock, and run against the machine in
//! `simulation.rs`, without a process boundary.

use std::fs;

use crate::HealthState;
use crate::vars::{FILE, STALE_SECS};
use polar_env::{Env, ProcessEnv};

/// What the probe is configured with.
#[derive(Debug, Clone)]
pub struct ProbeConfig {
    /// The health file the agent's actor writes.
    pub health_file: String,
    /// Fail if the health file is older than this.
    pub stale_threshold_secs: u64,
}

impl ProbeConfig {
    pub fn from_env() -> Result<Self, String> {
        Self::from_environment(ProcessEnv)
    }

    /// As [`from_env`](Self::from_env), for any process environment: the configuration
    /// document `POLAR_CONFIG` names if it is set (the probe reads the same document as the
    /// agent it watches, so the two cannot disagree about the health file), the process
    /// environment otherwise.
    pub fn from_environment(process: impl Env + 'static) -> Result<Self, String> {
        let (env, _) = polar_env::from_environment(process).map_err(|e| e.to_string())?;
        Self::read(&*env)
    }

    /// As [`from_env`](Self::from_env), reading variables from `env`. A variable that is
    /// set but is not a number is an error, as it is for the actor, which reads the same
    /// declaration.
    pub fn read(env: &dyn Env) -> Result<Self, String> {
        match (FILE.read(env), STALE_SECS.read(env)) {
            (Ok(health_file), Ok(stale_threshold_secs)) => Ok(Self {
                health_file,
                stale_threshold_secs,
            }),
            (file, stale) => Err([file.err(), stale.err()]
                .into_iter()
                .flatten()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join("; ")),
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

impl ProbeReport {
    fn failed(line: String) -> Self {
        Self {
            ok: false,
            lines: vec![format!("ERROR: {line}")],
        }
    }
}

/// Judges the contents of the health file at `now` (Unix seconds).
///
/// Passes if the file parses and is no older than `stale_threshold_secs`. What the
/// file says about the agent is reported, for whoever reads the pod's events, and
/// never judged.
pub fn judge(contents: &str, now: u64, stale_threshold_secs: u64) -> ProbeReport {
    let state = match serde_json::from_str::<HealthState>(contents) {
        Ok(state) => state,
        Err(e) => return ProbeReport::failed(format!("failed to parse health file: {e}")),
    };

    // A timestamp in the future (a clock that stepped back) counts as fresh: the
    // actor wrote it, and the probe's question is only whether it is still writing.
    let age = now.saturating_sub(state.timestamp);
    if age > stale_threshold_secs {
        return ProbeReport::failed(format!(
            "health file is {age}s old (threshold: {stale_threshold_secs}s)"
        ));
    }

    ProbeReport {
        ok: true,
        lines: vec![format!(
            "OK: health file is {age}s old (threshold: {stale_threshold_secs}s); the agent \
             reports '{}' (phase {}, cassini={}, graph={})",
            state.status, state.phase, state.cassini_connected, state.graph_connected
        )],
    }
}

/// Reads the health file and judges it at `now`.
pub fn run(config: &ProbeConfig, now: u64) -> ProbeReport {
    match fs::read_to_string(&config.health_file) {
        Ok(contents) => judge(&contents, now, config.stale_threshold_secs),
        Err(e) => ProbeReport::failed(format!(
            "failed to read health file {}: {e}",
            config.health_file
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::cert_file;
    use crate::{
        HealthStatus, POLAR_HEALTH_CERTS_ENV, POLAR_HEALTH_FILE_ENV, POLAR_HEALTH_STALE_SECS_ENV,
    };

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
            ProbeConfig::read(&|name: &str| match name {
                POLAR_HEALTH_CERTS_ENV => Some(joined.clone()),
                POLAR_HEALTH_FILE_ENV => Some(file.clone()),
                _ => None,
            })
            .unwrap()
        }
    }

    #[test]
    fn a_fresh_healthy_file_passes() {
        let f = Fixture::new();
        f.write_state(HealthStatus::Healthy, NOW - 10);
        assert!(run(&f.config(&[]), NOW).ok);
    }

    #[test]
    fn the_probe_does_not_care_which_phase_the_agent_is_in() {
        for phase in [
            crate::HealthPhase::Normal,
            crate::HealthPhase::WaitingForDeps,
            crate::HealthPhase::Jittering,
        ] {
            let f = Fixture::new();
            let state =
                HealthState::at(HealthStatus::Healthy, NOW - 10, true, true).with_phase(phase);
            fs::write(f.health_file(), serde_json::to_string(&state).unwrap()).unwrap();
            assert!(run(&f.config(&[]), NOW).ok, "{phase:?}");
        }
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
    fn an_unhealthy_status_does_not_fail_a_fresh_file() {
        // The actor decides health and acts on it. The probe only asks whether the
        // actor is still alive and reporting, so a fresh file passes whatever the
        // status says; judging it again here could only make the two disagree.
        for status in [HealthStatus::Healthy, HealthStatus::Unhealthy] {
            let f = Fixture::new();
            f.write_state(status.clone(), NOW - 10);
            assert!(run(&f.config(&[]), NOW).ok, "{status:?}");
        }
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
    fn certificates_are_not_the_probes_business() {
        // The actor checks the agent's own certificates and reports a verdict well
        // before they matter; the probe used to check them a second time.
        let f = Fixture::new();
        f.write_state(HealthStatus::Healthy, crate::HealthState::now());
        let short = cert_file(&f.dir, "short.pem", 30);
        let expired = cert_file(&f.dir, "expired.pem", -100);
        let report = run(
            &f.config(&[short, expired, "/nonexistent/polar-probe-test.pem".into()]),
            crate::HealthState::now(),
        );
        assert!(report.ok, "{:?}", report.lines);
        assert!(
            report.lines.iter().all(|l| !l.contains("cert")),
            "the probe says nothing about certificates: {:?}",
            report.lines
        );
    }

    #[test]
    fn a_stale_file_is_still_reported_when_certificates_are_bad() {
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
                .any(|l| l.contains("health file is 500s old"))
        );
    }

    #[test]
    fn settings_come_from_the_environment_with_defaults() {
        let d = ProbeConfig::read(&|_: &str| None::<String>).unwrap();
        assert_eq!(d.stale_threshold_secs, 90);
        assert_eq!(d.health_file, "/tmp/polar-health.json");

        let c = ProbeConfig::read(&|name: &str| match name {
            POLAR_HEALTH_STALE_SECS_ENV => Some("45".into()),
            POLAR_HEALTH_FILE_ENV => Some("/run/h.json".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(c.stale_threshold_secs, 45);
        assert_eq!(c.health_file, "/run/h.json");
    }

    #[test]
    fn a_malformed_stale_threshold_is_an_error_as_it_is_for_the_actor() {
        let err = ProbeConfig::read(&|name: &str| {
            (name == POLAR_HEALTH_STALE_SECS_ENV).then(|| "soon".to_string())
        })
        .unwrap_err();
        assert!(err.contains(POLAR_HEALTH_STALE_SECS_ENV), "{err}");
    }

    #[test]
    fn judge_needs_no_file() {
        let state = HealthState::at(HealthStatus::Healthy, 1000, true, true);
        let json = serde_json::to_string(&state).unwrap();
        assert!(judge(&json, 1090, 90).ok);
        assert!(!judge(&json, 1091, 90).ok);
        assert!(!judge("{not json", 1000, 90).ok);
    }

    #[test]
    fn a_fresh_file_reports_one_informational_line_and_no_error() {
        let f = Fixture::new();
        f.write_state(HealthStatus::Healthy, NOW - 4);
        let report = run(&f.config(&[]), NOW);
        assert!(report.ok);
        assert_eq!(report.lines.len(), 1, "{:?}", report.lines);
        let line = &report.lines[0];
        assert!(line.starts_with("OK: health file is 4s old"), "{line}");
        assert!(line.contains("'healthy'"), "{line}");
        assert!(line.contains("normal"), "the phase is shown: {line}");
    }

    #[test]
    fn what_the_agent_says_about_itself_is_shown_even_when_it_is_unhealthy() {
        // Informational only: it is what an operator reads in the pod events.
        let f = Fixture::new();
        f.write_state(HealthStatus::Unhealthy, NOW - 4);
        let report = run(&f.config(&[]), NOW);
        assert!(report.ok);
        assert!(
            report.lines[0].contains("'unhealthy'"),
            "{:?}",
            report.lines
        );
    }

    #[test]
    fn the_probe_reads_the_document_the_agent_reads() {
        use polar_env::{DocValue, Document};
        let f = Fixture::new();
        let mut document = Document::scaffold(&[(&crate::vars::SECTION, None)]);
        for variable in document.sections.iter_mut().flat_map(|s| s.variables.iter_mut()) {
            match variable.name.as_str() {
                POLAR_HEALTH_FILE_ENV => {
                    variable.value = Some(DocValue::Literal("/from/the/document".into()))
                }
                POLAR_HEALTH_STALE_SECS_ENV => variable.value = Some(DocValue::Literal("45".into())),
                _ => {}
            }
        }
        let path = f.dir.path().join("config.json");
        fs::write(&path, document.to_json()).unwrap();
        let path_text = path.to_string_lossy().into_owned();

        let process = move |name: &str| match name {
            "POLAR_CONFIG" => Some(path_text.clone()),
            // What the environment says is not what the probe is to go by.
            POLAR_HEALTH_FILE_ENV => Some("/from/the/environment".to_string()),
            _ => None,
        };
        let config = ProbeConfig::from_environment(process).unwrap();
        assert_eq!(config.health_file, "/from/the/document");
        assert_eq!(config.stale_threshold_secs, 45);
    }

    #[test]
    fn the_probe_still_reads_the_environment_when_there_is_no_document() {
        let process = |name: &str| (name == POLAR_HEALTH_FILE_ENV).then(|| "/env/health".to_string());
        let config = ProbeConfig::from_environment(process).unwrap();
        assert_eq!(config.health_file, "/env/health");
    }

    #[test]
    fn a_document_that_cannot_be_read_fails_the_probe_with_the_reason() {
        let process = |name: &str| (name == "POLAR_CONFIG").then(|| "/no/such.json".to_string());
        let error = ProbeConfig::from_environment(process).unwrap_err();
        assert!(error.contains("/no/such.json"), "{error}");
    }
}
