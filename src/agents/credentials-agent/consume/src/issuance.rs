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

//! Ties every issuance-flow piece together: credential classification,
//! IdP introspection, cert-issuer's two endpoints, the ledger's grant/
//! renewal checks, and reply construction. Shared by both real callers
//! of this shape -- user self-service issuance and admin bootstrap --
//! per "one mechanism, both flows."

use crate::credential::{CredentialKind, classify_credential};
use std::sync::Arc;
use crate::escrow::EscrowKeypair;
use crate::introspection::IntrospectionClient;
use crate::ledger::LedgerMessage;
use cert_client::handshake::HandshakeClient;
use cert_issuer_common::CertType;
use credentials_agent_common::events::CredentialsEvent;
use credentials_agent_common::issuance::{IssuanceReply, IssuanceRefusalReason, IssuanceRequest};
use ractor::ActorRef;
use tracing::{error, warn};

pub struct IssuanceHandler {
    pub ledger: ActorRef<LedgerMessage>,
    pub cert_issuer: HandshakeClient,
    /// `None` if introspection isn't configured for this deployment.
    /// A LikelyIdp credential arriving with no introspection client
    /// available is refused (TokenIntrospectionFailed), not silently
    /// allowed through unintrospected.
    pub introspection: Option<IntrospectionClient>,
    pub known_kubernetes_issuers: Vec<String>,
    pub escrow_keypair: Arc<EscrowKeypair>,
}

impl IssuanceHandler {
    pub async fn handle(&self, request: IssuanceRequest) -> IssuanceReply {
        // ---- Step 1: classify the credential ----
        let kind = match classify_credential(&request.credential, &self.known_kubernetes_issuers)
        {
            Ok(k) => k,
            Err(e) => {
                warn!("Credential classification failed: {e}");
                return IssuanceReply::Refused {
                    reason: IssuanceRefusalReason::TokenInvalid,
                };
            }
        };

        // ---- Step 2: IdP introspection (LikelyIdp only) ----
        if kind == CredentialKind::LikelyIdp {
            match &self.introspection {
                Some(client) => match client.is_active(&request.credential).await {
                    Ok(true) => {}
                    Ok(false) => {
                        // Genuinely inactive per the IdP's own live
                        // record -- this is the token itself being
                        // invalid, discovered via a different
                        // mechanism than local signature checking,
                        // not an introspection-infrastructure problem.
                        return IssuanceReply::Refused {
                            reason: IssuanceRefusalReason::TokenInvalid,
                        };
                    }
                    Err(e) => {
                        warn!("Token introspection call failed: {e}");
                        return IssuanceReply::Refused {
                            reason: IssuanceRefusalReason::TokenIntrospectionFailed,
                        };
                    }
                },
                None => {
                    warn!("LikelyIdp credential arrived but introspection is not configured");
                    return IssuanceReply::Refused {
                        reason: IssuanceRefusalReason::TokenIntrospectionFailed,
                    };
                }
            }
        }

        // ---- Step 3: real validation via cert-issuer's /validate ----
        let validated = match self
            .cert_issuer
            .validate(&request.credential, cert_issuer_common::ValidateRequest::default())
            .await
        {
            Ok(v) => v,
            Err(e) => {
                warn!("cert-issuer /validate rejected the credential: {e}");
                return IssuanceReply::Refused {
                    reason: IssuanceRefusalReason::TokenInvalid,
                };
            }
        };

        let identity = validated.workload_identity;
        // Fails closed rather than proceeding with a degraded renewal
        // check: instance_binding is what the pod-cannot-renew-itself
        // property is actually keyed on, so a token/config combination
        // that doesn't provide it can't be safely processed at all.
        let Some(instance_binding) = validated.instance_binding else {
            warn!("Validated token for '{identity}' has no instance_binding -- refusing rather than degrading the renewal check");
            return IssuanceReply::Refused {
                reason: IssuanceRefusalReason::TokenInvalid,
            };
        };

        // ---- Step 4: renewal-prevention check ----
        let already_valid = match call_check_renewal(&self.ledger, &identity, &instance_binding).await {
            Ok(v) => v,
            Err(e) => {
                error!("Ledger CheckRenewal call failed: {e}");
                return IssuanceReply::Refused {
                    reason: IssuanceRefusalReason::TokenInvalid,
                };
            }
        };
        if already_valid {
            return IssuanceReply::Refused {
                reason: IssuanceRefusalReason::AlreadyHasValidCertificate,
            };
        }

        // ---- Step 5: grant lookup ----
        let grant = match call_check_grant(&self.ledger, &identity).await {
            Ok(g) => g,
            Err(e) => {
                error!("Ledger CheckGrant call failed: {e}");
                return IssuanceReply::Refused {
                    reason: IssuanceRefusalReason::TokenInvalid,
                };
            }
        };

        let permissions = match (kind, grant) {
            (CredentialKind::LikelyIdp, None) => {
                return IssuanceReply::Refused {
                    reason: IssuanceRefusalReason::NoGrantFound,
                };
            }
            (CredentialKind::LikelyIdp, Some(g)) => g.permissions,
            // Kubernetes credentials (admin bootstrap, or any other
            // K8s-originated request) never require a prior grant --
            // a missing one just means no extra permissions embedded,
            // matching "no more privileged than any other agent gets."
            (CredentialKind::Kubernetes, grant) => {
                grant.map(|g| g.permissions).unwrap_or_default()
            }
        };

        // ---- Step 6: sign, via cert-issuer's /issue ----
        let issued = match self
            .cert_issuer
            .issue(&request.credential, &request.csr_pem, CertType::Client, permissions)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                error!("cert-issuer /issue failed after all prior checks passed: {e}");
                return IssuanceReply::Refused {
                    reason: IssuanceRefusalReason::SigningFailed,
                };
            }
        };

        // ---- Step 7: record the issuance ----
        //
        // The certificate is already real and signed at this point --
        // a failure here is a genuine audit-trail gap, not a reason to
        // tell the caller their (already valid) certificate failed.
        // Logged loudly for operator attention, not silently swallowed.
        let event = CredentialsEvent::IssuanceCompleted {
            identity,
            instance_binding,
            serial: issued.serial_number,
            timestamp: polar::time::now_millis(),
        };
        if let Err(e) = call_append(&self.ledger, event).await {
            error!(
                "Failed to append IssuanceCompleted to the ledger for an already-issued \
                 certificate -- genuine audit-trail gap, needs operator attention: {e}"
            );
        }

        IssuanceReply::Issued {
            certificate_pem: issued.certificate_pem,
            ca_chain_pem: issued.ca_chain_pem,
            escrow_public_key_pem: self.escrow_keypair.public_key_pem().to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Ledger call helpers
// ---------------------------------------------------------------------------

async fn call_check_grant(
    ledger: &ActorRef<LedgerMessage>,
    identity: &str,
) -> Result<Option<crate::state::GrantRecord>, String> {
    match ractor::rpc::call(
        ledger,
        |reply| LedgerMessage::CheckGrant {
            identity: identity.to_string(),
            reply,
        },
        None,
    )
    .await
    {
        Ok(ractor::rpc::CallResult::Success(result)) => Ok(result),
        Ok(other) => Err(format!("unexpected CallResult: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

async fn call_check_renewal(
    ledger: &ActorRef<LedgerMessage>,
    identity: &str,
    instance_binding: &str,
) -> Result<bool, String> {
    match ractor::rpc::call(
        ledger,
        |reply| LedgerMessage::CheckRenewal {
            identity: identity.to_string(),
            instance_binding: instance_binding.to_string(),
            reply,
        },
        None,
    )
    .await
    {
        Ok(ractor::rpc::CallResult::Success(result)) => Ok(result),
        Ok(other) => Err(format!("unexpected CallResult: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

async fn call_append(ledger: &ActorRef<LedgerMessage>, event: CredentialsEvent) -> Result<(), String> {
    match ractor::rpc::call(
        ledger,
        |reply| LedgerMessage::Append { event, reply },
        None,
    )
    .await
    {
        Ok(ractor::rpc::CallResult::Success(result)) => result,
        Ok(other) => Err(format!("unexpected CallResult: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{LedgerActor, LedgerActorArgs};
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_token(iss: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"iss":"{iss}"}}"#));
        format!("{header}.{payload}.")
    }

    async fn spawn_test_ledger() -> ActorRef<LedgerMessage> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        // Leak the tempdir so it isn't cleaned up while the actor's
        // still using it -- fine for a short-lived test process.
        std::mem::forget(dir);
        let (ledger, _) = ractor::Actor::spawn(
            None,
            LedgerActor,
            LedgerActorArgs {
                log_path: path.to_str().unwrap().to_string(),
                projection_actor: crate::projection::tests::fake_projection_actor().await,
            },
        )
        .await
        .unwrap();
        ledger
    }

    fn fake_issue_response() -> cert_issuer_common::IssueResponse {
        cert_issuer_common::IssueResponse {
            certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                .to_string(),
            ca_chain_pem: "-----BEGIN CERTIFICATE-----\nfake-ca\n-----END CERTIFICATE-----\n"
                .to_string(),
            serial_number: "01:23:45:67".to_string(),
            session_id: "00000000-0000-0000-0000-000000000001".to_string(),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        }
    }

    #[tokio::test]
    async fn kubernetes_credential_succeeds_without_a_grant() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "system:serviceaccount:polar:admin-tool".to_string(),
                    instance_binding: Some("admin-tool-uid".to_string()),
                    normalized_identity: None,
                },
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fake_issue_response()))
            .mount(&server)
            .await;

        let handler = IssuanceHandler {
            ledger: spawn_test_ledger().await,
            cert_issuer: HandshakeClient::new(server.uri()),
            introspection: None,
            known_kubernetes_issuers: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(IssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token("https://kubernetes.default.svc.cluster.local"),
            })
            .await;

        assert!(matches!(reply, IssuanceReply::Issued { .. }));
    }

    #[tokio::test]
    async fn idp_credential_with_grant_succeeds_and_sends_permissions_as_extra_sans() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "testuser".to_string(),
                    instance_binding: Some("session-1".to_string()),
                    normalized_identity: None,
                },
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .and(body_partial_json(serde_json::json!({
                "extra_sans": ["read", "write"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(fake_issue_response()))
            .mount(&server)
            .await;

        let ledger = spawn_test_ledger().await;
        // Seed a grant before the request arrives.
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: CredentialsEvent::Grant {
                    target_identity: "testuser".to_string(),
                    permissions: vec!["read".to_string(), "write".to_string()],
                    granted_by: "admin".to_string(),
                    timestamp: 1000,
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let handler = IssuanceHandler {
            ledger,
            cert_issuer: HandshakeClient::new(server.uri()),
            introspection: Some(IntrospectionClient::new(
                format!("{}/introspect", server.uri()),
                "test-client".to_string(),
                "test-secret".to_string(),
            )),
            known_kubernetes_issuers: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(IssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token("http://keycloak-svc.polar-auth.svc.cluster.local:8080/realms/polar"),
            })
            .await;

        assert!(matches!(reply, IssuanceReply::Issued { .. }));
    }

    #[tokio::test]
    async fn idp_credential_without_grant_is_refused() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": true
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "nobody-granted-this-user".to_string(),
                    instance_binding: Some("session-1".to_string()),
                    normalized_identity: None,
                },
            ))
            .mount(&server)
            .await;

        let handler = IssuanceHandler {
            ledger: spawn_test_ledger().await,
            cert_issuer: HandshakeClient::new(server.uri()),
            introspection: Some(IntrospectionClient::new(
                format!("{}/introspect", server.uri()),
                "test-client".to_string(),
                "test-secret".to_string(),
            )),
            known_kubernetes_issuers: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(IssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token("http://keycloak-svc.polar-auth.svc.cluster.local:8080/realms/polar"),
            })
            .await;

        assert!(matches!(
            reply,
            IssuanceReply::Refused {
                reason: IssuanceRefusalReason::NoGrantFound
            }
        ));
    }

    #[tokio::test]
    async fn already_valid_certificate_is_refused_and_issue_is_never_called() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "system:serviceaccount:polar:admin-tool".to_string(),
                    instance_binding: Some("admin-tool-uid".to_string()),
                    normalized_identity: None,
                },
            ))
            .mount(&server)
            .await;
        // /issue is deliberately NOT mocked -- if it's ever called, the
        // request fails with no matching mock, which the test explicitly
        // checks for below rather than just letting it error silently.
        let issue_mock = Mock::given(method("POST")).and(path("/issue"));
        issue_mock
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let ledger = spawn_test_ledger().await;
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: CredentialsEvent::IssuanceCompleted {
                    identity: "system:serviceaccount:polar:admin-tool".to_string(),
                    instance_binding: "admin-tool-uid".to_string(),
                    serial: "already-issued-serial".to_string(),
                    timestamp: 1000,
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let handler = IssuanceHandler {
            ledger,
            cert_issuer: HandshakeClient::new(server.uri()),
            introspection: None,
            known_kubernetes_issuers: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(IssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token("https://kubernetes.default.svc.cluster.local"),
            })
            .await;

        assert!(matches!(
            reply,
            IssuanceReply::Refused {
                reason: IssuanceRefusalReason::AlreadyHasValidCertificate
            }
        ));
        server.verify().await;
    }

    #[tokio::test]
    async fn inactive_token_is_refused_before_ever_reaching_cert_issuer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/introspect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active": false
            })))
            .mount(&server)
            .await;
        let validate_mock = Mock::given(method("POST")).and(path("/validate"));
        validate_mock
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let handler = IssuanceHandler {
            ledger: spawn_test_ledger().await,
            cert_issuer: HandshakeClient::new(server.uri()),
            introspection: Some(IntrospectionClient::new(
                format!("{}/introspect", server.uri()),
                "test-client".to_string(),
                "test-secret".to_string(),
            )),
            known_kubernetes_issuers: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(IssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token("http://keycloak-svc.polar-auth.svc.cluster.local:8080/realms/polar"),
            })
            .await;

        assert!(matches!(
            reply,
            IssuanceReply::Refused {
                reason: IssuanceRefusalReason::TokenInvalid
            }
        ));
        server.verify().await;
    }

    #[tokio::test]
    async fn missing_introspection_client_refuses_idp_credential() {
        let handler = IssuanceHandler {
            ledger: spawn_test_ledger().await,
            cert_issuer: HandshakeClient::new("http://127.0.0.1:1".to_string()),
            introspection: None,
            known_kubernetes_issuers: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(IssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token("http://keycloak-svc.polar-auth.svc.cluster.local:8080/realms/polar"),
            })
            .await;

        assert!(matches!(
            reply,
            IssuanceReply::Refused {
                reason: IssuanceRefusalReason::TokenIntrospectionFailed
            }
        ));
    }
}
