// cert_issuer_init/src/handshake.rs

use cert_issuer_common::{CertType, IssueError, IssueRequest, IssueResponse};
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
    ) -> Result<IssueResponse, HandshakeError> {
        let url = format!("{}/issue", self.base_url);

        let body = IssueRequest {
            csr_pem: csr_pem.to_string(),
            cert_type,
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

        let status = response.status();
        let body_bytes = response
            .bytes()
            .await
            .map_err(|e| HandshakeError::Malformed(format!("failed to read response body: {e}")))?;

        if status.is_success() {
            serde_json::from_slice::<IssueResponse>(&body_bytes)
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
}
