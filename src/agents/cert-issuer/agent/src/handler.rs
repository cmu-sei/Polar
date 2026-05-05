//! HTTP handler for `POST /issue`.
//!
//! Wires the OIDC validator, CSR parser, and CA client together.
//! The handler is the integration point where the protocol's
//! end-to-end behavior is specified.
//!
//! # Order of operations
//!
//! Each step short-circuits on failure. The CA is never called
//! unless OIDC validation, CSR parsing, and identity verification
//! all succeed. This is a security property, not just an
//! optimization: a failed token validation must not consume a CA
//! request slot, must not appear in CA audit logs, and must not
//! produce any side effect downstream of the cert issuer.
//!
//! 1. **OIDC validation.** Returns `InvalidToken`, `InvalidAudience`,
//!    `InvalidIssuer(_)`, `Expired`, `MissingClaim(_)`, or
//!    `ForbiddenAlgorithm(_)`. All of these short-circuit; no further
//!    work happens.
//! 2. **CSR parsing.** Returns `InvalidCsr` on any parse failure
//!    (malformed PEM, bad signature, multi-SAN, etc.).
//! 3. **Identity match.** The CSR's SAN must equal the token's
//!    workload identity. Mismatch returns `IdentityMismatch`.
//! 4. **Session ID generation.** Fresh UUID per request, used for
//!    audit correlation between this issuance event and downstream
//!    mTLS handshake logs.
//! 5. **CA call.** Forwards the CSR PEM (verbatim) and the SAN to
//!    step-ca for issuance. Failures here surface as
//!    `CaUnavailable`.
//!
//! # Why bearer token is a parameter, not extracted here
//!
//! `Handler::handle` takes the raw bearer token as a parameter so
//! the handler is testable without going through HTTP. The Axum
//! layer (in `crate::server`) extracts the `Authorization` header
//! and passes the token in. Tests bypass the Axum layer entirely.

use crate::ca::{CaClient, CaError, IssueRequest as CaIssueRequest};
use crate::csr;
use crate::oidc::{ValidationError, Validator};
use cert_issuer_common::{IssueError, IssueOutcome, IssueRequest, IssueResponse};
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

pub struct Handler {
    pub validator: Arc<Validator>,
    pub ca: Arc<dyn CaClient>,
    /// Default cert lifetime to request from the CA. The CA may
    /// clamp this further per its own provisioner policy. Comes
    /// from `CaConfig::default_lifetime`.
    pub default_lifetime: Duration,
}

#[derive(Debug)]
pub struct HandlerResponse {
    pub status: u16,
    pub body: HandlerBody,
}

#[derive(Debug)]
pub enum HandlerBody {
    Success(IssueResponse),
    Error(IssueError),
}

impl Handler {
    /// Process an `/issue` request.
    pub async fn handle(&self, bearer_token: &str, request: IssueRequest) -> HandlerResponse {
        // ---- Step 1: OIDC validation ----
        let claims = match self.validator.validate(bearer_token).await {
            Ok(claims) => claims,
            Err(e) => return error_response(map_validation_error(&e), &e.to_string()),
        };

        // ---- Step 2: CSR parsing ----
        let parsed_csr = match csr::parse_csr(&request.csr_pem) {
            Ok(c) => c,
            Err(e) => return error_response(IssueOutcome::InvalidCsr, &e.to_string()),
        };

        // ---- Step 3: identity match ----
        //
        // The CSR's SAN must equal the workload identity claim from
        // the OIDC token. This is what prevents one agent's token
        // from being used to obtain a cert for another agent's
        // identity. The CSR module's `verify_identity` does the
        // actual byte-exact comparison.
        if let Err(e) = csr::verify_identity(&parsed_csr, &claims.workload_identity) {
            return error_response(IssueOutcome::IdentityMismatch, &e.to_string());
        }

        // ---- Step 4: session ID ----
        //
        // Fresh UUID per request. Used downstream for audit
        // correlation between issuance events and certificate use.
        let session_id = generate_session_id();

        // ---- Step 5: CA call ----
        let ca_request = CaIssueRequest {
            csr_pem: request.csr_pem,
            san: claims.workload_identity.clone(),
            lifetime: self.default_lifetime,
        };

        let issued = match self.ca.issue(ca_request).await {
            Ok(c) => c,
            Err(e) => {
                // All CA failures map to CaUnavailable from the
                // client's perspective. The internal distinction
                // (Unreachable vs BadResponse vs Malformed) goes
                // into telemetry but isn't exposed in the wire
                // response — the init container's recovery is the
                // same in all three cases (exit and let Kubernetes
                // restart the pod).
                let detail = match e {
                    CaError::Unreachable(s) => format!("CA unreachable: {s}"),
                    CaError::BadResponse { status, .. } => {
                        format!("CA returned status {status}")
                    }
                    CaError::Malformed(s) => format!("CA response malformed: {s}"),
                };
                warn!(session_id = %session_id, detail = %detail, "CA failure");
                return error_response(IssueOutcome::CaUnavailable, &detail);
            }
        };

        // ---- Success ----
        let expires_at = issued.not_after;
        HandlerResponse {
            status: 200,
            body: HandlerBody::Success(IssueResponse {
                certificate_pem: issued.certificate_pem,
                ca_chain_pem: issued.ca_chain_pem,
                session_id,
                expires_at,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map a `ValidationError` to the corresponding wire-level outcome.
///
/// The mapping is the entire point of having a separate
/// `IssueOutcome` enum — operators and clients see a stable wire
/// contract even when the internal validation error variants change.
fn map_validation_error(e: &ValidationError) -> IssueOutcome {
    match e {
        ValidationError::InvalidAudience => IssueOutcome::InvalidAudience,
        ValidationError::InvalidIssuer(_) => IssueOutcome::InvalidToken,
        ValidationError::ForbiddenAlgorithm(_) => IssueOutcome::InvalidToken,
        ValidationError::Expired => IssueOutcome::InvalidToken,
        ValidationError::MissingClaim(_) => IssueOutcome::InvalidToken,
        ValidationError::JwksUnavailable(_) => IssueOutcome::InternalError,
        ValidationError::InvalidToken => IssueOutcome::InvalidToken,
    }
}

fn error_response(outcome: IssueOutcome, detail: &str) -> HandlerResponse {
    HandlerResponse {
        status: status_for_outcome(outcome),
        body: HandlerBody::Error(IssueError {
            outcome,
            detail: detail.to_string(),
        }),
    }
}

/// Generate a fresh session ID for audit correlation.
///
/// We use UUID v4 for human-recognizable formatting (it's what
/// operators expect to see in logs). The collision probability is
/// negligible at any cert issuance rate we'll hit.
fn generate_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Status code mapping for outcomes. Pinned in code so tests can
/// assert the exact mapping without reading the handler.
pub fn status_for_outcome(outcome: IssueOutcome) -> u16 {
    match outcome {
        IssueOutcome::Success => 200,
        IssueOutcome::InvalidToken => 401,
        IssueOutcome::InvalidAudience => 401,
        IssueOutcome::IdentityMismatch => 403,
        IssueOutcome::InvalidCsr => 400,
        IssueOutcome::CaUnavailable => 503,
        IssueOutcome::InternalError => 500,
    }
}
