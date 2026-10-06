//! polar-healthcheck
//!
//! Kubernetes liveness probe binary for Polar agents. The logic is in
//! `polar_health::probe`; this only reads the environment, prints the report
//! and exits.
//!
//! Checks:
//!   1. TLS cert expiry for all certs listed in POLAR_HEALTH_CERTS
//!   2. Agent health state file freshness and status
//!
//! Exit 0 = healthy, Exit 1 = unhealthy.
//!
//! Environment variables:
//!   POLAR_HEALTH_CERTS        Comma-separated list of PEM cert paths to check
//!   POLAR_HEALTH_EXPIRY_SECS  Fail if cert expires within N seconds (default: 60)
//!   POLAR_HEALTH_FILE         Path to agent health state JSON (default: /tmp/polar-health.json)
//!   POLAR_HEALTH_STALE_SECS   Fail if health file is older than N seconds (default: 90)

use polar_health::HealthState;
use polar_health::probe::{self, ProbeConfig};

fn main() {
    let report = probe::run(&ProbeConfig::from_env(), HealthState::now());
    for line in &report.lines {
        eprintln!("{line}");
    }
    std::process::exit(if report.ok { 0 } else { 1 });
}
