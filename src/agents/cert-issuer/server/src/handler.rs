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
//! 5. **CA call.** Signs the CSR with the in-process rcgen CA.
//!    `CaError::Malformed` surfaces as 400; `CaError::Internal`
//!    surfaces as 500.
//!
//! # Why bearer token is a parameter, not extracted here
//!
//! `Handler::handle` takes the raw bearer token as a parameter so
//! the handler is testable without going through HTTP. The Axum
//! layer (in `crate::server`) extracts the `Authorization` header
//! and passes the token in. Tests bypass the Axum layer entirely.

use crate::ca::{CaClient, CaError, CaIssueRequest};
use crate::csr;
use crate::oidc::{ValidationError, Validator};
use cert_issuer_common::{
    InternalIssueRequest, IssueError, IssueOutcome, IssueRequest, IssueResponse,
    identity::normalize_identity, ValidateRequest,
};
use cert_issuer_common::ValidateResponse;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, instrument, warn};

pub struct Handler {
    pub validator: Arc<Validator>,
    pub ca: Arc<dyn CaClient>,
    /// Lifetime for client certs.
    pub default_lifetime: Duration,
    /// Lifetime for server certs. Typically longer than client certs
    /// to reduce Cassini restart frequency.
    pub server_lifetime: Duration,
    pub identity_lifetime_overrides: std::collections::HashMap<String, Duration>,
    /// Peer identities (the SAN of the connection's verified mTLS
    /// client certificate) allowed to call `handle_internal_issue`.
    /// Empty means that endpoint refuses every caller, which is the
    /// safe default until credentials-agent has its own identity.
    pub internal_callers: Vec<String>,
    /// PEM of the CA certificate this service signs with, served
    /// unauthenticated at `GET /ca`. Public by nature: every issuance
    /// response already carries it, and it is presented in every TLS
    /// handshake. It exists so a caller on its very first contact can
    /// obtain the trust root it has no other way to get.
    pub ca_cert_pem: String,
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

#[derive(Debug)]
pub enum ValidateHandlerBody {
    Success(ValidateResponse),
    Error(IssueError),
}

#[derive(Debug)]
pub struct ValidateHandlerResponse {
    pub status: u16,
    pub body: ValidateHandlerBody,
}

impl Handler {
    /// Process an `/issue` request.
    #[instrument(skip(self))]
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
        // First, we normalize the identity string, using this
        // issuer's own strategy and (if configured) scope claim --
        // both carried through on ValidatedClaims from oidc.rs, which
        // already knows which issuer matched.
        let dns_identity = match normalize_identity(
            &claims.workload_identity,
            claims.identity_scope.as_deref(),
            &claims.identity_normalization,
        ) {
            Ok(id) => id,
            Err(e) => return error_response(IssueOutcome::InvalidToken, &e.to_string()),
        };
        if let Err(e) = csr::verify_identity(&parsed_csr, &dns_identity) {
            return error_response(IssueOutcome::IdentityMismatch, &e.to_string());
        }

        // ---- Steps 3.5-5: shared signing path ----
        //
        // Everything past identity verification is identical for
        // /issue and /internal/issue, so it lives in one place.
        self.sign_verified(
            &dns_identity,
            &parsed_csr,
            request.csr_pem,
            request.cert_type,
            request.extra_sans,
            request.subject_dn_defaults,
        )
        .await
    }

    /// The signing tail shared by `handle` and `handle_internal_issue`:
    /// extra-SAN validation, Subject DN merge, lifetime selection, and
    /// the CA call. Callers must already have authenticated the request
    /// and verified that the CSR's SAN matches `dns_identity` -- this
    /// function assumes both and checks neither.
    async fn sign_verified(
        &self,
        dns_identity: &str,
        parsed_csr: &csr::ParsedCsr,
        csr_pem: String,
        cert_type: cert_issuer_common::CertType,
        extra_sans: Vec<String>,
        subject_dn_defaults: Option<cert_issuer_common::SubjectDnDefaults>,
    ) -> HandlerResponse {
        // TODO: pass this in so we get a cert valid for multiple SANs
        let all_sans = match csr::validate_extra_sans(dns_identity, &extra_sans) {
            Ok(all_sans) => all_sans,
            Err(e) => {
                error!("{e}");
                return error_response(IssueOutcome::InvalidCsr, &e.to_string());
            }
        };

        // ---- Step 3.5: subject DN merge ----
        //
        // Per field: the CSR's own value wins if present (the user
        // spoke for themselves); otherwise fall back to the admin's
        // provisioned default, if any; otherwise the field is simply
        // absent from the issued cert. A default that fails
        // validation is an internal-caller bug (credentials-agent,
        // not the CSR/attacker), so it's reported as InternalError,
        // not InvalidCsr.
        let subject_dn = match merge_subject_dn(&parsed_csr.subject, subject_dn_defaults.as_ref()) {
            Ok(s) => s,
            Err(e) => {
                error!("subject_dn_defaults failed validation: {e}");
                return error_response(IssueOutcome::InternalError, &e.to_string());
            }
        };

        // ---- Step 4: session ID ----
        //
        // Fresh UUID per request. Used downstream for audit
        // correlation between issuance events and certificate use.
        let session_id = generate_session_id();

        // ---- Step 5: CA call ----
        //
        // We pass only the CSR PEM and the requested lifetime. The
        // CA does not need the expected identity separately — the SAN
        // is embedded in the CSR, and the handler has already verified
        // it matches the token. The CA's job is to sign; the
        // verification work is done.
        let lifetime = self
            .identity_lifetime_overrides
            .get(dns_identity)
            .copied()
            .unwrap_or(match cert_type {
                cert_issuer_common::CertType::Server => self.server_lifetime,
                cert_issuer_common::CertType::Client => self.default_lifetime,
                // Not short-lived like Client -- the actual security
                // mechanism protecting a service's own durable
                // identity is scheduled rotation plus real
                // revocation (once implemented), not a tight expiry
                // window, matching server_lifetime's own existing
                // "not meant to expire in an hour" reasoning exactly.
                cert_issuer_common::CertType::Service => self.server_lifetime,
            });

        let ca_request = CaIssueRequest {
            csr_pem,
            sans: all_sans,
            lifetime,
            cert_type,
            subject_dn,
        };

        let issued = match self.ca.issue(ca_request).await {
            Ok(c) => c,
            Err(e) => {
                // CaError::Malformed means the CSR passed our own
                // parser but rcgen's internal parser rejected it —
                // a structural issue in the CSR, surfaces as 400.
                // CaError::Internal means rcgen's signing step
                // failed, which is a bug or corrupted key — not
                // transient, surfaces as 500.
                let (outcome, detail) = match e {
                    CaError::Malformed(s) => {
                        (IssueOutcome::InvalidCsr, format!("CA rejected CSR: {s}"))
                    }
                    CaError::SigningFailed(s) => (
                        IssueOutcome::InternalError,
                        format!("CA signing error: {s}"),
                    ),
                };
                warn!(session_id = %session_id, detail = %detail, "CA failure");
                return error_response(outcome, &detail);
            }
        };

        // ---- Success ----
        let expires_at = issued.not_after;
        HandlerResponse {
            status: 200,
            body: HandlerBody::Success(IssueResponse {
                certificate_pem: issued.certificate_pem,
                ca_chain_pem: issued.ca_chain_pem,
                serial_number: issued.serial_number,
                session_id,
                expires_at,
            }),
        }
    }

    /// Process an `/internal/issue` request. There is no bearer token:
    /// the caller has already done its own authorization elsewhere
    /// (credentials-agent verifies a bootstrap token or an account
    /// claim) and asserts the identity to sign for directly. What this
    /// function verifies is who is asking -- the verified peer identity
    /// of the mTLS connection, passed in by the transport layer --
    /// against `internal_callers`, and then that the CSR's SAN matches
    /// the asserted identity, exactly as `/issue` does against a token.
    #[instrument(skip(self))]
    pub async fn handle_internal_issue(
        &self,
        peer_identity: Option<&str>,
        request: InternalIssueRequest,
    ) -> HandlerResponse {
        // ---- Step 1: caller authorization ----
        //
        // No client certificate, or one whose identity isn't on the
        // allowlist, is refused before the CSR is even parsed. The
        // detail deliberately doesn't say which of the two it was, or
        // what the allowlist contains.
        match peer_identity {
            Some(peer) if self.internal_callers.iter().any(|c| c == peer) => {}
            other => {
                warn!(peer_identity = ?other, "internal issuance refused: caller not on allowlist");
                return error_response(
                    IssueOutcome::CallerNotAuthorized,
                    "caller is not authorized for internal issuance",
                );
            }
        }

        // ---- Step 2: CSR parsing ----
        let parsed_csr = match csr::parse_csr(&request.csr_pem) {
            Ok(c) => c,
            Err(e) => return error_response(IssueOutcome::InvalidCsr, &e.to_string()),
        };

        // ---- Step 3: identity match ----
        //
        // Same byte-exact check /issue does, against the identity the
        // caller asserted instead of one derived from a token.
        if request.identity.is_empty() {
            return error_response(IssueOutcome::InvalidCsr, "identity must not be empty");
        }
        if let Err(e) = csr::verify_identity(&parsed_csr, &request.identity) {
            return error_response(IssueOutcome::IdentityMismatch, &e.to_string());
        }

        self.sign_verified(
            &request.identity,
            &parsed_csr,
            request.csr_pem,
            request.cert_type,
            request.extra_sans,
            request.subject_dn_defaults,
        )
        .await
    }

    /// Process a `/validate` request -- OIDC validation only, no CSR,
    /// no signing. Exists specifically for credentials-agent's
    /// two-call issuance flow: it needs the real, validated identity
    /// (for a grant lookup and the renewal-prevention check) *before*
    /// it has anything to embed into a CSR's SAN set, since permitted
    /// SANs come from the grant, not known upfront.
    #[instrument(skip(self))]
        pub async fn handle_validate(
        &self,
        bearer_token: &str,
        request: ValidateRequest,
    ) -> ValidateHandlerResponse {
        let claims = match self.validator.validate(bearer_token).await {
            Ok(claims) => claims,
            Err(e) => {
                return ValidateHandlerResponse {
                    status: status_for_outcome(map_validation_error(&e)),
                    body: ValidateHandlerBody::Error(IssueError {
                        outcome: map_validation_error(&e),
                        detail: e.to_string(),
                    }),
                };
            }
        };

        // Computed via the exact same normalize_identity call /issue
        // itself uses -- a client building a CSR from this value is
        // guaranteed to match what /issue will check, never a second,
        // independent computation that could drift. Only computed
        // when actually asked for.
        let normalized_identity = if request.include_normalized_identity {
            match normalize_identity(
                &claims.workload_identity,
                claims.identity_scope.as_deref(),
                &claims.identity_normalization,
            ) {
                Ok(id) => Some(id),
                Err(e) => {
                    return ValidateHandlerResponse {
                        status: status_for_outcome(IssueOutcome::InvalidToken),
                        body: ValidateHandlerBody::Error(IssueError {
                            outcome: IssueOutcome::InvalidToken,
                            detail: e.to_string(),
                        }),
                    };
                }
            }
        } else {
            None
        };

        ValidateHandlerResponse {
            status: 200,
            body: ValidateHandlerBody::Success(ValidateResponse {
                workload_identity: claims.workload_identity,
                instance_binding: claims.instance_binding,
                jti: claims.jti,
                normalized_identity,
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

/// Plain, ready-to-push Subject DN field values -- every field
/// already validated, either by the CSR's own extraction
/// (csr::extract_subject_fields) or here, against the same newtypes,
/// for an admin-supplied default.
#[derive(Debug, Clone, Default)]
pub struct MergedSubjectDn {
    pub given_name: Option<String>,
    pub surname: Option<String>,
    pub title: Option<String>,
    pub telephone_number: Option<String>,
}

/// Merges the CSR's own Subject DN fields with an admin's provisioned
/// defaults, per field: the CSR's value wins when present, the
/// default fills the gap otherwise. See MergedSubjectDn's own doc
/// comment for why this exists as a distinct step, and Step 3.5's
/// own comment above for the fail-closed behavior on a bad default.
fn merge_subject_dn(
    csr_fields: &crate::csr::SubjectFields,
    defaults: Option<&cert_issuer_common::SubjectDnDefaults>,
) -> Result<MergedSubjectDn, crate::csr::CsrError> {
    use crate::csr::{JobTitle, PersonalName, PhoneNumber};
    let d = defaults.cloned().unwrap_or_default();

    let given_name = match &csr_fields.given_name {
        Some(v) => Some(v.as_str().to_string()),
        None => d
            .given_name
            .as_deref()
            .map(PersonalName::parse)
            .transpose()?
            .map(|v| v.as_str().to_string()),
    };
    let surname = match &csr_fields.surname {
        Some(v) => Some(v.as_str().to_string()),
        None => d
            .surname
            .as_deref()
            .map(PersonalName::parse)
            .transpose()?
            .map(|v| v.as_str().to_string()),
    };
    let title = match &csr_fields.title {
        Some(v) => Some(v.as_str().to_string()),
        None => d
            .title
            .as_deref()
            .map(JobTitle::parse)
            .transpose()?
            .map(|v| v.as_str().to_string()),
    };
    let telephone_number = match &csr_fields.telephone_number {
        Some(v) => Some(v.as_str().to_string()),
        None => d
            .telephone_number
            .as_deref()
            .map(PhoneNumber::parse)
            .transpose()?
            .map(|v| v.as_str().to_string()),
    };

    Ok(MergedSubjectDn {
        given_name,
        surname,
        title,
        telephone_number,
    })
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
        // /internal/issue only -- same 403 as IdentityMismatch, since
        // both mean "the caller isn't who they'd need to be for this
        // request to proceed," just checked via different mechanisms
        // (mTLS peer identity here, token-derived SAN there).
        IssueOutcome::CallerNotAuthorized => 403,
    }
}
