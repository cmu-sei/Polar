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
//! and POST /issue for a person's tooling, and POST /workload/session and
//! POST /workload/issue for a workload that has no certificate yet and must
//! not send its service-account token in the clear. Distinct from cert-client's
//! HandshakeClient, which talks to cert-issuer directly; this talks to the
//! observer, which is what everything but the bootstrap services reaches.
//!
//! It was the user-cert-fetcher's own, and is shared so that the fetcher and the
//! workload side use one set of HTTP code and one idea of which failures are
//! worth retrying.

use credentials_agent_common::identity::{
    IdentityNormalizationReply, IdentityNormalizationRequest,
};
use credentials_agent_common::issuance::{IssuanceReply, IssuanceRequest};
use credentials_agent_common::workload_issuance::{SealedIssuanceRequest, SessionOffer};

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

/// What asking for a session came to.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionOutcome {
    Offer(SessionOffer),
    /// Too many sessions are open; try again shortly.
    Busy,
}

/// What a sealed request came to, short of a transport failure.
#[derive(Debug, Clone)]
pub enum SealedOutcome {
    /// The usual issuance reply, a certificate or a refusal.
    Issuance(IssuanceReply),
    /// The session is not there any more: open another and seal again.
    SessionGone,
    /// Too many sessions are open; try again shortly.
    Busy,
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
            cert_type: Default::default(),
            names: vec![],
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
    /// Asks for a session to seal a token to: `POST /workload/session`. The request carries nothing.
    pub async fn open_session(&self) -> Result<SessionOutcome, ObserverClientError> {
        let url = format!("{}/workload/session", self.base_url);
        let response = self
            .http
            .post(&url)
            .send()
            .await
            .map_err(|e| ObserverClientError::Unreachable(e.to_string()))?;

        let status = response.status();
        let body_bytes = response.bytes().await.map_err(|e| {
            ObserverClientError::Retryable(format!("failed to read response body: {e}"))
        })?;

        if let Ok(offer) = serde_json::from_slice::<SessionOffer>(&body_bytes) {
            return Ok(SessionOutcome::Offer(offer));
        }
        if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            return Ok(SessionOutcome::Busy);
        }
        Err(ObserverClientError::Retryable(format!(
            "unexpected response (status {status}): {}",
            String::from_utf8_lossy(&body_bytes)
        )))
    }

    /// Asks for a certificate with the token sealed to a session: `POST /workload/issue`.
    pub async fn issue_sealed(
        &self,
        request: &SealedIssuanceRequest,
    ) -> Result<SealedOutcome, ObserverClientError> {
        let url = format!("{}/workload/issue", self.base_url);
        let response = self
            .http
            .post(&url)
            .json(request)
            .send()
            .await
            .map_err(|e| ObserverClientError::Unreachable(e.to_string()))?;

        let status = response.status();
        let body_bytes = response.bytes().await.map_err(|e| {
            ObserverClientError::Retryable(format!("failed to read response body: {e}"))
        })?;

        // A well-formed reply is JSON whatever the status; a refusal is an answer, not a failure.
        if let Ok(reply) = serde_json::from_slice::<IssuanceReply>(&body_bytes) {
            return Ok(SealedOutcome::Issuance(reply));
        }
        match status {
            // The session was used, expired or never opened: open another.
            reqwest::StatusCode::GONE => Ok(SealedOutcome::SessionGone),
            reqwest::StatusCode::SERVICE_UNAVAILABLE => Ok(SealedOutcome::Busy),
            _ => Err(ObserverClientError::Retryable(format!(
                "unexpected response (status {status}): {}",
                String::from_utf8_lossy(&body_bytes)
            ))),
        }
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

    // ---- a workload's sealed request --------------------------------------------------------------------

    use credentials_agent_common::ephemeral_exchange::{
        EphemeralEncryptedPayload, EphemeralSession,
    };
    use credentials_agent_common::issuance::IssuanceRefusalReason;

    fn an_offer() -> SessionOffer {
        SessionOffer {
            session_id: "s1".to_string(),
            public_key: EphemeralSession::open().public_key(),
            valid_for_secs: 60,
        }
    }

    fn a_sealed_request() -> SealedIssuanceRequest {
        SealedIssuanceRequest {
            session_id: "s1".to_string(),
            csr_pem: "csr".to_string(),
            cert_type: Default::default(),
            names: vec![],
            sealed_credential: EphemeralEncryptedPayload::encrypt(b"token", &an_offer().public_key)
                .unwrap(),
        }
    }

    #[tokio::test]
    async fn a_session_offer_is_returned_as_the_offer() {
        let server = MockServer::start().await;
        let offer = an_offer();
        Mock::given(method("POST"))
            .and(path("/workload/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&offer))
            .mount(&server)
            .await;
        let outcome = ObserverClient::new(server.uri())
            .open_session()
            .await
            .unwrap();
        assert_eq!(outcome, SessionOutcome::Offer(offer));
    }

    #[tokio::test]
    async fn too_many_sessions_is_busy_and_not_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/workload/session"))
            .respond_with(ResponseTemplate::new(503).set_body_string("too many sessions are open"))
            .mount(&server)
            .await;
        let outcome = ObserverClient::new(server.uri())
            .open_session()
            .await
            .unwrap();
        assert_eq!(outcome, SessionOutcome::Busy);
    }

    #[tokio::test]
    async fn a_session_request_that_fails_otherwise_is_worth_retrying_and_an_absent_observer_is_unreachable()
     {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/workload/session"))
            .respond_with(ResponseTemplate::new(502).set_body_string("failed to reach the broker"))
            .mount(&server)
            .await;
        let err = ObserverClient::new(server.uri())
            .open_session()
            .await
            .unwrap_err();
        assert!(matches!(err, ObserverClientError::Retryable(_)), "{err:?}");

        let gone = ObserverClient::new("http://127.0.0.1:1".to_string());
        let err = gone.open_session().await.unwrap_err();
        assert!(
            matches!(err, ObserverClientError::Unreachable(_)),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_sealed_request_is_posted_as_json_and_a_certificate_comes_back() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/workload/issue"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "session_id": "s1", "csr_pem": "csr"
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(IssuanceReply::Issued {
                    certificate_pem: "cert".to_string(),
                    ca_chain_pem: "chain".to_string(),
                    escrow_public_key_pem: "key".to_string(),
                }),
            )
            .mount(&server)
            .await;
        let outcome = ObserverClient::new(server.uri())
            .issue_sealed(&a_sealed_request())
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            SealedOutcome::Issuance(IssuanceReply::Issued { .. })
        ));
    }

    #[tokio::test]
    async fn a_refusal_is_an_answer_whatever_its_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/workload/issue"))
            .respond_with(
                ResponseTemplate::new(403).set_body_json(IssuanceReply::Refused {
                    reason: IssuanceRefusalReason::AlreadyHasValidCertificate,
                }),
            )
            .mount(&server)
            .await;
        let outcome = ObserverClient::new(server.uri())
            .issue_sealed(&a_sealed_request())
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            SealedOutcome::Issuance(IssuanceReply::Refused {
                reason: IssuanceRefusalReason::AlreadyHasValidCertificate
            })
        ));
    }

    #[tokio::test]
    async fn a_503_that_is_a_signing_failure_is_told_from_a_503_that_is_a_full_table() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/workload/issue"))
            .respond_with(
                ResponseTemplate::new(503).set_body_json(IssuanceReply::Refused {
                    reason: IssuanceRefusalReason::SigningFailed,
                }),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/workload/issue"))
            .respond_with(ResponseTemplate::new(503).set_body_string("too many sessions are open"))
            .mount(&server)
            .await;
        let client = ObserverClient::new(server.uri());
        let first = client.issue_sealed(&a_sealed_request()).await.unwrap();
        assert!(matches!(
            first,
            SealedOutcome::Issuance(IssuanceReply::Refused {
                reason: IssuanceRefusalReason::SigningFailed
            })
        ));
        let second = client.issue_sealed(&a_sealed_request()).await.unwrap();
        assert!(matches!(second, SealedOutcome::Busy));
    }

    #[tokio::test]
    async fn a_session_that_is_gone_says_so_and_anything_else_unexpected_is_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/workload/issue"))
            .respond_with(
                ResponseTemplate::new(410)
                    .set_body_json(serde_json::json!({"reason": "unknown_session"})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/workload/issue"))
            .respond_with(
                ResponseTemplate::new(504).set_body_string("timed out waiting for a reply"),
            )
            .mount(&server)
            .await;
        let client = ObserverClient::new(server.uri());
        assert!(matches!(
            client.issue_sealed(&a_sealed_request()).await.unwrap(),
            SealedOutcome::SessionGone
        ));
        let err = client.issue_sealed(&a_sealed_request()).await.unwrap_err();
        assert!(matches!(err, ObserverClientError::Retryable(_)), "{err:?}");
    }
}
