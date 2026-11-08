/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

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
//! - A `SuffixedClaim` identity normalization strategy with no
//!   `identity_scope_claim` configured is rejected because the
//!   strategy has no way to produce a scoped identity without one --
//!   better to fail loudly at startup than to have every request
//!   under that issuer silently fail identity normalization later.
//!
//! Things we *don't* validate here include URL well-formedness
//! (network errors at first JWKS fetch will surface this), key file
//! existence (loaded by the secrets backend, not from disk in
//! production), and bind address parseability (axum will fail at
//! bind time with a clearer error than we'd produce).

use cert_issuer_common::identity::IdentityNormalization;
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
    /// For Keycloak, or similar: whatever bare-identity claim this
    /// issuer's IdentityNormalization strategy expects (e.g.
    /// "preferred_username").
    pub workload_identity_claim: String,

    /// JSON Pointer-style path for the per-instance binding identifier.
    /// For Kubernetes: "kubernetes.io/pod/uid" (the literal key
    /// `kubernetes.io` is preserved as a single segment because it
    /// contains a dot — see the doc comment on `extract_string_claim`
    /// in oidc.rs).
    pub instance_binding_claim: String,

    /// The same, for the instance's name, read for people beside the binding: for Kubernetes "kubernetes.io/pod/name"
    /// (a bound ServiceAccount token's `kubernetes.io.pod.name`). A multi-instance workload's certificate names its pod
    /// by it (docs/architecture/authority-model.md, "Multi-instance workloads"). None: no name is read.
    #[serde(default)]
    pub instance_name_claim: Option<String>,

    /// How this issuer's identity claim is turned into a DNS-safe SAN.
    /// See `cert_issuer_common::identity::IdentityNormalization` for
    /// the full design rationale.
    pub identity_normalization: IdentityNormalization,

    /// JSON path within the JWT claims for the identity-scoping value
    /// used by `IdentityNormalization::SuffixedClaim`. Required (and
    /// validated as such below) when that strategy is selected;
    /// meaningless and ignored for `KubernetesServiceAccount`, which
    /// derives its own scoping from config (`cluster_domain`) rather
    /// than a second claim.
    pub identity_scope_claim: Option<String>,

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

    /// Lifetime for account (user) certificates: the ones issued through
    /// `/internal/issue` with an account subject, including the one a browser
    /// holds. Deliberately separate from `default_lifetime`, which is the
    /// short, rotated lifetime of workload client certs. Required, with no
    /// built-in default: a missing value is a configuration error.
    pub user_lifetime: Duration,

    /// Lifetime for an agent's escrow certificate (`CertType::Service`), its durable encryption identity: senders
    /// encrypt to it long after the pod that asked for it has gone, so it lives long, like a server certificate's
    /// infrastructure role and longer. To be revisited with revocation. Required, with no built-in default.
    pub service_lifetime: Duration,

    /// Per-identity lifetime overrides, keyed by the exact workload
    /// identity string embedded in the CSR's SAN. Takes precedence over
    /// both default_lifetime and server_lifetime when the requesting
    /// identity matches a key here. Empty/absent by default.
    #[serde(default)]
    pub identity_lifetime_overrides: std::collections::HashMap<String, Duration>,

    /// Where this instance's CA comes from. Deliberately an explicit
    /// setting and never inferred from which files happen to exist: a
    /// lost or mis-mounted CA must be a startup error, not a quiet
    /// switch to a freshly generated root.
    #[serde(default)]
    pub provisioning: CaProvisioning,
}

/// How cert-issuer obtains its CA.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaProvisioning {
    /// The CA was generated ahead of time by an operator with cluster
    /// admin rights (the admin tool's `bootstrap-ca`) and mounted in.
    /// cert-issuer only loads it and never generates one. If it is
    /// missing, startup fails.
    Provisioned,
    /// cert-issuer generates its own CA on first start and persists it
    /// to its own volume. The degraded mode for clusters where no
    /// operator ran the provisioning step: callers can't verify the
    /// CA on their very first contact and fetch it once per boot.
    #[default]
    SelfBootstrap,
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
    pub mtls: MtlsConfig,
}

/// Config for the mTLS upgrade to the existing listener -- the same
/// bind_addr above now optionally accepts a client certificate.
/// /issue and /validate remain reachable with no client cert
/// presented at all, unchanged; /internal/issue requires one, and
/// only from an identity in `internal_callers`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MtlsConfig {
    /// DNS identity embedded in cert-issuer's own TLS server
    /// certificate -- minted at startup from its own in-process CA
    /// (the same one issuers.ca.issue already uses for every other
    /// certificate), never loaded from a file. Configurable per
    /// deployment domain, not hardcoded.
    pub server_identity: String,
    /// Identities allowed to reach /internal/issue, checked against
    /// the connection's own verified mTLS peer SAN --
    /// credentials-agent's identity, once it has one, and nothing
    /// else. Empty by default, meaning the endpoint is unreachable
    /// until explicitly configured -- a safe default, not an
    /// oversight.
    #[serde(default)]
    pub internal_callers: Vec<String>,
    /// The bootstrap identities (`floor.rs`): the only callers that may be issued with nothing but a token.
    /// Absent means the floor is not configured and `/issue` stays open to any token holder, which the
    /// service says plainly at start. Present, even if empty, closes it.
    #[serde(default)]
    pub bootstrap: Option<Vec<crate::floor::BootstrapIdentity>>,
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
        // ---- Empty mTLS server identity ----
        //
        // The listener can't mint its own TLS server cert without
        // this -- unlike internal_callers (fine to leave empty, that
        // just means the internal endpoint stays unreachable), a
        // missing server_identity would break every caller, including
        // the existing /issue and /validate paths.
        if self.mtls.server_identity.is_empty() {
            return Err(ConfigError::EmptyMtlsServerIdentity);
        }

        // ---- The floor ----
        //
        // A bootstrap entry that lists a permission, a capability, a wildcard or nothing is a mistake that
        // would either not do what it says or sign more than meant.
        if let Some(floor) = &self.mtls.bootstrap {
            let problems = crate::floor::problems(floor);
            if !problems.is_empty() {
                return Err(ConfigError::BadBootstrap(problems.join("; ")));
            }
        }

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

            // ---- SuffixedClaim requires a scope claim ----
            if matches!(
                issuer_cfg.identity_normalization,
                IdentityNormalization::SuffixedClaim
            ) && issuer_cfg.identity_scope_claim.is_none()
            {
                return Err(ConfigError::MissingScopeClaim {
                    issuer: issuer_cfg.issuer.clone(),
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
    #[error(
        "issuer '{issuer}' uses the SuffixedClaim identity normalization strategy but has no identity_scope_claim configured"
    )]
    MissingScopeClaim { issuer: String },
    #[error(
        "mtls.server_identity must be non-empty -- the listener cannot mint its own TLS server certificate without it"
    )]
    EmptyMtlsServerIdentity,
    #[error("mtls.bootstrap is not sound: {0}")]
    BadBootstrap(String),
}
