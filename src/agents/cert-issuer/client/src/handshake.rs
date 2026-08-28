// cert_issuer_init/src/handshake.rs

use cert_issuer_common::{CertType, IssueError, IssueRequest, IssueResponse, ValidateResponse};
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
    ) -> Result<IssueResponse, HandshakeError> {
        let url = format!("{}/issue", self.base_url);

        let body = IssueRequest {
            csr_pem: csr_pem.to_string(),
            cert_type,
            extra_sans,
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
    pub async fn validate(&self, bearer_token: &str) -> Result<ValidateResponse, HandshakeError> {
        let url = format!("{}/validate", self.base_url);

        let response = self
            .http
            .post(&url)
            .bearer_auth(bearer_token)
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
