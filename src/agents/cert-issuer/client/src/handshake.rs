// cert_issuer_init/src/handshake.rs

use cert_issuer_common::{
    CertType, InternalIssueRequest, IssueError, IssueRequest, IssueResponse, SubjectDnDefaults,
    ValidateRequest, ValidateResponse,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HandshakeError {
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
            .map_err(|e| format!("building HTTP client: {e}"))?;
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
            .add_root_certificate(ca)
            .identity(identity)
            .build()
            .map_err(|e| format!("building HTTP client: {e}"))?;
        Ok(Self { base_url, http })
    }

    /// Read an optional CA-path environment variable. Unset and empty
    /// both mean "no path configured", so an empty value in a manifest
    /// can't silently select a different trust mode than an absent one.
    pub fn ca_path_from_env(var: &str) -> Option<String> {
        std::env::var(var).ok().filter(|p| !p.is_empty())
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
    pub async fn for_trust_mode(
        base_url: String,
        ca_path: Option<&str>,
    ) -> Result<Self, String> {
        let pem = match ca_path {
            Some(path) => std::fs::read(path)
                .map_err(|e| format!("reading CA certificate from {path}: {e}"))?,
            None => Self::fetch_ca_pem(&base_url).await?.into_bytes(),
        };
        Self::with_root_ca(base_url, &pem)
    }

    /// `for_trust_mode`, plus a client certificate presented on the
    /// handshake -- what `/internal/issue` needs. `cert_path` and
    /// `key_path` are the PEM files the agent's own mTLS identity already
    /// lives in (TLS_CLIENT_CERT, TLS_CLIENT_KEY). Missing or malformed
    /// files are errors; there is no fallback to an anonymous client.
    pub async fn for_trust_mode_with_identity(
        base_url: String,
        ca_path: Option<&str>,
        cert_path: &str,
        key_path: &str,
    ) -> Result<Self, String> {
        let ca_pem = match ca_path {
            Some(path) => std::fs::read(path)
                .map_err(|e| format!("reading CA certificate from {path}: {e}"))?,
            None => Self::fetch_ca_pem(&base_url).await?.into_bytes(),
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
            .map_err(|e| format!("building HTTP client: {e}"))?;
        let response = http
            .get(format!("{base_url}/ca"))
            .send()
            .await
            .map_err(|e| format!("GET /ca failed: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("GET /ca returned {}", response.status()));
        }
        response.text().await.map_err(|e| format!("reading /ca body: {e}"))
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
