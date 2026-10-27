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

//! cert-issuer run whole, as `run` runs it, for the tests that start it (`agent_test.rs`, `agent_shutdown_test.rs`).
//! Each of them is its own test binary, so each runs in its own process: the agents' actors have registered names, and
//! two in one process would collide.

#![allow(dead_code)] // shared by several test binaries, each of which uses some of it

use cert_issuer::agent::CertIssuer;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::helpers::*;

pub const SERVER_IDENTITY: &str = "cert-issuer.test";
pub const WORKLOAD: &str = "system:serviceaccount:polar:cert-issuer";
pub const IDENTITY: &str = "cert-issuer.polar.serviceaccount.cluster.local";

pub fn own_token() -> String {
    mint_token(
        TEST_KID,
        primary_keypair(),
        serde_json::json!({
            "iss": TEST_ISSUER,
            "aud": TEST_AUDIENCE,
            "sub": WORKLOAD,
            "exp": now_plus(3600),
            "kubernetes.io": {
                "namespace": "polar",
                "pod": { "name": "cert-issuer-abc", "uid": "11111111-2222-3333-4444-555555555555" }
            }
        }),
    )
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// One TLS GET of `path`, trusting the CA at `ca_path`; the whole raw response.
pub async fn get(addr: SocketAddr, ca_path: &Path, path: &str) -> Result<String, String> {
    let ca = std::fs::read_to_string(ca_path).map_err(|e| e.to_string())?;
    let mut roots = RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut ca.as_bytes()) {
        roots
            .add(cert.map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tcp = TcpStream::connect(addr).await.map_err(|e| e.to_string())?;
    let mut tls = TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from(SERVER_IDENTITY).unwrap(), tcp)
        .await
        .map_err(|e| e.to_string())?;
    tls.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: {SERVER_IDENTITY}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .await
    .map_err(|e| e.to_string())?;
    let mut out = String::new();
    tls.read_to_string(&mut out)
        .await
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// The DNS and URI names of the certificate in `pem`.
pub fn names_of(pem: &str) -> Vec<String> {
    use x509_parser::prelude::*;
    let der = rustls_pemfile::certs(&mut pem.as_bytes())
        .next()
        .unwrap()
        .unwrap();
    let (_, cert) = X509Certificate::from_der(der.as_ref()).unwrap();
    let mut names = Vec::new();
    for ext in cert.extensions() {
        if let ParsedExtension::SubjectAlternativeName(san) = ext.parsed_extension() {
            for name in &san.general_names {
                match name {
                    GeneralName::DNSName(d) => names.push(d.to_string()),
                    GeneralName::URI(u) => names.push(u.to_string()),
                    _ => {}
                }
            }
        }
    }
    names.sort();
    names
}

/// cert-issuer, run whole on its own thread, with a broker nothing listens on.
pub struct Rig {
    pub runtime: tokio::runtime::Runtime,
    _jwks: MockServer,
    pub dir: tempfile::TempDir,
    pub addr: SocketAddr,
    /// The status `run` ended with, when it ends.
    pub ended: std::sync::mpsc::Receiver<u8>,
}

impl Rig {
    /// Started with these settings over the sound ones.
    pub fn start(changes: &[(&str, &str)]) -> Self {
        ensure_crypto_provider();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let jwks = runtime.block_on(async {
            let server = MockServer::start().await;
            Mock::given(path("/jwks"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_bytes(jwks_doc(&[jwk_for(TEST_KID, primary_keypair())])),
                )
                .mount(&server)
                .await;
            server
        });

        let dir = tempfile::tempdir().unwrap();
        let at = |name: &str| dir.path().join(name).to_string_lossy().into_owned();
        std::fs::write(at("token"), own_token()).unwrap();
        let port = free_port();
        let issuers = serde_json::json!([{
            "issuer": TEST_ISSUER, "audience": [TEST_AUDIENCE], "jwks_uri": format!("{}/jwks", jwks.uri()),
            "workload_identity_claim": "sub", "instance_binding_claim": "kubernetes.io/pod/uid",
            "identity_normalization": {"strategy": "kubernetes_service_account", "cluster_domain": "cluster.local"},
            "identity_scope_claim": null, "allowed_algorithms": ["RS256"],
            "jwks_cache_ttl_min": {"secs": 30, "nanos": 0}, "jwks_cache_ttl_max": {"secs": 3600, "nanos": 0}
        }]);
        let floor = serde_json::json!([{
            "workload": WORKLOAD, "cert_type": "CLIENT", "names": [], "groups": ["cassini-client"]
        }]);
        let mut env: HashMap<String, String> = [
            ("CERT_ISSUER_BIND_ADDR", format!("127.0.0.1:{port}")),
            ("CERT_ISSUER_ISSUERS", issuers.to_string()),
            ("CERT_ISSUER_CA_CERT_PATH", at("ca/ca.crt")),
            ("CERT_ISSUER_CA_KEY_PATH", at("ca/ca.key")),
            ("CERT_ISSUER_DEFAULT_LIFETIME_SECS", "3600".to_string()),
            ("CERT_ISSUER_SERVER_LIFETIME_SECS", "3600".to_string()),
            ("CERT_ISSUER_USER_LIFETIME_SECS", "3600".to_string()),
            ("CERT_ISSUER_SERVICE_LIFETIME_SECS", "3600".to_string()),
            ("CERT_ISSUER_SERVER_IDENTITY", SERVER_IDENTITY.to_string()),
            ("CERT_ISSUER_BOOTSTRAP", floor.to_string()),
            ("CERT_ISSUER_OWN_TOKEN", at("token")),
            ("CERT_ISSUER_RECORD_PATH", at("record/issued.jsonl")),
            ("KUBERNETES_SA_TOKEN", at("no-such-token")),
            // A broker nothing listens on.
            ("BROKER_ADDR", "127.0.0.1:1".to_string()),
            ("CASSINI_SERVER_NAME", "cassini.test".to_string()),
            ("TLS_CLIENT_CERT", at("tls/client.crt")),
            ("TLS_CLIENT_KEY", at("tls/client.key")),
            // Where the cassini capability puts it: cert-issuer writes the CA it signs under there.
            ("TLS_CA_CERT", at("tls/ca.pem")),
            ("POLAR_CAPABILITIES", "cassini".to_string()),
            ("POLAR_HEALTH_FILE", at("health.json")),
            ("POLAR_HEALTH_CERTS", at("tls/client.crt")),
            ("POLAR_HEALTH_CASSINI_DEADLINE_SECS", "1".to_string()),
            ("POLAR_HEALTH_TICK_SECS", "1".to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        for (k, v) in changes {
            env.insert(k.to_string(), v.to_string());
        }
        let (tell, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let status =
                polar_agent_builder::agent::run_with::<CertIssuer>(&[], move |name: &str| {
                    env.get(name).cloned()
                });
            let _ = tell.send(status);
        });
        let addr = format!("127.0.0.1:{port}").parse().unwrap();
        Self {
            runtime,
            _jwks: jwks,
            dir,
            addr,
            ended,
        }
    }

    pub fn ca(&self) -> std::path::PathBuf {
        self.dir.path().join("ca/ca.crt")
    }

    pub fn get(&self, path: &str) -> Result<String, String> {
        self.runtime.block_on(get(self.addr, &self.ca(), path))
    }

    /// Its first answer to `path`, once it is up.
    pub fn first(&self, path: &str) -> String {
        let began = Instant::now();
        loop {
            if let Ok(answer) = self.get(path) {
                return answer;
            }
            assert!(
                began.elapsed() < Duration::from_secs(30),
                "cert-issuer never served"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
