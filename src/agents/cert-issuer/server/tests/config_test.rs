//! Unit tests for config validation.
//!
//! Config validation is the first line of defense against
//! misconfigurations that would weaken security. The tests pin down
//! that forbidden algorithms (HS256, "none") are rejected at config
//! load time, not at token validation time, so a bad config fails
//! before any tokens are processed.

use cert_issuer::config::{CaConfig, ConfigError, IssuerConfig, ServiceConfig};
use std::time::Duration;

fn valid_issuer() -> IssuerConfig {
    IssuerConfig {
        issuer: "https://kubernetes.default.svc".to_string(),
        audience: "polar-cert-issuer".to_string(),
        jwks_uri: None,
        workload_identity_claim: "sub".to_string(),
        instance_binding_claim: "kubernetes.io.pod.uid".to_string(),
        allowed_algorithms: vec!["RS256".to_string(), "ES256".to_string()],
        jwks_cache_ttl_min: Duration::from_secs(30),
        jwks_cache_ttl_max: Duration::from_secs(3600),
    }
}

fn valid_ca() -> CaConfig {
    CaConfig {
        ca_cert_path: "/tmp/ca.crt".to_string(),
        ca_key_path: "/tmp/ca.key".to_string(),
        default_lifetime: Duration::from_secs(3600),
    }
}

fn valid_config() -> ServiceConfig {
    ServiceConfig {
        bind_addr: "0.0.0.0:8443".to_string(),
        issuer: valid_issuer(),
        ca: valid_ca(),
    }
}

#[test]
fn valid_config_passes_validation() {
    let cfg = valid_config();
    assert!(cfg.validate().is_ok());
}

#[test]
fn hs256_in_allowed_algorithms_is_rejected() {
    let mut cfg = valid_config();
    cfg.issuer.allowed_algorithms.push("HS256".to_string());
    let err = cfg.validate().expect_err("HS256 must be rejected");
    assert!(matches!(err, ConfigError::ForbiddenAlgorithm(ref a) if a == "HS256"));
}

#[test]
fn none_algorithm_in_allowed_list_is_rejected() {
    // The JWT "none" algorithm has been the source of multiple major
    // security vulnerabilities. We reject it at config-load time so
    // no operator can accidentally enable it.
    let mut cfg = valid_config();
    cfg.issuer.allowed_algorithms.push("none".to_string());
    let err = cfg.validate().expect_err("'none' must be rejected");
    assert!(matches!(err, ConfigError::ForbiddenAlgorithm(_)));
}

#[test]
fn empty_audience_is_rejected() {
    // Audience matching is non-negotiable; an empty audience would
    // accept any token regardless of intended recipient.
    let mut cfg = valid_config();
    cfg.issuer.audience = String::new();
    let err = cfg.validate().expect_err("empty audience must be rejected");
    assert!(matches!(err, ConfigError::EmptyAudience));
}

#[test]
fn empty_issuer_is_rejected() {
    let mut cfg = valid_config();
    cfg.issuer.issuer = String::new();
    let err = cfg.validate().expect_err("empty issuer must be rejected");
    assert!(matches!(err, ConfigError::EmptyIssuer));
}

#[test]
fn cache_ttl_min_greater_than_max_is_rejected() {
    let mut cfg = valid_config();
    cfg.issuer.jwks_cache_ttl_min = Duration::from_secs(7200);
    cfg.issuer.jwks_cache_ttl_max = Duration::from_secs(3600);
    let err = cfg
        .validate()
        .expect_err("inverted TTL bounds must be rejected");
    assert!(matches!(err, ConfigError::InvalidCacheTtl { .. }));
}

#[test]
fn empty_allowed_algorithms_is_rejected() {
    // An empty list would mean "no algorithm is acceptable," which
    // would make every token fail validation. This is almost certainly
    // a misconfiguration rather than an intentional choice.
    let mut cfg = valid_config();
    cfg.issuer.allowed_algorithms.clear();
    assert!(cfg.validate().is_err());
}
