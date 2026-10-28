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

//! The third issuance lifecycle -- automated agent escrow-identity
//! issuance. Deliberately its own handler, not a reuse of
//! IssuanceHandler: a service's escrow identity is durable, one per
//! identity, not one per pod instance.
//!
//! Asked for by the workload's custodian, never by the workload, and
//! with no token (docs/architecture/key-custody.md, "Part A's
//! bootstrap: the custodian"): the custodian's certificate is what its
//! pod's token bought, and it names the escrow key. The escrow key is
//! made in the custody daemon; its CSR names the workload, and the
//! certificate is signed on `/internal/issue` for the workload.

use crate::custodian::{CustodianRefusal, custodian};
use crate::escrow::EscrowKeypair;
use crate::ledger::LedgerMessage;
use cert_client::handshake::HandshakeClient;
use cert_issuer_common::CertType;
use cert_issuer_common::custody::CustodyPermission;
use cert_issuer_common::permission::{Permission, PermissionScope};
use credentials_agent_common::agent_escrow_issuance::{
    AgentEscrowIssuanceRefusalReason, AgentEscrowIssuanceReply, AgentEscrowIssuanceRequest,
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
/// still overrides this when one exists (see Step 3, below); this is
/// only ever the fallback, not a ceiling on what's expressible.
fn default_agent_escrow_permissions() -> Vec<Permission> {
    vec![Permission {
        scope: PermissionScope::ServiceToService,
        target: None,
    }]
}

pub struct AgentEscrowIssuanceHandler {
    pub ledger: ActorRef<LedgerMessage>,
    /// cert-issuer, reached with this agent's own certificate, for `/internal/issue`.
    pub cert_issuer: HandshakeClient,
    pub escrow_keypair: Arc<EscrowKeypair>,
}

impl AgentEscrowIssuanceHandler {
    pub async fn handle(
        &self,
        request: AgentEscrowIssuanceRequest,
        requested_by: Option<String>,
    ) -> AgentEscrowIssuanceReply {
        let refused = |reason| AgentEscrowIssuanceReply::Refused { reason };
        // ---- Step 1: who asks: the workload's custodian, live, in its window, its certificate naming the key ----
        let custodian = match custodian(
            &self.ledger,
            requested_by.as_deref(),
            CustodyPermission::Escrow,
        )
        .await
        {
            Ok(custodian) => custodian,
            Err(CustodianRefusal::WindowClosed) => {
                return refused(AgentEscrowIssuanceRefusalReason::CustodianWindowClosed);
            }
            Err(CustodianRefusal::NotACustodian(why)) => {
                warn!(?requested_by, "agent escrow issuance refused: {why}");
                return refused(AgentEscrowIssuanceRefusalReason::NotACustodian);
            }
            Err(CustodianRefusal::Unavailable(e)) => {
                error!("the ledger could not be asked about a custodian: {e}");
                return refused(AgentEscrowIssuanceRefusalReason::NotACustodian);
            }
        };
        let identity = custodian.workload;

        // ---- Step 2: renewal-prevention check, identity-scoped ----
        //
        // Not the pod-instance renewal rule: a service's escrow identity
        // isn't pod-scoped at all. See AgentEscrowCertificateRecord's own
        // doc comment (state.rs) for why a second, competing service
        // certificate for the same identity would break a real invariant
        // the certificate-lookup query relies on.
        match call_check_agent_escrow_renewal(&self.ledger, &identity).await {
            Ok(false) => {}
            Ok(true) => {
                return refused(
                    AgentEscrowIssuanceRefusalReason::AlreadyHasValidServiceCertificate,
                );
            }
            Err(e) => {
                error!("Ledger CheckAgentEscrowRenewal call failed: {e}");
                return refused(AgentEscrowIssuanceRefusalReason::CredentialInvalid);
            }
        }

        // ---- Step 3: grant lookup, defaulting to a uniform set ----
        //
        // A grant is written under the workload's token subject, which the
        // custodian's path has no token to read: it is the inverse of the
        // workload identity's normalization.
        let grant = match cert_issuer_common::identity::kubernetes_subject_of(&identity) {
            Some(subject) => match call_check_grant(&self.ledger, &subject).await {
                Ok(grant) => grant,
                Err(e) => {
                    error!("Ledger CheckGrant call failed: {e}");
                    return refused(AgentEscrowIssuanceRefusalReason::CredentialInvalid);
                }
            },
            None => None,
        };
        let permissions = grant
            .map(|g| g.permissions)
            .unwrap_or_else(default_agent_escrow_permissions);

        // ---- Step 4: sign, via cert-issuer's /internal/issue, for the workload ----
        //
        // CertType::Service: a service's own durable encryption identity,
        // never a TLS role, and not one that ends with the custodian: it
        // outlives every pod.
        let issued = match self
            .cert_issuer
            .issue_internal(cert_issuer_common::InternalIssueRequest {
                csr_pem: request.csr_pem,
                identity: identity.clone(),
                cert_type: CertType::Service,
                extra_sans: permissions.iter().map(|p| p.to_san_string()).collect(),
                subject_dn_defaults: None,
                account_subject: None,
                instance_binding: Some(custodian.instance_binding.clone()),
                expires_by: None,
            })
            .await
        {
            Ok(r) => r,
            Err(e) => {
                error!("cert-issuer /internal/issue failed after all prior checks passed: {e}");
                return refused(AgentEscrowIssuanceRefusalReason::SigningFailed);
            }
        };

        // ---- Step 5: record the issuance ----
        //
        // AgentEscrowIssuanceCompleted, not IssuanceCompleted -- a
        // genuinely different event for a genuinely different kind of
        // certificate. Already signed, so a logging failure here is an
        // audit-trail gap, not a reason to fail the caller's valid
        // certificate.
        let event = CredentialsEvent::AgentEscrowIssuanceCompleted {
            identity,
            instance_binding: custodian.instance_binding,
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

async fn call_check_agent_escrow_renewal(
    ledger: &ActorRef<LedgerMessage>,
    identity: &str,
) -> Result<bool, String> {
    match ractor::rpc::call(
        ledger,
        |reply| LedgerMessage::CheckAgentEscrowRenewal {
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
mod tests {
    use super::*;
    use crate::custodian::tests::{CUSTODIAN, POD, X, ledger_with_custodian};
    use credentials_agent_common::custodian::CUSTODIAN_WINDOW_SECS;
    use credentials_agent_common::events::GrantReason;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fake_issue_response() -> cert_issuer_common::IssueResponse {
        cert_issuer_common::IssueResponse {
            certificate_pem:
                "-----BEGIN CERTIFICATE-----\nfake-service\n-----END CERTIFICATE-----\n".to_string(),
            ca_chain_pem: "-----BEGIN CERTIFICATE-----\nfake-ca\n-----END CERTIFICATE-----\n"
                .to_string(),
            serial_number: "service-serial".to_string(),
            session_id: "00000000-0000-0000-0000-000000000001".to_string(),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::days(365),
        }
    }

    /// cert-issuer, signing a service certificate for the workload with `extra_sans`, and nothing else.
    async fn cert_issuer_signing(extra_sans: &[&str]) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/issue"))
            .and(body_partial_json(serde_json::json!({
                "identity": X,
                "cert_type": "SERVICE",
                "instance_binding": POD,
                "extra_sans": extra_sans,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(fake_issue_response()))
            .mount(&server)
            .await;
        server
    }

    fn handler(ledger: ActorRef<LedgerMessage>, server: &MockServer) -> AgentEscrowIssuanceHandler {
        AgentEscrowIssuanceHandler {
            ledger,
            cert_issuer: HandshakeClient::new(server.uri()),
            escrow_keypair: crate::escrow::tests::test_keypair(),
        }
    }

    fn request() -> AgentEscrowIssuanceRequest {
        AgentEscrowIssuanceRequest {
            csr_pem: "fake-csr".to_string(),
        }
    }

    fn reason(reply: &AgentEscrowIssuanceReply) -> Option<AgentEscrowIssuanceRefusalReason> {
        match reply {
            AgentEscrowIssuanceReply::Refused { reason } => Some(reason.clone()),
            _ => None,
        }
    }

    #[tokio::test]
    async fn a_custodian_is_issued_its_workload_s_service_certificate_recorded_under_the_workload()
    {
        let ledger = ledger_with_custodian(&[CustodyPermission::Escrow], 0).await;
        let server = cert_issuer_signing(&["p:s2s"]).await;
        let reply = handler(ledger.clone(), &server)
            .handle(request(), Some(CUSTODIAN.to_string()))
            .await;
        assert!(
            matches!(reply, AgentEscrowIssuanceReply::Issued { .. }),
            "{reply:?}"
        );
        assert!(
            call_check_agent_escrow_renewal(&ledger, X).await.unwrap(),
            "the service certificate is not found by the workload's identity"
        );
        assert!(
            !call_check_agent_escrow_renewal(&ledger, CUSTODIAN)
                .await
                .unwrap(),
            "the custodian's identity must not be what it is recorded under"
        );
    }

    #[tokio::test]
    async fn no_one_but_the_workload_s_live_custodian_in_its_window_naming_the_key_is_answered() {
        for (requester, permissions, age, expected) in [
            (
                Some(X),
                vec![CustodyPermission::Escrow],
                0,
                AgentEscrowIssuanceRefusalReason::NotACustodian,
            ),
            (
                None,
                vec![CustodyPermission::Escrow],
                0,
                AgentEscrowIssuanceRefusalReason::NotACustodian,
            ),
            (
                Some(CUSTODIAN),
                vec![CustodyPermission::Client],
                0,
                AgentEscrowIssuanceRefusalReason::NotACustodian,
            ),
            (
                Some(CUSTODIAN),
                vec![CustodyPermission::Escrow],
                (CUSTODIAN_WINDOW_SECS + 1) * 1000,
                AgentEscrowIssuanceRefusalReason::CustodianWindowClosed,
            ),
        ] {
            let ledger = ledger_with_custodian(&permissions, age).await;
            let server = cert_issuer_signing(&["p:s2s"]).await;
            let reply = handler(ledger, &server)
                .handle(request(), requester.map(str::to_string))
                .await;
            assert_eq!(
                reason(&reply),
                Some(expected),
                "{requester:?} {permissions:?} {age}"
            );
            assert!(server.received_requests().await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn already_has_a_service_certificate_is_refused_and_cert_issuer_is_never_asked() {
        let ledger = ledger_with_custodian(&[CustodyPermission::Escrow], 0).await;
        // A prior service certificate for the same identity but another pod: the check is identity-scoped.
        call_append(
            &ledger,
            CredentialsEvent::AgentEscrowIssuanceCompleted {
                identity: X.to_string(),
                instance_binding: "pod-uid-0".to_string(),
                serial: "already-issued-service-serial".to_string(),
                timestamp: 1000,
                certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                    .to_string(),
            },
        )
        .await
        .unwrap();
        let server = cert_issuer_signing(&["p:s2s"]).await;
        let reply = handler(ledger, &server)
            .handle(request(), Some(CUSTODIAN.to_string()))
            .await;
        assert_eq!(
            reason(&reply),
            Some(AgentEscrowIssuanceRefusalReason::AlreadyHasValidServiceCertificate)
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_existing_grant_overrides_the_default_permission_set() {
        let ledger = ledger_with_custodian(&[CustodyPermission::Escrow], 0).await;
        call_append(
            &ledger,
            CredentialsEvent::Grant {
                target_identity: "system:serviceaccount:polar:kube-consumer".to_string(),
                permissions: vec![Permission {
                    scope: PermissionScope::ServiceToService,
                    target: Some("cassini.cluster.local".to_string()),
                }],
                granted_by: "admin".to_string(),
                timestamp: 1000,
                reason: GrantReason::ServiceOnboarding,
            },
        )
        .await
        .unwrap();
        let server = cert_issuer_signing(&["p:s2s:cassini.cluster.local"]).await;
        let reply = handler(ledger, &server)
            .handle(request(), Some(CUSTODIAN.to_string()))
            .await;
        assert!(
            matches!(reply, AgentEscrowIssuanceReply::Issued { .. }),
            "{reply:?}"
        );
    }
}
