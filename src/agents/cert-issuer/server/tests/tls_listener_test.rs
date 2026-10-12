//! End-to-end tests for the TLS listener: `serve_tls` on a loopback port
//! with a real CA, spoken to over real TLS.
//!
//! What's being proven: every connection is TLS; a client certificate is
//! optional, so /issue and /ca stay reachable without one; and
//! /internal/issue is reachable only by a client certificate that chains
//! to our CA *and* whose SAN is on the allowlist. Plain HTTP is not served.

use cert_issuer::ca::{CaClient, CaIssueRequest, RcgenCaClient, load_or_bootstrap_ca};
use cert_issuer::handler::Handler;
use cert_issuer::oidc::Validator;
use cert_issuer::server::build_router;
use cert_issuer::tls::{build_server_config, serve_tls};
use cert_issuer_common::keypair::{KeyAlgorithm, generate_csr};
use cert_issuer_common::{CertType, InternalIssueRequest, IssueRequest};
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;

const SERVER_IDENTITY: &str = "cert-issuer.test";
const ALLOWED: &str = "credentials-agent.polar.serviceaccount.cluster.local";
const OTHER: &str = "cassini.polar.serviceaccount.cluster.local";
const TARGET: &str = "some-account-uuid.polar.serviceaccount.cluster.local";

struct Fixture {
    addr: SocketAddr,
    ca_cert_pem: String,
    ca: Arc<RcgenCaClient>,
}

async fn start() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let (ca_cert_pem, ca_key_pem) = load_or_bootstrap_ca(
        dir.path().join("ca.crt").to_str().unwrap(),
        dir.path().join("ca.key").to_str().unwrap(),
        "Test CA",
    )
    .expect("bootstrap test CA");
    // RcgenCaClient holds the PEM in memory, so the temp dir can drop.
    let ca = Arc::new(RcgenCaClient::new(&ca_cert_pem, &ca_key_pem).expect("CA client"));

    let tls = build_server_config(ca.as_ref(), &ca_cert_pem, SERVER_IDENTITY)
        .await
        .expect("TLS server config");

    let handler = Arc::new(Handler {
        // Never consulted by /internal/issue, /ca or a request refused
        // for a missing Authorization header.
        validator: Arc::new(Validator::new(vec![])),
        ca: ca.clone(),
        default_lifetime: Duration::from_secs(3600),
        server_lifetime: Duration::from_secs(43200),
        user_lifetime: Duration::from_secs(43200),
        identity_lifetime_overrides: HashMap::new(),
        internal_callers: vec![ALLOWED.to_string()],
        ca_cert_pem: ca_cert_pem.clone(),
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve_tls(
        listener,
        tokio_rustls::TlsAcceptor::from(tls),
        build_router(handler),
    ));

    Fixture {
        addr,
        ca_cert_pem,
        ca,
    }
}

/// A client certificate for `identity`, signed by the fixture's CA.
/// Returns (certificate PEM, private key PEM).
async fn client_cert(f: &Fixture, identity: &str) -> (String, String) {
    let csr = generate_csr(identity, &KeyAlgorithm::Ed25519, &[]).unwrap();
    let issued =
        f.ca.issue(CaIssueRequest {
            csr_pem: csr.csr_pem,
            sans: vec![identity.to_string()],
            lifetime: Duration::from_secs(3600),
            cert_type: CertType::Client,
            subject_dn: Default::default(),
        })
        .await
        .expect("issue client cert");
    (issued.certificate_pem, csr.private_key_pem)
}

/// One TLS connection, one request, the whole raw response. Verifies the
/// server against the fixture's CA, as a properly provisioned caller would.
async fn request(f: &Fixture, client: Option<(String, String)>, raw: String) -> String {
    let mut roots = RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut f.ca_cert_pem.as_bytes()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let builder = ClientConfig::builder().with_root_certificates(roots);
    let config = match client {
        Some((cert_pem, key_pem)) => {
            let certs: Vec<CertificateDer<'static>> =
                rustls_pemfile::certs(&mut cert_pem.as_bytes())
                    .collect::<Result<_, _>>()
                    .unwrap();
            let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
                .unwrap()
                .unwrap();
            builder
                .with_client_auth_cert(certs, key)
                .expect("client auth cert")
        }
        None => builder.with_no_client_auth(),
    };

    let tcp = TcpStream::connect(f.addr).await.unwrap();
    let mut tls = TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from(SERVER_IDENTITY).unwrap(), tcp)
        .await
        .expect("TLS handshake against the fixture's own CA");
    tls.write_all(raw.as_bytes()).await.unwrap();

    // The server may close without a TLS close_notify, which surfaces as a
    // read error; whatever arrived before that is the response.
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), tls.read_to_end(&mut buf)).await;
    String::from_utf8_lossy(&buf).into_owned()
}

fn get(path: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: {SERVER_IDENTITY}\r\nConnection: close\r\n\r\n")
}

fn post(path: &str, body: &str) -> String {
    format!(
        "POST {path} HTTP/1.1\r\nHost: {SERVER_IDENTITY}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn status(response: &str) -> u16 {
    response
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn internal_body() -> String {
    let csr = generate_csr(TARGET, &KeyAlgorithm::EcdsaP256, &[]).unwrap();
    serde_json::to_string(&InternalIssueRequest {
        csr_pem: csr.csr_pem,
        identity: TARGET.to_string(),
        cert_type: CertType::Client,
        extra_sans: vec![],
        subject_dn_defaults: None,
        account_subject: None,
    })
    .unwrap()
}

#[tokio::test]
async fn ca_certificate_is_served_over_tls_without_a_client_certificate() {
    let f = start().await;
    let response = request(&f, None, get("/ca")).await;
    assert_eq!(status(&response), 200);
    assert!(response.contains("BEGIN CERTIFICATE"));
}

#[tokio::test]
async fn issue_is_reachable_without_a_client_certificate() {
    // No client certificate and no bearer token: the request must get as
    // far as the handler and be refused for the missing Authorization
    // header (401), not fail at the TLS layer.
    let f = start().await;
    let body = serde_json::to_string(&IssueRequest {
        csr_pem: "unused".to_string(),
        cert_type: CertType::Client,
        extra_sans: vec![],
        subject_dn_defaults: None,
    })
    .unwrap();
    let response = request(&f, None, post("/issue", &body)).await;
    assert_eq!(status(&response), 401);
}

#[tokio::test]
async fn internal_issue_without_a_client_certificate_is_refused() {
    let f = start().await;
    let response = request(&f, None, post("/internal/issue", &internal_body())).await;
    assert_eq!(status(&response), 403);
}

#[tokio::test]
async fn internal_issue_with_an_unlisted_client_certificate_is_refused() {
    // Chains to our CA, so the handshake succeeds -- but the SAN isn't on
    // the allowlist.
    let f = start().await;
    let cert = client_cert(&f, OTHER).await;
    let response = request(&f, Some(cert), post("/internal/issue", &internal_body())).await;
    assert_eq!(status(&response), 403);
}

#[tokio::test]
async fn internal_issue_with_an_allowlisted_client_certificate_succeeds() {
    let f = start().await;
    let cert = client_cert(&f, ALLOWED).await;
    let response = request(&f, Some(cert), post("/internal/issue", &internal_body())).await;
    assert_eq!(status(&response), 200);
    assert!(response.contains("BEGIN CERTIFICATE"));
}

#[tokio::test]
async fn plain_http_is_not_served() {
    let f = start().await;
    let mut tcp = TcpStream::connect(f.addr).await.unwrap();
    tcp.write_all(get("/ca").as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), tcp.read_to_end(&mut buf)).await;
    assert!(
        !String::from_utf8_lossy(&buf).starts_with("HTTP/"),
        "a plaintext request must not get an HTTP response from a TLS-only listener"
    );
}
