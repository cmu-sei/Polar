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

//! Ties every issuance-flow piece together: credential classification,
//! IdP introspection, cert-issuer's two endpoints, the ledger's grant/
//! renewal checks, and reply construction. Shared by both real callers
//! of this shape -- user self-service issuance and admin bootstrap --
//! per "one mechanism, both flows."

use crate::credential::{CredentialKind, classify_credential};
use crate::escrow::EscrowKeypair;
use crate::introspection::IntrospectionClient;
use crate::ledger::LedgerMessage;
use cert_client::handshake::HandshakeClient;
use cert_issuer_common::CertType;
use credentials_agent_common::events::CredentialsEvent;
use credentials_agent_common::issuance::{
    IssuanceRefusalReason, IssuanceReply, IssuanceRequest, WorkloadCertType,
};
use ractor::ActorRef;
use std::sync::Arc;
use tracing::{error, warn};

/// The most names a server certificate may carry besides its identity.
pub const MAX_SERVER_NAMES: usize = 16;

/// Whether `name` is a plain DNS name a server certificate may carry: letters, digits, dots and hyphens, and
/// not a wildcard, an address, a permission (`p:`) or a capability (`c:`). cert-issuer checks the names against
/// the workload's own allowlist; this is the credentials agent refusing what is not a name at all before asking.
fn is_plain_dns_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !name.starts_with('.')
        && !name.ends_with('.')
        && name.parse::<std::net::IpAddr>().is_err()
}

/// The request asks for something its credential may not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestNotPermitted;

/// Whether the credential may be issued what it asks for, judged on the request alone, before any network call.
///
/// * a server certificate is for a workload, so it needs a Kubernetes credential, and an IdP credential (a person)
///   is refused;
/// * a client request has no names: its only extras are the permissions of a grant, which are added from the
///   ledger and never from the caller;
/// * a server request's names are plain DNS names, not too many.
pub fn check_request_shape(
    kind: CredentialKind,
    cert_type: WorkloadCertType,
    names: &[String],
) -> Result<(), RequestNotPermitted> {
    match cert_type {
        WorkloadCertType::Client => {
            if names.is_empty() {
                Ok(())
            } else {
                Err(RequestNotPermitted)
            }
        }
        WorkloadCertType::Server => {
            if kind != CredentialKind::Kubernetes
                || names.len() > MAX_SERVER_NAMES
                || !names.iter().all(|n| is_plain_dns_name(n))
            {
                Err(RequestNotPermitted)
            } else {
                Ok(())
            }
        }
    }
}

/// The extra names the certificate is signed with: a client certificate carries the permissions of its grant, a
/// server certificate the names it asked for and no permissions.
fn extra_sans_for(
    cert_type: WorkloadCertType,
    names: &[String],
    grant_permissions: &[cert_issuer_common::permission::Permission],
) -> Vec<String> {
    match cert_type {
        WorkloadCertType::Client => grant_permissions
            .iter()
            .map(|p| p.to_san_string())
            .collect(),
        WorkloadCertType::Server => names.to_vec(),
    }
}

fn signing_type(cert_type: WorkloadCertType) -> CertType {
    match cert_type {
        WorkloadCertType::Client => CertType::Client,
        WorkloadCertType::Server => CertType::Server,
    }
}

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
        let kind = match classify_credential(&request.credential, &self.known_kubernetes_issuers) {
            Ok(k) => k,
            Err(e) => {
                warn!("Credential classification failed: {e}");
                return IssuanceReply::Refused {
                    reason: IssuanceRefusalReason::TokenInvalid,
                };
            }
        };

        // ---- Step 1.5: what is being asked for ----
        //
        // Judged on the request alone, so a request this credential may not make is refused before any
        // introspection or call to cert-issuer.
        if check_request_shape(kind, request.cert_type, &request.names).is_err() {
            warn!(
                cert_type = ?request.cert_type,
                names = request.names.len(),
                "refused a request for a certificate this credential may not have"
            );
            return IssuanceReply::Refused {
                reason: IssuanceRefusalReason::RequestNotPermitted,
            };
        }

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
            .validate(
                &request.credential,
                // The certificate's own identity: what the pod will present on every mTLS connection, and so
                // what the retrieve, store and decrypt handlers look the requester up by.
                cert_issuer_common::ValidateRequest {
                    include_normalized_identity: true,
                },
            )
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

        // Two names for one credential. The token's own identity is what a grant is written under (an
        // administrator names the claim); the certificate's is what everything else in the ledger is keyed on.
        // With no normalized identity there is no telling where the certificate would be looked for, so the
        // request is refused and not recorded under the raw name.
        let token_identity = validated.workload_identity;
        let Some(identity) = validated.normalized_identity else {
            warn!(
                "cert-issuer gave no normalized identity for '{token_identity}' -- refusing rather than recording the certificate where nothing looks for it"
            );
            return IssuanceReply::Refused {
                reason: IssuanceRefusalReason::TokenInvalid,
            };
        };
        // Fails closed rather than proceeding with a degraded renewal
        // check: instance_binding is what the pod-cannot-renew-itself
        // property is actually keyed on, so a token/config combination
        // that doesn't provide it can't be safely processed at all.
        let Some(instance_binding) = validated.instance_binding else {
            warn!(
                "Validated token for '{identity}' has no instance_binding -- refusing rather than degrading the renewal check"
            );
            return IssuanceReply::Refused {
                reason: IssuanceRefusalReason::TokenInvalid,
            };
        };

        // ---- Step 4: renewal-prevention check ----
        let already_valid = match call_check_renewal(
            &self.ledger,
            &identity,
            &instance_binding,
            request.cert_type,
        )
        .await
        {
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
        let grant = match call_check_grant(&self.ledger, &token_identity).await {
            Ok(g) => g,
            Err(e) => {
                error!("Ledger CheckGrant call failed: {e}");
                return IssuanceReply::Refused {
                    reason: IssuanceRefusalReason::TokenInvalid,
                };
            }
        };

        // The grant, if there is one, is used up under the token's identity when the event is folded.
        let grant_key = grant.as_ref().map(|_| token_identity.clone());
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
            (CredentialKind::Kubernetes, grant) => grant.map(|g| g.permissions).unwrap_or_default(),
        };

        // ---- Step 6: sign, via cert-issuer's /issue ----
        let extra_sans = extra_sans_for(request.cert_type, &request.names, &permissions);
        let issued = match self
            .cert_issuer
            .issue(
                &request.credential,
                &request.csr_pem,
                signing_type(request.cert_type),
                extra_sans,
                None,
            )
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
            cert_type: request.cert_type,
            grant_key,
            certificate_pem: issued.certificate_pem.clone(),
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
    cert_type: WorkloadCertType,
) -> Result<bool, String> {
    match ractor::rpc::call(
        ledger,
        |reply| LedgerMessage::CheckRenewal {
            identity: identity.to_string(),
            instance_binding: instance_binding.to_string(),
            cert_type,
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

async fn call_append(
    ledger: &ActorRef<LedgerMessage>,
    event: CredentialsEvent,
) -> Result<(), String> {
    match ractor::rpc::call(ledger, |reply| LedgerMessage::Append { event, reply }, None).await {
        Ok(ractor::rpc::CallResult::Success(result)) => result,
        Ok(other) => Err(format!("unexpected CallResult: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ledger::{LedgerActor, LedgerActorArgs};
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use credentials_agent_common::events::GrantReason;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub(crate) fn make_token(iss: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"iss":"{iss}"}}"#));
        format!("{header}.{payload}.")
    }

    pub(crate) async fn spawn_test_ledger() -> ActorRef<LedgerMessage> {
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

    pub(crate) fn fake_issue_response() -> cert_issuer_common::IssueResponse {
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
    async fn a_workloads_certificate_can_be_found_by_the_identity_the_certificate_carries() {
        // The retrieve, store and decrypt handlers find the requester's certificate by the identity of its mTLS
        // connection, which is the certificate's DNS name: the normalized identity, not the token's raw one.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            // cert-issuer returns the normalized identity only when asked for it.
            .and(body_partial_json(serde_json::json!({
                "include_normalized_identity": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "system:serviceaccount:polar:admin-tool".to_string(),
                    instance_binding: Some("admin-tool-uid".to_string()),
                    jti: None,
                    normalized_identity: Some(
                        "admin-tool.polar.serviceaccount.cluster.local".to_string(),
                    ),
                },
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fake_issue_response()))
            .mount(&server)
            .await;
        let ledger = spawn_test_ledger().await;
        let handler = IssuanceHandler {
            ledger: ledger.clone(),
            cert_issuer: HandshakeClient::new(server.uri()),
            introspection: None,
            known_kubernetes_issuers: vec![
                "https://kubernetes.default.svc.cluster.local".to_string(),
            ],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };
        let reply = handler
            .handle(IssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token("https://kubernetes.default.svc.cluster.local"),
                cert_type: Default::default(),
                names: vec![],
            })
            .await;
        assert!(matches!(reply, IssuanceReply::Issued { .. }));

        let found = crate::retrieve_secret::call_check_certificate(
            &ledger,
            "admin-tool.polar.serviceaccount.cluster.local",
        )
        .await
        .unwrap();
        assert!(
            found.is_some(),
            "the certificate the pod was issued is not found by the identity it presents"
        );
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
                    jti: None,
                    normalized_identity: Some(
                        "admin-tool.polar.serviceaccount.cluster.local".to_string(),
                    ),
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
            known_kubernetes_issuers: vec![
                "https://kubernetes.default.svc.cluster.local".to_string(),
            ],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(IssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token("https://kubernetes.default.svc.cluster.local"),
                cert_type: Default::default(),
                names: vec![],
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
                    jti: None,
                    normalized_identity: Some("testuser.polar-auth".to_string()),
                },
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .and(body_partial_json(serde_json::json!({
                "extra_sans": ["p:s2s", "p:u2s"]
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
                    permissions: vec![
                        cert_issuer_common::permission::Permission {
                            scope:
                                cert_issuer_common::permission::PermissionScope::ServiceToService,
                            target: None,
                        },
                        cert_issuer_common::permission::Permission {
                            scope: cert_issuer_common::permission::PermissionScope::UserToService,
                            target: None,
                        },
                    ],
                    granted_by: "admin".to_string(),
                    timestamp: 1000,
                    reason: GrantReason::UserOnboarding,
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
            known_kubernetes_issuers: vec![
                "https://kubernetes.default.svc.cluster.local".to_string(),
            ],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(IssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token(
                    "http://keycloak-svc.polar-auth.svc.cluster.local:8080/realms/polar",
                ),
                cert_type: Default::default(),
                names: vec![],
            })
            .await;

        assert!(matches!(reply, IssuanceReply::Issued { .. }));

        // The grant was written from the identity provider's claim (`testuser`); the certificate carries
        // the normalized identity. The grant is used up under the first and the certificate found under
        // the second, and the claim is not a certificate identity at all.
        assert!(
            call_check_grant(&handler.ledger, "testuser")
                .await
                .unwrap()
                .is_none(),
            "the grant was not used up"
        );
        assert!(
            crate::retrieve_secret::call_check_certificate(&handler.ledger, "testuser.polar-auth")
                .await
                .unwrap()
                .is_some(),
            "the certificate is not found by the identity it carries"
        );
        assert!(
            crate::retrieve_secret::call_check_certificate(&handler.ledger, "testuser")
                .await
                .unwrap()
                .is_none(),
            "the raw claim must not find a certificate"
        );
    }

    #[tokio::test]
    async fn a_credential_the_issuer_gave_no_normalized_identity_for_is_refused() {
        // Fails closed: recording the raw identity would put the certificate where nothing looks for it.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "system:serviceaccount:polar:admin-tool".to_string(),
                    instance_binding: Some("admin-tool-uid".to_string()),
                    jti: None,
                    normalized_identity: None,
                },
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let handler = handler_for(&server, spawn_test_ledger().await).await;
        let reply = handler.handle(request_for(Client, &[])).await;
        assert!(
            matches!(
                reply,
                IssuanceReply::Refused {
                    reason: IssuanceRefusalReason::TokenInvalid
                }
            ),
            "{reply:?}"
        );
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
                    jti: None,
                    normalized_identity: Some("nobody-granted-this-user.polar-auth".to_string()),
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
            known_kubernetes_issuers: vec![
                "https://kubernetes.default.svc.cluster.local".to_string(),
            ],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(IssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token(
                    "http://keycloak-svc.polar-auth.svc.cluster.local:8080/realms/polar",
                ),
                cert_type: Default::default(),
                names: vec![],
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
                    jti: None,
                    normalized_identity: Some(
                        "admin-tool.polar.serviceaccount.cluster.local".to_string(),
                    ),
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
        let _ =
            ractor::rpc::call(
                &ledger,
                |reply| LedgerMessage::Append {
                    event: CredentialsEvent::IssuanceCompleted {
                        identity: "admin-tool.polar.serviceaccount.cluster.local".to_string(),
                        instance_binding: "admin-tool-uid".to_string(),
                        serial: "already-issued-serial".to_string(),
                        timestamp: 1000,
                        cert_type: credentials_agent_common::issuance::WorkloadCertType::Client,
                        grant_key: None,
                        certificate_pem:
                            "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                                .to_string(),
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
            known_kubernetes_issuers: vec![
                "https://kubernetes.default.svc.cluster.local".to_string(),
            ],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(IssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token("https://kubernetes.default.svc.cluster.local"),
                cert_type: Default::default(),
                names: vec![],
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
            known_kubernetes_issuers: vec![
                "https://kubernetes.default.svc.cluster.local".to_string(),
            ],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(IssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token(
                    "http://keycloak-svc.polar-auth.svc.cluster.local:8080/realms/polar",
                ),
                cert_type: Default::default(),
                names: vec![],
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
            known_kubernetes_issuers: vec![
                "https://kubernetes.default.svc.cluster.local".to_string(),
            ],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let reply = handler
            .handle(IssuanceRequest {
                csr_pem: "fake-csr".to_string(),
                credential: make_token(
                    "http://keycloak-svc.polar-auth.svc.cluster.local:8080/realms/polar",
                ),
                cert_type: Default::default(),
                names: vec![],
            })
            .await;

        assert!(matches!(
            reply,
            IssuanceReply::Refused {
                reason: IssuanceRefusalReason::TokenIntrospectionFailed
            }
        ));
    }

    // ---- the two kinds of pod-instance certificate ---------------------------------------------------------

    use credentials_agent_common::issuance::WorkloadCertType::{Client, Server};

    pub(crate) const K8S: &str = "https://kubernetes.default.svc.cluster.local";
    pub(crate) const CASSINI_NAME: &str = "cassini-ip-svc.polar.svc.cluster.local";

    pub(crate) fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_client_request_has_no_names_of_its_own() {
        assert!(check_request_shape(CredentialKind::Kubernetes, Client, &[]).is_ok());
        assert!(check_request_shape(CredentialKind::LikelyIdp, Client, &[]).is_ok());
        assert!(
            check_request_shape(CredentialKind::Kubernetes, Client, &names(&[CASSINI_NAME]))
                .is_err()
        );
    }

    #[test]
    fn a_server_request_is_for_a_workload_and_asks_for_plain_dns_names() {
        assert!(
            check_request_shape(
                CredentialKind::Kubernetes,
                Server,
                &names(&[CASSINI_NAME, "localhost"])
            )
            .is_ok()
        );
        // A person's credential is not a workload's.
        assert!(
            check_request_shape(CredentialKind::LikelyIdp, Server, &names(&[CASSINI_NAME]))
                .is_err()
        );
        for bad in [
            "",
            "*.polar.svc.cluster.local",
            "10.0.0.1",
            "::1",
            "p:u2s:polar-db-svc",
            "c:provision-account",
            "has space.example",
            "under_score.example",
            ".leading.dot",
            "trailing.dot.",
        ] {
            assert!(
                check_request_shape(CredentialKind::Kubernetes, Server, &names(&[bad])).is_err(),
                "{bad:?}"
            );
        }
        let too_long = "a".repeat(254);
        assert!(
            check_request_shape(CredentialKind::Kubernetes, Server, &names(&[&too_long])).is_err()
        );
        let many: Vec<String> = (0..=MAX_SERVER_NAMES)
            .map(|i| format!("n{i}.example"))
            .collect();
        assert!(check_request_shape(CredentialKind::Kubernetes, Server, &many).is_err());
    }

    #[test]
    fn a_client_certificate_carries_a_grants_permissions_and_a_server_certificate_its_names_and_no_permissions()
     {
        let grant = vec![cert_issuer_common::permission::Permission {
            scope: cert_issuer_common::permission::PermissionScope::ServiceToService,
            target: None,
        }];
        assert_eq!(
            extra_sans_for(Client, &[], &grant),
            vec!["p:s2s".to_string()]
        );
        assert!(extra_sans_for(Client, &[], &[]).is_empty());
        assert_eq!(
            extra_sans_for(Server, &names(&[CASSINI_NAME]), &grant),
            names(&[CASSINI_NAME])
        );
    }

    pub(crate) fn response_with_serial(serial: &str) -> cert_issuer_common::IssueResponse {
        cert_issuer_common::IssueResponse {
            serial_number: serial.to_string(),
            ..fake_issue_response()
        }
    }

    pub(crate) async fn kubernetes_pod(server: &MockServer, instance_binding: &str) {
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "system:serviceaccount:polar:git-server".to_string(),
                    instance_binding: Some(instance_binding.to_string()),
                    jti: None,
                    normalized_identity: Some(
                        "git-server.polar.serviceaccount.cluster.local".to_string(),
                    ),
                },
            ))
            .mount(server)
            .await;
    }

    pub(crate) async fn handler_for(
        server: &MockServer,
        ledger: ActorRef<LedgerMessage>,
    ) -> IssuanceHandler {
        IssuanceHandler {
            ledger,
            cert_issuer: HandshakeClient::new(server.uri()),
            introspection: None,
            known_kubernetes_issuers: vec![K8S.to_string()],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        }
    }

    pub(crate) fn request_for(cert_type: WorkloadCertType, names_: &[&str]) -> IssuanceRequest {
        IssuanceRequest {
            csr_pem: "fake-csr".to_string(),
            credential: make_token(K8S),
            cert_type,
            names: names(names_),
        }
    }

    #[tokio::test]
    async fn a_pod_is_issued_a_client_certificate_and_then_a_server_certificate_and_not_a_second_of_either()
     {
        let server = MockServer::start().await;
        kubernetes_pod(&server, "pod-1").await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .and(body_partial_json(
                serde_json::json!({"cert_type": "CLIENT"}),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(response_with_serial("client-1")),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .and(body_partial_json(serde_json::json!({
                "cert_type": "SERVER",
                "extra_sans": [CASSINI_NAME]
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(response_with_serial("server-1")),
            )
            .mount(&server)
            .await;
        let handler = handler_for(&server, spawn_test_ledger().await).await;

        let client = handler.handle(request_for(Client, &[])).await;
        assert!(matches!(client, IssuanceReply::Issued { .. }), "{client:?}");
        let server_cert = handler.handle(request_for(Server, &[CASSINI_NAME])).await;
        assert!(
            matches!(server_cert, IssuanceReply::Issued { .. }),
            "{server_cert:?}"
        );

        // The pod cannot be issued a second of either: the renewal rule, per type.
        for again in [
            request_for(Client, &[]),
            request_for(Server, &[CASSINI_NAME]),
        ] {
            let reply = handler.handle(again).await;
            assert!(
                matches!(
                    reply,
                    IssuanceReply::Refused {
                        reason: IssuanceRefusalReason::AlreadyHasValidCertificate
                    }
                ),
                "{reply:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_server_certificate_leaves_the_grant_for_the_client_certificate_that_follows() {
        let server = MockServer::start().await;
        kubernetes_pod(&server, "pod-1").await;
        // The server certificate must carry no permission; the client certificate must carry the grant's.
        Mock::given(method("POST"))
            .and(path("/issue"))
            .and(body_partial_json(serde_json::json!({
                "cert_type": "SERVER",
                "extra_sans": [CASSINI_NAME]
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(response_with_serial("server-1")),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .and(body_partial_json(serde_json::json!({
                "cert_type": "CLIENT",
                "extra_sans": ["p:s2s"]
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(response_with_serial("client-1")),
            )
            .mount(&server)
            .await;
        let ledger = spawn_test_ledger().await;
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: CredentialsEvent::Grant {
                    target_identity: "system:serviceaccount:polar:git-server".to_string(),
                    permissions: vec![cert_issuer_common::permission::Permission {
                        scope: cert_issuer_common::permission::PermissionScope::ServiceToService,
                        target: None,
                    }],
                    granted_by: "admin".to_string(),
                    timestamp: 1000,
                    reason: GrantReason::UserOnboarding,
                },
                reply,
            },
            None,
        )
        .await;
        let handler = handler_for(&server, ledger).await;

        let first = handler.handle(request_for(Server, &[CASSINI_NAME])).await;
        assert!(matches!(first, IssuanceReply::Issued { .. }), "{first:?}");
        let second = handler.handle(request_for(Client, &[])).await;
        assert!(matches!(second, IssuanceReply::Issued { .. }), "{second:?}");
    }

    #[tokio::test]
    async fn a_request_for_what_the_credential_may_not_have_is_refused_before_anyone_is_asked() {
        // No mock is mounted: any call to cert-issuer would answer 404 and show up as a different refusal.
        let server = MockServer::start().await;
        let handler = handler_for(&server, spawn_test_ledger().await).await;

        let refused = |reply: IssuanceReply| {
            matches!(
                reply,
                IssuanceReply::Refused {
                    reason: IssuanceRefusalReason::RequestNotPermitted
                }
            )
        };
        // A person's credential asking for a server certificate.
        let idp = IssuanceRequest {
            credential: make_token("https://keycloak.example/realms/polar"),
            ..request_for(Server, &[CASSINI_NAME])
        };
        assert!(refused(handler.handle(idp).await));
        // A client request choosing names.
        assert!(refused(
            handler.handle(request_for(Client, &[CASSINI_NAME])).await
        ));
        // A server request with names that are not names.
        for bad in [
            "*.polar.svc.cluster.local",
            "p:u2s:polar-db-svc",
            "c:grant",
            "10.0.0.1",
        ] {
            assert!(
                refused(handler.handle(request_for(Server, &[bad])).await),
                "{bad}"
            );
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}
