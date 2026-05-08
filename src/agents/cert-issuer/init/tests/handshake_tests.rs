// tests/handshake_tests.rs
//! Integration tests for the HTTP handshake client.
//!
//! Uses wiremock to stand up a fake cert issuer. Tests verify that
//! the client sends the right request shape and maps every response
//! category to the correct HandshakeError variant. The exit code
//! the init container emits is derived from the error variant, so
//! the mapping must be exact — operators distinguish deployment
//! misconfigurations from infrastructure failures from CA failures
//! by looking at the pod's init container exit code.

use cert_issuer_common::{CertType, IssueError, IssueOutcome, IssueResponse};
use cert_issuer_init::handshake::{HandshakeClient, HandshakeError};
use time::OffsetDateTime;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fake_success_response() -> IssueResponse {
    IssueResponse {
        certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
            .to_string(),
        ca_chain_pem: "-----BEGIN CERTIFICATE-----\nfake-ca\n-----END CERTIFICATE-----\n"
            .to_string(),
        session_id: "00000000-0000-0000-0000-000000000001".to_string(),
        expires_at: OffsetDateTime::now_utc() + time::Duration::hours(1),
    }
}

fn make_client(base_url: String) -> HandshakeClient {
    HandshakeClient::new(base_url)
}

// ---------------------------------------------------------------------------
// Request shape
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sends_bearer_token_in_authorization_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .and(header("authorization", "Bearer the-specific-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_success_response()))
        .mount(&server)
        .await;

    make_client(server.uri())
        .issue("the-specific-token", "fake-csr-pem", CertType::Client)
        .await
        .expect("must succeed when authorization header matches exactly");
}

#[tokio::test]
async fn sends_csr_pem_in_request_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .and(wiremock::matchers::body_partial_json(
            serde_json::json!({ "csr_pem": "the-csr-content" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_success_response()))
        .mount(&server)
        .await;

    make_client(server.uri())
        .issue("any-token", "the-csr-content", CertType::Client)
        .await
        .expect("must send csr_pem in request body");
}

#[tokio::test]
async fn sends_cert_type_client_in_request_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .and(wiremock::matchers::body_partial_json(
            serde_json::json!({ "cert_type": "CLIENT" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_success_response()))
        .mount(&server)
        .await;

    make_client(server.uri())
        .issue("any-token", "fake-csr-pem", CertType::Client)
        .await
        .expect("must send cert_type CLIENT in request body");
}

#[tokio::test]
async fn sends_cert_type_server_in_request_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .and(wiremock::matchers::body_partial_json(
            serde_json::json!({ "cert_type": "SERVER" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_success_response()))
        .mount(&server)
        .await;

    make_client(server.uri())
        .issue("any-token", "fake-csr-pem", CertType::Server)
        .await
        .expect("must send cert_type SERVER in request body");
}

#[tokio::test]
async fn sends_content_type_application_json() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_success_response()))
        .mount(&server)
        .await;

    make_client(server.uri())
        .issue("any-token", "fake-csr-pem", CertType::Client)
        .await
        .expect("must set Content-Type: application/json");
}

// ---------------------------------------------------------------------------
// Success
// ---------------------------------------------------------------------------

#[tokio::test]
async fn successful_response_returns_issue_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_success_response()))
        .mount(&server)
        .await;

    let response = make_client(server.uri())
        .issue("test-token", "fake-csr-pem", CertType::Client)
        .await
        .expect("200 response must return Ok");

    assert_eq!(response.session_id, "00000000-0000-0000-0000-000000000001");
    assert!(!response.certificate_pem.is_empty());
    assert!(!response.ca_chain_pem.is_empty());
}

// ---------------------------------------------------------------------------
// Rejection variants
// ---------------------------------------------------------------------------

#[tokio::test]
async fn invalid_audience_rejection_maps_to_rejected_variant() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .respond_with(ResponseTemplate::new(401).set_body_json(IssueError {
            outcome: IssueOutcome::InvalidAudience,
            detail: "audience mismatch".to_string(),
        }))
        .mount(&server)
        .await;

    let err = make_client(server.uri())
        .issue("bad-token", "fake-csr-pem", CertType::Client)
        .await
        .expect_err("401 must be an error");

    assert!(
        matches!(err, HandshakeError::Rejected(ref e) if e.outcome == IssueOutcome::InvalidAudience),
        "expected Rejected(InvalidAudience), got {err:?}"
    );
}

#[tokio::test]
async fn invalid_token_rejection_maps_to_rejected_variant() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .respond_with(ResponseTemplate::new(401).set_body_json(IssueError {
            outcome: IssueOutcome::InvalidToken,
            detail: "token validation failed".to_string(),
        }))
        .mount(&server)
        .await;

    let err = make_client(server.uri())
        .issue("bad-token", "fake-csr-pem", CertType::Client)
        .await
        .expect_err("401 must be an error");

    assert!(
        matches!(err, HandshakeError::Rejected(ref e) if e.outcome == IssueOutcome::InvalidToken),
        "expected Rejected(InvalidToken), got {err:?}"
    );
}

#[tokio::test]
async fn identity_mismatch_maps_to_rejected_variant() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .respond_with(ResponseTemplate::new(403).set_body_json(IssueError {
            outcome: IssueOutcome::IdentityMismatch,
            detail: "CSR SAN does not match token identity".to_string(),
        }))
        .mount(&server)
        .await;

    let err = make_client(server.uri())
        .issue("test-token", "fake-csr-pem", CertType::Client)
        .await
        .expect_err("403 must be an error");

    assert!(
        matches!(err, HandshakeError::Rejected(ref e) if e.outcome == IssueOutcome::IdentityMismatch),
        "expected Rejected(IdentityMismatch), got {err:?}"
    );
}

#[tokio::test]
async fn ca_unavailable_maps_to_rejected_variant() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .respond_with(ResponseTemplate::new(503).set_body_json(IssueError {
            outcome: IssueOutcome::CaUnavailable,
            detail: "CA signing service unavailable".to_string(),
        }))
        .mount(&server)
        .await;

    let err = make_client(server.uri())
        .issue("test-token", "fake-csr-pem", CertType::Client)
        .await
        .expect_err("503 must be an error");

    assert!(
        matches!(err, HandshakeError::Rejected(ref e) if e.outcome == IssueOutcome::CaUnavailable),
        "expected Rejected(CaUnavailable), got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Infrastructure failures
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unreachable_server_maps_to_unreachable_variant() {
    let err = make_client("http://127.0.0.1:1".to_string())
        .issue("test-token", "fake-csr-pem", CertType::Client)
        .await
        .expect_err("unreachable server must be an error");

    assert!(
        matches!(err, HandshakeError::Unreachable(_)),
        "expected Unreachable, got {err:?}"
    );
}

#[tokio::test]
async fn malformed_success_body_maps_to_malformed_variant() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .respond_with(ResponseTemplate::new(200).set_body_string("this is not json"))
        .mount(&server)
        .await;

    let err = make_client(server.uri())
        .issue("test-token", "fake-csr-pem", CertType::Client)
        .await
        .expect_err("malformed body must be an error");

    assert!(
        matches!(err, HandshakeError::Malformed(_)),
        "expected Malformed, got {err:?}"
    );
}

#[tokio::test]
async fn malformed_error_body_maps_to_malformed_variant() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
        .mount(&server)
        .await;

    let err = make_client(server.uri())
        .issue("test-token", "fake-csr-pem", CertType::Client)
        .await
        .expect_err("non-JSON error body must be an error");

    assert!(
        matches!(err, HandshakeError::Malformed(_)),
        "expected Malformed for unparseable error body, got {err:?}"
    );
}
