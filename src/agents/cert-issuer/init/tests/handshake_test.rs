//! Integration tests for the init container's handshake client.
//!
//! These tests use wiremock to stand up a fake cert issuer and verify
//! that the init container handles each response category correctly.
//! The point is to pin down that the init container's exit code
//! reflects the actual failure mode, since that's what operators
//! see in `kubectl describe pod`.

use cert_issuer_common::{IssueError, IssueOutcome, IssueResponse};
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
    HandshakeClient {
        base_url,
        http: reqwest::Client::new(),
    }
}

#[tokio::test]
async fn successful_issuance_returns_response_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_success_response()))
        .mount(&server)
        .await;

    let client = make_client(server.uri());
    let response = client
        .issue("test-token", "fake-csr-pem")
        .await
        .expect("issuance should succeed");

    assert_eq!(response.session_id, "00000000-0000-0000-0000-000000000001");
    assert!(!response.certificate_pem.is_empty());
}

#[tokio::test]
async fn cert_issuer_unreachable_returns_unreachable_error() {
    // No mock server running at this URL.
    let client = make_client("http://127.0.0.1:1".to_string());
    let err = client
        .issue("test-token", "fake-csr-pem")
        .await
        .expect_err("should fail when cert issuer is unreachable");
    assert!(matches!(err, HandshakeError::Unreachable(_)));
}

#[tokio::test]
async fn cert_issuer_returns_invalid_audience_is_propagated() {
    let server = MockServer::start().await;
    let error_body = IssueError {
        outcome: IssueOutcome::InvalidAudience,
        detail: "audience 'wrong-aud' does not match configured 'polar-cert-issuer'"
            .to_string(),
    };
    Mock::given(method("POST"))
        .and(path("/issue"))
        .respond_with(ResponseTemplate::new(401).set_body_json(error_body.clone()))
        .mount(&server)
        .await;

    let client = make_client(server.uri());
    let err = client
        .issue("bad-token", "fake-csr-pem")
        .await
        .expect_err("should propagate cert issuer rejection");

    match err {
        HandshakeError::Rejected(e) => {
            assert_eq!(e.outcome, IssueOutcome::InvalidAudience);
        }
        _ => panic!("expected Rejected variant, got {err:?}"),
    }
}

#[tokio::test]
async fn cert_issuer_returns_ca_unavailable_is_propagated() {
    let server = MockServer::start().await;
    let error_body = IssueError {
        outcome: IssueOutcome::CaUnavailable,
        detail: "CA timed out".to_string(),
    };
    Mock::given(method("POST"))
        .and(path("/issue"))
        .respond_with(ResponseTemplate::new(503).set_body_json(error_body))
        .mount(&server)
        .await;

    let client = make_client(server.uri());
    let err = client
        .issue("test-token", "fake-csr-pem")
        .await
        .expect_err("should propagate CA unavailability");

    match err {
        HandshakeError::Rejected(e) => assert_eq!(e.outcome, IssueOutcome::CaUnavailable),
        _ => panic!("expected Rejected variant"),
    }
}

#[tokio::test]
async fn malformed_response_body_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;

    let client = make_client(server.uri());
    let err = client
        .issue("test-token", "fake-csr-pem")
        .await
        .expect_err("malformed response should be reported");
    assert!(matches!(err, HandshakeError::Malformed(_)));
}

#[tokio::test]
async fn bearer_token_is_sent_in_authorization_header() {
    // Pin down that the token actually goes in the Authorization
    // header and not, say, a query parameter or request body.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .and(header("authorization", "Bearer the-specific-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_success_response()))
        .mount(&server)
        .await;

    let client = make_client(server.uri());
    client
        .issue("the-specific-token", "fake-csr-pem")
        .await
        .expect("auth header must match exactly");
}
