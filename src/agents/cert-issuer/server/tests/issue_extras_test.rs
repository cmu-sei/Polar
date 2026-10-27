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

//! What `/issue` and `/validate` will do for a caller, by who the caller is.
//!
//! A service-account token proves one thing: whose workload this is. Everything else a request carries (the
//! certificate type, the extra names) is the caller's own say-so. So, once the floor is configured
//! (`docs/architecture/cert-issuer-isolation.md`):
//!
//! * a caller **vouched for** by its verified mTLS identity (the credentials agent) states the type and the names
//!   itself, having applied its own controls;
//! * a caller with **only a token** may be one of the bootstrap identities, and then gets only the type and the
//!   names written down for it;
//! * any other token-only caller is refused, whatever it asks for.
//!
//! Until the floor is configured the handler keeps its old behaviour (it signs what a token holder asks for), and
//! a test here says so, so that the open mode is never an accident.
//!
//! The CA records what it is asked to sign, so each test says what would have been signed.

use cert_issuer::ca::{CaClient, CaError, CaIssueRequest, IssuedCert};
use cert_issuer::config::IssuerConfig;
use cert_issuer::floor::BootstrapIdentity;
use cert_issuer::handler::{Handler, HandlerBody, ValidateHandlerBody};
use cert_issuer::oidc::Validator;
use cert_issuer_common::identity::IdentityNormalization;
use cert_issuer_common::keypair::{KeyAlgorithm, generate_csr};
use cert_issuer_common::{CertType, IssueOutcome, IssueRequest, ValidateRequest};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod helpers;
use helpers::*;

/// The identity `mint_default_token` is for, in its DNS form, and as its token names it.
const IDENTITY: &str = "git-observer.polar.serviceaccount.cluster.local";
const WORKLOAD: &str = "system:serviceaccount:polar:git-observer";
/// The one internal caller.
const CREDENTIALS_AGENT: &str = "credentials-agent.polar.serviceaccount.cluster.local";

#[derive(Default)]
struct RecordingCa {
    asked: Mutex<Vec<(CertType, Vec<String>)>>,
}

#[async_trait::async_trait]
impl CaClient for RecordingCa {
    async fn issue(&self, req: CaIssueRequest) -> Result<IssuedCert, CaError> {
        self.asked.lock().unwrap().push((req.cert_type, req.sans));
        Ok(IssuedCert {
            certificate_pem: "-----BEGIN CERTIFICATE-----\nrecorded\n-----END CERTIFICATE-----\n"
                .to_string(),
            ca_chain_pem: String::new(),
            serial_number: "01".to_string(),
            not_after: time::OffsetDateTime::now_utc(),
        })
    }
}

fn issuer_config() -> IssuerConfig {
    IssuerConfig {
        issuer: TEST_ISSUER.to_string(),
        audience: vec![TEST_AUDIENCE.to_string()],
        jwks_uri: Some("https://test-issuer.example.com/jwks".to_string()),
        workload_identity_claim: "sub".to_string(),
        instance_binding_claim: "kubernetes.io/pod/uid".to_string(),
        identity_normalization: IdentityNormalization::KubernetesServiceAccount {
            cluster_domain: "cluster.local".to_string(),
        },
        identity_scope_claim: None,
        allowed_algorithms: vec!["RS256".to_string(), "ES256".to_string()],
        jwks_cache_ttl_min: Duration::from_secs(30),
        jwks_cache_ttl_max: Duration::from_secs(3600),
    }
}

fn rig(floor: Option<Vec<BootstrapIdentity>>) -> (Handler, Arc<RecordingCa>) {
    let jwks = jwks_doc(&[jwk_for(TEST_KID, primary_keypair())]);
    let source = Arc::new(FakeJwksSource::new(jwks));
    let ca = Arc::new(RecordingCa::default());
    let handler = Handler {
        validator: Arc::new(Validator::with_jwks_source(vec![issuer_config()], source)),
        ca: ca.clone(),
        default_lifetime: Duration::from_secs(3600),
        server_lifetime: Duration::from_secs(43200),
        user_lifetime: Duration::from_secs(43200),
        service_lifetime: Duration::from_secs(31_536_000),
        identity_lifetime_overrides: HashMap::new(),
        internal_callers: vec![CREDENTIALS_AGENT.to_string()],
        floor,
        ca_cert_pem: String::new(),
    };
    (handler, ca)
}

fn request(cert_type: CertType, extra_sans: &[&str]) -> IssueRequest {
    let csr = generate_csr(IDENTITY, &KeyAlgorithm::EcdsaP256, &[]).expect("csr");
    IssueRequest {
        csr_pem: csr.csr_pem,
        cert_type,
        extra_sans: extra_sans.iter().map(|s| s.to_string()).collect(),
        subject_dn_defaults: None,
    }
}

/// What the CA was asked to sign, as words, for a failure message.
fn signed(ca: &RecordingCa) -> String {
    format!("{:?}", ca.asked.lock().unwrap())
}

/// A floor in which the token's own workload is a bootstrap identity wanting a plain client certificate.
fn floor_with_this_workload() -> Option<Vec<BootstrapIdentity>> {
    Some(vec![BootstrapIdentity {
        workload: WORKLOAD.to_string(),
        cert_type: CertType::Client,
        names: vec![],
        vouched_only: false,
        publish: vec![],
        subscribe: vec![],
        groups: vec![],
    }])
}

/// A floor that names someone else only: the token's workload is an ordinary one.
fn floor_without_this_workload() -> Option<Vec<BootstrapIdentity>> {
    Some(vec![BootstrapIdentity {
        workload: "system:serviceaccount:polar:cassini".to_string(),
        cert_type: CertType::Server,
        names: vec!["cassini-ip-svc.polar.svc.cluster.local".to_string()],
        vouched_only: false,
        publish: vec![],
        subscribe: vec![],
        groups: vec![],
    }])
}

fn refused_as_not_authorized(response: &cert_issuer::handler::HandlerResponse) -> bool {
    response.status == 403
        && matches!(
            &response.body,
            HandlerBody::Error(e) if e.outcome == IssueOutcome::CallerNotAuthorized
        )
}

// ---- before the floor is configured ---------------------------------------------------------------------------

#[tokio::test]
async fn with_no_floor_configured_a_token_holder_is_still_signed_what_it_asks_for() {
    // The open mode, kept until polar-init goes through the credentials agent. This test exists so that it is a
    // decision and never an accident: configuring a floor is what closes it.
    let (handler, ca) = rig(None);
    let response = handler
        .handle(
            None,
            &mint_default_token(),
            request(
                CertType::Server,
                &["p:u2s:x", "cassini-ip-svc.polar.svc.cluster.local"],
            ),
        )
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(ca.asked.lock().unwrap().len(), 1);
}

// ---- a token and nothing else ----------------------------------------------------------------------------------

#[tokio::test]
async fn a_token_only_caller_that_is_not_a_bootstrap_identity_is_refused_whatever_it_asks_for() {
    let (handler, ca) = rig(floor_without_this_workload());
    for req in [
        request(CertType::Client, &[]),
        request(
            CertType::Client,
            &["p:u2s:polar-db-svc.polar-graph.svc.cluster.local"],
        ),
        request(
            CertType::Server,
            &["cassini-ip-svc.polar.svc.cluster.local"],
        ),
        request(CertType::Client, &["cert-issuer.polar.svc.cluster.local"]),
    ] {
        let response = handler.handle(None, &mint_default_token(), req).await;
        assert!(refused_as_not_authorized(&response), "{response:?}");
    }
    assert!(ca.asked.lock().unwrap().is_empty(), "{}", signed(&ca));
}

#[tokio::test]
async fn a_bootstrap_identity_is_signed_the_certificate_written_down_for_it() {
    let (handler, ca) = rig(floor_with_this_workload());
    let response = handler
        .handle(None, &mint_default_token(), request(CertType::Client, &[]))
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(
        *ca.asked.lock().unwrap(),
        vec![(CertType::Client, vec![IDENTITY.to_string()])]
    );
}

#[tokio::test]
async fn a_bootstrap_identity_cannot_ask_for_more_than_is_written_down_for_it() {
    let (handler, ca) = rig(floor_with_this_workload());
    for req in [
        request(CertType::Server, &[]),
        request(
            CertType::Client,
            &["p:u2s:polar-db-svc.polar-graph.svc.cluster.local"],
        ),
        request(CertType::Client, &["c:provision-account"]),
        request(
            CertType::Client,
            &["cassini-ip-svc.polar.svc.cluster.local"],
        ),
    ] {
        let response = handler.handle(None, &mint_default_token(), req).await;
        assert!(refused_as_not_authorized(&response), "{response:?}");
    }
    assert!(ca.asked.lock().unwrap().is_empty(), "{}", signed(&ca));
}

#[tokio::test]
async fn a_peer_that_is_not_the_credentials_agent_does_not_vouch_for_anyone() {
    let (handler, ca) = rig(floor_without_this_workload());
    let response = handler
        .handle(
            Some("kube-observer.polar.serviceaccount.cluster.local"),
            &mint_default_token(),
            request(CertType::Client, &[]),
        )
        .await;
    assert!(refused_as_not_authorized(&response), "{response:?}");
    assert!(ca.asked.lock().unwrap().is_empty(), "{}", signed(&ca));
}

// ---- vouched for by the credentials agent ----------------------------------------------------------------------

/// A floor in which the token's workload is reached only through the credentials agent and has one server name.
fn floor_with_this_workload_vouched_only() -> Option<Vec<BootstrapIdentity>> {
    Some(vec![BootstrapIdentity {
        workload: WORKLOAD.to_string(),
        cert_type: CertType::Server,
        names: vec!["polar-git-server.polar.svc.cluster.local".to_string()],
        vouched_only: true,
        publish: vec![],
        subscribe: vec![],
        groups: vec![],
    }])
}

#[tokio::test]
async fn the_credentials_agent_may_have_a_workload_signed_with_the_permissions_and_names_written_down_for_it()
 {
    let (handler, ca) = rig(floor_with_this_workload_vouched_only());
    let plain = handler
        .handle(
            Some(CREDENTIALS_AGENT),
            &mint_default_token(),
            request(CertType::Client, &[]),
        )
        .await;
    assert_eq!(plain.status, 200);
    let with_grant = handler
        .handle(
            Some(CREDENTIALS_AGENT),
            &mint_default_token(),
            request(
                CertType::Client,
                &["p:u2s:polar-db-svc.polar-graph.svc.cluster.local"],
            ),
        )
        .await;
    assert_eq!(with_grant.status, 200);
    let server = handler
        .handle(
            Some(CREDENTIALS_AGENT),
            &mint_default_token(),
            request(
                CertType::Server,
                &["polar-git-server.polar.svc.cluster.local"],
            ),
        )
        .await;
    assert_eq!(server.status, 200);
    assert_eq!(ca.asked.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn the_credentials_agent_cannot_have_a_workload_signed_with_a_name_that_is_not_written_down_for_it()
 {
    // The token's workload has an entry with one name; another workload's name, and a name for a workload with
    // no entry at all, are both refused even though the credentials agent vouches for the request.
    for floor in [
        floor_with_this_workload_vouched_only(),
        floor_without_this_workload(),
    ] {
        let (handler, ca) = rig(floor);
        for (cert_type, name) in [
            (CertType::Server, "cassini-ip-svc.polar.svc.cluster.local"),
            (
                CertType::Server,
                "polar-git-server.polar.svc.cluster.local.evil",
            ),
            (CertType::Client, "polar-git-server.polar.svc.cluster.local"),
        ] {
            let response = handler
                .handle(
                    Some(CREDENTIALS_AGENT),
                    &mint_default_token(),
                    request(cert_type, &[name]),
                )
                .await;
            assert!(refused_as_not_authorized(&response), "{name}: {response:?}");
        }
        assert!(ca.asked.lock().unwrap().is_empty(), "{}", signed(&ca));
    }
}

#[tokio::test]
async fn a_workload_that_is_reached_only_through_the_credentials_agent_is_refused_on_its_token_alone()
 {
    let (handler, ca) = rig(floor_with_this_workload_vouched_only());
    let response = handler
        .handle(
            None,
            &mint_default_token(),
            request(
                CertType::Server,
                &["polar-git-server.polar.svc.cluster.local"],
            ),
        )
        .await;
    assert!(refused_as_not_authorized(&response), "{response:?}");
    assert!(ca.asked.lock().unwrap().is_empty(), "{}", signed(&ca));
}

#[tokio::test]
async fn a_capability_is_refused_on_issue_even_for_the_credentials_agent() {
    let (handler, ca) = rig(floor_without_this_workload());
    for peer in [None, Some(CREDENTIALS_AGENT)] {
        let response = handler
            .handle(
                peer,
                &mint_default_token(),
                request(CertType::Client, &["c:provision-account"]),
            )
            .await;
        assert_ne!(response.status, 200);
    }
    assert!(ca.asked.lock().unwrap().is_empty(), "{}", signed(&ca));
}

#[tokio::test]
async fn a_signing_certificate_is_refused_on_issue_to_every_caller_whatever_the_floor_says() {
    // Signed only on /internal/issue, for the internal caller itself: a token proves whose workload it is, and the
    // credentials agent vouches for workloads, never for a key to sign as it with.
    let floor_writing_it_down = Some(vec![BootstrapIdentity {
        workload: WORKLOAD.to_string(),
        cert_type: CertType::Signing,
        names: vec![],
        vouched_only: false,
        publish: vec![],
        subscribe: vec![],
        groups: vec![],
    }]);
    for floor in [
        None,
        floor_with_this_workload(),
        floor_without_this_workload(),
        floor_writing_it_down,
    ] {
        let (handler, ca) = rig(floor.clone());
        for peer in [None, Some(CREDENTIALS_AGENT)] {
            let response = handler
                .handle(peer, &mint_default_token(), request(CertType::Signing, &[]))
                .await;
            assert!(
                refused_as_not_authorized(&response),
                "{peer:?}, {floor:?}: {response:?}"
            );
        }
        assert!(ca.asked.lock().unwrap().is_empty(), "{}", signed(&ca));
    }
}

// ---- /validate -------------------------------------------------------------------------------------------------

#[tokio::test]
async fn validate_is_for_the_credentials_agent_and_the_bootstrap_identities_only() {
    // Not a bootstrap identity, no voucher: refused.
    let (handler, _) = rig(floor_without_this_workload());
    let refused = handler
        .handle_validate(None, &mint_default_token(), ValidateRequest::default())
        .await;
    assert_eq!(refused.status, 403);
    assert!(matches!(
        refused.body,
        ValidateHandlerBody::Error(ref e) if e.outcome == IssueOutcome::CallerNotAuthorized
    ));
    // Vouched for by the credentials agent: answered.
    let vouched = handler
        .handle_validate(
            Some(CREDENTIALS_AGENT),
            &mint_default_token(),
            ValidateRequest::default(),
        )
        .await;
    assert_eq!(vouched.status, 200);
    // A bootstrap identity validating its own token: answered.
    let (handler, _) = rig(floor_with_this_workload());
    let bootstrap = handler
        .handle_validate(None, &mint_default_token(), ValidateRequest::default())
        .await;
    assert_eq!(bootstrap.status, 200);
}

#[tokio::test]
async fn with_no_floor_configured_validate_answers_a_token_holder_as_before() {
    let (handler, _) = rig(None);
    let response = handler
        .handle_validate(None, &mint_default_token(), ValidateRequest::default())
        .await;
    assert_eq!(response.status, 200);
}

#[tokio::test]
async fn validate_does_not_answer_a_workload_that_is_reached_only_through_the_credentials_agent() {
    let (handler, _) = rig(floor_with_this_workload_vouched_only());
    let refused = handler
        .handle_validate(None, &mint_default_token(), ValidateRequest::default())
        .await;
    assert_eq!(refused.status, 403);
}

// ---- the topics and groups of the floor (docs/architecture/policy-enforcement.md, stage 3) ---------------------

/// A bootstrap service's entry with the topics and groups the catalog gives it.
fn floor_with_topics() -> Option<Vec<BootstrapIdentity>> {
    Some(vec![BootstrapIdentity {
        workload: WORKLOAD.to_string(),
        cert_type: CertType::Client,
        names: vec![],
        vouched_only: false,
        publish: vec!["polar-authority-updates".to_string()],
        subscribe: vec!["credentials-grants".to_string()],
        groups: vec!["cassini-client".to_string(), "self-test".to_string()],
    }])
}

#[tokio::test]
async fn a_bootstrap_service_is_signed_with_the_topics_and_groups_its_floor_gives_it() {
    let (handler, ca) = rig(floor_with_topics());
    let response = handler
        .handle(None, &mint_default_token(), request(CertType::Client, &[]))
        .await;
    assert_eq!(response.status, 200, "{:?}", response.body);
    let asked = ca.asked.lock().unwrap();
    let sans = &asked[0].1;
    for expected in [
        "p:pub:polar-authority-updates",
        "p:sub:credentials-grants",
        "g:cassini-client",
        "g:self-test",
    ] {
        assert!(
            sans.iter().any(|s| s == expected),
            "{expected} is not in {sans:?}"
        );
    }
}

#[tokio::test]
async fn a_bootstrap_service_cannot_ask_for_a_topic_or_a_group_itself() {
    // What it carries is what the catalog wrote down, never what it asks for.
    let (handler, ca) = rig(floor_with_topics());
    for extra in ["p:pub:somebody.elses.topic", "g:operators"] {
        let response = handler
            .handle(
                None,
                &mint_default_token(),
                request(CertType::Client, &[extra]),
            )
            .await;
        assert!(
            refused_as_not_authorized(&response),
            "{extra}: {:?}",
            response.body
        );
    }
    assert!(ca.asked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_credentials_agent_states_a_workload_s_topics_and_groups_on_the_vouched_path() {
    // Like the permissions of a grant, they are the credentials agent's to state, from its own floor.
    let (handler, ca) = rig(floor_without_this_workload());
    let response = handler
        .handle(
            Some(CREDENTIALS_AGENT),
            &mint_default_token(),
            request(
                CertType::Client,
                &["p:pub:kubernetes.cluster.consumer.Pod", "g:cassini-client"],
            ),
        )
        .await;
    assert_eq!(response.status, 200, "{:?}", response.body);
    let asked = signed(&ca);
    assert!(
        asked.contains("p:pub:kubernetes.cluster.consumer.Pod")
            && asked.contains("g:cassini-client"),
        "{asked}"
    );
}
