//! polar-healthcheck
//!
//! Kubernetes liveness probe binary for Polar agents.
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

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use polar_health::{
    HealthState, POLAR_HEALTH_CERTS_ENV, POLAR_HEALTH_EXPIRY_SECS_ENV, POLAR_HEALTH_FILE_DEFAULT,
    POLAR_HEALTH_FILE_ENV, POLAR_HEALTH_STALE_SECS_ENV,
};
use x509_parser::prelude::*;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn check_certs(expiry_threshold: i64) -> bool {
    let certs_env = std::env::var(POLAR_HEALTH_CERTS_ENV).unwrap_or_default();
    if certs_env.is_empty() {
        eprintln!("WARN: {POLAR_HEALTH_CERTS_ENV} not set, skipping cert checks");
        return true;
    }

    let now = now_secs() as i64;
    let mut all_ok = true;

    for path in certs_env.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match fs::read(path) {
            Err(e) => {
                eprintln!("ERROR: failed to read cert {path}: {e}");
                all_ok = false;
            }
            Ok(data) => match parse_x509_pem(&data) {
                Err(e) => {
                    eprintln!("ERROR: failed to parse PEM {path}: {e}");
                    all_ok = false;
                }
                Ok((_, pem)) => match pem.parse_x509() {
                    Err(e) => {
                        eprintln!("ERROR: failed to parse x509 in {path}: {e}");
                        all_ok = false;
                    }
                    Ok(cert) => {
                        let not_after = cert.validity().not_after.timestamp();
                        let secs_remaining = not_after - now;
                        if secs_remaining < expiry_threshold {
                            eprintln!(
                                "ERROR: cert {path} expires in {secs_remaining}s (threshold: {expiry_threshold}s)"
                            );
                            all_ok = false;
                        } else {
                            eprintln!("OK: cert {path} valid for {secs_remaining}s");
                        }
                    }
                },
            },
        }
    }

    all_ok
}

fn check_health_file(stale_threshold: u64) -> bool {
    let path = std::env::var(POLAR_HEALTH_FILE_ENV)
        .unwrap_or_else(|_| POLAR_HEALTH_FILE_DEFAULT.to_string());

    match fs::read_to_string(&path) {
        Err(e) => {
            eprintln!("ERROR: failed to read health file {path}: {e}");
            false
        }
        Ok(contents) => match serde_json::from_str::<HealthState>(&contents) {
            Err(e) => {
                eprintln!("ERROR: failed to parse health file {path}: {e}");
                false
            }
            Ok(state) => {
                let age = now_secs().saturating_sub(state.timestamp);
                if age > stale_threshold {
                    eprintln!("ERROR: health file is {age}s old (threshold: {stale_threshold}s)");
                    return false;
                }
                if state.status != "healthy" {
                    eprintln!(
                        "ERROR: agent status is '{}' (cassini={}, neo4j={})",
                        state.status, state.cassini_connected, state.neo4j_connected
                    );
                    return false;
                }
                eprintln!(
                    "OK: agent healthy, age={age}s, cassini={}, neo4j={}",
                    state.cassini_connected, state.neo4j_connected
                );
                true
            }
        },
    }
}

fn main() {
    let expiry_threshold: i64 = std::env::var(POLAR_HEALTH_EXPIRY_SECS_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let stale_threshold: u64 = std::env::var(POLAR_HEALTH_STALE_SECS_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(90);

    let certs_ok = check_certs(expiry_threshold);
    let health_ok = check_health_file(stale_threshold);

    if certs_ok && health_ok {
        std::process::exit(0);
    } else {
        std::process::exit(1);
    }
}
