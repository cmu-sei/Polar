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

//! The third issuance lifecycle -- automated agent escrow-identity
//! issuance. Deliberately its own handler, not a reuse of
//! IssuanceHandler, mirroring that handler's own overall shape
//! (credential classification through cert-issuer's two endpoints,
//! the ledger's own checks, reply construction) but with every step
//! adapted to this flow's genuinely different semantics -- see each
//! step's own comment for exactly what differs and why.

use crate::credential::{CredentialKind, classify_credential};
use crate::escrow::EscrowKeypair;
use crate::ledger::LedgerMessage;
use cert_client::handshake::HandshakeClient;
use cert_issuer_common::CertType;
use cert_issuer_common::permission::{Permission, PermissionScope};
use credentials_agent_common::agent_escrow_issuance::{
    AgentEscrowIssuanceReply, AgentEscrowIssuanceRefusalReason, AgentEscrowIssuanceRequest,
};
use credentials_agent_common::events::CredentialsEvent;
use ractor::ActorRef;
use std::sync::Arc;
use tracing::{error, warn};

/// Applied when no explicit `Grant` exists for the requesting
/// identity -- "uniform across all agents today," per
/// credentials-agent-architecture.md's own "Automated agent
/// escrow-identity issuance" section. The broadest, simplest possible
/// default: unrestricted service-to-service communication, no
/// specific target -- matching admin bootstrap's own "nothing more
/// privileged than any other agent gets" reasoning. A real grant
/// still overrides this when one exists (see Step 5, below); this is
/// only ever the fallback, not a ceiling on what's expressible.
fn default_agent_escrow_permissions() -> Vec<Permission> {
    vec![Permission {
        scope: PermissionScope::ServiceToService,
        target: None,
    }]
}

pub struct AgentEscrowIssuanceHandler {
    pub ledger: ActorRef<LedgerMessage>,
    pub cert_issuer: HandshakeClient,
    pub known_kubernetes_issuers: Vec<String>,
    pub escrow_keypair: Arc<EscrowKeypair>,
}

impl AgentEscrowIssuanceHandler {
    pub async fn handle(&self, request: AgentEscrowIssuanceRequest) -> AgentEscrowIssuanceReply {
        // ---- Step 1: classify the credential ----
        // Same mechanism as IssuanceHandler's own Step 1, but this
        // flow only ever accepts a genuine Kubernetes credential --
        // an agent's own service identity, never a human's IdP token.
        // Anything else is a real anomaly worth a clear, explicit
        // refusal, not a silent misroute or a fallback attempt to
        // treat it as one anyway.
        let kind = match classify_credential(&request.credential, &self.known_kubernetes_issuers)
        {
            Ok(k) => k,
            Err(e) => {
                warn!("Credential classification failed: {e}");
                return AgentEscrowIssuanceReply::Refused {
                    reason: AgentEscrowIssuanceRefusalReason::CredentialInvalid,
                };
            }
        };
        if kind != CredentialKind::Kubernetes {
            warn!(
                "Agent escrow-identity issuance request arrived with a non-Kubernetes credential -- refusing"
            );
            return AgentEscrowIssuanceReply::Refused {
                reason: AgentEscrowIssuanceRefusalReason::NotAKubernetesCredential,
            };
        }

        // No introspection step here at all -- a Kubernetes
        // credential never goes through it, matching how
        // IssuanceHandler's own Kubernetes-credential path already
        // skips it entirely (only LikelyIdp calls introspection
        // there).

        // ---- Step 2: real validation via cert-issuer's /validate ----
        // Identical mechanism to IssuanceHandler's own Step 3.
        let validated = match self
            .cert_issuer
            .validate(&request.credential, cert_issuer_common::ValidateRequest::default())
            .await
        {
            Ok(v) => v,
            Err(e) => {
                warn!("cert-issuer /validate rejected the credential: {e}");
                return AgentEscrowIssuanceReply::Refused {
                    reason: AgentEscrowIssuanceRefusalReason::CredentialInvalid,
                };
            }
        };

        let identity = validated.workload_identity;
        // Still extracted and still recorded on the resulting event
        // (see Step 6, below), for the same audit-visibility reason
        // IssuanceCompleted records it -- but, unlike IssuanceHandler's
        // own Step 3, a missing instance_binding doesn't fail this
        // request closed: the renewal-prevention check this flow uses
        // (Step 3, below) is identity-scoped, not
        // (identity, instance_binding)-scoped, so it doesn't actually
        // depend on this claim being present the way the ordinary
        // mTLS issuance flow's own check does.
        let instance_binding = validated.instance_binding.unwrap_or_default();

        // ---- Step 3: renewal-prevention check, identity-scoped ----
        // Deliberately not IssuanceHandler's own CheckRenewal (which
        // is keyed on (identity, instance_binding), a pod-instance
        // concept) -- a service's escrow identity isn't pod-scoped at
        // all. See AgentEscrowCertificateRecord's own doc comment
        // (state.rs) for why a second, competing service certificate
        // for the same identity would break a real invariant the
        // certificate-lookup query relies on.
        let already_has_one = match call_check_agent_escrow_certificate(&self.ledger, &identity).await
        {
            Ok(v) => v,
            Err(e) => {
                error!("Ledger CheckAgentEscrowCertificate call failed: {e}");
                return AgentEscrowIssuanceReply::Refused {
                    reason: AgentEscrowIssuanceRefusalReason::CredentialInvalid,
                };
            }
        };
        if already_has_one {
            return AgentEscrowIssuanceReply::Refused {
                reason: AgentEscrowIssuanceRefusalReason::AlreadyHasValidServiceCertificate,
            };
        }

        // ---- Step 4: grant lookup, defaulting to a uniform set ----
        // Same real infrastructure IssuanceHandler's own Kubernetes-
        // credential path already uses (grant.map(|g| g.permissions)
        // .unwrap_or_default()) -- "uniform for now" means this real
        // mechanism exists and currently falls back to a default when
        // no grant is found, not that the mechanism was skipped
        // entirely. An admin who has explicitly granted specific
        // permissions to this identity ahead of time still overrides
        // the default.
        let grant = match call_check_grant(&self.ledger, &identity).await {
            Ok(g) => g,
            Err(e) => {
                error!("Ledger CheckGrant call failed: {e}");
                return AgentEscrowIssuanceReply::Refused {
                    reason: AgentEscrowIssuanceRefusalReason::CredentialInvalid,
                };
            }
        };
        let permissions = grant
            .map(|g| g.permissions)
            .unwrap_or_else(default_agent_escrow_permissions);

        // ---- Step 5: sign, via cert-issuer's /issue ----
        // CertType::Service, not CertType::Client -- this is a
        // service's own durable encryption identity, never a TLS
        // handshake role. See cert-issuer-common's own CertType::Service
        // doc comment for the full X.509 extension reasoning.
        let extra_sans: Vec<String> = permissions.iter().map(|p| p.to_san_string()).collect();
        let issued = match self
            .cert_issuer
            .issue(&request.credential, &request.csr_pem, CertType::Service, extra_sans)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                error!("cert-issuer /issue failed after all prior checks passed: {e}");
                return AgentEscrowIssuanceReply::Refused {
                    reason: AgentEscrowIssuanceRefusalReason::SigningFailed,
                };
            }
        };

        // ---- Step 6: record the issuance ----
        // AgentEscrowIssuanceCompleted, not IssuanceCompleted -- a
        // genuinely different event for a genuinely different kind of
        // certificate. Same "already signed, so a logging failure
        // here is an audit-trail gap, not a reason to fail the
        // caller's already-valid certificate" reasoning as
        // IssuanceHandler's own Step 7.
        let event = CredentialsEvent::AgentEscrowIssuanceCompleted {
            identity,
            instance_binding,
            serial: issued.serial_number,
            timestamp: polar::time::now_millis(),
            certificate_pem: issued.certificate_pem.clone(),
        };
        if let Err(e) = call_append(&self.ledger, event).await {
            error!(
                "Failed to append AgentEscrowIssuanceCompleted to the ledger for an \
                 already-issued certificate -- genuine audit-trail gap, needs operator \
                 attention: {e}"
            );
        }

        AgentEscrowIssuanceReply::Issued {
            certificate_pem: issued.certificate_pem,
            ca_chain_pem: issued.ca_chain_pem,
            escrow_public_key_pem: self.escrow_keypair.public_key_pem().to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Ledger call helpers -- mirror issuance.rs's own, private helpers of the
// same shape exactly; not reused directly since those are module-private
// to that file.
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

async fn call_check_agent_escrow_certificate(
    ledger: &ActorRef<LedgerMessage>,
    identity: &str,
) -> Result<bool, String> {
    match ractor::rpc::call(
        ledger,
        |reply| LedgerMessage::CheckAgentEscrowCertificate {
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
    use credentials_agent_common::events::GrantReason;
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
            certificate_pem: "-----BEGIN CERTIFICATE-----\nfake-service\n-----END CERTIFICATE-----\n"
                .to_string(),
            ca_chain_pem: "-----BEGIN CERTIFICATE-----\nfake-ca\n-----END CERTIFICATE-----\n"
                .to_string(),
            serial_number: "service-01:23:45".to_string(),
            session_id: "00000000-0000-0000-0000-000000000002".to_string(),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        }
    }

    #[tokio::test]
    async fn kubernetes_credential_succeeds_and_issues_a_service_certificate() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "system:serviceaccount:polar:git-observer".to_string(),
                    instance_binding: Some("pod-uid-1".to_string()),
                    normalized_identity: None,
                },
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .and(body_partial_json(serde_json::json!({
                "cert_type": "SERVICE"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(fake_issue_response()))
            .mount(&server)
            .await;

        let handler = AgentEscrowIssuanceHandler {
            ledger: spawn_test_ledger().await,
            cert_issuer: HandshakeClient::new(server.uri()),
            known_kubernetes_issuers: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(AgentEscrowIssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token("https://kubernetes.default.svc.cluster.local"),
            })
            .await;

        assert!(matches!(reply, AgentEscrowIssuanceReply::Issued { .. }));
    }

    #[tokio::test]
    async fn idp_credential_is_refused_as_not_a_kubernetes_credential() {
        let handler = AgentEscrowIssuanceHandler {
            ledger: spawn_test_ledger().await,
            cert_issuer: HandshakeClient::new("http://127.0.0.1:1".to_string()),
            known_kubernetes_issuers: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(AgentEscrowIssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token("http://keycloak-svc.polar-auth.svc.cluster.local:8080/realms/polar"),
            })
            .await;

        assert!(matches!(
            reply,
            AgentEscrowIssuanceReply::Refused {
                reason: AgentEscrowIssuanceRefusalReason::NotAKubernetesCredential
            }
        ));
    }

    #[tokio::test]
    async fn already_has_a_service_certificate_is_refused_and_issue_is_never_called() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "system:serviceaccount:polar:git-observer".to_string(),
                    instance_binding: Some("pod-uid-2".to_string()),
                    normalized_identity: None,
                },
            ))
            .mount(&server)
            .await;
        // /issue is deliberately NOT mocked -- if it's ever called, the
        // request fails with no matching mock, which this test
        // explicitly checks for below.
        let issue_mock = Mock::given(method("POST")).and(path("/issue"));
        issue_mock
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let ledger = spawn_test_ledger().await;
        // Seed a prior service certificate for the SAME identity but a
        // DIFFERENT instance_binding -- proving the check is genuinely
        // identity-scoped, not pod-instance-scoped the way ordinary
        // mTLS renewal-prevention is.
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: CredentialsEvent::AgentEscrowIssuanceCompleted {
                    identity: "system:serviceaccount:polar:git-observer".to_string(),
                    instance_binding: "pod-uid-1".to_string(),
                    serial: "already-issued-service-serial".to_string(),
                    timestamp: 1000,
                    certificate_pem:
                        "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n".to_string(),
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let handler = AgentEscrowIssuanceHandler {
            ledger,
            cert_issuer: HandshakeClient::new(server.uri()),
            known_kubernetes_issuers: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(AgentEscrowIssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token("https://kubernetes.default.svc.cluster.local"),
            })
            .await;

        assert!(matches!(
            reply,
            AgentEscrowIssuanceReply::Refused {
                reason: AgentEscrowIssuanceRefusalReason::AlreadyHasValidServiceCertificate
            }
        ));
        server.verify().await;
    }

    #[tokio::test]
    async fn no_grant_found_falls_back_to_the_uniform_default_permission_set() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "system:serviceaccount:polar:nobody-granted-this".to_string(),
                    instance_binding: Some("pod-uid-1".to_string()),
                    normalized_identity: None,
                },
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .and(body_partial_json(serde_json::json!({
                "extra_sans": ["p:s2s"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(fake_issue_response()))
            .mount(&server)
            .await;

        let handler = AgentEscrowIssuanceHandler {
            ledger: spawn_test_ledger().await,
            cert_issuer: HandshakeClient::new(server.uri()),
            known_kubernetes_issuers: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(AgentEscrowIssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token("https://kubernetes.default.svc.cluster.local"),
            })
            .await;

        assert!(matches!(reply, AgentEscrowIssuanceReply::Issued { .. }));
    }

    #[tokio::test]
    async fn existing_grant_overrides_the_default_permission_set() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "system:serviceaccount:polar:special-agent".to_string(),
                    instance_binding: Some("pod-uid-1".to_string()),
                    normalized_identity: None,
                },
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .and(body_partial_json(serde_json::json!({
                "extra_sans": ["p:s2s:cassini.cluster.local"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(fake_issue_response()))
            .mount(&server)
            .await;

        let ledger = spawn_test_ledger().await;
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: CredentialsEvent::Grant {
                    target_identity: "system:serviceaccount:polar:special-agent".to_string(),
                    permissions: vec![Permission {
                        scope: PermissionScope::ServiceToService,
                        target: Some("cassini.cluster.local".to_string()),
                    }],
                    granted_by: "admin".to_string(),
                    timestamp: 1000,
                    reason: GrantReason::ServiceOnboarding,
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let handler = AgentEscrowIssuanceHandler {
            ledger,
            cert_issuer: HandshakeClient::new(server.uri()),
            known_kubernetes_issuers: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(AgentEscrowIssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token("https://kubernetes.default.svc.cluster.local"),
            })
            .await;

        assert!(matches!(reply, AgentEscrowIssuanceReply::Issued { .. }));
    }
}
