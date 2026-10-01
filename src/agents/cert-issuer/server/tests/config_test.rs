//! Unit tests for config validation.
//!
//! Config validation is the first line of defense against
//! misconfigurations that would weaken security. The tests pin down
//! that forbidden algorithms (HS256, "none") are rejected at config
//! load time, not at token validation time, so a bad config fails
//! before any tokens are processed.

use cert_issuer::config::{
    CaConfig, CaProvisioning, ConfigError, IssuerConfig, MtlsConfig, ServiceConfig,
};
use cert_issuer_common::identity::IdentityNormalization;
use std::time::Duration;

fn valid_issuer() -> IssuerConfig {
    IssuerConfig {
        issuer: "https://kubernetes.default.svc".to_string(),
        audience: vec!["polar-cert-issuer".to_string()],
        jwks_uri: None,
        workload_identity_claim: "sub".to_string(),
        instance_binding_claim: "kubernetes.io.pod.uid".to_string(),
        identity_normalization: IdentityNormalization::KubernetesServiceAccount {
            cluster_domain: "cluster.local".to_string(),
        },
        identity_scope_claim: None,
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
        server_lifetime: Duration::from_secs(3600),
        user_lifetime: Duration::from_secs(43200),
        identity_lifetime_overrides: std::collections::HashMap::new(),
        provisioning: CaProvisioning::SelfBootstrap,
    }
}

fn valid_mtls() -> MtlsConfig {
    MtlsConfig {
        server_identity: "cert-issuer.polar.svc.cluster.local".to_string(),
        internal_callers: vec![],
    }
}

fn valid_config() -> ServiceConfig {
    ServiceConfig {
        bind_addr: "0.0.0.0:8443".to_string(),
        issuers: vec![valid_issuer()],
        ca: valid_ca(),
        mtls: valid_mtls(),
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
    cfg.issuers[0].allowed_algorithms.push("HS256".to_string());
    let err = cfg.validate().expect_err("HS256 must be rejected");
    assert!(matches!(
        err,
        ConfigError::ForbiddenAlgorithm { algorithm, .. } if algorithm == "HS256"
    ));
}

#[test]
fn none_algorithm_in_allowed_list_is_rejected() {
    // The JWT "none" algorithm has been the source of multiple major
    // security vulnerabilities. We reject it at config-load time so
    // no operator can accidentally enable it.
    let mut cfg = valid_config();
    cfg.issuers[0].allowed_algorithms.push("none".to_string());
    let err = cfg.validate().expect_err("'none' must be rejected");
    assert!(matches!(err, ConfigError::ForbiddenAlgorithm { .. }));
}

#[test]
fn empty_audience_is_rejected() {
    // Audience matching is non-negotiable; an empty audience would
    // accept any token regardless of intended recipient.
    let mut cfg = valid_config();
    cfg.issuers[0].audience = vec![];
    let err = cfg.validate().expect_err("empty audience must be rejected");
    assert!(matches!(err, ConfigError::EmptyAudience { .. }));
}

#[test]
fn empty_issuer_is_rejected() {
    let mut cfg = valid_config();
    cfg.issuers[0].issuer = String::new();
    let err = cfg.validate().expect_err("empty issuer must be rejected");
    assert!(matches!(err, ConfigError::EmptyIssuer));
}

#[test]
fn cache_ttl_min_greater_than_max_is_rejected() {
    let mut cfg = valid_config();
    cfg.issuers[0].jwks_cache_ttl_min = Duration::from_secs(7200);
    cfg.issuers[0].jwks_cache_ttl_max = Duration::from_secs(3600);
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
    cfg.issuers[0].allowed_algorithms.clear();
    assert!(cfg.validate().is_err());
}

// ---------------------------------------------------------------------
// New coverage for multi-issuer support -- these validation rules
// (EmptyIssuerList, DuplicateIssuer) didn't exist before today; adding
// tests for them alongside the fixes to the rest of this file rather
// than leaving them uncovered.
// ---------------------------------------------------------------------

#[test]
fn empty_issuer_list_is_rejected() {
    let mut cfg = valid_config();
    cfg.issuers = vec![];
    let err = cfg
        .validate()
        .expect_err("a config with no issuers must be rejected");
    assert!(matches!(err, ConfigError::EmptyIssuerList));
}

#[test]
fn duplicate_issuer_urls_are_rejected() {
    let mut cfg = valid_config();
    let mut second_issuer = valid_issuer();
    // Same issuer URL as the first, deliberately -- everything else
    // about it can differ; the URL collision alone must be rejected,
    // since Validator keys its issuer map by this exact string.
    second_issuer.audience = vec!["some-other-audience".to_string()];
    cfg.issuers.push(second_issuer);
    let err = cfg
        .validate()
        .expect_err("two issuers sharing a URL must be rejected");
    assert!(matches!(err, ConfigError::DuplicateIssuer(ref u) if u == &valid_issuer().issuer));
}


// ---------------------------------------------------------------------
// mTLS config coverage
// ---------------------------------------------------------------------

#[test]
fn empty_mtls_server_identity_is_rejected() {
    // The listener can't mint its own TLS server cert without this,
    // which would break every caller including /issue and /validate.
    let mut cfg = valid_config();
    cfg.mtls.server_identity = String::new();
    let err = cfg
        .validate()
        .expect_err("empty mtls server_identity must be rejected");
    assert!(matches!(err, ConfigError::EmptyMtlsServerIdentity));
}

#[test]
fn empty_internal_callers_list_is_accepted() {
    // Empty means /internal/issue is unreachable until explicitly
    // configured -- a safe default, not a misconfiguration.
    let mut cfg = valid_config();
    cfg.mtls.internal_callers = vec![];
    assert!(cfg.validate().is_ok());
}


// ---------------------------------------------------------------------
// CA provisioning mode
// ---------------------------------------------------------------------

#[test]
fn ca_provisioning_defaults_to_self_bootstrap_when_omitted() {
    let json = r#"{"ca_cert_path":"/c","ca_key_path":"/k",
        "default_lifetime":{"secs":1,"nanos":0},
        "server_lifetime":{"secs":1,"nanos":0},
        "user_lifetime":{"secs":1,"nanos":0}}"#;
    let ca: CaConfig = serde_json::from_str(json).expect("parse");
    assert_eq!(ca.provisioning, CaProvisioning::SelfBootstrap);
}

#[test]
fn ca_provisioning_accepts_provisioned() {
    let json = r#"{"ca_cert_path":"/c","ca_key_path":"/k",
        "default_lifetime":{"secs":1,"nanos":0},
        "server_lifetime":{"secs":1,"nanos":0},
        "user_lifetime":{"secs":1,"nanos":0},
        "provisioning":"provisioned"}"#;
    let ca: CaConfig = serde_json::from_str(json).expect("parse");
    assert_eq!(ca.provisioning, CaProvisioning::Provisioned);
}
