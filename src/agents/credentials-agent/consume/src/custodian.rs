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

//! A custodian asking on its workload's behalf (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian").
//!
//! The custodian is the custody daemon's own identity, `custody.<X>`. Its pod's token bought its certificate, through
//! the observer like any other; that certificate is what it asks with after, over its own Cassini session, and only for
//! what it names (`p:custody:`), only within [`CUSTODIAN_WINDOW_SECS`] of its issue. X's certificates are signed on
//! `/internal/issue` for X, under the renewal rule for X's pod instance (the custodian's), and never past the custodian's
//! own end. So the custodian's authority is spent, not kept: each type once (the renewal rule), nothing after the
//! window, and a certificate that can only listen after that.

use crate::escrow::EscrowKeypair;
use crate::issuance::{
    call_admit, call_append, call_check_grant, check_request_shape, extra_sans_for,
    instance_sans_for, signing_type,
};
use crate::ledger::LedgerMessage;
use crate::state::CertificateRecord;
use cert_client::handshake::HandshakeClient;
use cert_issuer_common::custody::{CustodyPermission, custodian_of, extract_custody_permissions};
use credentials_agent_common::custodian::{CUSTODIAN_WINDOW_SECS, CustodianIssuanceRequest};
use credentials_agent_common::events::CredentialsEvent;
use credentials_agent_common::issuance::{IssuanceRefusalReason, IssuanceReply, WorkloadCertType};
use ractor::ActorRef;
use std::sync::Arc;
use tracing::{error, warn};

/// A custodian the ledger holds a live certificate for, within its window, for something its certificate names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Custodian {
    /// The workload it keeps keys for.
    pub workload: String,
    /// Its pod instance: its workload's too.
    pub instance_binding: String,
    /// When its certificate ends (Unix seconds): none of its workload's may end later.
    pub expires_at: i64,
    /// Its pod's name, as its certificate names its instance (a multi-instance workload's custodian's): what its
    /// workload's certificates name the pod by, beside `instance_binding`.
    pub instance_pod: Option<String>,
}

/// Why a requester is not answered as a custodian.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodianRefusal {
    /// Not a custodian's identity, no live certificate for it, or one that does not name what is asked for.
    NotACustodian(String),
    /// Its window is past: its authority is spent.
    WindowClosed,
    /// The ledger could not be asked.
    Unavailable(String),
}

/// Whether `requester`, whose custodian certificate the ledger holds as `record`, may ask for `needs` at `now_millis`.
/// Pure: what the ledger holds and the clock decide it.
pub fn judge(
    requester: &str,
    record: Option<&CertificateRecord>,
    needs: CustodyPermission,
    now_millis: i64,
) -> Result<Custodian, CustodianRefusal> {
    let not = |why: &str| Err(CustodianRefusal::NotACustodian(why.to_string()));
    let Some(workload) = custodian_of(requester) else {
        return not("not a custodian's identity");
    };
    let Some(record) = record else {
        return not("no live custodian certificate in the ledger");
    };
    if record.identity != requester
        || record.cert_type != WorkloadCertType::Custody
        || record.revoked
    {
        return not("no live custodian certificate in the ledger");
    }
    if now_millis.saturating_sub(record.timestamp) > CUSTODIAN_WINDOW_SECS * 1000 {
        return Err(CustodianRefusal::WindowClosed);
    }
    match extract_custody_permissions(&record.certificate_pem) {
        Ok(permissions) if permissions.contains(&needs) => {}
        _ => return not("its certificate does not name what it asks for"),
    }
    let Some(expires_at) = cert_issuer_common::serial::not_after_from_pem(&record.certificate_pem)
    else {
        return not("its certificate has no end that can be read");
    };
    // A multi-instance workload's custodian names its pod (docs/architecture/authority-model.md, "Multi-instance
    // workloads"); a certificate whose instance names do not read as one is no custodian's.
    let Ok(instance) = cert_issuer_common::instance::extract_instance(&record.certificate_pem)
    else {
        return not("its certificate's instance names do not read as one instance");
    };
    Ok(Custodian {
        workload: workload.to_string(),
        instance_binding: record.instance_binding.clone(),
        expires_at,
        instance_pod: instance.and_then(|instance| instance.pod),
    })
}

/// [`judge`], with the ledger asked for the requester's custodian certificate.
pub async fn custodian(
    ledger: &ActorRef<LedgerMessage>,
    requester: Option<&str>,
    needs: CustodyPermission,
) -> Result<Custodian, CustodianRefusal> {
    let Some(requester) = requester else {
        return Err(CustodianRefusal::NotACustodian(
            "no verified publisher".to_string(),
        ));
    };
    if custodian_of(requester).is_none() {
        return Err(CustodianRefusal::NotACustodian(
            "not a custodian's identity".to_string(),
        ));
    }
    let record = ractor::call!(ledger, |reply| LedgerMessage::CustodianCertificate {
        identity: requester.to_string(),
        reply,
    })
    .map_err(|e| CustodianRefusal::Unavailable(e.to_string()))?;
    judge(requester, record.as_ref(), needs, polar::time::now_millis())
}

/// The custody permission a certificate of `cert_type` is asked under.
fn permission_for(cert_type: WorkloadCertType) -> Option<CustodyPermission> {
    match cert_type {
        WorkloadCertType::Client => Some(CustodyPermission::Client),
        WorkloadCertType::Server => Some(CustodyPermission::Server),
        WorkloadCertType::Custody => None,
    }
}

/// Answers a custodian's request for one of its workload's certificates.
pub struct CustodianIssuanceHandler {
    pub ledger: ActorRef<LedgerMessage>,
    /// cert-issuer, reached with this agent's own certificate, for `/internal/issue`.
    pub cert_issuer: HandshakeClient,
    pub escrow_keypair: Arc<EscrowKeypair>,
    /// What each workload's client certificate carries, as on the token path.
    pub authority_model: Option<Arc<credentials_agent_model::Model>>,
}

impl CustodianIssuanceHandler {
    pub async fn handle(
        &self,
        request: CustodianIssuanceRequest,
        requested_by: Option<String>,
    ) -> IssuanceReply {
        let refused = |reason| IssuanceReply::Refused { reason };
        // ---- Step 1: what is asked for, judged on the request alone ----
        let Some(needs) = permission_for(request.cert_type) else {
            warn!(
                ?requested_by,
                "a custodian asked for a custodian's certificate: refused"
            );
            return refused(IssuanceRefusalReason::RequestNotPermitted);
        };
        if check_request_shape(
            crate::credential::CredentialKind::Kubernetes,
            request.cert_type,
            &request.names,
        )
        .is_err()
        {
            return refused(IssuanceRefusalReason::RequestNotPermitted);
        }
        // ---- Step 2: who asks: a live custodian, in its window, for what its certificate names ----
        let custodian = match custodian(&self.ledger, requested_by.as_deref(), needs).await {
            Ok(custodian) => custodian,
            Err(CustodianRefusal::WindowClosed) => {
                warn!(?requested_by, "a custodian asked after its window: refused");
                return refused(IssuanceRefusalReason::CustodianWindowClosed);
            }
            Err(CustodianRefusal::NotACustodian(why)) => {
                warn!(?requested_by, "refused on the custodian's path: {why}");
                return refused(IssuanceRefusalReason::NotACustodian);
            }
            Err(CustodianRefusal::Unavailable(e)) => {
                error!("the ledger could not be asked about a custodian: {e}");
                return refused(IssuanceRefusalReason::NotACustodian);
            }
        };
        let identity = custodian.workload.clone();
        // ---- Step 3: the renewal rule, for the workload's pod instance: once of each type ----
        let admitted_at = polar::time::now_millis();
        match call_admit(
            &self.ledger,
            &identity,
            &custodian.instance_binding,
            request.cert_type,
            admitted_at,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => {
                warn!(%identity, cert_type = ?request.cert_type, "refused: issued one of this type within the renewal bound");
                return refused(IssuanceRefusalReason::TooSoon);
            }
            Err(e) => {
                error!("Ledger Admit call failed: {e}");
                return refused(IssuanceRefusalReason::NotACustodian);
            }
        }
        let release = || {
            let _ = self.ledger.send_message(LedgerMessage::Release {
                identity: identity.clone(),
                instance_binding: custodian.instance_binding.clone(),
                cert_type: request.cert_type,
                at: admitted_at,
            });
        };
        // ---- Step 4: the grant, written under the workload's token subject ----
        let grant_key = cert_issuer_common::identity::kubernetes_subject_of(&identity);
        let grant = match &grant_key {
            Some(key) => match call_check_grant(&self.ledger, key).await {
                Ok(grant) => grant,
                Err(e) => {
                    error!("Ledger CheckGrant call failed: {e}");
                    release();
                    return refused(IssuanceRefusalReason::NotACustodian);
                }
            },
            None => None,
        };
        let grant_key = grant.as_ref().and(grant_key);
        let permissions = grant.map(|g| g.permissions).unwrap_or_default();
        // ---- Step 5: sign, for the workload, never past the custodian's end ----
        let mut extra_sans = extra_sans_for(
            request.cert_type,
            &request.names,
            &permissions,
            self.authority_model
                .as_ref()
                .and_then(|model| model.issued_by_the_credentials_agent(&identity)),
        );
        // A multi-instance workload's client certificate names its pod, as its custodian's does: the UID its token
        // proved, and the name its custodian's certificate carries (docs/architecture/authority-model.md,
        // "Multi-instance workloads").
        match instance_sans_for(
            self.authority_model.as_deref(),
            &identity,
            request.cert_type,
            &custodian.instance_binding,
            custodian.instance_pod.as_deref(),
        ) {
            Ok(names) => extra_sans.extend(names),
            Err(e) => {
                warn!(%identity, "refused: the custodian's pod cannot be named in a certificate: {e}");
                release();
                return refused(IssuanceRefusalReason::RequestNotPermitted);
            }
        }
        let issued = match self
            .cert_issuer
            .issue_internal(cert_issuer_common::InternalIssueRequest {
                csr_pem: request.csr_pem,
                identity: identity.clone(),
                cert_type: signing_type(request.cert_type),
                extra_sans,
                subject_dn_defaults: None,
                account_subject: None,
                instance_binding: Some(custodian.instance_binding.clone()),
                expires_by: Some(custodian.expires_at),
            })
            .await
        {
            Ok(issued) => issued,
            Err(e) => {
                error!("cert-issuer /internal/issue failed for a custodian's workload: {e}");
                release();
                return refused(IssuanceRefusalReason::SigningFailed);
            }
        };
        // ---- Step 6: record it, as the token path does ----
        let event = CredentialsEvent::IssuanceCompleted {
            identity,
            instance_binding: custodian.instance_binding,
            serial: issued.serial_number,
            timestamp: polar::time::now_millis(),
            cert_type: request.cert_type,
            grant_key,
            certificate_pem: issued.certificate_pem.clone(),
        };
        if let Err(e) = call_append(&self.ledger, event).await {
            error!(
                "Failed to append IssuanceCompleted to the ledger for an already-issued certificate -- genuine \
                 audit-trail gap, needs operator attention: {e}"
            );
        }
        IssuanceReply::Issued {
            certificate_pem: issued.certificate_pem,
            ca_chain_pem: issued.ca_chain_pem,
            escrow_public_key_pem: self.escrow_keypair.public_key_pem().to_string(),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::issuance::tests::{fake_issue_response, spawn_test_ledger};
    use proptest::prelude::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub(crate) const X: &str = "kube-consumer.polar.serviceaccount.cluster.local";
    pub(crate) const CUSTODIAN: &str = "custody.kube-consumer.polar.serviceaccount.cluster.local";
    pub(crate) const POD: &str = "pod-uid-1";

    /// A custodian's certificate naming `permissions`, ending `ends_in` seconds from now.
    pub(crate) fn custodian_pem(permissions: &[CustodyPermission], ends_in: i64) -> String {
        custodian_pem_naming(permissions, ends_in, &[])
    }

    /// [`custodian_pem`], its certificate naming `others` too (an instance's names).
    pub(crate) fn custodian_pem_naming(
        permissions: &[CustodyPermission],
        ends_in: i64,
        others: &[&str],
    ) -> String {
        let mut params = rcgen::CertificateParams::new(vec![CUSTODIAN.to_string()]).unwrap();
        for name in others {
            params
                .subject_alt_names
                .push(rcgen::SanType::URI((*name).to_string().try_into().unwrap()));
        }
        for p in permissions {
            params
                .subject_alt_names
                .push(rcgen::SanType::URI(p.to_san_string().try_into().unwrap()));
        }
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::minutes(1);
        params.not_after = now + time::Duration::seconds(ends_in);
        let key = rcgen::KeyPair::generate().unwrap();
        params.self_signed(&key).unwrap().pem()
    }

    fn record(
        permissions: &[CustodyPermission],
        age_millis: i64,
        revoked: bool,
    ) -> CertificateRecord {
        CertificateRecord {
            identity: CUSTODIAN.to_string(),
            instance_binding: POD.to_string(),
            cert_type: WorkloadCertType::Custody,
            serial: "0c".to_string(),
            timestamp: polar::time::now_millis() - age_millis,
            certificate_pem: custodian_pem(permissions, 3600),
            revoked,
            revoked_reason: None,
            revoked_by: None,
            revoked_at: None,
            reported: false,
            instance: None,
        }
    }

    /// The ledger, holding a custodian issued `age_millis` ago naming `permissions`.
    pub(crate) async fn ledger_with_custodian(
        permissions: &[CustodyPermission],
        age_millis: i64,
    ) -> ActorRef<LedgerMessage> {
        let ledger = spawn_test_ledger().await;
        let event = CredentialsEvent::IssuanceCompleted {
            identity: CUSTODIAN.to_string(),
            instance_binding: POD.to_string(),
            serial: "0c".to_string(),
            timestamp: polar::time::now_millis() - age_millis,
            cert_type: WorkloadCertType::Custody,
            grant_key: None,
            certificate_pem: custodian_pem(permissions, 3600),
        };
        call_append(&ledger, event).await.unwrap();
        ledger
    }

    async fn cert_issuer_signing_for(identity: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/issue"))
            .and(body_partial_json(serde_json::json!({
                "identity": identity,
                "instance_binding": POD,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(fake_issue_response()))
            .mount(&server)
            .await;
        server
    }

    fn handler(ledger: ActorRef<LedgerMessage>, server: &MockServer) -> CustodianIssuanceHandler {
        CustodianIssuanceHandler {
            ledger,
            cert_issuer: HandshakeClient::new(server.uri()),
            escrow_keypair: crate::escrow::tests::test_keypair(),
            authority_model: None,
        }
    }

    fn asking(cert_type: WorkloadCertType) -> CustodianIssuanceRequest {
        CustodianIssuanceRequest {
            csr_pem: "fake-csr".to_string(),
            cert_type,
            names: vec![],
        }
    }

    fn reason(reply: &IssuanceReply) -> Option<IssuanceRefusalReason> {
        match reply {
            IssuanceReply::Refused { reason } => Some(reason.clone()),
            _ => None,
        }
    }

    #[tokio::test]
    async fn a_custodian_is_issued_its_workload_s_certificate_once_and_it_is_recorded_for_the_workload()
     {
        let ledger = ledger_with_custodian(&[CustodyPermission::Client], 0).await;
        let server = cert_issuer_signing_for(X).await;
        let handler = handler(ledger.clone(), &server);
        let reply = handler
            .handle(
                asking(WorkloadCertType::Client),
                Some(CUSTODIAN.to_string()),
            )
            .await;
        assert!(matches!(reply, IssuanceReply::Issued { .. }), "{reply:?}");
        let found = crate::retrieve_secret::call_check_certificate(&ledger, X)
            .await
            .unwrap();
        assert!(
            found.is_some_and(|r| r.instance_binding == POD),
            "recorded for the workload's pod"
        );
        // The cap is the custodian's end.
        let asked: serde_json::Value = server.received_requests().await.unwrap()[0]
            .body_json()
            .unwrap();
        assert!(asked["expires_by"].as_i64().is_some(), "{asked}");
        // A second of the same type is the renewal rule's to refuse.
        let again = handler
            .handle(
                asking(WorkloadCertType::Client),
                Some(CUSTODIAN.to_string()),
            )
            .await;
        assert_eq!(reason(&again), Some(IssuanceRefusalReason::TooSoon));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    /// docs/architecture/authority-model.md, "Multi-instance workloads": a multi-instance workload's client certificate
    /// names its pod as its custodian's certificate does (the UID its token proved, and the pod's name); a custodian
    /// whose certificate's instance names do not read as one instance is no custodian.
    #[tokio::test]
    async fn a_multi_instance_workload_s_certificate_names_its_custodian_s_pod() {
        let pod_name = "i:pod:kube-consumer-7f9c-x2";
        let uid = format!("i:uid:{POD}");
        let ledger = spawn_test_ledger().await;
        call_append(
            &ledger,
            CredentialsEvent::IssuanceCompleted {
                identity: CUSTODIAN.to_string(),
                instance_binding: POD.to_string(),
                serial: "0c".to_string(),
                timestamp: polar::time::now_millis(),
                cert_type: WorkloadCertType::Custody,
                grant_key: None,
                certificate_pem: custodian_pem_naming(
                    &[CustodyPermission::Client],
                    3600,
                    &[&uid, pod_name],
                ),
            },
        )
        .await
        .unwrap();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/issue"))
            .and(body_partial_json(serde_json::json!({
                "identity": X,
                "instance_binding": POD,
                "extra_sans": [
                    "p:pub:polar.provenance.discovery",
                    "p:sub:kubernetes.cluster.consumer.Pod",
                    "g:cassini-client",
                    "g:encryption",
                    uid,
                    pod_name
                ],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(fake_issue_response()))
            .mount(&server)
            .await;
        let handler = CustodianIssuanceHandler {
            authority_model: crate::issuance::tests::model_with_kube_consumer_many(),
            ..handler(ledger, &server)
        };
        let reply = handler
            .handle(
                asking(WorkloadCertType::Client),
                Some(CUSTODIAN.to_string()),
            )
            .await;
        assert!(matches!(reply, IssuanceReply::Issued { .. }), "{reply:?}");

        let mut two_uids = record(&[CustodyPermission::Client], 0, false);
        two_uids.certificate_pem = custodian_pem_naming(
            &[CustodyPermission::Client],
            3600,
            &[&uid, "i:uid:pod-uid-2"],
        );
        assert!(matches!(
            judge(
                CUSTODIAN,
                Some(&two_uids),
                CustodyPermission::Client,
                polar::time::now_millis()
            ),
            Err(CustodianRefusal::NotACustodian(_))
        ));
    }

    #[tokio::test]
    async fn no_one_but_a_live_custodian_in_its_window_is_answered_and_cert_issuer_is_never_asked()
    {
        for (requester, permissions, age, expected) in [
            // the workload itself
            (
                Some(X),
                vec![CustodyPermission::Client],
                0,
                IssuanceRefusalReason::NotACustodian,
            ),
            // no one at all
            (
                None,
                vec![CustodyPermission::Client],
                0,
                IssuanceRefusalReason::NotACustodian,
            ),
            // another workload's custodian, the ledger holding none for it
            (
                Some("custody.other.polar.serviceaccount.cluster.local"),
                vec![CustodyPermission::Client],
                0,
                IssuanceRefusalReason::NotACustodian,
            ),
            // a custodian whose certificate does not name a client certificate
            (
                Some(CUSTODIAN),
                vec![CustodyPermission::Escrow],
                0,
                IssuanceRefusalReason::NotACustodian,
            ),
            // past its window
            (
                Some(CUSTODIAN),
                vec![CustodyPermission::Client],
                (CUSTODIAN_WINDOW_SECS + 1) * 1000,
                IssuanceRefusalReason::CustodianWindowClosed,
            ),
        ] {
            let ledger = ledger_with_custodian(&permissions, age).await;
            let server = cert_issuer_signing_for(X).await;
            let reply = handler(ledger, &server)
                .handle(
                    asking(WorkloadCertType::Client),
                    requester.map(str::to_string),
                )
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
    async fn a_custodian_asks_for_no_custodian_and_a_client_certificate_with_no_names() {
        let ledger = ledger_with_custodian(&CustodyPermission::ALL, 0).await;
        let server = cert_issuer_signing_for(X).await;
        let handler = handler(ledger, &server);
        let reply = handler
            .handle(
                asking(WorkloadCertType::Custody),
                Some(CUSTODIAN.to_string()),
            )
            .await;
        assert_eq!(
            reason(&reply),
            Some(IssuanceRefusalReason::RequestNotPermitted)
        );
        let mut named = asking(WorkloadCertType::Client);
        named.names = vec!["extra.example.com".to_string()];
        let reply = handler.handle(named, Some(CUSTODIAN.to_string())).await;
        assert_eq!(
            reason(&reply),
            Some(IssuanceRefusalReason::RequestNotPermitted)
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_revoked_custodian_is_answered_nothing() {
        let ledger = ledger_with_custodian(&[CustodyPermission::Client], 0).await;
        call_append(
            &ledger,
            CredentialsEvent::Revoke {
                serial: "0c".to_string(),
                reason: credentials_agent_common::events::RevocationReason::Compromised,
                revoked_by: "admin".to_string(),
                timestamp: polar::time::now_millis(),
            },
        )
        .await
        .unwrap();
        let server = cert_issuer_signing_for(X).await;
        let reply = handler(ledger, &server)
            .handle(
                asking(WorkloadCertType::Client),
                Some(CUSTODIAN.to_string()),
            )
            .await;
        assert_eq!(reason(&reply), Some(IssuanceRefusalReason::NotACustodian));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    fn a_permission() -> impl Strategy<Value = CustodyPermission> {
        prop::sample::select(CustodyPermission::ALL.to_vec())
    }

    proptest! {
        // The rule, for every input: answered only as the custodian of the workload its name says, live, in its
        // window, for something its certificate names, and then for its own pod instance and end.
        #[test]
        fn a_custodian_is_answered_only_by_the_rule(
            named in prop::collection::btree_set(a_permission(), 0..=3),
            needs in a_permission(),
            age_secs in 0i64..(2 * CUSTODIAN_WINDOW_SECS),
            revoked in any::<bool>(),
            requester in prop::sample::select(vec![CUSTODIAN, X, "custody.", "custody.custody.x"]),
        ) {
            let named: Vec<_> = named.into_iter().collect();
            let record = record(&named, age_secs * 1000, revoked);
            match judge(requester, Some(&record), needs, polar::time::now_millis()) {
                Ok(custodian) => {
                    prop_assert_eq!(requester, CUSTODIAN);
                    prop_assert!(!revoked);
                    prop_assert!(age_secs <= CUSTODIAN_WINDOW_SECS);
                    prop_assert!(named.contains(&needs));
                    prop_assert_eq!(custodian.workload.as_str(), X);
                    prop_assert_eq!(custodian.instance_binding.as_str(), POD);
                }
                Err(CustodianRefusal::WindowClosed) => {
                    prop_assert!(age_secs > CUSTODIAN_WINDOW_SECS);
                }
                Err(_) => prop_assert!(
                    requester != CUSTODIAN || revoked || !named.contains(&needs)
                ),
            }
        }
    }
}
