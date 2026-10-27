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

// cert_issuer_init/src/handshake.rs

use cert_issuer_common::{
    CertType, InternalIssueRequest, IssueError, IssueRequest, IssueResponse, SubjectDnDefaults,
    ValidateRequest, ValidateResponse,
};
use polar_custody::client::ClientKey;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HandshakeError {
    /// The request is held (202, `IssueOutcome::Held`): a bootstrap service's request the floor state does not admit,
    /// waiting for the credentials agent to vouch for it. Send the same request again shortly.
    #[error("held: {0}")]
    Held(String),

    /// The cert issuer was not reachable at the network level.
    /// This is a transient infrastructure failure — Kubernetes
    /// restart policy will retry. Distinct from Rejected so the
    /// init container can emit a different exit code.
    #[error("cert issuer unreachable: {0}")]
    Unreachable(String),

    /// The cert issuer returned a structured error response. The
    /// `IssueError` contains the outcome enum and human-readable
    /// detail. The init container maps specific outcomes to specific
    /// exit codes so operators can distinguish misconfiguration
    /// (InvalidAudience, IdentityMismatch) from transient failure
    /// (CaUnavailable) from bugs (InternalError).
    #[error("cert issuer rejected request: {0:?}")]
    Rejected(IssueError),

    /// The response body was not valid JSON in the expected shape.
    /// Indicates either a proxy intercepting responses or a cert
    /// issuer version mismatch.
    #[error("malformed response from cert issuer: {0}")]
    Malformed(String),
}

pub struct HandshakeClient {
    base_url: String,
    http: reqwest::Client,
}

impl HandshakeClient {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url,
            http: reqwest::Client::builder()
                // A generous timeout: the cert issuer needs to fetch
                // JWKS and call rcgen. In a healthy cluster this is
                // well under a second, but we allow 30s to absorb
                // JWKS cache misses and CA cold starts.
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest client construction is infallible with these settings"),
        }
    }

    /// Build a client that trusts only the given CA certificate (PEM), with
    /// the platform's built-in roots turned off. For callers that were
    /// handed the Polar CA ahead of time (a mounted ConfigMap): they verify
    /// cert-issuer's server certificate from the very first request.
    pub fn with_root_ca(base_url: String, ca_pem: &[u8]) -> Result<Self, String> {
        let cert = reqwest::Certificate::from_pem(ca_pem)
            .map_err(|e| format!("invalid CA certificate PEM: {e}"))?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .tls_built_in_root_certs(false)
            .add_root_certificate(cert)
            .build()
            .map_err(|e| format!("building HTTP client: {}", Self::describe_error(&e)))?;
        Ok(Self { base_url, http })
    }

    /// Like `with_root_ca`, but the client also presents a certificate on
    /// the TLS handshake. `/internal/issue` needs this: cert-issuer derives
    /// the caller's identity from the verified peer certificate and checks
    /// it against its `internal_callers` allowlist. `identity_pem` is the
    /// certificate chain followed by its PKCS#8 private key, both PEM.
    pub fn with_root_ca_and_identity(
        base_url: String,
        ca_pem: &[u8],
        identity_pem: &[u8],
    ) -> Result<Self, String> {
        let ca = reqwest::Certificate::from_pem(ca_pem)
            .map_err(|e| format!("invalid CA certificate PEM: {e}"))?;
        let identity = reqwest::Identity::from_pem(identity_pem)
            .map_err(|e| format!("invalid client identity PEM: {e}"))?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .tls_built_in_root_certs(false)
            // Both TLS backends are compiled in (the workspace enables
            // rustls-tls on top of the default), and the default backend
            // is native-tls, which cannot use a PEM identity. Say so.
            .use_rustls_tls()
            .add_root_certificate(ca)
            .identity(identity)
            .build()
            .map_err(|e| format!("building HTTP client: {}", Self::describe_error(&e)))?;
        Ok(Self { base_url, http })
    }

    /// Read an optional CA-path environment variable. Unset and empty
    /// both mean "no path configured", so an empty value in a manifest
    /// can't silently select a different trust mode than an absent one.
    pub fn ca_path_from_env(var: &str) -> Option<String> {
        std::env::var(var).ok().filter(|p| !p.is_empty())
    }

    /// An error and every cause beneath it, joined. `reqwest` reports a
    /// builder failure as just "builder error" and keeps the reason in
    /// `source()`, which `{e}` alone drops.
    fn describe_error(error: &dyn std::error::Error) -> String {
        let mut message = error.to_string();
        let mut source = error.source();
        while let Some(cause) = source {
            message.push_str(": ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
        message
    }

    /// Choose how to trust cert-issuer, in one place for every caller.
    ///
    /// `ca_path` set: read that file and trust only it. A configured
    /// path that is missing or malformed is an error and never falls back
    /// to fetching, because silently degrading a configured trust root
    /// defeats the point of provisioning one.
    ///
    /// `ca_path` unset: the degraded self-bootstrap mode. Fetch the CA
    /// once from `GET /ca` without verifying the server, then trust only
    /// what came back.
    pub async fn for_trust_mode(base_url: String, ca_path: Option<&str>) -> Result<Self, String> {
        let pem = match ca_path {
            Some(path) => std::fs::read(path)
                .map_err(|e| format!("reading CA certificate from {path}: {e}"))?,
            None => Self::fetch_ca_pem(&base_url).await?.into_bytes(),
        };
        Self::with_root_ca(base_url, &pem)
    }

    /// `for_trust_mode`, plus a client certificate presented on the
    /// handshake -- what `/internal/issue` needs. `cert_path` is the PEM
    /// file the agent's own mTLS certificate already lives in
    /// (TLS_CLIENT_CERT), and `key` its key: the file it lives in
    /// (TLS_CLIENT_KEY), or the custody daemon that holds it
    /// (POLAR_CUSTODY_SOCKET). Missing or malformed files are errors;
    /// there is no fallback to an anonymous client.
    pub async fn for_trust_mode_with_identity(
        base_url: String,
        ca_path: Option<&str>,
        cert_path: &str,
        key: &ClientKey,
    ) -> Result<Self, String> {
        let ca_pem = match ca_path {
            Some(path) => std::fs::read(path)
                .map_err(|e| format!("reading CA certificate from {path}: {e}"))?,
            None => Self::fetch_ca_pem(&base_url).await?.into_bytes(),
        };
        let key_path = match key {
            ClientKey::File(path) => path,
            ClientKey::Custody(_) => {
                return Self::with_root_ca_and_custody(base_url, &ca_pem, cert_path, key);
            }
        };
        let mut identity_pem = std::fs::read(cert_path)
            .map_err(|e| format!("reading client certificate from {cert_path}: {e}"))?;
        identity_pem.push(b'\n');
        identity_pem.extend(
            std::fs::read(key_path)
                .map_err(|e| format!("reading client key from {key_path}: {e}"))?,
        );
        Self::with_root_ca_and_identity(base_url, &ca_pem, &identity_pem)
    }

    /// Like `with_root_ca_and_identity`, the key being the custody
    /// daemon's (docs/architecture/key-custody.md): a PEM identity cannot
    /// carry a key the client never holds, so rustls is configured here,
    /// TLS 1.3 alone, and handed to reqwest whole.
    fn with_root_ca_and_custody(
        base_url: String,
        ca_pem: &[u8],
        cert_path: &str,
        key: &ClientKey,
    ) -> Result<Self, String> {
        use rustls::pki_types::CertificateDer;
        use rustls::pki_types::pem::PemObject as _;
        let mut roots = rustls::RootCertStore::empty();
        for certificate in CertificateDer::pem_slice_iter(ca_pem) {
            roots
                .add(certificate.map_err(|e| format!("invalid CA certificate PEM: {e}"))?)
                .map_err(|e| format!("invalid CA certificate: {e}"))?;
        }
        let verifier = rustls::client::WebPkiServerVerifier::builder(std::sync::Arc::new(roots))
            .build()
            .map_err(|e| format!("building the server verifier: {e}"))?;
        let chain = CertificateDer::pem_file_iter(cert_path)
            .and_then(|certificates| certificates.collect::<Result<Vec<_>, _>>())
            .map_err(|e| format!("reading client certificate from {cert_path}: {e}"))?;
        let tls = polar_custody::client::client_config(verifier, chain, key)?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .use_preconfigured_tls(tls)
            .build()
            .map_err(|e| format!("building HTTP client: {}", Self::describe_error(&e)))?;
        Ok(Self { base_url, http })
    }

    /// Fetch the CA certificate from `GET /ca` WITHOUT verifying the
    /// server. Only for the degraded self-bootstrap mode, where no CA was
    /// provisioned ahead of time. The request carries no credential, and
    /// the caller must pin the result and verify every later request
    /// against it (`with_root_ca`). It trusts in-cluster name resolution
    /// for this one fetch, which is the accepted trade-off there.
    pub async fn fetch_ca_pem(base_url: &str) -> Result<String, String> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .danger_accept_invalid_certs(true)
            .build()
            .map_err(|e| format!("building HTTP client: {}", Self::describe_error(&e)))?;
        let response = http
            .get(format!("{base_url}/ca"))
            .send()
            .await
            .map_err(|e| format!("GET /ca failed: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("GET /ca returned {}", response.status()));
        }
        response
            .text()
            .await
            .map_err(|e| format!("reading /ca body: {e}"))
    }

    /// Execute the issuance handshake: POST the CSR with the SA token
    /// as the Bearer credential, return the issued cert response.
    ///
    /// The caller is responsible for:
    ///   - Reading the projected SA token from the filesystem
    ///   - Generating the CSR via `keypair::generate_csr`
    ///   - Writing the result via `output::write_bundle`
    ///
    /// This function only handles the HTTP exchange.
    pub async fn issue(
        &self,
        bearer_token: &str,
        csr_pem: &str,
        cert_type: CertType,
        extra_sans: Vec<String>,
        subject_dn_defaults: Option<SubjectDnDefaults>,
    ) -> Result<IssueResponse, HandshakeError> {
        let url = format!("{}/issue", self.base_url);

        let body = IssueRequest {
            csr_pem: csr_pem.to_string(),
            cert_type,
            extra_sans,
            subject_dn_defaults,
        };

        let response = self
            .http
            .post(&url)
            .bearer_auth(bearer_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                // reqwest surfaces connection refused, DNS failure,
                // and timeout all as the same error kind at the top
                // level. We collapse them all to Unreachable — the
                // distinction between "DNS failed" and "connection
                // refused" doesn't drive different operator actions.
                HandshakeError::Unreachable(e.to_string())
            })?;

        parse_response(response).await
    }

    /// Execute the /validate handshake -- OIDC validation only, no
    /// CSR, no signing. Built for credentials-agent's two-call
    /// issuance flow: it needs the real, validated identity (for a
    /// grant lookup and the renewal-prevention check) before it has
    /// permitted SANs to embed into a CSR, since those come from a
    /// grant that can't be looked up until identity is known.
    pub async fn validate(
        &self,
        bearer_token: &str,
        request: ValidateRequest,
    ) -> Result<ValidateResponse, HandshakeError> {
        let url = format!("{}/validate", self.base_url);

        let response = self
            .http
            .post(&url)
            .bearer_auth(bearer_token)
            .json(&request)
            .send()
            .await
            .map_err(|e| HandshakeError::Unreachable(e.to_string()))?;

        parse_response(response).await
    }

    /// `POST /internal/issue`: no bearer token. The caller has already done
    /// its own authorization (a Kubernetes-token check for bootstrap, a
    /// username/email match for a claim) and asserts the identity to sign
    /// for. Only works on a client built with an identity
    /// (`with_root_ca_and_identity`), and only for a caller on cert-issuer's
    /// `internal_callers` allowlist.
    pub async fn issue_internal(
        &self,
        request: InternalIssueRequest,
    ) -> Result<IssueResponse, HandshakeError> {
        let url = format!("{}/internal/issue", self.base_url);

        let response = self
            .http
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| HandshakeError::Unreachable(e.to_string()))?;

        parse_response(response).await
    }
}

impl HandshakeClient {
    /// `GET /internal/held`: the held requests not yet answered, for the credentials agent while its Cassini session
    /// is lost (docs/architecture/core-restarts.md, "3 in detail"). Only works on a client built with its identity.
    pub async fn held_list(&self) -> Result<cert_issuer_common::held::HeldList, HandshakeError> {
        let url = format!("{}{}", self.base_url, cert_issuer_common::held::HELD_ROUTE);
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| HandshakeError::Unreachable(e.to_string()))?;
        parse_response(response).await
    }

    /// `POST /internal/held/<id>`: the credentials agent's answer to a held request. `Ok(taken)` says whether
    /// cert-issuer took it (a request it holds, not yet answered).
    pub async fn held_answer(
        &self,
        id: &str,
        decision: &cert_issuer_common::held::HeldDecision,
    ) -> Result<bool, HandshakeError> {
        #[derive(serde::Deserialize)]
        struct Taken {
            taken: bool,
        }
        let url = format!(
            "{}{}/{id}",
            self.base_url,
            cert_issuer_common::held::HELD_ROUTE
        );
        let response = self
            .http
            .post(&url)
            .json(decision)
            .send()
            .await
            .map_err(|e| HandshakeError::Unreachable(e.to_string()))?;
        parse_response::<Taken>(response).await.map(|t| t.taken)
    }
}

/// Shared response-parsing tail for both `/issue` and `/validate`:
/// a 2xx body deserializes as `T`, anything else deserializes as
/// `IssueError` (both endpoints share the same error shape).
async fn parse_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, HandshakeError> {
    let status = response.status();
    let body_bytes = response
        .bytes()
        .await
        .map_err(|e| HandshakeError::Malformed(format!("failed to read response body: {e}")))?;

    // A held request is an answer, not a success: it carries no certificate.
    if status == reqwest::StatusCode::ACCEPTED
        && let Ok(held) = serde_json::from_slice::<IssueError>(&body_bytes)
        && held.outcome == cert_issuer_common::IssueOutcome::Held
    {
        return Err(HandshakeError::Held(held.detail));
    }
    if status.is_success() {
        serde_json::from_slice::<T>(&body_bytes)
            .map_err(|e| HandshakeError::Malformed(format!("invalid success body: {e}")))
    } else {
        // Non-2xx: attempt to deserialize as IssueError. If that
        // fails, the response isn't from the cert issuer (proxy,
        // load balancer, wrong endpoint) and Malformed is correct.
        let issue_error = serde_json::from_slice::<IssueError>(&body_bytes).map_err(|e| {
            HandshakeError::Malformed(format!(
                "non-2xx response ({status}) with unparseable body: {e}"
            ))
        })?;
        Err(HandshakeError::Rejected(issue_error))
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn a_client_identity_that_is_not_pem_is_refused_up_front() {
        let result = HandshakeClient::with_root_ca_and_identity(
            "https://cert-issuer.invalid".to_string(),
            b"not a certificate",
            b"not an identity",
        );
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod identity_build_tests {
    use super::*;

    /// The real build path, not just the file reading: a client with a
    /// client identity must actually construct. A CA certificate and key
    /// stand in for any PEM identity, since the builder only needs a
    /// parseable certificate followed by a PKCS#8 key.
    #[test]
    fn a_client_carrying_an_identity_builds() {
        let (cert, key) = cert_issuer_common::ca_root::bootstrap_ca("Test CA").unwrap();
        let identity = format!("{cert}\n{key}");

        let built = HandshakeClient::with_root_ca_and_identity(
            "https://cert-issuer.invalid:8443".to_string(),
            cert.as_bytes(),
            identity.as_bytes(),
        );

        assert!(
            built.is_ok(),
            "client with identity did not build: {:?}",
            built.err()
        );
    }
}

#[cfg(test)]
mod custody_identity_tests {
    use super::*;
    use polar_custody::custody::Custody;
    use polar_custody::protocol::Slot;
    use polar_custody::server::{Callers, bind, serve};
    use rustls::pki_types::pem::PemObject as _;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use std::os::unix::fs::MetadataExt as _;
    use std::sync::{Arc, RwLock};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    const IDENTITY: &str = "credentials-agent.polar.serviceaccount.cluster.local";

    fn first_dns_name(der: &[u8]) -> String {
        use x509_parser::prelude::*;
        let (_, cert) = X509Certificate::from_der(der).unwrap();
        cert.subject_alternative_name()
            .unwrap()
            .unwrap()
            .value
            .general_names
            .iter()
            .find_map(|n| match n {
                GeneralName::DNSName(d) => Some(d.to_string()),
                _ => None,
            })
            .unwrap()
    }

    fn ca() -> (rcgen::Issuer<'static, rcgen::KeyPair>, String) {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Polar Internal CA");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate().unwrap();
        let pem = params.self_signed(&key).unwrap().pem();
        (rcgen::Issuer::new(params, key), pem)
    }

    /// An HTTPS server under the CA for one request, demanding a client certificate: it answers with the first name on
    /// the certificate it was shown.
    async fn server(issuer: &rcgen::Issuer<'static, rcgen::KeyPair>, ca_pem: &str) -> u16 {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = params.signed_by(&key, issuer).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from_pem_slice(ca_pem.as_bytes()).unwrap())
            .unwrap();
        let config = rustls::ServerConfig::builder()
            .with_client_cert_verifier(
                rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
                    .build()
                    .unwrap(),
            )
            .with_single_cert(
                vec![certificate.der().clone()],
                PrivateKeyDer::from_pem_slice(key.serialize_pem().as_bytes()).unwrap(),
            )
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut tls = acceptor.accept(tcp).await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0u8; 1];
                tls.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            let peer = tls.get_ref().1.peer_certificates().unwrap()[0].clone();
            let body = first_dns_name(peer.as_ref());
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            tls.write_all(response.as_bytes()).await.unwrap();
            tls.shutdown().await.ok();
        });
        port
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn with_the_custody_daemon_s_key_the_client_presents_its_certificate() {
        let (issuer, ca_pem) = ca();
        let dir = tempfile::tempdir().unwrap();
        let uid = std::fs::metadata(dir.path()).unwrap().uid();
        let socket = dir.path().join("custody.sock");
        tokio::spawn(serve(
            bind(&socket).unwrap(),
            Arc::new(RwLock::new(Custody::new(&ca_pem).unwrap())),
            Arc::new(Callers {
                installer_uid: uid,
                workload_uids: vec![uid],
            }),
        ));
        // polar-init's part: the daemon's CSR certified and installed, and the certificate in the bundle.
        let daemon = polar_custody::client::CustodyClient::new(&socket);
        let csr =
            tokio::task::block_in_place(|| daemon.make_csr(Slot::Client, IDENTITY, &[])).unwrap();
        let mut request = rcgen::CertificateSigningRequestParams::from_pem(&csr).unwrap();
        request.params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
        let certificate = request.signed_by(&issuer).unwrap().pem();
        tokio::task::block_in_place(|| daemon.install(Slot::Client, vec![certificate.clone()]))
            .unwrap();
        let cert_path = dir.path().join("cert.pem");
        std::fs::write(&cert_path, &certificate).unwrap();
        let ca_path = dir.path().join("ca.pem");
        std::fs::write(&ca_path, &ca_pem).unwrap();

        let port = server(&issuer, &ca_pem).await;
        let client = HandshakeClient::for_trust_mode_with_identity(
            format!("https://localhost:{port}"),
            Some(ca_path.to_str().unwrap()),
            cert_path.to_str().unwrap(),
            &polar_custody::client::ClientKey::Custody(socket.display().to_string()),
        )
        .await
        .unwrap();
        let seen = client
            .http
            .get(format!("{}/who", client.base_url))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(seen, IDENTITY);
    }
}
