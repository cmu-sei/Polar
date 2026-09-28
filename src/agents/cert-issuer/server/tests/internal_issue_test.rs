//! Tests for `Handler::handle_internal_issue` -- the token-free signing
//! path for callers that have already authorized a request elsewhere
//! (credentials-agent, for admin bootstrap and account claims).
//!
//! What's being proven: the caller allowlist is checked before anything
//! else happens, a refused caller never reaches the CA, and an allowed
//! caller gets the same identity-matching guarantee /issue gives against
//! a token -- the CSR's SAN must equal the identity the caller asserts.

use cert_issuer::ca::{CaClient, CaError, CaIssueRequest, IssuedCert, RcgenCaClient, load_or_bootstrap_ca};
use cert_issuer::handler::{Handler, HandlerBody};
use cert_issuer::oidc::Validator;
use cert_issuer_common::keypair::{KeyAlgorithm, generate_csr};
use cert_issuer_common::{CertType, InternalIssueRequest, IssueOutcome};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const ALLOWED_CALLER: &str = "credentials-agent.polar.serviceaccount.cluster.local";
const TARGET_IDENTITY: &str = "some-account-uuid.polar.serviceaccount.cluster.local";

/// Panics if ever called -- used by the refusal tests to prove a refused
/// caller never reaches signing at all.
struct PanicCa;

#[async_trait::async_trait]
impl CaClient for PanicCa {
    async fn issue(&self, _req: CaIssueRequest) -> Result<IssuedCert, CaError> {
        panic!("a refused internal-issuance request must never reach the CA");
    }
}

fn real_ca() -> Arc<dyn CaClient> {
    let dir = tempfile::tempdir().unwrap();
    let cert_path = dir.path().join("ca.crt");
    let key_path = dir.path().join("ca.key");
    let (cert_pem, key_pem) = load_or_bootstrap_ca(
        cert_path.to_str().unwrap(),
        key_path.to_str().unwrap(),
        "Test CA",
    )
    .expect("bootstrap test CA");
    // RcgenCaClient holds the PEM in memory, so the temp dir can drop.
    Arc::new(RcgenCaClient::new(&cert_pem, &key_pem).expect("construct CA client"))
}

fn handler(ca: Arc<dyn CaClient>, internal_callers: Vec<&str>) -> Handler {
    Handler {
        // Never consulted on this path -- there is no token to validate.
        validator: Arc::new(Validator::new(vec![])),
        ca,
        default_lifetime: Duration::from_secs(3600),
        server_lifetime: Duration::from_secs(43200),
        identity_lifetime_overrides: HashMap::new(),
        internal_callers: internal_callers.into_iter().map(String::from).collect(),
        ca_cert_pem: String::new(),
    }
}

fn request_for(csr_identity: &str, asserted_identity: &str) -> InternalIssueRequest {
    let csr = generate_csr(csr_identity, &KeyAlgorithm::EcdsaP256, &[]).expect("csr");
    InternalIssueRequest {
        csr_pem: csr.csr_pem,
        identity: asserted_identity.to_string(),
        cert_type: CertType::Client,
        extra_sans: vec![],
        subject_dn_defaults: None,
    }
}

#[tokio::test]
async fn no_client_certificate_is_refused_before_the_ca() {
    let h = handler(Arc::new(PanicCa), vec![ALLOWED_CALLER]);
    let response = h
        .handle_internal_issue(None, request_for(TARGET_IDENTITY, TARGET_IDENTITY))
        .await;

    assert_eq!(response.status, 403);
    match response.body {
        HandlerBody::Error(e) => assert_eq!(e.outcome, IssueOutcome::CallerNotAuthorized),
        other => panic!("expected an error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_peer_not_on_the_allowlist_is_refused_before_the_ca() {
    let h = handler(Arc::new(PanicCa), vec![ALLOWED_CALLER]);
    let response = h
        .handle_internal_issue(
            Some("cassini.polar.serviceaccount.cluster.local"),
            request_for(TARGET_IDENTITY, TARGET_IDENTITY),
        )
        .await;

    assert_eq!(response.status, 403);
    match response.body {
        HandlerBody::Error(e) => assert_eq!(e.outcome, IssueOutcome::CallerNotAuthorized),
        other => panic!("expected an error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_empty_allowlist_refuses_every_caller() {
    let h = handler(Arc::new(PanicCa), vec![]);
    let response = h
        .handle_internal_issue(
            Some(ALLOWED_CALLER),
            request_for(TARGET_IDENTITY, TARGET_IDENTITY),
        )
        .await;

    assert_eq!(response.status, 403);
    match response.body {
        HandlerBody::Error(e) => assert_eq!(e.outcome, IssueOutcome::CallerNotAuthorized),
        other => panic!("expected an error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_allowed_caller_with_a_matching_csr_gets_a_certificate() {
    let h = handler(real_ca(), vec![ALLOWED_CALLER]);
    let response = h
        .handle_internal_issue(
            Some(ALLOWED_CALLER),
            request_for(TARGET_IDENTITY, TARGET_IDENTITY),
        )
        .await;

    assert_eq!(response.status, 200);
    match response.body {
        HandlerBody::Success(issued) => {
            assert!(issued.certificate_pem.contains("BEGIN CERTIFICATE"));
        }
        other => panic!("expected success, got {other:?}"),
    }
}

#[tokio::test]
async fn an_allowed_caller_asserting_a_different_identity_than_the_csr_is_refused() {
    // The caller is trusted to say who the certificate is for, but the
    // CSR must still agree with it -- same guarantee /issue gives
    // against a token-derived identity.
    let h = handler(Arc::new(PanicCa), vec![ALLOWED_CALLER]);
    let response = h
        .handle_internal_issue(
            Some(ALLOWED_CALLER),
            request_for(
                "someone-else.polar.serviceaccount.cluster.local",
                TARGET_IDENTITY,
            ),
        )
        .await;

    assert_eq!(response.status, 403);
    match response.body {
        HandlerBody::Error(e) => assert_eq!(e.outcome, IssueOutcome::IdentityMismatch),
        other => panic!("expected an error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_empty_asserted_identity_is_rejected() {
    let h = handler(Arc::new(PanicCa), vec![ALLOWED_CALLER]);
    let response = h
        .handle_internal_issue(Some(ALLOWED_CALLER), request_for(TARGET_IDENTITY, ""))
        .await;

    assert_eq!(response.status, 400);
    match response.body {
        HandlerBody::Error(e) => assert_eq!(e.outcome, IssueOutcome::InvalidCsr),
        other => panic!("expected an error, got {other:?}"),
    }
}
