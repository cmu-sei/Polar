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
    /// The identities whose escrow certificate is being signed now: the first escrow issuance of an identity is
    /// serialized, whoever asks and however the requests are interleaved.
    pub in_flight: InFlight,
}

/// The identities an escrow issuance is under way for (`AgentEscrowIssuanceHandler::in_flight`).
pub type InFlight = Arc<std::sync::Mutex<std::collections::HashSet<String>>>;

/// An identity's place in [`InFlight`], taken for one issuance and given back when it is dropped, however it ends.
struct Reserved {
    in_flight: InFlight,
    identity: String,
}

impl Reserved {
    /// The place of `identity`, unless another issuance holds it.
    fn take(in_flight: &InFlight, identity: &str) -> Option<Self> {
        let mut held = in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.insert(identity.to_string()).then(|| Reserved {
            in_flight: in_flight.clone(),
            identity: identity.to_string(),
        })
    }
}

impl Drop for Reserved {
    fn drop(&mut self) {
        self.in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.identity);
    }
}

/// Whether an escrow issuance for one pod of a workload is made, given the workload's current escrow certificate
/// (`existing`, none or revoked being none), the pod asking (`requester`, its UID), the key its CSR asks for (`asked`,
/// none when it does not read), whether the workload's escrow key is stored, and the time (Unix milliseconds). Pure.
///
/// - The first is issued.
/// - The same key asked for again, its reply lost, is issued again (docs/architecture/deployments.md, "A retry is not a
///   second request"); the certificate before is superseded as it is recorded.
/// - Another pod, while the issuing pod's custodian is in its window, is told it is being issued, and asks again and
///   retrieves the key once escrowed (docs/architecture/authority-model.md, "Multi-instance workloads").
/// - Past that window, a certificate whose key was never stored is abandoned: its custodian can no longer store it, so
///   no one ever will. The workload is issued another over it (docs/architecture/key-custody.md, "An escrow certificate
///   asked again, or abandoned").
/// - Anything else is refused, as before: one escrow certificate per identity.
pub(crate) fn first_issuance(
    existing: Option<&crate::state::AgentEscrowCertificateRecord>,
    requester: &str,
    asked: Option<&[u8]>,
    stored: bool,
    now_millis: i64,
) -> Result<(), AgentEscrowIssuanceRefusalReason> {
    use credentials_agent_common::custodian::CUSTODIAN_WINDOW_SECS;
    let Some(record) = existing else {
        return Ok(());
    };
    let asked_again = asked.is_some_and(|asked| {
        cert_issuer_common::public_key::certificate_key(&record.certificate_pem).as_deref()
            == Some(asked)
    });
    let in_window = now_millis.saturating_sub(record.timestamp) <= CUSTODIAN_WINDOW_SECS * 1000;
    if asked_again || (!in_window && !stored) {
        Ok(())
    } else if in_window && record.instance_binding != requester {
        Err(AgentEscrowIssuanceRefusalReason::BeingIssued)
    } else {
        Err(AgentEscrowIssuanceRefusalReason::AlreadyHasValidServiceCertificate)
    }
}

impl AgentEscrowIssuanceHandler {
    /// [`handle_from`](Self::handle_from), from a publisher with no instance (a single-instance workload's).
    pub async fn handle(
        &self,
        request: AgentEscrowIssuanceRequest,
        requested_by: Option<String>,
    ) -> AgentEscrowIssuanceReply {
        self.handle_from(request, requested_by, None).await
    }

    /// An escrow issuance asked by `requested_by`, the pod `instance` of it when the broker stamped one: its custodian.
    pub async fn handle_from(
        &self,
        request: AgentEscrowIssuanceRequest,
        requested_by: Option<String>,
        instance: Option<String>,
    ) -> AgentEscrowIssuanceReply {
        let refused = |reason| AgentEscrowIssuanceReply::Refused { reason };
        // ---- Step 1: who asks: the workload's custodian, live, in its window, its certificate naming the key ----
        let custodian = match custodian(
            &self.ledger,
            requested_by.as_deref(),
            instance.as_deref(),
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

        // ---- Step 2: the first issuance of an identity, serialized; one escrow certificate per identity ----
        //
        // Not the pod-instance renewal rule: a service's escrow identity isn't pod-scoped at all, one key per
        // workload shared by its instances. Its place is held until the certificate is recorded, so a concurrent
        // request, another pod's, is told it is being issued and retrieves the key once escrowed. Issued again for
        // the same key asked again, or over one abandoned with no key stored (`first_issuance`).
        let Some(_reserved) = Reserved::take(&self.in_flight, &identity) else {
            warn!(%identity, pod = %custodian.instance_binding, "an escrow issuance is under way for this workload: told so");
            return refused(AgentEscrowIssuanceRefusalReason::BeingIssued);
        };
        let existing = match call_check_agent_escrow_key(&self.ledger, &identity).await {
            Ok(existing) => existing,
            Err(e) => {
                error!("Ledger CheckAgentEscrowKey call failed: {e}");
                return refused(AgentEscrowIssuanceRefusalReason::CredentialInvalid);
            }
        };
        let stored = match &existing {
            None => false,
            Some(_) => {
                let name = credentials_agent_common::agent_escrow_keypair_bootstrap::agent_escrow_keypair_secret_name(
                    &identity,
                );
                match crate::retrieve_secret::call_check_secret(&self.ledger, &name).await {
                    Ok(stored) => stored.is_some(),
                    Err(e) => {
                        error!("Ledger CheckSecret call failed: {e}");
                        return refused(AgentEscrowIssuanceRefusalReason::CredentialInvalid);
                    }
                }
            }
        };
        let asked = cert_issuer_common::public_key::csr_key(&request.csr_pem);
        if let Err(reason) = first_issuance(
            existing.as_ref(),
            &custodian.instance_binding,
            asked.as_deref(),
            stored,
            polar::time::now_millis(),
        ) {
            return refused(reason);
        }
        if let Some(existing) = &existing {
            warn!(
                %identity,
                pod = %custodian.instance_binding,
                serial = %existing.serial,
                "the workload's escrow certificate is issued again, the one before superseded: asked again for its key, or abandoned with no key stored"
            );
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

async fn call_check_agent_escrow_key(
    ledger: &ActorRef<LedgerMessage>,
    identity: &str,
) -> Result<Option<crate::state::AgentEscrowCertificateRecord>, String> {
    match ractor::rpc::call(
        ledger,
        |reply| LedgerMessage::CheckAgentEscrowKey {
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
            in_flight: Default::default(),
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
            call_check_agent_escrow_key(&ledger, X)
                .await
                .unwrap()
                .is_some(),
            "the service certificate is not found by the workload's identity"
        );
        assert!(
            call_check_agent_escrow_key(&ledger, CUSTODIAN)
                .await
                .unwrap()
                .is_none(),
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
        // A prior service certificate for the same identity but another pod: the check is identity-scoped. Its key is
        // stored, so it is the workload's (one never stored is abandoned past its window, and issued over).
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
        store_the_key(&ledger).await;
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

    // ---- one key per workload, shared by its instances (docs/architecture/authority-model.md, "Multi-instance
    // workloads"): the first escrow issuance of an identity is serialized ----------------------------------------

    /// The ledger, holding a live custodian of X for each pod of `pods`, each naming its pod, its certificate naming the
    /// escrow key.
    async fn ledger_with_custodians_of(pods: &[&str]) -> ActorRef<LedgerMessage> {
        let ledger = crate::issuance::tests::spawn_test_ledger().await;
        for pod in pods {
            call_append(
                &ledger,
                CredentialsEvent::IssuanceCompleted {
                    identity: CUSTODIAN.to_string(),
                    instance_binding: pod.to_string(),
                    serial: format!("custodian-{pod}"),
                    timestamp: polar::time::now_millis(),
                    cert_type: credentials_agent_common::issuance::WorkloadCertType::Custody,
                    grant_key: None,
                    certificate_pem: crate::custodian::tests::custodian_pem_naming(
                        &[CustodyPermission::Escrow],
                        3600,
                        &[&format!("i:uid:{pod}"), &format!("i:pod:{pod}-name")],
                    ),
                },
            )
            .await
            .unwrap();
        }
        ledger
    }

    /// A cert-issuer that signs any service certificate, slowly: long enough for a second request to arrive while the
    /// first is being signed.
    async fn slow_cert_issuer() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/issue"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(fake_issue_response())
                    .set_delay(std::time::Duration::from_millis(400)),
            )
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn concurrent_first_requests_of_two_pods_are_one_issuance_and_the_other_is_told_it_is_being_issued()
     {
        let ledger = ledger_with_custodians_of(&["pod-a", "pod-b"]).await;
        let server = slow_cert_issuer().await;
        let handler = handler(ledger.clone(), &server);
        let ask = |pod: &str| {
            handler.handle_from(
                request(),
                Some(CUSTODIAN.to_string()),
                Some(pod.to_string()),
            )
        };
        let (a, b) = tokio::join!(ask("pod-a"), ask("pod-b"));
        let issued = [&a, &b]
            .iter()
            .filter(|r| matches!(r, AgentEscrowIssuanceReply::Issued { .. }))
            .count();
        assert_eq!(issued, 1, "{a:?} {b:?}");
        assert!(
            [reason(&a), reason(&b)].contains(&Some(AgentEscrowIssuanceRefusalReason::BeingIssued)),
            "{a:?} {b:?}"
        );
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "cert-issuer is asked once"
        );
        // Asked again within the issuing custodian's window: the other pod is told it is being issued (it retrieves the
        // key once escrowed); the pod that was issued it is refused a second, as before.
        let (winner, other) = if matches!(a, AgentEscrowIssuanceReply::Issued { .. }) {
            ("pod-a", "pod-b")
        } else {
            ("pod-b", "pod-a")
        };
        assert_eq!(
            reason(&ask(other).await),
            Some(AgentEscrowIssuanceRefusalReason::BeingIssued)
        );
        assert_eq!(
            reason(&ask(winner).await),
            Some(AgentEscrowIssuanceRefusalReason::AlreadyHasValidServiceCertificate)
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    /// A revoked escrow certificate does not count toward the one per identity: its workload is issued another, its
    /// key stored or not (an administrator's revocation; an orphan never stored needs none, abandoned past its window).
    #[tokio::test]
    async fn a_workload_whose_only_escrow_certificate_was_revoked_is_issued_another() {
        let ledger = ledger_with_custodian(&[CustodyPermission::Escrow], 0).await;
        call_append(
            &ledger,
            CredentialsEvent::AgentEscrowIssuanceCompleted {
                identity: X.to_string(),
                instance_binding: "pod-uid-0".to_string(),
                serial: "old-service".to_string(),
                timestamp: 1000,
                certificate_pem: "-----BEGIN CERTIFICATE-----\nold\n-----END CERTIFICATE-----\n"
                    .to_string(),
            },
        )
        .await
        .unwrap();
        store_the_key(&ledger).await;
        let server = cert_issuer_signing(&["p:s2s"]).await;
        assert_eq!(
            reason(
                &handler(ledger.clone(), &server)
                    .handle(request(), Some(CUSTODIAN.to_string()))
                    .await
            ),
            Some(AgentEscrowIssuanceRefusalReason::AlreadyHasValidServiceCertificate)
        );
        call_append(
            &ledger,
            CredentialsEvent::Revoke {
                serial: "old-service".to_string(),
                reason: credentials_agent_common::events::RevocationReason::IssuedInError,
                revoked_by: "admin".to_string(),
                timestamp: 2000,
            },
        )
        .await
        .unwrap();
        assert!(
            call_check_agent_escrow_key(&ledger, X)
                .await
                .unwrap()
                .is_none(),
            "a revoked escrow certificate is no one's to encrypt to"
        );
        let reply = handler(ledger, &server)
            .handle(request(), Some(CUSTODIAN.to_string()))
            .await;
        assert!(
            matches!(reply, AgentEscrowIssuanceReply::Issued { .. }),
            "{reply:?}"
        );
    }

    /// A real escrow CSR of a key of its own, as the custody daemon makes one.
    fn escrow_csr() -> String {
        cert_issuer_common::keypair::generate_csr(
            X,
            &cert_issuer_common::keypair::KeyAlgorithm::EcdsaP256,
            &[],
        )
        .unwrap()
        .csr_pem
    }

    fn with_csr(csr_pem: &str) -> AgentEscrowIssuanceRequest {
        AgentEscrowIssuanceRequest {
            csr_pem: csr_pem.to_string(),
        }
    }

    /// cert-issuer signing the CSR it is sent, so the escrow certificate is of the key asked for.
    async fn cert_issuer_signing_the_csr() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/issue"))
            .respond_with(crate::issuance::tests::SignsTheCsr::new())
            .mount(&server)
            .await;
        server
    }

    // docs/architecture/key-custody.md, "An escrow certificate asked again, or abandoned": the custodian, its reply
    // overdue, asks again for the same key (docs/architecture/deployments.md, "A retry is not a second request"), and
    // is issued it again; the certificate before is superseded, one current per identity.
    #[tokio::test]
    async fn a_custodian_asking_again_for_the_same_key_is_issued_it_again_and_the_first_superseded()
    {
        let ledger = ledger_with_custodian(&[CustodyPermission::Escrow], 0).await;
        let server = cert_issuer_signing_the_csr().await;
        let handler = handler(ledger.clone(), &server);
        let csr = escrow_csr();
        let ask = || handler.handle(with_csr(&csr), Some(CUSTODIAN.to_string()));
        assert!(matches!(
            ask().await,
            AgentEscrowIssuanceReply::Issued { .. }
        ));
        let first = call_check_agent_escrow_key(&ledger, X)
            .await
            .unwrap()
            .unwrap();
        let again = ask().await;
        assert!(
            matches!(again, AgentEscrowIssuanceReply::Issued { .. }),
            "{again:?}"
        );
        let current = call_check_agent_escrow_key(&ledger, X)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(current.serial, first.serial);
        let revoked = ractor::call!(ledger, LedgerMessage::RevokedSerials).unwrap();
        assert_eq!(revoked, vec![first.serial]);
        // Another key from it is not the same request, as before.
        assert_eq!(
            reason(
                &handler
                    .handle(with_csr(&escrow_csr()), Some(CUSTODIAN.to_string()))
                    .await
            ),
            Some(AgentEscrowIssuanceRefusalReason::AlreadyHasValidServiceCertificate)
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    /// The ledger holding X's escrow certificate, issued to another pod `age_millis` ago, and, when `stored`, the key
    /// it is for escrowed.
    async fn ledger_with_an_escrow_certificate(
        age_millis: i64,
        stored: bool,
    ) -> ActorRef<LedgerMessage> {
        let ledger = ledger_with_custodian(&[CustodyPermission::Escrow], 0).await;
        call_append(
            &ledger,
            CredentialsEvent::AgentEscrowIssuanceCompleted {
                identity: X.to_string(),
                instance_binding: "pod-uid-0".to_string(),
                serial: "abandoned".to_string(),
                timestamp: polar::time::now_millis() - age_millis,
                certificate_pem: "-----BEGIN CERTIFICATE-----\nold\n-----END CERTIFICATE-----\n"
                    .to_string(),
            },
        )
        .await
        .unwrap();
        if stored {
            store_the_key(&ledger).await;
        }
        ledger
    }

    /// X's escrow key, escrowed, as its custodian stores it.
    async fn store_the_key(ledger: &ActorRef<LedgerMessage>) {
        let escrow_keypair = crate::escrow::tests::test_keypair();
        call_append(
            ledger,
            CredentialsEvent::SecretStored {
                name: credentials_agent_common::agent_escrow_keypair_bootstrap::agent_escrow_keypair_secret_name(X),
                secret: credentials_agent_common::escrow_secret::EscrowedSecret::encrypt(
                    b"the key",
                    escrow_keypair.public_key_pem(),
                )
                .unwrap(),
                stored_by: CUSTODIAN.to_string(),
                timestamp: 1000,
                labels: Default::default(),
            },
        )
        .await
        .unwrap();
    }

    // docs/architecture/key-custody.md, "An escrow certificate asked again, or abandoned" (the maintainer, 2026-11-01):
    // past the issuing custodian's window, an escrow certificate whose key was never stored is abandoned, and the
    // workload is issued another, the abandoned one superseded. One whose key was stored is the workload's, as before,
    // and within the window another pod is told it is being issued, as before.
    #[tokio::test]
    async fn an_escrow_certificate_whose_key_was_never_stored_is_abandoned_past_the_window_and_issued_over()
     {
        let past = (CUSTODIAN_WINDOW_SECS + 1) * 1000;
        let ledger = ledger_with_an_escrow_certificate(past, false).await;
        let server = cert_issuer_signing_the_csr().await;
        let reply = handler(ledger.clone(), &server)
            .handle(with_csr(&escrow_csr()), Some(CUSTODIAN.to_string()))
            .await;
        assert!(
            matches!(reply, AgentEscrowIssuanceReply::Issued { .. }),
            "{reply:?}"
        );
        assert_ne!(
            call_check_agent_escrow_key(&ledger, X)
                .await
                .unwrap()
                .unwrap()
                .serial,
            "abandoned"
        );
        let revoked = ractor::call!(ledger, LedgerMessage::RevokedSerials).unwrap();
        assert_eq!(revoked, vec!["abandoned".to_string()]);

        for (age, stored, expected) in [
            (
                past,
                true,
                AgentEscrowIssuanceRefusalReason::AlreadyHasValidServiceCertificate,
            ),
            (0, false, AgentEscrowIssuanceRefusalReason::BeingIssued),
            (0, true, AgentEscrowIssuanceRefusalReason::BeingIssued),
        ] {
            let ledger = ledger_with_an_escrow_certificate(age, stored).await;
            let server = cert_issuer_signing_the_csr().await;
            let reply = handler(ledger, &server)
                .handle(with_csr(&escrow_csr()), Some(CUSTODIAN.to_string()))
                .await;
            assert_eq!(reason(&reply), Some(expected), "{age} {stored}");
            assert!(server.received_requests().await.unwrap().is_empty());
        }
    }

    /// Two keys, each with a certificate of it and its CSR's key; and a certificate that does not read (the last).
    fn keys() -> &'static [(String, Vec<u8>)] {
        static KEYS: std::sync::OnceLock<Vec<(String, Vec<u8>)>> = std::sync::OnceLock::new();
        KEYS.get_or_init(|| {
            let mut keys: Vec<(String, Vec<u8>)> = (0..2)
                .map(|_| {
                    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
                    let certificate = rcgen::CertificateParams::new(vec![X.to_string()])
                        .unwrap()
                        .self_signed(&key)
                        .unwrap()
                        .pem();
                    (
                        certificate,
                        rcgen::PublicKeyData::subject_public_key_info(&key),
                    )
                })
                .collect();
            keys.push((
                "-----BEGIN CERTIFICATE-----\nx\n-----END CERTIFICATE-----\n".to_string(),
                Vec::new(),
            ));
            keys
        })
    }

    proptest::proptest! {
        /// For any existing certificate (its pod, time and key), pod asking, key asked for, stored or not, and time:
        /// - it is issued exactly when there is none, the same key is asked for again, or the existing one is past the
        ///   issuing custodian's window with no key stored (abandoned); otherwise one escrow certificate per identity
        ///   stands, a different key never issued over a live one that may still be stored;
        /// - the pod that was issued it is never told to wait for itself, and once the window is past no one is told
        ///   to wait, so no pod waits for good.
        #[test]
        fn one_certificate_per_identity_issued_again_only_when_asked_again_or_abandoned(
            existing in proptest::option::of((0u8..3, 0i64..1_000_000, 0usize..3)),
            requester in 0u8..3,
            asked in 0usize..3,
            stored in proptest::bool::ANY,
            now in 0i64..2_000_000,
        ) {
            use credentials_agent_common::custodian::CUSTODIAN_WINDOW_SECS;
            let record = existing.map(|(pod, at, key)| crate::state::AgentEscrowCertificateRecord {
                instance_binding: format!("pod-{pod}"),
                serial: "s".to_string(),
                timestamp: at,
                certificate_pem: keys()[key].0.clone(),
            });
            let requester = format!("pod-{requester}");
            // The last key does not read: a CSR that cannot be read asks for no key.
            let asked_key = (asked < 2).then(|| keys()[asked].1.as_slice());
            let decided = first_issuance(record.as_ref(), &requester, asked_key, stored, now);
            let past = |r: &crate::state::AgentEscrowCertificateRecord| {
                now - r.timestamp > CUSTODIAN_WINDOW_SECS * 1000
            };
            let expected = match (&existing, &record) {
                (Some((_, _, key)), Some(r)) => (*key == asked && asked < 2) || (past(r) && !stored),
                _ => true,
            };
            proptest::prop_assert_eq!(decided.is_ok(), expected);
            if let Some(r) = &record
                && (r.instance_binding == requester || past(r))
            {
                proptest::prop_assert_ne!(decided, Err(AgentEscrowIssuanceRefusalReason::BeingIssued));
            }
        }
    }
}
