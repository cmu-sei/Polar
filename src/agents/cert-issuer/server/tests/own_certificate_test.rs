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

//! cert-issuer's own client certificate (docs/architecture/cert-issuer-isolation.md, "1b in detail"): signed on its own
//! token, through its own handler and floor, as every bootstrap service's is, so its certificate carries what its floor
//! entry gives it and nothing more, and nothing about it goes around the floor.

use cert_issuer::ca::{
    CaClient, CaError, CaIssueRequest, IssuedCert, RcgenCaClient, load_or_bootstrap_ca,
};
use cert_issuer::config::IssuerConfig;
use cert_issuer::floor::BootstrapIdentity;
use cert_issuer::handler::Handler;
use cert_issuer::handler::HandlerBody;
use cert_issuer::oidc::Validator;
use cert_issuer::own_certificate::issue_own;
use cert_issuer_common::CertType;
use cert_issuer_common::identity::IdentityNormalization;
use cert_issuer_common::record::IssuePath;
use cert_issuer_common::{IssueOutcome, IssueRequest};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

mod helpers;
use helpers::*;

const WORKLOAD: &str = "system:serviceaccount:polar:cert-issuer";
const IDENTITY: &str = "cert-issuer.polar.serviceaccount.cluster.local";

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
        allowed_algorithms: vec!["RS256".to_string()],
        jwks_cache_ttl_min: Duration::from_secs(30),
        jwks_cache_ttl_max: Duration::from_secs(3600),
    }
}

const CREDENTIALS_AGENT: &str = "credentials-agent.polar.serviceaccount.cluster.local";

fn handler(floor: Option<Vec<BootstrapIdentity>>) -> Handler {
    handler_with(floor, None)
}

/// Wraps the handler's CA, to see what it is asked.
type WrapCa = Arc<dyn Fn(Arc<dyn CaClient>) -> Arc<dyn CaClient>>;

/// The handler, its CA wrapped by `ca` when one is given.
fn handler_with(floor: Option<Vec<BootstrapIdentity>>, ca: Option<WrapCa>) -> Handler {
    ensure_crypto_provider();
    let dir = tempfile::tempdir().unwrap();
    let (ca_cert_pem, ca_key_pem) = load_or_bootstrap_ca(
        dir.path().join("ca.crt").to_str().unwrap(),
        dir.path().join("ca.key").to_str().unwrap(),
        "Test CA",
    )
    .unwrap();
    let jwks = jwks_doc(&[jwk_for(TEST_KID, primary_keypair())]);
    Handler {
        validator: Arc::new(Validator::with_jwks_source(
            vec![issuer_config()],
            Arc::new(FakeJwksSource::new(jwks)),
        )),
        ca: {
            let inner: Arc<dyn CaClient> =
                Arc::new(RcgenCaClient::new(&ca_cert_pem, &ca_key_pem).unwrap());
            ca.map_or(inner.clone(), |wrap| wrap(inner))
        },
        default_lifetime: Duration::from_secs(3600),
        server_lifetime: Duration::from_secs(3600),
        user_lifetime: Duration::from_secs(3600),
        service_lifetime: Duration::from_secs(31_536_000),
        own_workload: Default::default(),
        floor_state: None,
        identity_lifetime_overrides: HashMap::new(),
        internal_callers: vec![CREDENTIALS_AGENT.to_string()],
        floor,
        ca_cert_pem,
    }
}

/// cert-issuer's own token, as its projected service account token says.
fn own_token() -> String {
    mint_token(
        TEST_KID,
        primary_keypair(),
        serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": TEST_AUDIENCE,
            "sub": WORKLOAD,
            "exp": now_plus(300),
            "kubernetes.io": {
                "namespace": "polar",
                "pod": { "name": "cert-issuer-abc", "uid": "11111111-2222-3333-4444-555555555555" }
            }
        }),
    )
}

fn own_entry() -> BootstrapIdentity {
    BootstrapIdentity {
        workload: WORKLOAD.to_string(),
        cert_type: CertType::Client,
        names: vec![],
        vouched_only: false,
        publish: vec!["cert-issuer.records".to_string()],
        subscribe: vec![],
        groups: vec!["cassini-client".to_string()],
    }
}

/// The DNS and URI names of the first certificate in `pem`, and whether it is for client authentication.
fn names_of(pem: &str) -> (Vec<String>, Vec<String>, bool) {
    use x509_parser::prelude::*;
    let der = rustls_pemfile::certs(&mut pem.as_bytes())
        .next()
        .unwrap()
        .unwrap();
    let (_, cert) = X509Certificate::from_der(der.as_ref()).unwrap();
    let mut dns = Vec::new();
    let mut uris = Vec::new();
    let mut client = false;
    for ext in cert.extensions() {
        match ext.parsed_extension() {
            ParsedExtension::SubjectAlternativeName(san) => {
                for name in &san.general_names {
                    match name {
                        GeneralName::DNSName(d) => dns.push(d.to_string()),
                        GeneralName::URI(u) => uris.push(u.to_string()),
                        _ => {}
                    }
                }
            }
            ParsedExtension::ExtendedKeyUsage(eku) => client = eku.client_auth,
            _ => {}
        }
    }
    (dns, uris, client)
}

#[tokio::test]
async fn cert_issuer_is_signed_its_own_client_certificate_with_what_its_floor_entry_gives_it() {
    let own = issue_own(&handler(Some(vec![own_entry()])), &own_token())
        .await
        .unwrap();
    let (dns, uris, client) = names_of(&own.certificate_pem);
    assert_eq!(dns, vec![IDENTITY.to_string()], "its one identity");
    let mut uris = uris;
    uris.sort();
    assert_eq!(
        uris,
        vec![
            "g:cassini-client".to_string(),
            "p:pub:cert-issuer.records".to_string()
        ],
        "its floor entry's groups and topics, and nothing more"
    );
    assert!(client, "a client certificate");
    assert!(own.private_key_pem.contains("PRIVATE KEY"));
    assert!(
        own.ca_chain_pem.contains("BEGIN CERTIFICATE"),
        "the chain it is signed under"
    );
}

#[tokio::test]
async fn with_no_entry_for_itself_in_an_enforced_floor_it_is_refused_its_certificate() {
    let refused = issue_own(&handler(Some(vec![])), &own_token()).await;
    assert!(
        refused.is_err(),
        "the floor decides cert-issuer's certificate as it decides anyone's"
    );
}

#[tokio::test]
async fn a_token_that_does_not_validate_gets_it_no_certificate() {
    assert!(issue_own(&handler(None), "not.a.token").await.is_err());
}

#[tokio::test]
async fn with_the_floor_open_it_is_signed_its_identity_alone() {
    let own = issue_own(&handler(None), &own_token()).await.unwrap();
    let (dns, uris, client) = names_of(&own.certificate_pem);
    assert_eq!(dns, vec![IDENTITY.to_string()]);
    assert!(uris.is_empty(), "{uris:?}");
    assert!(client);
}

#[tokio::test]
async fn its_certificate_key_and_ca_are_written_where_its_client_reads_them_with_the_key_kept_private()
 {
    use std::os::unix::fs::PermissionsExt;
    let own = issue_own(&handler(None), &own_token()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("tls/client.crt");
    let key = dir.path().join("tls/client.key");
    let ca = dir.path().join("tls/ca.pem");
    own.write(&cert, &key, &ca).unwrap();
    assert_eq!(std::fs::read_to_string(&cert).unwrap(), own.certificate_pem);
    assert_eq!(std::fs::read_to_string(&key).unwrap(), own.private_key_pem);
    assert_eq!(
        std::fs::read_to_string(&ca).unwrap(),
        own.ca_chain_pem,
        "the CA chain it was signed under, where its client looks for the broker's CA"
    );
    let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "{mode:o}");
}

/// A CA that remembers the path each certificate was signed on.
struct PathsOf {
    inner: Arc<dyn CaClient>,
    paths: Arc<std::sync::Mutex<Vec<IssuePath>>>,
}

#[async_trait::async_trait]
impl CaClient for PathsOf {
    async fn issue(&self, req: CaIssueRequest) -> Result<IssuedCert, CaError> {
        self.paths.lock().unwrap().push(req.path);
        self.inner.issue(req).await
    }
}

#[tokio::test]
async fn its_own_certificate_is_recorded_as_its_own_and_not_as_a_token_only_signing() {
    let paths = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = paths.clone();
    let h = handler_with(
        Some(vec![own_entry()]),
        Some(Arc::new(move |inner| {
            Arc::new(PathsOf {
                inner,
                paths: seen.clone(),
            }) as Arc<dyn CaClient>
        })),
    );
    issue_own(&h, &own_token()).await.unwrap();
    assert_eq!(*paths.lock().unwrap(), vec![IssuePath::Own]);
}

#[tokio::test]
async fn once_it_has_signed_itself_no_caller_over_http_is_signed_as_cert_issuer() {
    // docs/architecture/core-restarts.md, "2 in detail": its entry is for its own start, in process, and nobody else's.
    for floor in [Some(vec![own_entry()]), None] {
        let h = handler(floor.clone());
        issue_own(&h, &own_token()).await.unwrap();
        for peer in [None, Some(CREDENTIALS_AGENT)] {
            let csr = cert_issuer_common::keypair::generate_csr(
                IDENTITY,
                &cert_issuer_common::keypair::KeyAlgorithm::Ed25519,
                &[],
            )
            .unwrap();
            let response = h
                .handle(
                    peer,
                    &own_token(),
                    IssueRequest {
                        csr_pem: csr.csr_pem,
                        cert_type: CertType::Client,
                        extra_sans: vec![],
                        subject_dn_defaults: None,
                    },
                )
                .await;
            assert_eq!(
                response.status,
                403,
                "{peer:?}, floor {:?}",
                floor.is_some()
            );
            match response.body {
                HandlerBody::Error(e) => assert_eq!(e.outcome, IssueOutcome::CallerNotAuthorized),
                other => panic!("expected a refusal, got {other:?}"),
            }
        }
    }
}
