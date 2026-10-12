/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

//! HTTP client for the Credentials Agent's observer -- POST /identity
//! and POST /issue. Distinct from cert-client's HandshakeClient, which
//! talks to cert-issuer directly; this talks to the observer, the
//! only thing a human's own tooling ever reaches directly.

use credentials_agent_common::identity::{
    IdentityNormalizationReply, IdentityNormalizationRequest,
};
use credentials_agent_common::issuance::{IssuanceReply, IssuanceRequest};

#[derive(Debug, thiserror::Error)]
pub enum ObserverClientError {
    #[error("observer unreachable: {0}")]
    Unreachable(String),
    /// A genuine infrastructure-level failure (broker unreachable, a
    /// real gateway timeout, a serialization error on the observer's
    /// side) -- these return a plain-text body, never JSON, which is
    /// exactly how this variant is distinguished from a real,
    /// well-formed refusal (always JSON) without needing to branch on
    /// HTTP status code at all -- some refusal reasons share the same
    /// status codes as genuine infrastructure failures. Worth
    /// retrying; a real refusal (returned as Ok(...Refused{..})) is
    /// not.
    #[error("observer request failed, worth retrying: {0}")]
    Retryable(String),
}

pub struct ObserverClient {
    base_url: String,
    http: reqwest::Client,
}

impl ObserverClient {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url,
            http: reqwest::Client::builder()
                // Slightly above the observer's own 100s per-attempt
                // ceiling, so the observer's own timeout always fires
                // first with a real, informative response rather than
                // this client's HTTP layer timing out first with
                // nothing.
                .timeout(std::time::Duration::from_secs(110))
                .build()
                .expect("reqwest client construction is infallible with these settings"),
        }
    }

    pub async fn identity(
        &self,
        credential: &str,
    ) -> Result<IdentityNormalizationReply, ObserverClientError> {
        let url = format!("{}/identity", self.base_url);
        let body = IdentityNormalizationRequest {
            credential: credential.to_string(),
        };

        let response = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ObserverClientError::Unreachable(e.to_string()))?;

        let status = response.status();
        let body_bytes = response.bytes().await.map_err(|e| {
            ObserverClientError::Retryable(format!("failed to read response body: {e}"))
        })?;

        serde_json::from_slice::<IdentityNormalizationReply>(&body_bytes).map_err(|_| {
            ObserverClientError::Retryable(format!(
                "non-JSON response (status {status}): {}",
                String::from_utf8_lossy(&body_bytes)
            ))
        })
    }

    pub async fn issue(
        &self,
        csr_pem: &str,
        credential: &str,
    ) -> Result<IssuanceReply, ObserverClientError> {
        let url = format!("{}/issue", self.base_url);
        let body = IssuanceRequest {
            csr_pem: csr_pem.to_string(),
            credential: credential.to_string(),
        };

        let response = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ObserverClientError::Unreachable(e.to_string()))?;

        let status = response.status();
        let body_bytes = response.bytes().await.map_err(|e| {
            ObserverClientError::Retryable(format!("failed to read response body: {e}"))
        })?;

        serde_json::from_slice::<IssuanceReply>(&body_bytes).map_err(|_| {
            ObserverClientError::Retryable(format!(
                "non-JSON response (status {status}): {}",
                String::from_utf8_lossy(&body_bytes)
            ))
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn identity_returns_normalized_reply_on_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/identity"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                IdentityNormalizationReply::Normalized {
                    identity: "testuser.polar.keycloak-issuer".to_string(),
                },
            ))
            .mount(&server)
            .await;

        let client = ObserverClient::new(server.uri());
        let reply = client.identity("any-token").await.unwrap();

        match reply {
            IdentityNormalizationReply::Normalized { identity } => {
                assert_eq!(identity, "testuser.polar.keycloak-issuer");
            }
            other => panic!("expected Normalized, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn identity_returns_refused_reply_when_well_formed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/identity"))
            .respond_with(ResponseTemplate::new(401).set_body_json(
                IdentityNormalizationReply::Refused {
                    reason: credentials_agent_common::identity::IdentityNormalizationRefusalReason::TokenInvalid,
                },
            ))
            .mount(&server)
            .await;

        let client = ObserverClient::new(server.uri());
        let reply = client.identity("bad-token").await.unwrap();

        assert!(matches!(reply, IdentityNormalizationReply::Refused { .. }));
    }

    #[tokio::test]
    async fn identity_returns_retryable_on_non_json_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/identity"))
            .respond_with(
                ResponseTemplate::new(504).set_body_string("timed out waiting for a reply"),
            )
            .mount(&server)
            .await;

        let client = ObserverClient::new(server.uri());
        let err = client.identity("any-token").await.unwrap_err();

        assert!(matches!(err, ObserverClientError::Retryable(_)));
    }

    #[tokio::test]
    async fn issue_returns_issued_reply_on_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(IssuanceReply::Issued {
                    certificate_pem: "fake-cert".to_string(),
                    ca_chain_pem: "fake-chain".to_string(),
                    escrow_public_key_pem: "fake-key".to_string(),
                }),
            )
            .mount(&server)
            .await;

        let client = ObserverClient::new(server.uri());
        let reply = client.issue("fake-csr", "any-token").await.unwrap();

        assert!(matches!(reply, IssuanceReply::Issued { .. }));
    }

    #[tokio::test]
    async fn issue_returns_refused_reply_when_well_formed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .respond_with(
                ResponseTemplate::new(403).set_body_json(IssuanceReply::Refused {
                    reason: credentials_agent_common::issuance::IssuanceRefusalReason::NoGrantFound,
                }),
            )
            .mount(&server)
            .await;

        let client = ObserverClient::new(server.uri());
        let reply = client.issue("fake-csr", "any-token").await.unwrap();

        assert!(matches!(reply, IssuanceReply::Refused { .. }));
    }

    #[tokio::test]
    async fn issue_returns_retryable_on_non_json_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .respond_with(ResponseTemplate::new(503).set_body_string("failed to reach the broker"))
            .mount(&server)
            .await;

        let client = ObserverClient::new(server.uri());
        let err = client.issue("fake-csr", "any-token").await.unwrap_err();

        assert!(matches!(err, ObserverClientError::Retryable(_)));
    }

    #[tokio::test]
    async fn unreachable_server_returns_unreachable_error() {
        let client = ObserverClient::new("http://127.0.0.1:1".to_string());
        let err = client.identity("any-token").await.unwrap_err();

        assert!(matches!(err, ObserverClientError::Unreachable(_)));
    }
}
