//! Unit tests for outcome -> HTTP status code mapping.
//!
//! Pinned in tests so the wire contract is explicit. Operators
//! distinguishing between 401 (auth problem) and 403 (auth ok but
//! policy violation) and 503 (CA down) drives different alerting,
//! so the mapping needs to be stable.

use cert_issuer::handler::status_for_outcome;
use cert_issuer_common::IssueOutcome;

#[test]
fn success_is_200() {
    assert_eq!(status_for_outcome(IssueOutcome::Success), 200);
}

#[test]
fn invalid_token_is_401() {
    assert_eq!(status_for_outcome(IssueOutcome::InvalidToken), 401);
}

#[test]
fn invalid_audience_is_401_not_403() {
    // Even though audience is technically a policy mismatch, it's
    // an auth-time check on the token itself, so 401 is correct.
    // The wire-level distinction operators care about is not in the
    // status code but in the outcome enum in the response body.
    assert_eq!(status_for_outcome(IssueOutcome::InvalidAudience), 401);
}

#[test]
fn identity_mismatch_is_403() {
    // 403 because the token is valid but the request asks for
    // something the token doesn't authorize.
    assert_eq!(status_for_outcome(IssueOutcome::IdentityMismatch), 403);
}

#[test]
fn invalid_csr_is_400() {
    assert_eq!(status_for_outcome(IssueOutcome::InvalidCsr), 400);
}

#[test]
fn ca_unavailable_is_503() {
    // 503 not 500 — this signals "try again later" to the init
    // container, which can drive a different retry policy than 500.
    assert_eq!(status_for_outcome(IssueOutcome::CaUnavailable), 503);
}

#[test]
fn internal_error_is_500() {
    assert_eq!(status_for_outcome(IssueOutcome::InternalError), 500);
}

// Serialization tests

#[test]
fn outcome_serializes_to_screaming_snake_case() {
    // The wire format of the outcome enum is part of the contract;
    // changing it breaks every client. Pin it down.
    let json = serde_json::to_string(&IssueOutcome::InvalidAudience).unwrap();
    assert_eq!(json, "\"INVALID_AUDIENCE\"");
}

#[test]
fn outcome_deserializes_from_screaming_snake_case() {
    let outcome: IssueOutcome = serde_json::from_str("\"CA_UNAVAILABLE\"").unwrap();
    assert_eq!(outcome, IssueOutcome::CaUnavailable);
}

#[test]
fn unknown_outcome_string_fails_to_deserialize() {
    // If a client sends an unknown outcome, we don't silently map
    // it to anything — we fail. Forward-compatibility for new
    // outcomes is the cert issuer's responsibility, not the client's.
    let result: Result<IssueOutcome, _> = serde_json::from_str("\"NEWFANGLED_OUTCOME\"");
    assert!(result.is_err());
}
