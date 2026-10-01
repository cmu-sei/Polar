//! Shared types between the cert issuer and its init container client.
//!
//! Kept separate so the wire protocol is defined in exactly one place
//! and both ends are forced to agree on it.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
pub mod ca_root;
pub mod identity;
pub mod permission;
pub mod public_key;
pub mod keypair;
pub mod serial;
pub mod capability;
pub mod csr_san;
pub mod key_match;

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

/// Admin-provisioned fallback values for Subject DN fields the
/// user's own CSR didn't set -- see the Polar user-account CSR flow
/// (credentials-agent-architecture.md's "User self-service issuance"
/// section and its account-provisioning extension). Plain,
/// unvalidated strings on the wire, matching IssueRequest's own
/// convention for extra_sans (also a plain `Vec<String>`); real
/// validation happens exactly once, in cert-issuer/server's csr
/// module, via the same PersonalName/JobTitle/PhoneNumber
/// constructors the CSR's own fields go through -- not duplicated
/// here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubjectDnDefaults {
    pub given_name: Option<String>,
    pub surname: Option<String>,
    pub title: Option<String>,
    pub telephone_number: Option<String>,
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
    /// Admin-provisioned defaults for Subject DN fields (given name,
    /// surname, title, telephone number), used only for whichever
    /// fields the CSR itself doesn't set -- the CSR's own value
    /// always wins when present. `None` for every issuance path that
    /// has no such defaults (ordinary workload issuance, admin
    /// bootstrap).
    #[serde(default)]
    pub subject_dn_defaults: Option<SubjectDnDefaults>,
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
    /// The standard JWT `jti` claim, if present. See
    /// oidc::ValidatedClaims::jti's own doc comment on cert-issuer's
    /// side for what this is -- generic claim extraction, no policy
    /// attached here either; credentials-agent decides what (if
    /// anything) to do with it.
    pub jti: Option<String>,
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

/// The account an internally-issued certificate belongs to, written into
/// its Subject DN as the standard `uid` and `mail` attributes. Unlike
/// `SubjectDnDefaults`, these are authoritative: they come only from the
/// caller (credentials-agent, which matched them against the ledger), never
/// from the CSR, and a CSR that claims its own `uid` or `mail` is ignored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountSubject {
    pub username: String,
    pub email: String,
}

/// Request body for the internal, mTLS-gated /internal/issue
/// endpoint. Reachable only over cert-issuer's mTLS listener, from a
/// caller whose own peer certificate matches the configured
/// internal-caller allowlist -- there is no bearer token here at all,
/// unlike IssueRequest. The caller (credentials-agent) has already
/// done its own authorization work before ever making this call
/// (a K8s-token check for admin bootstrap, a username/email match
/// for an account claim); cert-issuer's only remaining job is
/// verifying the caller's own mTLS identity and then signing exactly
/// what it's asked to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalIssueRequest {
    pub csr_pem: String,
    /// The identity to sign for -- asserted directly by the caller,
    /// not derived from any token. Every caller of this endpoint
    /// (account claims, admin bootstrap) has no token asserting an
    /// identity at all; the caller's own prior authorization work is
    /// what makes asserting this directly safe.
    pub identity: String,
    pub cert_type: CertType,
    #[serde(default)]
    pub extra_sans: Vec<String>,
    #[serde(default)]
    pub subject_dn_defaults: Option<SubjectDnDefaults>,
    /// Authoritative username and email for an account certificate.
    /// `None` for every other internal issuance.
    #[serde(default)]
    pub account_subject: Option<AccountSubject>,
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
    /// /internal/issue only: the connection's own verified mTLS peer
    /// identity is not on the configured internal-caller allowlist --
    /// or no client certificate was presented at all. Distinct from
    /// InvalidToken, since no token is involved in this path.
    CallerNotAuthorized,
}

#[derive(Debug, Error)]
pub enum WireError {
    #[error("malformed request: {0}")]
    Malformed(String),
}
