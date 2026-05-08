//! Shared types between the cert issuer and its init container client.
//!
//! Kept separate so the wire protocol is defined in exactly one place
//! and both ends are forced to agree on it.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
pub mod identity;
/// Request body for `POST /issue`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueRequest {
    /// PEM-encoded CSR. The SAN must match the workload identity claim
    /// from the bearer token, or the cert issuer rejects with `IDENTITY_MISMATCH`.
    pub csr_pem: String,
}

/// Successful response body for `POST /issue`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueResponse {
    /// PEM-encoded certificate chain (leaf first, then intermediates).
    pub certificate_pem: String,
    /// PEM-encoded CA certificate(s) for the trust chain.
    pub ca_chain_pem: String,
    /// UUID for audit correlation. The init container surfaces this
    /// in its log output so operators can grep across logs and graph events.
    pub session_id: String,
    /// Certificate `notAfter` in RFC3339. The init container uses this
    /// to compute renewal-restart timing if it ever needs to.
    pub expires_at: OffsetDateTime,
}

/// Error response body. Always returned on non-2xx responses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueError {
    pub outcome: IssueOutcome,
    /// Human-readable detail. Safe to log; should not contain secrets.
    pub detail: String,
}

/// The full set of issuance outcomes. Telemetry events use the same
/// enum so that grepping for an outcome in logs and metrics gives
/// matching results.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IssueOutcome {
    Success,
    /// JWT signature verification failed, token expired, malformed, etc.
    InvalidToken,
    /// Token's `aud` claim does not match the configured audience.
    /// Surfaced as a distinct outcome because it's the most common
    /// deployment misconfiguration.
    InvalidAudience,
    /// The SAN in the submitted CSR does not match the workload identity
    /// claim extracted from the token.
    IdentityMismatch,
    /// CSR is malformed, uses a disallowed algorithm, or otherwise
    /// fails structural validation.
    InvalidCsr,
    /// Smallstep CA returned an error or was unreachable.
    CaUnavailable,
    /// Anything else. Indicates a bug or unexpected infrastructure failure.
    InternalError,
}

#[derive(Debug, Error)]
pub enum WireError {
    #[error("malformed request: {0}")]
    Malformed(String),
}
