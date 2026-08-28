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

//! RFC 7662 token introspection -- corroborates a token's local
//! signature validation (which cert-issuer's /validate already does)
//! with a live call to the IdP, since local validation alone can't
//! detect a session revoked or logged out server-side before natural
//! expiry. Only ever called for credentials classified as
//! `CredentialKind::LikelyIdp` -- Kubernetes tokens have no RFC 7662
//! endpoint at all.

use serde::Deserialize;

pub const INTROSPECTION_ENDPOINT_ENV: &str = "CREDENTIALS_AGENT_INTROSPECTION_ENDPOINT";
pub const INTROSPECTION_CLIENT_ID_ENV: &str = "CREDENTIALS_AGENT_INTROSPECTION_CLIENT_ID";
pub const INTROSPECTION_CLIENT_SECRET_ENV: &str = "CREDENTIALS_AGENT_INTROSPECTION_CLIENT_SECRET";

#[derive(Debug, thiserror::Error)]
pub enum IntrospectionError {
    #[error("introspection endpoint unreachable: {0}")]
    Unreachable(String),
    #[error("malformed introspection response: {0}")]
    Malformed(String),
}

#[derive(Debug, Deserialize)]
struct IntrospectionResponse {
    active: bool,
}

pub struct IntrospectionClient {
    endpoint: String,
    client_id: String,
    client_secret: String,
    http: reqwest::Client,
}

impl IntrospectionClient {
    pub fn new(endpoint: String, client_id: String, client_secret: String) -> Self {
        Self {
            endpoint,
            client_id,
            client_secret,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client construction is infallible with these settings"),
        }
    }

    /// Reads client credentials from
    /// `CREDENTIALS_AGENT_INTROSPECTION_{ENDPOINT,CLIENT_ID,CLIENT_SECRET}`.
    /// Fails loudly rather than falling back to a default -- there is
    /// no sane default for an IdP-specific introspection endpoint or
    /// this agent's own registered client credentials.
    pub fn from_env() -> Result<Self, String> {
        let endpoint = std::env::var(INTROSPECTION_ENDPOINT_ENV)
            .map_err(|_| format!("{INTROSPECTION_ENDPOINT_ENV} not set"))?;
        let client_id = std::env::var(INTROSPECTION_CLIENT_ID_ENV)
            .map_err(|_| format!("{INTROSPECTION_CLIENT_ID_ENV} not set"))?;
        let client_secret = std::env::var(INTROSPECTION_CLIENT_SECRET_ENV)
            .map_err(|_| format!("{INTROSPECTION_CLIENT_SECRET_ENV} not set"))?;
        Ok(Self::new(endpoint, client_id, client_secret))
    }

    /// True if `token` is currently active per the IdP's own record --
    /// the real point of this call: catching a session revoked or
    /// logged out server-side, which a valid signature alone can never
    /// reveal.
    pub async fn is_active(&self, token: &str) -> Result<bool, IntrospectionError> {
        let response = self
            .http
            .post(&self.endpoint)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&[("token", token), ("token_type_hint", "access_token")])
            .send()
            .await
            .map_err(|e| IntrospectionError::Unreachable(e.to_string()))?;

        let body: IntrospectionResponse = response
            .json()
            .await
            .map_err(|e| IntrospectionError::Malformed(e.to_string()))?;

        Ok(body.active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn active_token_returns_true() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true
            })))
            .mount(&server)
            .await;

        let client = IntrospectionClient::new(
            format!("{}/introspect", server.uri()),
            "test-client".to_string(),
            "test-secret".to_string(),
        );

        assert!(client.is_active("some-token").await.unwrap());
    }

    #[tokio::test]
    async fn inactive_token_returns_false() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": false
            })))
            .mount(&server)
            .await;

        let client = IntrospectionClient::new(
            format!("{}/introspect", server.uri()),
            "test-client".to_string(),
            "test-secret".to_string(),
        );

        assert!(!client.is_active("revoked-token").await.unwrap());
    }

    #[tokio::test]
    async fn sends_client_credentials_via_basic_auth() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .and(header(
                "authorization",
                "Basic dGVzdC1jbGllbnQ6dGVzdC1zZWNyZXQ=", // base64("test-client:test-secret")
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true
            })))
            .mount(&server)
            .await;

        let client = IntrospectionClient::new(
            format!("{}/introspect", server.uri()),
            "test-client".to_string(),
            "test-secret".to_string(),
        );

        client
            .is_active("some-token")
            .await
            .expect("must succeed when Basic auth header matches exactly");
    }

    #[tokio::test]
    async fn sends_token_in_form_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .and(body_string_contains("token=the-specific-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true
            })))
            .mount(&server)
            .await;

        let client = IntrospectionClient::new(
            format!("{}/introspect", server.uri()),
            "test-client".to_string(),
            "test-secret".to_string(),
        );

        client
            .is_active("the-specific-token")
            .await
            .expect("must send token in form body");
    }

    #[tokio::test]
    async fn unreachable_server_is_an_error() {
        let client = IntrospectionClient::new(
            "http://127.0.0.1:1/introspect".to_string(),
            "test-client".to_string(),
            "test-secret".to_string(),
        );

        let err = client.is_active("any-token").await.unwrap_err();
        assert!(matches!(err, IntrospectionError::Unreachable(_)));
    }

    #[tokio::test]
    async fn malformed_response_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let client = IntrospectionClient::new(
            format!("{}/introspect", server.uri()),
            "test-client".to_string(),
            "test-secret".to_string(),
        );

        let err = client.is_active("any-token").await.unwrap_err();
        assert!(matches!(err, IntrospectionError::Malformed(_)));
    }
}
