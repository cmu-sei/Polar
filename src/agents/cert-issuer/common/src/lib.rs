//! Shared types between the cert issuer and its init container client.
//!
//! Kept separate so the wire protocol is defined in exactly one place
//! and both ends are forced to agree on it.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
pub mod identity;
pub mod permission;

// cert_issuer_common/src/lib.rs — add to IssueRequest
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum,)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CertType {
    #[default]
    Client,
    Server,
    /// A service's own durable, agent-to-agent encryption identity --
    /// not a client/server TLS role at all, and deliberately never
    /// treated as one. See credentials-agent-architecture.md's own
    /// "Agent escrow identity" section (which calls this a service's
    /// "escrow keypair" -- named Service here specifically to avoid
    /// conflating the certificate's own type with the separate
    /// mechanism, described in the same document, that later escrows
    /// it alongside its private key).
    Service,
}

impl std::fmt::Display for CertType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Client => "client",
            Self::Server => "server",
            Self::Service => "service",
        };
        write!(f, "{s}")
    }
}

/// Request body for `POST /issue`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueRequest {
    /// PEM-encoded CSR. The SAN must match the workload identity claim
    /// from the bearer token, or the cert issuer rejects with `IDENTITY_MISMATCH`.
    pub csr_pem: String,
    pub cert_type: CertType,
    #[serde(default)]
    pub extra_sans: Vec<String>,
}

/// Successful response body for `POST /issue`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueResponse {
    /// PEM-encoded certificate chain (leaf first, then intermediates).
    pub certificate_pem: String,
    /// PEM-encoded CA certificate(s) for the trust chain.
    pub ca_chain_pem: String,
    /// The issued certificate's own X.509 serial number -- the join
    /// key for revocation (credentials-agent's Revoke events, and any
    /// future CRL, are keyed on this, not session_id below, which is
    /// a separate, per-request audit-correlation value).
    pub serial_number: String,
    /// UUID for audit correlation. The init container surfaces this
    /// in its log output so operators can grep across logs and graph events.
    pub session_id: String,
    /// Certificate `notAfter` in RFC3339. The init container uses this
    /// to compute renewal-restart timing if it ever needs to.
    pub expires_at: OffsetDateTime,
}

/// Successful response body for `POST /validate`. Distinct from
/// `IssueResponse` -- exists specifically for credentials-agent's
/// two-call issuance flow (validate first to get the real identity
/// for a grant lookup, then issue separately once permitted SANs are
/// known).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidateResponse {
    /// The raw claim value -- not the normalized, DNS-safe identity
    /// /issue's CSR-matching logic uses. Grants are keyed on this
    /// exact raw value, per credentials-agent-architecture.md.
    pub workload_identity: String,
    pub instance_binding: Option<String>,
    /// The normalized, DNS-safe identity /issue's CSR-matching logic
    /// will actually check against -- computed via the same
    /// normalize_identity call /issue itself uses, so a client
    /// building a CSR from this value is guaranteed to match, never a
    /// second, independent computation that could drift. Only
    /// populated when the request explicitly asks for it via
    /// ValidateRequest::include_normalized_identity -- never returned
    /// unrequested, since most callers of this endpoint (the
    /// issuance flow's own internal /validate call) have no use for
    /// it at all.
    pub normalized_identity: Option<String>,
}

/// Request body for `POST /validate`. Previously took no body at all
/// (just the bearer token via the Authorization header) -- this is
/// the first real parameter it's ever had.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ValidateRequest {
    #[serde(default)]
    pub include_normalized_identity: bool,
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
