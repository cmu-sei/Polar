//! Handshake logic: call cert issuer's /issue endpoint and parse response.

use cert_issuer_common::{IssueError, IssueResponse};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HandshakeError {
    #[error("cert issuer unreachable: {0}")]
    Unreachable(String),
    #[error("cert issuer returned error: {0:?}")]
    Rejected(IssueError),
    #[error("cert issuer response was malformed: {0}")]
    Malformed(String),
}

pub struct HandshakeClient {
    pub base_url: String,
    pub http: reqwest::Client,
}

impl HandshakeClient {
    pub async fn issue(
        &self,
        _bearer_token: &str,
        _csr_pem: &str,
    ) -> Result<IssueResponse, HandshakeError> {
        todo!("POST /issue with bearer token and CSR; parse response");
    }
}
