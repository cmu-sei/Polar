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

//! A thin wrapper around cert-issuer's /validate, called with
//! include_normalized_identity: true -- no introspection, no grant
//! lookup, no signing. Deliberately separate from IssuanceHandler
//! rather than a mode/flag on it, since the two do genuinely
//! different amounts of work for genuinely different purposes.

use cert_client::handshake::HandshakeClient;
use cert_issuer_common::ValidateRequest;
use credentials_agent_common::identity::{
    IdentityNormalizationRefusalReason, IdentityNormalizationReply, IdentityNormalizationRequest,
};

pub struct IdentityNormalizationHandler {
    pub cert_issuer: HandshakeClient,
}

impl IdentityNormalizationHandler {
    pub async fn handle(
        &self,
        request: IdentityNormalizationRequest,
    ) -> IdentityNormalizationReply {
        let result = self
            .cert_issuer
            .validate(
                &request.credential,
                ValidateRequest {
                    include_normalized_identity: true,
                },
            )
            .await;

        match result {
            Ok(validated) => match validated.normalized_identity {
                Some(identity) => IdentityNormalizationReply::Normalized { identity },
                // Shouldn't happen, given this handler always asks
                // for it -- but fail safe rather than unwrap/panic on
                // an unexpected server response shape.
                None => IdentityNormalizationReply::Refused {
                    reason: IdentityNormalizationRefusalReason::TokenInvalid,
                },
            },
            Err(_) => IdentityNormalizationReply::Refused {
                reason: IdentityNormalizationRefusalReason::TokenInvalid,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn valid_credential_returns_the_normalized_identity() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "testuser".to_string(),
                    instance_binding: None,
                    jti: None,
                    instance_name: None,
                    normalized_identity: Some("testuser.polar.keycloak-issuer".to_string()),
                },
            ))
            .mount(&server)
            .await;

        let handler = IdentityNormalizationHandler {
            cert_issuer: HandshakeClient::new(server.uri()),
        };

        let reply = handler
            .handle(IdentityNormalizationRequest {
                credential: "any-token".to_string(),
            })
            .await;

        match reply {
            IdentityNormalizationReply::Normalized { identity } => {
                assert_eq!(identity, "testuser.polar.keycloak-issuer");
            }
            other => panic!("expected Normalized, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_credential_is_refused() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(401).set_body_json(
                cert_issuer_common::IssueError {
                    outcome: cert_issuer_common::IssueOutcome::InvalidToken,
                    detail: "token validation failed".to_string(),
                },
            ))
            .mount(&server)
            .await;

        let handler = IdentityNormalizationHandler {
            cert_issuer: HandshakeClient::new(server.uri()),
        };

        let reply = handler
            .handle(IdentityNormalizationRequest {
                credential: "bad-token".to_string(),
            })
            .await;

        assert!(matches!(
            reply,
            IdentityNormalizationReply::Refused {
                reason: IdentityNormalizationRefusalReason::TokenInvalid
            }
        ));
    }
}
