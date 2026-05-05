//! Configuration loaded at startup.
//!
//! `IssuerConfig` is written generically (issuer URL, audience, claim
//! paths) even though v1 only configures one Kubernetes issuer. v2's
//! multi-issuer support becomes a config change rather than a refactor.
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
    pub audience: String,

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
    /// Base URL of the Smallstep CA.
    pub url: String,
    /// JWK provisioner name registered with the CA.
    pub provisioner: String,
    /// Path to the JWK provisioner credential. The credential itself
    /// is loaded via the secrets backend, not from this path directly
    /// when running in production — but tests load from disk.
    pub provisioner_key_path: String,
    /// Default cert lifetime. Per-class overrides may layer on this.
    pub default_lifetime: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub bind_addr: String,
    pub issuer: IssuerConfig,
    pub ca: CaConfig,
}

impl ServiceConfig {
    /// Validate that no field has an obviously invalid value.
    /// Called once at startup; returns the first error found.
    ///
    /// The order of checks is deliberate: we check the most
    /// security-critical invariants first (forbidden algorithms,
    /// empty audience, empty issuer) so that if multiple things are
    /// wrong, the operator hears about the most serious one first.
    pub fn validate(&self) -> Result<(), ConfigError> {
        // ---- Forbidden algorithms ----
        //
        // HS* (symmetric) and "none" are categorically forbidden.
        // We use case-insensitive matching for "none" because some
        // tooling emits it as "None" or "NONE"; HS* is matched
        // case-sensitively because the JWT spec is case-sensitive
        // there and any deviation is itself a sign of a malformed
        // config.
        for alg in &self.issuer.allowed_algorithms {
            if alg.eq_ignore_ascii_case("none")
                || matches!(alg.as_str(), "HS256" | "HS384" | "HS512")
            {
                return Err(ConfigError::ForbiddenAlgorithm(alg.clone()));
            }
        }

        // ---- Empty allowed_algorithms ----
        //
        // After the forbidden-algorithm check (so the message is
        // about emptiness, not about a forbidden value being the
        // only thing in the list and then getting filtered out).
        if self.issuer.allowed_algorithms.is_empty() {
            return Err(ConfigError::EmptyAllowedAlgorithms);
        }

        // ---- Empty audience ----
        if self.issuer.audience.is_empty() {
            return Err(ConfigError::EmptyAudience);
        }

        // ---- Empty issuer ----
        if self.issuer.issuer.is_empty() {
            return Err(ConfigError::EmptyIssuer);
        }

        // ---- TTL bounds ----
        //
        // The cache uses `Duration::clamp(min, max)`, which panics
        // if min > max. We catch that here at config load rather
        // than letting it crash the service mid-request.
        if self.issuer.jwks_cache_ttl_min > self.issuer.jwks_cache_ttl_max {
            return Err(ConfigError::InvalidCacheTtl {
                min: self.issuer.jwks_cache_ttl_min,
                max: self.issuer.jwks_cache_ttl_max,
            });
        }

        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("forbidden algorithm in allowed list: {0}")]
    ForbiddenAlgorithm(String),
    #[error("audience must be non-empty")]
    EmptyAudience,
    #[error("issuer must be non-empty")]
    EmptyIssuer,
    #[error("jwks cache TTL min ({min:?}) exceeds max ({max:?})")]
    InvalidCacheTtl { min: Duration, max: Duration },
    #[error("allowed_algorithms must contain at least one algorithm")]
    EmptyAllowedAlgorithms,
}
