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

//! Tests for `Handler::handle_internal_issue` -- the token-free signing
//! path for callers that have already authorized a request elsewhere
//! (credentials-agent, for admin bootstrap and account claims).
//!
//! What's being proven: the caller allowlist is checked before anything
//! else happens, a refused caller never reaches the CA, and an allowed
//! caller gets the same identity-matching guarantee /issue gives against
//! a token -- the CSR's SAN must equal the identity the caller asserts.

use cert_issuer::ca::{
    CaClient, CaError, CaIssueRequest, IssuedCert, RcgenCaClient, load_or_bootstrap_ca,
};
use cert_issuer::handler::{Handler, HandlerBody};
use cert_issuer::oidc::Validator;
use cert_issuer_common::keypair::{KeyAlgorithm, generate_csr};
use cert_issuer_common::{AccountSubject, CertType, InternalIssueRequest, IssueOutcome};
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
        user_lifetime: Duration::from_secs(43200),
        identity_lifetime_overrides: HashMap::new(),
        internal_callers: internal_callers.into_iter().map(String::from).collect(),
        floor: None,
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
        account_subject: None,
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

// ---- authoritative account subject (uid, mail) ----

const OID_UID: &str = "0.9.2342.19200300.100.1.1";
const OID_MAIL: &str = "0.9.2342.19200300.100.1.3";

fn request_with_subject(username: &str, email: &str) -> InternalIssueRequest {
    let mut request = request_for(TARGET_IDENTITY, TARGET_IDENTITY);
    request.account_subject = Some(AccountSubject {
        username: username.to_string(),
        email: email.to_string(),
    });
    request
}

fn certificate_of(body: HandlerBody) -> String {
    match body {
        HandlerBody::Success(issued) => issued.certificate_pem,
        other => panic!("expected success, got {other:?}"),
    }
}

/// The value of one Subject DN attribute in the (leaf) certificate.
fn subject_attribute(certificate_pem: &str, oid: &str) -> Option<String> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(certificate_pem.as_bytes()).unwrap();
    let (_, cert) = x509_parser::parse_x509_certificate(&pem.contents).unwrap();
    for rdn in cert.subject().iter() {
        for atv in rdn.iter() {
            if atv.attr_type().to_id_string() == oid {
                return atv.as_str().ok().map(String::from);
            }
        }
    }
    None
}

#[tokio::test]
async fn an_account_subject_becomes_uid_and_mail_in_the_certificate() {
    let h = handler(real_ca(), vec![ALLOWED_CALLER]);
    let response = h
        .handle_internal_issue(
            Some(ALLOWED_CALLER),
            request_with_subject("alice", "alice@example.com"),
        )
        .await;

    assert_eq!(response.status, 200);
    let pem = certificate_of(response.body);
    assert_eq!(subject_attribute(&pem, OID_UID).as_deref(), Some("alice"));
    assert_eq!(
        subject_attribute(&pem, OID_MAIL).as_deref(),
        Some("alice@example.com")
    );
}

#[tokio::test]
async fn without_an_account_subject_the_certificate_has_no_uid_or_mail() {
    let h = handler(real_ca(), vec![ALLOWED_CALLER]);
    let response = h
        .handle_internal_issue(
            Some(ALLOWED_CALLER),
            request_for(TARGET_IDENTITY, TARGET_IDENTITY),
        )
        .await;

    assert_eq!(response.status, 200);
    let pem = certificate_of(response.body);
    assert_eq!(subject_attribute(&pem, OID_UID), None);
    assert_eq!(subject_attribute(&pem, OID_MAIL), None);
}

#[tokio::test]
async fn a_csr_cannot_claim_its_own_uid_or_mail() {
    // A CSR that puts uid and mail in its own subject is the user speaking
    // for themselves about fields that are admin-authoritative. Both are
    // ignored: with an account subject the caller's values win, and
    // without one the certificate carries neither.
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec![TARGET_IDENTITY.to_string()]).unwrap();
    params.distinguished_name.push(
        rcgen::DnType::CustomDnType(vec![0, 9, 2342, 19200300, 100, 1, 1]),
        "attacker",
    );
    params.distinguished_name.push(
        rcgen::DnType::CustomDnType(vec![0, 9, 2342, 19200300, 100, 1, 3]),
        "attacker@evil.example",
    );
    let csr_pem = params.serialize_request(&key).unwrap().pem().unwrap();

    let h = handler(real_ca(), vec![ALLOWED_CALLER]);

    let mut with_subject = request_with_subject("alice", "alice@example.com");
    with_subject.csr_pem = csr_pem.clone();
    let response = h
        .handle_internal_issue(Some(ALLOWED_CALLER), with_subject)
        .await;
    assert_eq!(response.status, 200);
    let pem = certificate_of(response.body);
    assert_eq!(subject_attribute(&pem, OID_UID).as_deref(), Some("alice"));
    assert_eq!(
        subject_attribute(&pem, OID_MAIL).as_deref(),
        Some("alice@example.com")
    );

    let mut without_subject = request_for(TARGET_IDENTITY, TARGET_IDENTITY);
    without_subject.csr_pem = csr_pem;
    let response = h
        .handle_internal_issue(Some(ALLOWED_CALLER), without_subject)
        .await;
    assert_eq!(response.status, 200);
    let pem = certificate_of(response.body);
    assert_eq!(subject_attribute(&pem, OID_UID), None);
    assert_eq!(subject_attribute(&pem, OID_MAIL), None);
}

#[tokio::test]
async fn a_malformed_account_subject_is_an_internal_error_and_never_reaches_the_ca() {
    for (username, email) in [
        ("alice", "not-an-email"),
        ("alice", "a@b@c"),
        ("alice", "al ice@example.com"),
        ("", "alice@example.com"),
        ("ali\nce", "alice@example.com"),
    ] {
        let h = handler(Arc::new(PanicCa), vec![ALLOWED_CALLER]);
        let response = h
            .handle_internal_issue(Some(ALLOWED_CALLER), request_with_subject(username, email))
            .await;

        assert_eq!(response.status, 500, "{username:?} / {email:?}");
        match response.body {
            HandlerBody::Error(e) => assert_eq!(e.outcome, IssueOutcome::InternalError),
            other => panic!("expected an error, got {other:?}"),
        }
    }
}
