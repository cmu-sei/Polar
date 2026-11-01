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

use cert_client::handshake::{HandshakeClient, HandshakeError};
use cert_issuer_common::{
    CertType, IssueError, IssueOutcome, IssueResponse, ValidateRequest, ValidateResponse,
};
use time::OffsetDateTime;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fake_success_response() -> IssueResponse {
    IssueResponse {
        certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
            .to_string(),
        ca_chain_pem: "-----BEGIN CERTIFICATE-----\nfake-ca\n-----END CERTIFICATE-----\n"
            .to_string(),
        serial_number: "01:23:45:67:89:AB:CD:EF".to_string(),
        session_id: "00000000-0000-0000-0000-000000000001".to_string(),
        expires_at: OffsetDateTime::now_utc() + time::Duration::hours(1),
    }
}

fn make_client(base_url: String) -> HandshakeClient {
    HandshakeClient::new(base_url)
}

// Helper to call issue with no extra SANs — keeps test callsites clean
async fn issue(
    client: &HandshakeClient,
    token: &str,
    csr: &str,
    cert_type: CertType,
) -> Result<cert_issuer_common::IssueResponse, cert_client::handshake::HandshakeError> {
    client.issue(token, csr, cert_type, Vec::new(), None).await
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

    issue(
        &make_client(server.uri()),
        "the-specific-token",
        "fake-csr-pem",
        CertType::Client,
    )
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

    issue(
        &make_client(server.uri()),
        "the-specific-token",
        "the-csr-content",
        CertType::Client,
    )
    .await
    .expect("must send csr_pem in request body");

    let received = server.received_requests().await.unwrap();
    println!("{}", std::str::from_utf8(&received[0].body).unwrap());
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

    issue(
        &make_client(server.uri()),
        "the-specific-token",
        "fake-csr-pem",
        CertType::Client,
    )
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

    issue(
        &make_client(server.uri()),
        "the-specific-token",
        "fake-csr-pem",
        CertType::Server,
    )
    .await
    .expect("must send cert_type SERVER in request body");

    let received = server.received_requests().await.unwrap();
    println!("{}", std::str::from_utf8(&received[0].body).unwrap());
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

    issue(
        &make_client(server.uri()),
        "the-specific-token",
        "fake-csr-pem",
        CertType::Client,
    )
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

    let response = issue(
        &make_client(server.uri()),
        "the-specific-token",
        "fake-csr-pem",
        CertType::Client,
    )
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

    let err = issue(
        &make_client(server.uri()),
        "the-specific-token",
        "fake-csr-pem",
        CertType::Client,
    )
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

    let err = issue(
        &make_client(server.uri()),
        "the-specific-token",
        "fake-csr-pem",
        CertType::Client,
    )
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

    let err = issue(
        &make_client(server.uri()),
        "the-specific-token",
        "fake-csr-pem",
        CertType::Client,
    )
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

    let err = issue(
        &make_client(server.uri()),
        "the-specific-token",
        "fake-csr-pem",
        CertType::Client,
    )
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
    let err = issue(
        &make_client("127.0.0.1:1".into()),
        "the-specific-token",
        "fake-csr-pem",
        CertType::Client,
    )
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

    let err = issue(
        &make_client(server.uri()),
        "the-specific-token",
        "fake-csr-pem",
        CertType::Client,
    )
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

    let err = issue(
        &make_client(server.uri()),
        "the-specific-token",
        "fake-csr-pem",
        CertType::Client,
    )
    .await
    .expect_err("non-JSON error body must be an error");

    assert!(
        matches!(err, HandshakeError::Malformed(_)),
        "expected Malformed for unparseable error body, got {err:?}"
    );
}

#[tokio::test]
async fn sends_extra_sans_in_request_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .and(wiremock::matchers::body_partial_json(serde_json::json!({
            "extra_sans": ["neo4j.polar.svc.cluster.local"]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(fake_success_response()))
        .mount(&server)
        .await;

    make_client(server.uri())
        .issue(
            "any-token",
            "fake-csr-pem",
            CertType::Client,
            vec!["neo4j.polar.svc.cluster.local".to_string()],
            None,
        )
        .await
        .expect("must send extra_sans in request body");
}

// ---------------------------------------------------------------------------
// /validate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn validate_sends_bearer_token_in_authorization_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/validate"))
        .and(header("authorization", "Bearer the-specific-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ValidateResponse {
            workload_identity: "testuser".to_string(),
            instance_binding: None,
            jti: None,
            instance_name: None,
            normalized_identity: None,
        }))
        .mount(&server)
        .await;

    make_client(server.uri())
        .validate("the-specific-token", ValidateRequest::default())
        .await
        .expect("must succeed when authorization header matches exactly");
}

#[tokio::test]
async fn validate_successful_response_returns_workload_identity() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/validate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ValidateResponse {
            workload_identity: "testuser".to_string(),
            instance_binding: Some("pod-uid-1".to_string()),
            jti: None,
            instance_name: None,
            normalized_identity: None,
        }))
        .mount(&server)
        .await;

    let response = make_client(server.uri())
        .validate("any-token", ValidateRequest::default())
        .await
        .expect("200 response must return Ok");

    assert_eq!(response.workload_identity, "testuser");
    assert_eq!(response.instance_binding.as_deref(), Some("pod-uid-1"));
}

#[tokio::test]
async fn validate_invalid_token_rejection_maps_to_rejected_variant() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/validate"))
        .respond_with(ResponseTemplate::new(401).set_body_json(IssueError {
            outcome: IssueOutcome::InvalidToken,
            detail: "token validation failed".to_string(),
        }))
        .mount(&server)
        .await;

    let err = make_client(server.uri())
        .validate("bad-token", ValidateRequest::default())
        .await
        .expect_err("401 must be an error");

    assert!(
        matches!(err, HandshakeError::Rejected(ref e) if e.outcome == IssueOutcome::InvalidToken),
        "expected Rejected(InvalidToken), got {err:?}"
    );
}

#[tokio::test]
async fn validate_unreachable_server_maps_to_unreachable_variant() {
    let err = make_client("127.0.0.1:1".into())
        .validate("any-token", ValidateRequest::default())
        .await
        .expect_err("unreachable server must be an error");

    assert!(
        matches!(err, HandshakeError::Unreachable(_)),
        "expected Unreachable, got {err:?}"
    );
}

#[tokio::test]
async fn validate_malformed_success_body_maps_to_malformed_variant() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/validate"))
        .respond_with(ResponseTemplate::new(200).set_body_string("this is not json"))
        .mount(&server)
        .await;

    let err = make_client(server.uri())
        .validate("any-token", ValidateRequest::default())
        .await
        .expect_err("malformed body must be an error");

    assert!(
        matches!(err, HandshakeError::Malformed(_)),
        "expected Malformed, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Held requests (docs/architecture/core-restarts.md, "3 in detail")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_held_request_is_held_and_not_a_success_nor_a_rejection() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/issue"))
        .respond_with(ResponseTemplate::new(202).set_body_json(IssueError {
            outcome: IssueOutcome::Held,
            detail: "held until the credentials agent vouches for it".to_string(),
        }))
        .mount(&server)
        .await;
    let err = issue(&make_client(server.uri()), "t", "csr", CertType::Client)
        .await
        .unwrap_err();
    match err {
        HandshakeError::Held(detail) => assert!(detail.contains("vouches"), "{detail}"),
        other => panic!("expected Held, got {other:?}"),
    }
}

#[tokio::test]
async fn the_held_requests_are_listed_and_answered_on_their_routes() {
    use cert_issuer_common::held::{HeldDecision, HeldList, HeldOffer};
    use wiremock::matchers::body_json;
    let server = MockServer::start().await;
    let offer = HeldOffer {
        id: "h1".to_string(),
        workload: "system:serviceaccount:polar:cassini".to_string(),
        identity: "cassini.polar.serviceaccount.cluster.local".to_string(),
        instance_binding: Some("pod-1".to_string()),
        cert_type: CertType::Server,
        names: vec![],
        csr_pem: "csr".to_string(),
    };
    Mock::given(method("GET"))
        .and(path("/internal/held"))
        .respond_with(ResponseTemplate::new(200).set_body_json(HeldList {
            held: vec![offer.clone()],
        }))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/internal/held/h1"))
        .and(body_json(HeldDecision::Vouch))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"taken": true})))
        .expect(1)
        .mount(&server)
        .await;
    let client = make_client(server.uri());
    assert_eq!(client.held_list().await.unwrap().held, vec![offer]);
    assert!(
        client
            .held_answer("h1", &HeldDecision::Vouch)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_held_list_refused_to_a_caller_is_a_rejection() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/internal/held"))
        .respond_with(ResponseTemplate::new(403).set_body_json(IssueError {
            outcome: IssueOutcome::CallerNotAuthorized,
            detail: "only the credentials agent may list the held requests".to_string(),
        }))
        .mount(&server)
        .await;
    assert!(matches!(
        make_client(server.uri()).held_list().await,
        Err(HandshakeError::Rejected(e)) if e.outcome == IssueOutcome::CallerNotAuthorized
    ));
}
