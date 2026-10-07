//! polar-healthcheck
//!
//! Kubernetes liveness probe binary for Polar agents. The logic is in
//! `polar_health::probe`; this only reads the environment, prints the report
//! and exits.
//!
//! A dead-man's switch, nothing more: it checks that the agent's health actor is
//! still writing its health file, and fails if the file is missing, unreadable or
//! older than the threshold. It does not judge health, connections or certificates;
//! the actor does, and acts on it in an orderly way. See `polar_health::probe`.
//!
//! Exit 0 = the file is fresh, Exit 1 = it is not (or the settings are malformed).
//!
//! Environment variables:
//!   POLAR_HEALTH_FILE         Path to agent health state JSON (default: /tmp/polar-health.json)
//!   POLAR_HEALTH_STALE_SECS   Fail if health file is older than N seconds (default: 90)

use polar_health::HealthState;
use polar_health::probe::{self, ProbeConfig};

fn main() {
    let config = match ProbeConfig::from_env() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    };
    let report = probe::run(&config, HealthState::now());
    for line in &report.lines {
        eprintln!("{line}");
    }
    std::process::exit(if report.ok { 0 } else { 1 });
}
