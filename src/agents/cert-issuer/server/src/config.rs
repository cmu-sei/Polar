//! Configuration loaded at startup.
//!
//! `IssuerConfig` is written generically (issuer URL, audience, claim
//! paths). `ServiceConfig.issuers` holds one or more of them; the
//! validator selects which to use per-request by the token's own
//! (unverified, pre-signature) `iss` claim. See oidc.rs's Validator
//! for the selection logic.
//!
//! # Validation philosophy
//!
//! Validation runs once at startup. Anything we can catch at config
//! load is something we don't have to defend against at request time.
//! In particular:
//!
//! - Forbidden algorithms (HS*, "none") are rejected here, not just
//!   at token validation, because rejecting them at load means the
//!   service can't even start with a dangerous configuration.
//! - Empty audience is rejected because audience matching is the
//!   primary defense against token replay across services. An empty
//!   audience would silently accept any audience.
//! - Empty issuer is rejected for the same reason — accepting any
//!   `iss` value would break our trust anchor.
//! - TTL bounds are checked because an inverted min/max would cause
//!   `clamp()` to panic at request time.
//! - Empty `allowed_algorithms` is rejected because it makes every
//!   token fail validation, which is almost certainly not what the
//!   operator meant.
//!
//! Things we *don't* validate here include URL well-formedness
//! (network errors at first JWKS fetch will surface this), key file
//! existence (loaded by the secrets backend, not from disk in
//! production), and bind address parseability (axum will fail at
//! bind time with a clearer error than we'd produce).

use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssuerConfig {
    /// Expected `iss` claim. Tokens with non-matching iss are rejected
    /// before signature work.
    pub issuer: String,

    /// Expected `aud` claim. REQUIRED — there is no "match any audience"
    /// mode. Misconfigurations here are surfaced as INVALID_AUDIENCE so
    /// they're distinguishable from other token failures.
    pub audience: Vec<String>,

    /// JWKS URL. If None, derived from the OIDC discovery document.
    pub jwks_uri: Option<String>,

    /// JSON path within the JWT claims for the workload identity.
    /// For Kubernetes: "sub" (yields system:serviceaccount:ns:sa).
    pub workload_identity_claim: String,

    /// JSON Pointer-style path for the per-instance binding identifier.
    /// For Kubernetes: "kubernetes.io/pod/uid" (the literal key
    /// `kubernetes.io` is preserved as a single segment because it
    /// contains a dot — see the doc comment on `extract_string_claim`
    /// in oidc.rs).
    /// Used in audit telemetry; not used for protocol binding in v1.
    pub instance_binding_claim: String,

    /// Allowed signing algorithms. HS256/384/512 and "none" are NEVER
    /// permitted regardless of what is in this list — `validate()`
    /// rejects them at load time.
    pub allowed_algorithms: Vec<String>,

    pub jwks_cache_ttl_min: Duration,
    pub jwks_cache_ttl_max: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaConfig {
    pub ca_cert_path: String,
    pub ca_key_path: String,
    pub default_lifetime: Duration,

    /// Lifetime for server certs (e.g. Cassini). Allows long-lived
    /// infrastructure certs without affecting client cert TTL.
    pub server_lifetime: Duration,

    /// Per-identity lifetime overrides, keyed by the exact workload
    /// identity string embedded in the CSR's SAN. Takes precedence over
    /// both default_lifetime and server_lifetime when the requesting
    /// identity matches a key here. Empty/absent by default.
    #[serde(default)]
    pub identity_lifetime_overrides: std::collections::HashMap<String, Duration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub bind_addr: String,
    /// Every issuer this instance trusts. v1 configured exactly one
    /// (Kubernetes); v2 supports any number, selected per-request by
    /// the token's own (unverified, pre-signature-check) `iss` claim.
    /// See oidc.rs's Validator for the selection logic -- this is just
    /// the config surface.
    pub issuers: Vec<IssuerConfig>,
    pub ca: CaConfig,
}

impl ServiceConfig {
    /// Validate that no field has an obviously invalid value.
    /// Called once at startup; returns the first error found.
    ///
    /// The order of checks is deliberate: we check the most
    /// security-critical invariants first (forbidden algorithms,
    /// empty audience, empty issuer, duplicate issuers) so that if
    /// multiple things are wrong, the operator hears about the most
    /// serious one first. Every per-issuer check runs across all
    /// configured issuers, not just the first.
    pub fn validate(&self) -> Result<(), ConfigError> {
        // ---- Empty issuer list ----
        //
        // A service with no trusted issuers can't validate any token
        // at all. This is almost certainly a config mistake, not an
        // intentional "accept nothing" mode -- if that's ever a real
        // need, it should be a different, explicit config flag.
        if self.issuers.is_empty() {
            return Err(ConfigError::EmptyIssuerList);
        }

        // ---- Duplicate issuer URLs ----
        //
        // The validator keys its issuer map by this exact string; two
        // configs sharing one would let the second silently shadow the
        // first, with whichever config "wins" depending on map
        // insertion order rather than anything the operator intended.
        // Caught here, at load time, rather than surfacing as
        // mysterious per-request behavior later.
        let mut seen_issuers = std::collections::HashSet::new();
        for issuer_cfg in &self.issuers {
            if !seen_issuers.insert(issuer_cfg.issuer.clone()) {
                return Err(ConfigError::DuplicateIssuer(issuer_cfg.issuer.clone()));
            }
        }

        for issuer_cfg in &self.issuers {
            // ---- Forbidden algorithms ----
            for alg in &issuer_cfg.allowed_algorithms {
                if alg.eq_ignore_ascii_case("none")
                    || matches!(alg.as_str(), "HS256" | "HS384" | "HS512")
                {
                    return Err(ConfigError::ForbiddenAlgorithm {
                        issuer: issuer_cfg.issuer.clone(),
                        algorithm: alg.clone(),
                    });
                }
            }

            // ---- Empty allowed_algorithms ----
            if issuer_cfg.allowed_algorithms.is_empty() {
                return Err(ConfigError::EmptyAllowedAlgorithms {
                    issuer: issuer_cfg.issuer.clone(),
                });
            }

            // ---- Empty audience ----
            if issuer_cfg.audience.is_empty() {
                return Err(ConfigError::EmptyAudience {
                    issuer: issuer_cfg.issuer.clone(),
                });
            }
            for aud in &issuer_cfg.audience {
                if aud.is_empty() {
                    return Err(ConfigError::EmptyAudience {
                        issuer: issuer_cfg.issuer.clone(),
                    });
                }
            }

            // ---- Empty issuer ----
            if issuer_cfg.issuer.is_empty() {
                return Err(ConfigError::EmptyIssuer);
            }

            // ---- TTL bounds ----
            if issuer_cfg.jwks_cache_ttl_min > issuer_cfg.jwks_cache_ttl_max {
                return Err(ConfigError::InvalidCacheTtl {
                    issuer: issuer_cfg.issuer.clone(),
                    min: issuer_cfg.jwks_cache_ttl_min,
                    max: issuer_cfg.jwks_cache_ttl_max,
                });
            }
        }

        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no issuers configured -- at least one is required")]
    EmptyIssuerList,
    #[error("issuer '{0}' is configured more than once")]
    DuplicateIssuer(String),
    #[error("forbidden algorithm in allowed list for issuer '{issuer}': {algorithm}")]
    ForbiddenAlgorithm { issuer: String, algorithm: String },
    #[error("audience must be non-empty for issuer '{issuer}'")]
    EmptyAudience { issuer: String },
    #[error("issuer must be non-empty")]
    EmptyIssuer,
    #[error("jwks cache TTL min ({min:?}) exceeds max ({max:?}) for issuer '{issuer}'")]
    InvalidCacheTtl {
        issuer: String,
        min: Duration,
        max: Duration,
    },
    #[error("allowed_algorithms must contain at least one algorithm for issuer '{issuer}'")]
    EmptyAllowedAlgorithms { issuer: String },
}
