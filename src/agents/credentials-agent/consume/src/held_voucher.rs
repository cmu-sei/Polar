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

//! The credentials agent's side of the requests cert-issuer holds (docs/architecture/core-restarts.md, "3 in detail"):
//! a bootstrap service's token-only request that cert-issuer's floor state does not admit (the observer's restart,
//! Cassini's) is offered here, and this agent vouches for it or refuses it, by its own rules.
//!
//! - Over Cassini: cert-issuer offers each held request on `cert-issuer-held-requests`; an offer is taken only from
//!   cert-issuer's identity, as the broker verified it, and answered on `cert-issuer-held-replies`.
//! - Over mTLS, while this agent's Cassini session is lost (Cassini restarting): it lists the held requests on
//!   cert-issuer's `/internal/held` every [`HELD_POLL_EVERY`] and answers there. The asking is also its presence to
//!   cert-issuer, so a Cassini restart completes with only cert-issuer and this agent up.
//!
//! The rules ([`rule`]): a bootstrap service in the authority model, not this agent's own successor (vouched for by no
//! one), bound to a pod, for a client or server certificate, and admitted by the renewal rule: not issued one of that
//! type for that pod within the bound (docs/architecture/deployments.md, "The renewal rule"). The attestation agent's
//! instance check joins them once it exists (step 6).

use crate::ledger::LedgerMessage;
use cassini_client::TcpClientMessage;
use cert_client::handshake::HandshakeClient;
use cert_issuer_common::CertType;
use cert_issuer_common::held::{HELD_REPLIES_TOPIC, HeldDecision, HeldOffer, HeldReply};
use credentials_agent_common::issuance::WorkloadCertType;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::collections::BTreeSet;
use std::time::Duration;
use tracing::{info, warn};

/// How often the held requests are asked for over mTLS while the Cassini session is lost: well inside cert-issuer's
/// presence bound, so it keeps hearing this agent.
pub const HELD_POLL_EVERY: Duration = Duration::from_secs(10);

/// Whether to vouch for `offer`: a bootstrap service's (`bootstrap`, from the authority model), not `own_identity`,
/// bound to a pod, for a client or server certificate, and not `too_soon` by the renewal rule.
pub fn rule(
    offer: &HeldOffer,
    bootstrap: &BTreeSet<String>,
    own_identity: &str,
    too_soon: bool,
) -> HeldDecision {
    let refuse = |reason: &str| HeldDecision::Refuse {
        reason: reason.to_string(),
    };
    // A bootstrap service's custodian is held and vouched for as the service is (docs/architecture/key-custody.md,
    // "Part A's bootstrap: the custodian"): judged by the service it keeps keys for.
    let service = if offer.cert_type == CertType::Custody {
        match cert_issuer_common::custody::custodian_of(&offer.identity) {
            Some(service) => service,
            None => return refuse("a custodian's certificate for no custodian's identity"),
        }
    } else {
        offer.identity.as_str()
    };
    if service == own_identity {
        return refuse("the credentials agent's own successor is vouched for by no one");
    }
    if !bootstrap.contains(service) {
        return refuse("not a bootstrap service");
    }
    if offer.instance_binding.is_none() {
        return refuse("its token is bound to no pod");
    }
    if workload_cert_type(&offer.cert_type).is_none() {
        return refuse("neither a client, a server nor a custodian's certificate");
    }
    if too_soon {
        return refuse("this pod was issued a certificate of this type less than five minutes ago");
    }
    HeldDecision::Vouch
}

fn workload_cert_type(cert_type: &CertType) -> Option<WorkloadCertType> {
    match cert_type {
        CertType::Client => Some(WorkloadCertType::Client),
        CertType::Server => Some(WorkloadCertType::Server),
        CertType::Custody => Some(WorkloadCertType::Custody),
        CertType::Service | CertType::Signing => None,
    }
}

pub enum HeldVoucherMessage {
    /// An offer arrived over Cassini, published (the broker says) by `published_by`.
    Offered {
        payload: Vec<u8>,
        published_by: Option<String>,
    },
    /// This agent's Cassini session was lost: poll cert-issuer over mTLS.
    CassiniLost,
    /// It is back: offers come over Cassini again.
    CassiniUp,
    /// The interval: poll, if the session is lost.
    Poll,
}

pub struct HeldVoucherArgs {
    pub client: ActorRef<TcpClientMessage>,
    pub ledger: ActorRef<LedgerMessage>,
    /// cert-issuer's identity: the only publisher an offer is taken from.
    pub cert_issuer: String,
    /// The bootstrap services' identities, from the authority model.
    pub bootstrap: BTreeSet<String>,
    /// This agent's own identity.
    pub own_identity: String,
    /// cert-issuer over mTLS, presenting this agent's certificate.
    pub cert_issuer_http: HandshakeClient,
}

pub struct HeldVoucherState {
    args: HeldVoucherArgs,
    lost: bool,
}

pub struct HeldVoucher;

impl HeldVoucherState {
    async fn decide(&self, offer: &HeldOffer) -> HeldDecision {
        let (bootstrap, own) = (&self.args.bootstrap, &self.args.own_identity);
        // The renewal rule is asked last, and only of an offer the other rules allow, since admitting records it.
        let decision = match (
            rule(offer, bootstrap, own, false),
            &offer.instance_binding,
            workload_cert_type(&offer.cert_type),
        ) {
            (HeldDecision::Vouch, Some(binding), Some(cert_type)) => {
                match ractor::call!(self.args.ledger, |reply| LedgerMessage::Admit {
                    identity: offer.identity.clone(),
                    instance_binding: binding.clone(),
                    cert_type,
                    now: polar::time::now_millis(),
                    reply,
                }) {
                    Ok(admitted) => rule(offer, bootstrap, own, !admitted),
                    Err(e) => {
                        // Not knowing is not a reason to vouch: refused, and the pod asks again as a new one.
                        warn!("the ledger could not apply the renewal rule: {e}");
                        HeldDecision::Refuse {
                            reason: "the credentials agent could not check its ledger".to_string(),
                        }
                    }
                }
            }
            (decision, _, _) => decision,
        };
        info!(held = %offer.id, identity = %offer.identity, decision = ?decision, "a held request was decided");
        decision
    }
}

#[ractor::async_trait]
impl Actor for HeldVoucher {
    type Msg = HeldVoucherMessage;
    type State = HeldVoucherState;
    type Arguments = HeldVoucherArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: HeldVoucherArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(HeldVoucherState { args, lost: false })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            HeldVoucherMessage::Offered {
                payload,
                published_by,
            } => {
                if published_by.as_deref() != Some(state.args.cert_issuer.as_str()) {
                    warn!(published_by = ?published_by, "a held request offered by someone other than cert-issuer: not taken");
                    return Ok(());
                }
                let offer: HeldOffer = match serde_json::from_slice(&payload) {
                    Ok(offer) => offer,
                    Err(e) => {
                        warn!("a held request that does not read: {e}");
                        return Ok(());
                    }
                };
                let reply = HeldReply {
                    id: offer.id.clone(),
                    decision: state.decide(&offer).await,
                };
                state.args.client.send_message(TcpClientMessage::publish(
                    HELD_REPLIES_TOPIC.to_string(),
                    serde_json::to_vec(&reply)?,
                    None,
                ))?;
            }
            HeldVoucherMessage::CassiniLost => state.lost = true,
            HeldVoucherMessage::CassiniUp => state.lost = false,
            HeldVoucherMessage::Poll if state.lost => {
                let held = match state.args.cert_issuer_http.held_list().await {
                    Ok(list) => list.held,
                    Err(e) => {
                        warn!("cert-issuer's held requests could not be listed over mTLS: {e}");
                        return Ok(());
                    }
                };
                for offer in held {
                    let decision = state.decide(&offer).await;
                    if let Err(e) = state
                        .args
                        .cert_issuer_http
                        .held_answer(&offer.id, &decision)
                        .await
                    {
                        warn!(held = %offer.id, "the answer to a held request could not be given over mTLS: {e}");
                    }
                }
            }
            HeldVoucherMessage::Poll => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cert_issuer_common::record::{IssuePath, RecordEntry, RecordReply};
    use std::sync::{Arc, Mutex};
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CERT_ISSUER: &str = "cert-issuer.polar.serviceaccount.cluster.local";
    const OWN: &str = "credentials-agent.polar.serviceaccount.cluster.local";
    const OBSERVER: &str = "credentials-agent-observer.polar.serviceaccount.cluster.local";
    const CASSINI: &str = "cassini.polar.serviceaccount.cluster.local";

    fn bootstrap() -> BTreeSet<String> {
        [OWN, OBSERVER, CASSINI]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn offer(identity: &str, binding: Option<&str>, cert_type: CertType) -> HeldOffer {
        HeldOffer {
            id: "h1".to_string(),
            workload: format!(
                "system:serviceaccount:polar:{}",
                identity.split('.').next().unwrap()
            ),
            identity: identity.to_string(),
            instance_binding: binding.map(str::to_string),
            cert_type,
            names: vec![],
            csr_pem: "csr".to_string(),
        }
    }

    fn refused(decision: HeldDecision) -> String {
        match decision {
            HeldDecision::Refuse { reason } => reason,
            HeldDecision::Vouch => panic!("expected a refusal"),
        }
    }

    #[test]
    fn a_bootstrap_service_s_pod_not_issued_one_of_its_type_lately_is_vouched_for_and_nothing_else_is()
     {
        let ok = |o: &HeldOffer, too_soon| rule(o, &bootstrap(), OWN, too_soon);
        assert_eq!(
            ok(&offer(OBSERVER, Some("pod-1"), CertType::Client), false),
            HeldDecision::Vouch
        );
        assert_eq!(
            ok(&offer(CASSINI, Some("pod-2"), CertType::Server), false),
            HeldDecision::Vouch
        );
        assert!(
            refused(ok(&offer(OWN, Some("pod-3"), CertType::Client), false))
                .contains("own successor")
        );
        assert!(
            refused(ok(
                &offer(
                    "kube-observer.polar.serviceaccount.cluster.local",
                    Some("p"),
                    CertType::Client
                ),
                false
            ))
            .contains("not a bootstrap")
        );
        assert!(refused(ok(&offer(OBSERVER, None, CertType::Client), false)).contains("no pod"));
        for t in [CertType::Service, CertType::Signing] {
            assert!(refused(ok(&offer(OBSERVER, Some("p"), t), false)).contains("neither"));
        }
        // A bootstrap service's custodian is the service's to vouch for; the credentials agent's own is no one's, and
        // one named for no service, or for a service that is not bootstrap, is refused.
        let custodian = cert_issuer_common::custody::custodian_identity;
        assert_eq!(
            ok(
                &offer(&custodian(CASSINI), Some("pod-2"), CertType::Custody),
                false
            ),
            HeldDecision::Vouch
        );
        assert!(
            refused(ok(
                &offer(&custodian(OWN), Some("pod-3"), CertType::Custody),
                false
            ))
            .contains("own successor")
        );
        assert!(
            refused(ok(&offer(CASSINI, Some("pod-2"), CertType::Custody), false))
                .contains("no custodian")
        );
        assert!(
            refused(ok(
                &offer(
                    &custodian("kube-observer.polar.serviceaccount.cluster.local"),
                    Some("p"),
                    CertType::Custody
                ),
                false
            ))
            .contains("not a bootstrap")
        );
        assert!(
            refused(ok(&offer(OBSERVER, Some("pod-1"), CertType::Client), true))
                .contains("five minutes")
        );
    }

    /// Every message published, read back as a held reply.
    struct Client(Arc<Mutex<Vec<(String, HeldReply)>>>);

    #[ractor::async_trait]
    impl Actor for Client {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();
        async fn pre_start(&self, _: ActorRef<Self::Msg>, _: ()) -> Result<(), ActorProcessingErr> {
            Ok(())
        }
        async fn handle(
            &self,
            _: ActorRef<Self::Msg>,
            message: Self::Msg,
            _: &mut (),
        ) -> Result<(), ActorProcessingErr> {
            if let TcpClientMessage::Publish { topic, payload, .. } = message {
                self.0
                    .lock()
                    .unwrap()
                    .push((topic, serde_json::from_slice(&payload).unwrap()));
            }
            Ok(())
        }
    }

    struct Rig {
        voucher: ActorRef<HeldVoucherMessage>,
        ledger: ActorRef<LedgerMessage>,
        published: Arc<Mutex<Vec<(String, HeldReply)>>>,
        _dir: tempfile::TempDir,
    }

    async fn rig(cert_issuer_http: HandshakeClient) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let ledger =
            crate::ledger::tests::spawn_ledger(dir.path().join("events.jsonl").to_str().unwrap())
                .await;
        let published = Arc::new(Mutex::new(Vec::new()));
        let (client, _) = Actor::spawn(None, Client(published.clone()), ())
            .await
            .unwrap();
        let (voucher, _) = Actor::spawn(
            None,
            HeldVoucher,
            HeldVoucherArgs {
                client,
                ledger: ledger.clone(),
                cert_issuer: CERT_ISSUER.to_string(),
                bootstrap: bootstrap(),
                own_identity: OWN.to_string(),
                cert_issuer_http,
            },
        )
        .await
        .unwrap();
        Rig {
            voucher,
            ledger,
            published,
            _dir: dir,
        }
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    fn offered(o: &HeldOffer, by: Option<&str>) -> HeldVoucherMessage {
        HeldVoucherMessage::Offered {
            payload: serde_json::to_vec(o).unwrap(),
            published_by: by.map(str::to_string),
        }
    }

    /// The ledger told, as cert-issuer's record tells it, that `identity`'s pod `binding` was issued a client
    /// certificate `ago` seconds ago.
    async fn issued_ago(ledger: &ActorRef<LedgerMessage>, identity: &str, binding: &str, ago: i64) {
        let page = RecordReply {
            log: "log-1".to_string(),
            entries: vec![RecordEntry {
                log: "log-1".to_string(),
                seq: 1,
                serial: "s1".to_string(),
                identity: identity.to_string(),
                cert_type: CertType::Client,
                path: IssuePath::Vouched,
                instance_binding: Some(binding.to_string()),
                signed_at: polar::time::now_millis() / 1000 - ago,
                not_after: 4_000_000_000,
                certificate_pem: "pem".to_string(),
            }],
            more: false,
        };
        ractor::call!(ledger, |reply| LedgerMessage::TakeRecordPage {
            page,
            bootstrap: None,
            reply
        })
        .unwrap()
        .unwrap();
    }

    #[tokio::test]
    async fn an_offer_from_cert_issuer_is_answered_over_cassini_and_one_from_anyone_else_is_not() {
        let rig = rig(HandshakeClient::new("http://127.0.0.1:1".to_string())).await;
        let o = offer(OBSERVER, Some("pod-1"), CertType::Client);
        for stranger in [None, Some(CASSINI)] {
            rig.voucher.send_message(offered(&o, stranger)).unwrap();
        }
        settle().await;
        assert!(rig.published.lock().unwrap().is_empty());
        rig.voucher
            .send_message(offered(&o, Some(CERT_ISSUER)))
            .unwrap();
        settle().await;
        assert_eq!(
            *rig.published.lock().unwrap(),
            vec![(
                HELD_REPLIES_TOPIC.to_string(),
                HeldReply {
                    id: "h1".to_string(),
                    decision: HeldDecision::Vouch
                }
            )]
        );
    }

    #[tokio::test]
    async fn a_pod_the_ledger_says_was_issued_one_of_the_type_lately_is_refused() {
        let rig = rig(HandshakeClient::new("http://127.0.0.1:1".to_string())).await;
        issued_ago(&rig.ledger, OBSERVER, "pod-1", 60).await;
        rig.voucher
            .send_message(offered(
                &offer(OBSERVER, Some("pod-1"), CertType::Client),
                Some(CERT_ISSUER),
            ))
            .unwrap();
        settle().await;
        let published = rig.published.lock().unwrap().clone();
        assert_eq!(published.len(), 1);
        assert!(refused(published[0].1.decision.clone()).contains("five minutes"));
    }

    // docs/architecture/deployments.md, "The renewal rule": a bootstrap service restarted in place, its certificate
    // still valid, is vouched for once the bound has passed.
    #[tokio::test]
    async fn a_pod_issued_one_before_the_bound_is_vouched_for_again() {
        let rig = rig(HandshakeClient::new("http://127.0.0.1:1".to_string())).await;
        issued_ago(&rig.ledger, OBSERVER, "pod-1", 6 * 60).await;
        rig.voucher
            .send_message(offered(
                &offer(OBSERVER, Some("pod-1"), CertType::Client),
                Some(CERT_ISSUER),
            ))
            .unwrap();
        settle().await;
        let published = rig.published.lock().unwrap().clone();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].1.decision, HeldDecision::Vouch);
    }

    #[tokio::test]
    async fn while_its_cassini_session_is_lost_it_lists_and_answers_over_mtls_and_not_otherwise() {
        let server = MockServer::start().await;
        let o = offer(CASSINI, Some("pod-9"), CertType::Server);
        Mock::given(method("GET"))
            .and(path("/internal/held"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::held::HeldList {
                    held: vec![o.clone()],
                },
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/internal/held/h1"))
            .and(body_json(HeldDecision::Vouch))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"taken": true})),
            )
            .mount(&server)
            .await;
        let rig = rig(HandshakeClient::new(server.uri())).await;
        rig.voucher.send_message(HeldVoucherMessage::Poll).unwrap();
        settle().await;
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "Cassini up: offers come over it"
        );
        rig.voucher
            .send_message(HeldVoucherMessage::CassiniLost)
            .unwrap();
        rig.voucher.send_message(HeldVoucherMessage::Poll).unwrap();
        settle().await;
        let paths: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| r.url.path().to_string())
            .collect();
        assert_eq!(paths, vec!["/internal/held", "/internal/held/h1"]);
        rig.voucher
            .send_message(HeldVoucherMessage::CassiniUp)
            .unwrap();
        rig.voucher.send_message(HeldVoucherMessage::Poll).unwrap();
        settle().await;
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            2,
            "back over Cassini: no more polling"
        );
    }
}
