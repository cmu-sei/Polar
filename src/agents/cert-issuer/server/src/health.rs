//! Shared health state for liveness and readiness endpoints.
//!
//! # Design
//!
//! Liveness and readiness answer different questions and must never
//! be conflated:
//!
//!   /liveness  — "is this process alive and able to serve HTTP?"
//!                No dependency checks. If this fails, Kubernetes
//!                restarts the pod.
//!
//!   /readiness — "would sending an issuance request to this instance
//!                reasonably be expected to succeed?"
//!                Kubernetes stops routing traffic here if this fails,
//!                but does NOT restart the pod — a stale JWKS cache
//!                is not fixed by a restart, only by the dependency
//!                recovering.
//!
//! `/issue` remains the ultimate authority on whether any single
//! request succeeds. These endpoints are a coarser, cheaper signal
//! for routing decisions, not a substitute for handling issuance
//! errors correctly.
//!
//! # Readiness model
//!
//! Readiness is derived state, not a live dependency check:
//!
//!   READY = ca_loaded AND jwks_usable
//!
//! `ca_loaded` is set exactly once at startup and never changes for
//! the life of the process. The CA keypair is held in memory (see
//! `ca::load_or_bootstrap_ca`) — there is nothing to re-check at
//! runtime. If CA loading fails, the process must not reach a
//! running state at all; that is a startup-failure invariant
//! enforced in `main.rs`, not a runtime health signal. Readiness
//! therefore only ever has one dynamic condition to evaluate.
//!
//! `jwks_usable` is computed from the timestamp of the *last
//! successful* JWKS fetch, not the last attempted fetch:
//!
//!   jwks_usable = (now - jwks_last_success) < jwks_cache_ttl_max
//!
//! This distinction matters. An unknown `kid` in an incoming token
//! triggers an opportunistic cache refetch (see `oidc.rs`). If that
//! refetch fails but the cache is still within its TTL window, the
//! issuer can still validate tokens signed with the currently cached
//! keys — marking it not-ready in that moment would be reporting a
//! capability the instance doesn't actually lack.
//!
//! # Explicitly not implemented here
//!
//! The readiness handler built on top of this state must be purely
//! observational — it reads `jwks_last_success` and compares it to
//! `now`, and never itself triggers a JWKS fetch or any other network
//! call. A readiness probe that causes outbound calls under load
//! (e.g. a probe storm during a rolling deploy) couples the health
//! endpoint's latency and failure behavior to the reachability of an
//! external dependency — precisely what this design avoids. If the
//! cache is stale, this state reports not-ready; the existing
//! request-triggered refresh logic in the OIDC validator is what
//! recovers it, not the probe.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Shared, cheaply-cloneable health state.
///
/// Both the OIDC validator (writer, on successful JWKS fetch) and
/// the health HTTP handlers (reader) hold an `Arc<HealthState>`.
/// All fields are atomics so reads never block writes and vice
/// versa — there is no lock here because there is nothing that
/// needs multi-field consistency. `ca_loaded` and
/// `jwks_last_success_unix_secs` are independent facts.
#[derive(Debug)]
pub struct HealthState {
    /// Set exactly once, at startup, after `load_or_bootstrap_ca`
    /// succeeds. Never transitions back to `false`. Exists mainly
    /// so the readiness computation has a single well-documented
    /// place to note "the CA is a startup invariant, not a runtime
    /// check" rather than silently assuming it.
    ca_loaded: AtomicBool,

    /// Unix timestamp (seconds) of the last *successful* JWKS
    /// fetch. `0` means "never successfully fetched" — readiness
    /// treats that identically to "expired," which is correct: a
    /// process that has never fetched JWKS cannot validate tokens.
    jwks_last_success_unix_secs: AtomicU64,

    /// The cache TTL max from `IssuerConfig::jwks_cache_ttl_max`.
    /// Copied in at construction so the readiness check doesn't
    /// need to reach into the validator's config on every request.
    jwks_cache_ttl_max: Duration,
}

impl HealthState {
    /// Construct fresh health state. Called once at startup before
    /// the CA or JWKS have necessarily been initialized — both
    /// fields start in their "not yet ready" state.
    pub fn new(jwks_cache_ttl_max: Duration) -> Arc<Self> {
        Arc::new(Self {
            ca_loaded: AtomicBool::new(false),
            jwks_last_success_unix_secs: AtomicU64::new(0),
            jwks_cache_ttl_max,
        })
    }

    /// Mark the CA as loaded. Called once, after
    /// `load_or_bootstrap_ca` returns successfully in `main.rs`.
    ///
    /// There is deliberately no corresponding `mark_ca_failed` —
    /// CA load failure is a startup error that should prevent the
    /// process from ever serving traffic, not a runtime state this
    /// struct needs to represent. See module docs.
    pub fn mark_ca_loaded(&self) {
        self.ca_loaded.store(true, Ordering::Relaxed);
    }

    /// Record a successful JWKS fetch. Called from the JWKS fetch
    /// path in `oidc.rs` every time a fetch succeeds — both the
    /// scheduled/TTL-driven refresh and any opportunistic refetch
    /// triggered by an unrecognized `kid`.
    ///
    /// Intentionally does nothing on failure. A failed fetch simply
    /// leaves the previous success timestamp in place, which is
    /// exactly the "still usable until the cache genuinely expires"
    /// behavior this design wants.
    pub fn mark_jwks_fetch_succeeded(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.jwks_last_success_unix_secs
            .store(now, Ordering::Relaxed);
    }

    /// Whether the JWKS cache is still within its usable window.
    ///
    /// `false` if JWKS has never been successfully fetched, or if
    /// the last successful fetch is older than `jwks_cache_ttl_max`.
    fn jwks_usable(&self) -> bool {
        let last = self.jwks_last_success_unix_secs.load(Ordering::Relaxed);
        if last == 0 {
            return false;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let age = Duration::from_secs(now.saturating_sub(last));
        age < self.jwks_cache_ttl_max
    }

    /// Full readiness computation: `ca_loaded AND jwks_usable`.
    ///
    /// Returns a `ReadinessReport` rather than a bare bool so the
    /// HTTP handler can tell an operator *which* condition failed —
    /// "CA never loaded" and "JWKS cache stale" call for different
    /// responses and shouldn't be conflated in an opaque 503.
    pub fn readiness(&self) -> ReadinessReport {
        ReadinessReport {
            ca_loaded: self.ca_loaded.load(Ordering::Relaxed),
            jwks_usable: self.jwks_usable(),
        }
    }
}

/// The result of a readiness check, broken out by condition so the
/// HTTP layer can report which dependency is the problem.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ReadinessReport {
    pub ca_loaded: bool,
    pub jwks_usable: bool,
}

impl ReadinessReport {
    /// True only if every condition holds. Mirrors the `READY =
    /// ca_loaded AND jwks_usable` formula from the module docs.
    pub fn is_ready(&self) -> bool {
        self.ca_loaded && self.jwks_usable
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_state_is_not_ready() {
        let state = HealthState::new(Duration::from_secs(3600));
        let report = state.readiness();
        assert!(
            !report.ca_loaded,
            "CA must not be loaded before mark_ca_loaded is called"
        );
        assert!(
            !report.jwks_usable,
            "JWKS must not be usable before any fetch succeeds"
        );
        assert!(!report.is_ready());
    }

    #[test]
    fn ca_loaded_alone_is_not_ready() {
        let state = HealthState::new(Duration::from_secs(3600));
        state.mark_ca_loaded();
        let report = state.readiness();
        assert!(report.ca_loaded);
        assert!(!report.jwks_usable);
        assert!(
            !report.is_ready(),
            "CA loaded but no JWKS fetch yet must not be ready"
        );
    }

    #[test]
    fn jwks_success_alone_is_not_ready() {
        let state = HealthState::new(Duration::from_secs(3600));
        state.mark_jwks_fetch_succeeded();
        let report = state.readiness();
        assert!(
            !report.ca_loaded,
            "CA is a startup invariant and must be explicitly marked"
        );
        assert!(report.jwks_usable);
        assert!(!report.is_ready());
    }

    #[test]
    fn both_conditions_met_is_ready() {
        let state = HealthState::new(Duration::from_secs(3600));
        state.mark_ca_loaded();
        state.mark_jwks_fetch_succeeded();
        assert!(state.readiness().is_ready());
    }

    #[test]
    fn stale_jwks_cache_is_not_ready() {
        // Use a zero-duration TTL so the fetch is immediately "expired"
        // without needing to sleep in the test.
        let state = HealthState::new(Duration::from_secs(0));
        state.mark_ca_loaded();
        state.mark_jwks_fetch_succeeded();

        // A zero TTL means even a fetch that "just happened" is stale,
        // since `age < ttl_max` is `age < 0`, which is never true for
        // a non-negative age.
        assert!(!state.readiness().jwks_usable);
        assert!(!state.readiness().is_ready());
    }

    #[test]
    fn readiness_does_not_mutate_state() {
        // Calling readiness() repeatedly must be side-effect-free —
        // this is the "purely observational" invariant from the
        // module docs. Verified indirectly: two consecutive reads
        // produce identical results with no fetch in between.
        let state = HealthState::new(Duration::from_secs(3600));
        state.mark_ca_loaded();
        state.mark_jwks_fetch_succeeded();

        let first = state.readiness();
        let second = state.readiness();
        assert_eq!(first.ca_loaded, second.ca_loaded);
        assert_eq!(first.jwks_usable, second.jwks_usable);
    }

    #[test]
    fn failed_refetch_does_not_regress_last_success() {
        // Simulates: successful fetch, then an unknown-kid triggers
        // an opportunistic refetch that fails. mark_jwks_fetch_succeeded
        // is simply never called again — the timestamp from the first
        // success must remain in place and the cache must still read
        // as usable.
        let state = HealthState::new(Duration::from_secs(3600));
        state.mark_ca_loaded();
        state.mark_jwks_fetch_succeeded();

        // No second call to mark_jwks_fetch_succeeded — this models
        // the failed opportunistic refetch. Readiness must be
        // unaffected because the prior success is still within TTL.
        assert!(state.readiness().is_ready());
    }
}
