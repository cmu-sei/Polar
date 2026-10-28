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

//! The custodian's certificate (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian"): what a pod's
//! token buys, named `custody.<X>` for the workload X the token proves, and the only certificate that name or a
//! `p:custody:` name may be in. And what `/internal/issue` does for the credentials agent when a custodian asks for
//! its workload's certificates: no custodian certificate there, no name in the custodians' space, the workload's
//! pod instance recorded, and no certificate that outlives the custodian's.

use cert_issuer::ca::{CaClient, CaError, CaIssueRequest, IssuedCert};
use cert_issuer::config::IssuerConfig;
use cert_issuer::floor::BootstrapIdentity;
use cert_issuer::handler::{Handler, HandlerBody};
use cert_issuer::oidc::Validator;
use cert_issuer_common::identity::IdentityNormalization;
use cert_issuer_common::keypair::{KeyAlgorithm, generate_csr};
use cert_issuer_common::{CertType, InternalIssueRequest, IssueOutcome, IssueRequest};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod helpers;
use helpers::*;

/// The workload `mint_default_token` is for, its custodian, and the workload as its token names it.
const IDENTITY: &str = "git-observer.polar.serviceaccount.cluster.local";
const CUSTODIAN: &str = "custody.git-observer.polar.serviceaccount.cluster.local";
const WORKLOAD: &str = "system:serviceaccount:polar:git-observer";
const POD_UID: &str = "11111111-2222-3333-4444-555555555555";
const CREDENTIALS_AGENT: &str = "credentials-agent.polar.serviceaccount.cluster.local";
const DEFAULT_LIFETIME: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone)]
struct Asked {
    cert_type: CertType,
    sans: Vec<String>,
    lifetime: Duration,
    instance_binding: Option<String>,
}

#[derive(Default)]
struct RecordingCa {
    asked: Mutex<Vec<Asked>>,
}

#[async_trait::async_trait]
impl CaClient for RecordingCa {
    async fn issue(&self, req: CaIssueRequest) -> Result<IssuedCert, CaError> {
        self.asked.lock().unwrap().push(Asked {
            cert_type: req.cert_type,
            sans: req.sans,
            lifetime: req.lifetime,
            instance_binding: req.instance_binding,
        });
        Ok(IssuedCert {
            certificate_pem: "-----BEGIN CERTIFICATE-----\nrecorded\n-----END CERTIFICATE-----\n"
                .to_string(),
            ca_chain_pem: String::new(),
            serial_number: "01".to_string(),
            not_after: time::OffsetDateTime::now_utc(),
        })
    }
}

impl RecordingCa {
    fn asked(&self) -> Vec<Asked> {
        self.asked.lock().unwrap().clone()
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
        default_lifetime: DEFAULT_LIFETIME,
        server_lifetime: Duration::from_secs(43200),
        user_lifetime: Duration::from_secs(43200),
        service_lifetime: Duration::from_secs(31_536_000),
        own_workload: Default::default(),
        gate: None,
        identity_lifetime_overrides: HashMap::new(),
        internal_callers: vec![CREDENTIALS_AGENT.to_string()],
        floor,
        ca_cert_pem: String::new(),
    };
    (handler, ca)
}

/// A token for `subject`, otherwise `mint_default_token`'s.
fn token_for(subject: &str) -> String {
    mint_token(
        TEST_KID,
        primary_keypair(),
        serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": TEST_AUDIENCE,
            "sub": subject,
            "exp": now_plus(300),
            "kubernetes.io": {
                "namespace": "polar",
                "pod": { "name": "a-pod", "uid": POD_UID }
            }
        }),
    )
}

fn request(csr_identity: &str, cert_type: CertType, extra_sans: &[&str]) -> IssueRequest {
    let csr = generate_csr(csr_identity, &KeyAlgorithm::EcdsaP256, &[]).expect("csr");
    IssueRequest {
        csr_pem: csr.csr_pem,
        cert_type,
        extra_sans: extra_sans.iter().map(|s| s.to_string()).collect(),
        subject_dn_defaults: None,
    }
}

fn outcome(body: &HandlerBody) -> Option<IssueOutcome> {
    match body {
        HandlerBody::Error(e) => Some(e.outcome),
        _ => None,
    }
}

/// A floor in which the token's workload is a bootstrap identity (as Cassini and the credentials agent are).
fn bootstrap_floor() -> Option<Vec<BootstrapIdentity>> {
    Some(vec![BootstrapIdentity {
        workload: WORKLOAD.to_string(),
        cert_type: CertType::Client,
        names: vec![],
        vouched_only: false,
        publish: vec!["some-topic".to_string()],
        subscribe: vec![],
        groups: vec![],
    }])
}

// ---- on a token ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_custodian_vouched_for_is_signed_as_custody_of_its_workload_with_its_custody_names() {
    let (handler, ca) = rig(Some(vec![]));
    let response = handler
        .handle(
            Some(CREDENTIALS_AGENT),
            &mint_default_token(),
            request(
                CUSTODIAN,
                CertType::Custody,
                &["p:custody:client", "p:custody:escrow"],
            ),
        )
        .await;
    assert_eq!(response.status, 200, "{:?}", response.body);
    let asked = ca.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].cert_type, CertType::Custody);
    assert_eq!(
        asked[0].sans,
        vec![CUSTODIAN, "p:custody:client", "p:custody:escrow"]
    );
    assert_eq!(
        asked[0].lifetime, DEFAULT_LIFETIME,
        "a custodian lives as its pod's client certificate"
    );
    assert_eq!(asked[0].instance_binding.as_deref(), Some(POD_UID));
}

#[tokio::test]
async fn a_custodian_certificate_names_the_custodian_of_the_token_s_workload_and_no_one_else() {
    for csr_identity in [
        IDENTITY,
        "custody.kube-consumer.polar.serviceaccount.cluster.local",
        "custody.custody.git-observer.polar.serviceaccount.cluster.local",
    ] {
        let (handler, ca) = rig(Some(vec![]));
        let response = handler
            .handle(
                Some(CREDENTIALS_AGENT),
                &mint_default_token(),
                request(csr_identity, CertType::Custody, &[]),
            )
            .await;
        assert_eq!(
            outcome(&response.body),
            Some(IssueOutcome::IdentityMismatch),
            "{csr_identity}"
        );
        assert!(ca.asked().is_empty(), "{csr_identity}");
    }
}

#[tokio::test]
async fn a_custody_name_is_in_a_custodian_certificate_only_and_only_custody_names_are() {
    // On any other type, a custody name is refused, however written.
    for cert_type in [CertType::Client, CertType::Server] {
        for name in ["p:custody:client", "P:Custody:escrow", "p:custody:anything"] {
            let (handler, ca) = rig(Some(vec![]));
            let response = handler
                .handle(
                    Some(CREDENTIALS_AGENT),
                    &mint_default_token(),
                    request(IDENTITY, cert_type.clone(), &[name]),
                )
                .await;
            assert_eq!(
                outcome(&response.body),
                Some(IssueOutcome::InvalidCsr),
                "{cert_type:?} {name}"
            );
            assert!(ca.asked().is_empty());
        }
    }
    // And a custodian certificate carries custody names and nothing else.
    for name in [
        "p:custody:anything",
        "p:sub:some-topic",
        "g:graph-writer",
        "extra.example.com",
    ] {
        let (handler, ca) = rig(Some(vec![]));
        let response = handler
            .handle(
                Some(CREDENTIALS_AGENT),
                &mint_default_token(),
                request(CUSTODIAN, CertType::Custody, &[name]),
            )
            .await;
        // A name not on the workload's list is the floor's to refuse first; the rest are not custody names.
        assert!(
            matches!(
                outcome(&response.body),
                Some(IssueOutcome::InvalidCsr | IssueOutcome::CallerNotAuthorized)
            ),
            "{name}: {:?}",
            response.body
        );
        assert!(ca.asked().is_empty());
    }
}

#[tokio::test]
async fn a_bootstrap_service_s_token_alone_buys_its_custodian_with_nothing_else_in_it() {
    let (handler, ca) = rig(bootstrap_floor());
    let response = handler
        .handle(
            None,
            &mint_default_token(),
            request(CUSTODIAN, CertType::Custody, &[]),
        )
        .await;
    assert_eq!(response.status, 200, "{:?}", response.body);
    assert_eq!(
        ca.asked()[0].sans,
        vec![CUSTODIAN],
        "not the service's own topics: it is not the service"
    );
    // It asks for nothing on its service's behalf: the service's own certificates come from cert-issuer as now.
    let (handler, ca) = rig(bootstrap_floor());
    let response = handler
        .handle(
            None,
            &mint_default_token(),
            request(CUSTODIAN, CertType::Custody, &["p:custody:client"]),
        )
        .await;
    assert_eq!(response.status, 403, "{:?}", response.body);
    assert!(ca.asked().is_empty());
}

#[tokio::test]
async fn any_other_workload_s_token_alone_buys_no_custodian() {
    let (handler, ca) = rig(Some(vec![]));
    let response = handler
        .handle(
            None,
            &mint_default_token(),
            request(CUSTODIAN, CertType::Custody, &[]),
        )
        .await;
    assert_eq!(
        outcome(&response.body),
        Some(IssueOutcome::CallerNotAuthorized)
    );
    assert!(ca.asked().is_empty());
}

#[tokio::test]
async fn a_token_whose_workload_is_named_like_a_custodian_buys_nothing() {
    // A service account may have dots in its name: `custody.git-observer` in `polar` would be the custodian of
    // `git-observer` by name. The custodians' space is cert-issuer's to sign in, never a workload's.
    let token = token_for("system:serviceaccount:polar:custody.git-observer");
    for (csr_identity, cert_type) in [
        (CUSTODIAN, CertType::Client),
        (CUSTODIAN, CertType::Server),
        (
            "custody.custody.git-observer.polar.serviceaccount.cluster.local",
            CertType::Custody,
        ),
    ] {
        let (handler, ca) = rig(Some(vec![]));
        let response = handler
            .handle(
                Some(CREDENTIALS_AGENT),
                &token,
                request(csr_identity, cert_type.clone(), &[]),
            )
            .await;
        assert_eq!(
            outcome(&response.body),
            Some(IssueOutcome::CallerNotAuthorized),
            "{cert_type:?}"
        );
        assert!(ca.asked().is_empty());
    }
}

// ---- on /internal/issue ---------------------------------------------------------------------------------------

fn internal(csr_identity: &str, identity: &str, cert_type: CertType) -> InternalIssueRequest {
    let csr = generate_csr(csr_identity, &KeyAlgorithm::EcdsaP256, &[]).expect("csr");
    InternalIssueRequest {
        csr_pem: csr.csr_pem,
        identity: identity.to_string(),
        cert_type,
        extra_sans: vec![],
        subject_dn_defaults: None,
        account_subject: None,
        instance_binding: None,
        expires_by: None,
    }
}

fn unix_now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[tokio::test]
async fn a_custodian_is_never_signed_on_the_internal_path() {
    let (handler, ca) = rig(None);
    let response = handler
        .handle_internal_issue(
            Some(CREDENTIALS_AGENT),
            internal(CUSTODIAN, CUSTODIAN, CertType::Custody),
        )
        .await;
    assert_eq!(
        outcome(&response.body),
        Some(IssueOutcome::CallerNotAuthorized)
    );
    assert!(ca.asked().is_empty());
}

#[tokio::test]
async fn no_name_in_the_custodians_space_is_signed_on_the_internal_path() {
    for cert_type in [
        CertType::Client,
        CertType::Server,
        CertType::Service,
        CertType::Signing,
    ] {
        let (handler, ca) = rig(None);
        let response = handler
            .handle_internal_issue(
                Some(CREDENTIALS_AGENT),
                internal(CUSTODIAN, CUSTODIAN, cert_type.clone()),
            )
            .await;
        assert_eq!(
            outcome(&response.body),
            Some(IssueOutcome::CallerNotAuthorized),
            "{cert_type:?}"
        );
        assert!(ca.asked().is_empty());
    }
    let (handler, ca) = rig(None);
    let mut request = internal(IDENTITY, IDENTITY, CertType::Client);
    request.extra_sans = vec!["p:custody:client".to_string()];
    let response = handler
        .handle_internal_issue(Some(CREDENTIALS_AGENT), request)
        .await;
    assert_eq!(outcome(&response.body), Some(IssueOutcome::InvalidCsr));
    assert!(ca.asked().is_empty());
}

#[tokio::test]
async fn a_workload_certificate_asked_for_a_custodian_records_its_pod_and_ends_by_the_custodian_s_end()
 {
    let (handler, ca) = rig(None);
    let mut request = internal(IDENTITY, IDENTITY, CertType::Client);
    request.instance_binding = Some(POD_UID.to_string());
    request.expires_by = Some(unix_now() + 600);
    let response = handler
        .handle_internal_issue(Some(CREDENTIALS_AGENT), request)
        .await;
    assert_eq!(response.status, 200, "{:?}", response.body);
    let asked = &ca.asked()[0];
    assert_eq!(asked.instance_binding.as_deref(), Some(POD_UID));
    assert!(
        asked.lifetime <= Duration::from_secs(600) && asked.lifetime >= Duration::from_secs(590),
        "capped at the custodian's end: {:?}",
        asked.lifetime
    );
    // A later end than its own lifetime changes nothing.
    let (handler, ca) = rig(None);
    let mut request = internal(IDENTITY, IDENTITY, CertType::Client);
    request.expires_by = Some(unix_now() + 100_000);
    handler
        .handle_internal_issue(Some(CREDENTIALS_AGENT), request)
        .await;
    assert_eq!(ca.asked()[0].lifetime, DEFAULT_LIFETIME);
    // And an end already past signs nothing.
    let (handler, ca) = rig(None);
    let mut request = internal(IDENTITY, IDENTITY, CertType::Client);
    request.expires_by = Some(unix_now() - 1);
    let response = handler
        .handle_internal_issue(Some(CREDENTIALS_AGENT), request)
        .await;
    assert_eq!(
        outcome(&response.body),
        Some(IssueOutcome::CallerNotAuthorized)
    );
    assert!(ca.asked().is_empty());
}

#[tokio::test]
async fn a_server_certificate_on_the_internal_path_carries_only_the_names_its_workload_may_have() {
    let floor = Some(vec![BootstrapIdentity {
        workload: WORKLOAD.to_string(),
        cert_type: CertType::Server,
        names: vec!["git.polar.svc.cluster.local".to_string()],
        vouched_only: true,
        publish: vec![],
        subscribe: vec![],
        groups: vec![],
    }]);
    let mut allowed = internal(IDENTITY, IDENTITY, CertType::Server);
    allowed.extra_sans = vec!["git.polar.svc.cluster.local".to_string()];
    let (handler, ca) = rig(floor.clone());
    let response = handler
        .handle_internal_issue(Some(CREDENTIALS_AGENT), allowed)
        .await;
    assert_eq!(response.status, 200, "{:?}", response.body);
    assert_eq!(ca.asked().len(), 1);
    let mut other = internal(IDENTITY, IDENTITY, CertType::Server);
    other.extra_sans = vec!["cassini.polar.svc.cluster.local".to_string()];
    let (handler, ca) = rig(floor);
    let response = handler
        .handle_internal_issue(Some(CREDENTIALS_AGENT), other)
        .await;
    assert_eq!(
        outcome(&response.body),
        Some(IssueOutcome::CallerNotAuthorized)
    );
    assert!(ca.asked().is_empty());
}
