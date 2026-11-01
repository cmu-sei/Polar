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

//! The record through the handler (docs/architecture/cert-issuer-isolation.md, "Step 2"): every route's signing is in
//! the log, with how it came: a bootstrap service on its token alone, a workload the credentials agent vouches for, an
//! account on `/internal/issue`.

use cert_issuer::ca::{RcgenCaClient, load_or_bootstrap_ca};
use cert_issuer::config::IssuerConfig;
use cert_issuer::floor::BootstrapIdentity;
use cert_issuer::handler::{Handler, HandlerBody};
use cert_issuer::oidc::Validator;
use cert_issuer::record::{Record, RecordingCa};
use cert_issuer_common::identity::IdentityNormalization;
use cert_issuer_common::keypair::{KeyAlgorithm, generate_csr};
use cert_issuer_common::record::IssuePath;
use cert_issuer_common::{CertType, InternalIssueRequest, IssueRequest};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

mod helpers;
use helpers::*;

const CREDENTIALS_AGENT: &str = "credentials-agent.polar.serviceaccount.cluster.local";
const POD: &str = "11111111-2222-3333-4444-555555555555";

fn issuer_config() -> IssuerConfig {
    IssuerConfig {
        issuer: TEST_ISSUER.to_string(),
        audience: vec![TEST_AUDIENCE.to_string()],
        jwks_uri: Some("https://test-issuer.example.com/jwks".to_string()),
        workload_identity_claim: "sub".to_string(),
        instance_binding_claim: "kubernetes.io/pod/uid".to_string(),
        instance_name_claim: None,
        identity_normalization: IdentityNormalization::KubernetesServiceAccount {
            cluster_domain: "cluster.local".to_string(),
        },
        identity_scope_claim: None,
        allowed_algorithms: vec!["RS256".to_string()],
        jwks_cache_ttl_min: Duration::from_secs(30),
        jwks_cache_ttl_max: Duration::from_secs(3600),
    }
}

/// A handler signing through a recording CA, with a log at `log`.
fn handler(log: &Path) -> Handler {
    ensure_crypto_provider();
    let dir = tempfile::tempdir().unwrap();
    let (ca_cert_pem, ca_key_pem) = load_or_bootstrap_ca(
        dir.path().join("ca.crt").to_str().unwrap(),
        dir.path().join("ca.key").to_str().unwrap(),
        "Test CA",
    )
    .unwrap();
    let ca = RcgenCaClient::new(&ca_cert_pem, &ca_key_pem).unwrap();
    let jwks = jwks_doc(&[jwk_for(TEST_KID, primary_keypair())]);
    Handler {
        validator: Arc::new(Validator::with_jwks_source(
            vec![issuer_config()],
            Arc::new(FakeJwksSource::new(jwks)),
        )),
        ca: Arc::new(RecordingCa::new(Arc::new(ca), Record::open(log).unwrap())),
        default_lifetime: Duration::from_secs(3600),
        server_lifetime: Duration::from_secs(3600),
        user_lifetime: Duration::from_secs(3600),
        service_lifetime: Duration::from_secs(31_536_000),
        own_workload: Default::default(),
        gate: None,
        identity_lifetime_overrides: HashMap::new(),
        internal_callers: vec![CREDENTIALS_AGENT.to_string()],
        floor: Some(vec![BootstrapIdentity {
            workload: "system:serviceaccount:polar:cassini".to_string(),
            cert_type: CertType::Client,
            names: vec![],
            vouched_only: false,
            publish: vec![],
            subscribe: vec![],
            groups: vec![],
        }]),
        ca_cert_pem,
    }
}

fn token_for(service_account: &str) -> String {
    mint_token(
        TEST_KID,
        primary_keypair(),
        serde_json::json!({
            "iss": TEST_ISSUER, "aud": TEST_AUDIENCE,
            "sub": format!("system:serviceaccount:polar:{service_account}"),
            "exp": now_plus(300),
            "kubernetes.io": { "namespace": "polar", "pod": { "name": "p", "uid": POD } }
        }),
    )
}

fn issue_request(identity: &str) -> IssueRequest {
    IssueRequest {
        csr_pem: generate_csr(identity, &KeyAlgorithm::EcdsaP256, &[])
            .unwrap()
            .csr_pem,
        cert_type: CertType::Client,
        extra_sans: vec![],
        subject_dn_defaults: None,
    }
}

fn signed(body: &HandlerBody) {
    assert!(matches!(body, HandlerBody::Success(_)), "{body:?}");
}

#[tokio::test]
async fn every_route_s_signing_is_recorded_with_how_it_came() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("issued.jsonl");
    let h = handler(&log);

    let cassini = "cassini.polar.serviceaccount.cluster.local";
    signed(
        &h.handle(None, &token_for("cassini"), issue_request(cassini))
            .await
            .body,
    );
    let observer = "kube-observer.polar.serviceaccount.cluster.local";
    signed(
        &h.handle(
            Some(CREDENTIALS_AGENT),
            &token_for("kube-observer"),
            issue_request(observer),
        )
        .await
        .body,
    );
    let account = "an-account.polar.users.cluster.local";
    let csr = generate_csr(account, &KeyAlgorithm::EcdsaP256, &[]).unwrap();
    signed(
        &h.handle_internal_issue(
            Some(CREDENTIALS_AGENT),
            InternalIssueRequest {
                instance_binding: None,
                expires_by: None,
                csr_pem: csr.csr_pem,
                identity: account.to_string(),
                cert_type: CertType::Client,
                extra_sans: vec![],
                subject_dn_defaults: None,
                account_subject: None,
            },
        )
        .await
        .body,
    );

    let record = Record::open(&log).unwrap();
    let got: Vec<(&str, IssuePath, Option<&str>)> = record
        .entries()
        .iter()
        .map(|e| (e.identity.as_str(), e.path, e.instance_binding.as_deref()))
        .collect();
    assert_eq!(
        got,
        vec![
            (cassini, IssuePath::TokenOnly, Some(POD)),
            (observer, IssuePath::Vouched, Some(POD)),
            (account, IssuePath::Internal, None),
        ]
    );
    assert!(
        record
            .entries()
            .iter()
            .all(|e| e.certificate_pem.contains("BEGIN CERTIFICATE"))
    );
}

#[tokio::test]
async fn a_refused_request_signs_nothing_and_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("issued.jsonl");
    let h = handler(&log);
    // Not a bootstrap identity, and nobody vouches for it.
    let refused = h
        .handle(
            None,
            &token_for("kube-observer"),
            issue_request("kube-observer.polar.serviceaccount.cluster.local"),
        )
        .await;
    assert!(matches!(refused.body, HandlerBody::Error(_)));
    assert!(Record::open(&log).unwrap().entries().is_empty());
}
