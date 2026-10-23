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

//! The credentials agent's side of cert-issuer's record (docs/architecture/cert-issuer-isolation.md, "Step 2"): it asks
//! for every entry after the last it holds, when it registers and on an interval, and at once again while an answer says
//! there is more; and it takes an answer only from cert-issuer's identity, as the broker verified it. What it is told
//! goes to the ledger, which records the certificates it did not hold, so that no certificate cert-issuer signs stays
//! unknown to the credentials agent.

use crate::ledger::LedgerMessage;
use cassini_client::TcpClientMessage;
use cert_issuer_common::record::{RECORD_REQUESTS_TOPIC, RecordReply, RecordRequest};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::collections::BTreeSet;
use tracing::warn;

pub enum RecordAskerMessage {
    /// Ask for what follows the ledger's cursor.
    Ask,
    /// An answer arrived, published (the broker says) by `published_by`.
    Answered {
        payload: Vec<u8>,
        published_by: Option<String>,
    },
}

pub struct RecordAskerArgs {
    pub client: ActorRef<TcpClientMessage>,
    pub ledger: ActorRef<LedgerMessage>,
    /// cert-issuer's identity, as the authority model knows its workload.
    pub cert_issuer: String,
    /// The identities of the bootstrap services, for what is unexpected.
    pub bootstrap: Option<BTreeSet<String>>,
}

pub struct RecordAsker;

/// The workload whose record is asked for, as the catalog names it.
pub const CERT_ISSUER_WORKLOAD: &str = "cert-issuer";

/// How often the record is asked for, beside each registration: short against a certificate's hour.
pub const ASK_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// Whom to ask, from the authority model: cert-issuer's one identity, and the identities of the bootstrap services, for
/// what is unexpected. None with no model, or one with no cert-issuer: then there is no one to ask.
pub fn record_target(
    model: Option<&credentials_agent_model::Model>,
) -> Option<(String, BTreeSet<String>)> {
    let model = model?;
    let cert_issuer = model.workload(CERT_ISSUER_WORKLOAD)?.identity.clone();
    let bootstrap = model
        .workloads
        .iter()
        .filter(|w| w.bootstrap)
        .map(|w| w.identity.clone())
        .collect();
    Some((cert_issuer, bootstrap))
}

/// The answer in `payload`, if it is cert-issuer's: published, the broker says, by `cert_issuer`, and readable.
pub fn accepted(
    payload: &[u8],
    published_by: Option<&str>,
    cert_issuer: &str,
) -> Result<RecordReply, String> {
    match published_by {
        Some(who) if who == cert_issuer => {}
        Some(who) => {
            return Err(format!(
                "{who} answered for cert-issuer's record, and only {cert_issuer} may"
            ));
        }
        None => {
            return Err(
                "an answer for cert-issuer's record with no verified publisher".to_string(),
            );
        }
    }
    serde_json::from_slice(payload)
        .map_err(|e| format!("an answer for cert-issuer's record that does not read: {e}"))
}

#[ractor::async_trait]
impl Actor for RecordAsker {
    type Msg = RecordAskerMessage;
    type State = RecordAskerArgs;
    type Arguments = RecordAskerArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: RecordAskerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(args)
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            RecordAskerMessage::Ask => {
                let cursor =
                    ractor::call!(state.ledger, |reply| LedgerMessage::RecordCursor { reply })?;
                let request = match cursor {
                    Some((log, after)) => RecordRequest {
                        log: Some(log),
                        after,
                    },
                    None => RecordRequest {
                        log: None,
                        after: 0,
                    },
                };
                state.client.send_message(TcpClientMessage::publish(
                    RECORD_REQUESTS_TOPIC.to_string(),
                    serde_json::to_vec(&request)?,
                    None,
                ))?;
            }
            RecordAskerMessage::Answered {
                payload,
                published_by,
            } => {
                let page = match accepted(&payload, published_by.as_deref(), &state.cert_issuer) {
                    Ok(page) => page,
                    Err(why) => {
                        warn!("not taken: {why}");
                        return Ok(());
                    }
                };
                let more = page.more;
                let taken = ractor::call!(state.ledger, |reply| LedgerMessage::TakeRecordPage {
                    page,
                    bootstrap: state.bootstrap.clone(),
                    reply,
                })?;
                match taken {
                    // There is more: ask again at once, from where the ledger has read to.
                    Ok(_) if more => myself.send_message(RecordAskerMessage::Ask)?,
                    Ok(_) => {}
                    Err(e) => warn!("a page of cert-issuer's record could not be recorded: {e}"),
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cert_issuer_common::record::{IssuePath, RECORD_PAGE, RecordEntry};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const CERT_ISSUER: &str = "cert-issuer.polar.serviceaccount.cluster.local";
    const CASSINI: &str = "cassini.polar.serviceaccount.cluster.local";

    /// Every request published, read back.
    struct Client(Arc<Mutex<Vec<(String, RecordRequest)>>>);

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
        asker: ActorRef<RecordAskerMessage>,
        ledger: ActorRef<LedgerMessage>,
        asked: Arc<Mutex<Vec<(String, RecordRequest)>>>,
        _dir: tempfile::TempDir,
    }

    async fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let ledger =
            crate::ledger::tests::spawn_ledger(dir.path().join("events.jsonl").to_str().unwrap())
                .await;
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (client, _) = Actor::spawn(None, Client(asked.clone()), ()).await.unwrap();
        let (asker, _) = Actor::spawn(
            None,
            RecordAsker,
            RecordAskerArgs {
                client,
                ledger: ledger.clone(),
                cert_issuer: CERT_ISSUER.to_string(),
                bootstrap: Some([CASSINI.to_string()].into_iter().collect()),
            },
        )
        .await
        .unwrap();
        Rig {
            asker,
            ledger,
            asked,
            _dir: dir,
        }
    }

    fn entry(seq: u64, serial: &str) -> RecordEntry {
        RecordEntry {
            log: "log-1".to_string(),
            seq,
            serial: serial.to_string(),
            identity: CASSINI.to_string(),
            cert_type: cert_issuer_common::CertType::Client,
            path: IssuePath::TokenOnly,
            instance_binding: None,
            signed_at: 1000,
            not_after: 5000,
            certificate_pem: format!("pem {serial}"),
        }
    }

    fn answer(entries: Vec<RecordEntry>, more: bool) -> Vec<u8> {
        serde_json::to_vec(&RecordReply {
            log: "log-1".to_string(),
            entries,
            more,
        })
        .unwrap()
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    async fn cursor(ledger: &ActorRef<LedgerMessage>) -> Option<(String, u64)> {
        ractor::call!(ledger, |reply| LedgerMessage::RecordCursor { reply }).unwrap()
    }

    #[tokio::test]
    async fn it_asks_from_the_ledger_s_cursor() {
        let r = rig().await;
        r.asker.send_message(RecordAskerMessage::Ask).unwrap();
        settle().await;
        assert_eq!(
            r.asked.lock().unwrap().clone(),
            vec![(
                RECORD_REQUESTS_TOPIC.to_string(),
                RecordRequest {
                    log: None,
                    after: 0
                }
            )]
        );
        r.asker
            .send_message(RecordAskerMessage::Answered {
                payload: answer(vec![entry(1, "c1"), entry(2, "c2")], false),
                published_by: Some(CERT_ISSUER.to_string()),
            })
            .unwrap();
        settle().await;
        r.asker.send_message(RecordAskerMessage::Ask).unwrap();
        settle().await;
        assert_eq!(
            r.asked.lock().unwrap().last().unwrap().1,
            RecordRequest {
                log: Some("log-1".to_string()),
                after: 2
            },
            "from where the ledger has read to"
        );
    }

    #[tokio::test]
    async fn cert_issuer_s_answer_goes_to_the_ledger_and_more_is_asked_for_at_once() {
        let r = rig().await;
        r.asker
            .send_message(RecordAskerMessage::Answered {
                payload: answer(vec![entry(1, "c1")], true),
                published_by: Some(CERT_ISSUER.to_string()),
            })
            .unwrap();
        settle().await;
        assert_eq!(cursor(&r.ledger).await, Some(("log-1".to_string(), 1)));
        assert_eq!(
            r.asked.lock().unwrap().clone(),
            vec![(
                RECORD_REQUESTS_TOPIC.to_string(),
                RecordRequest {
                    log: Some("log-1".to_string()),
                    after: 1
                }
            )],
            "there is more: it asks again"
        );
    }

    #[tokio::test]
    async fn an_answer_that_says_there_is_no_more_is_taken_and_nothing_more_is_asked() {
        let r = rig().await;
        r.asker
            .send_message(RecordAskerMessage::Answered {
                payload: answer(vec![entry(1, "c1")], false),
                published_by: Some(CERT_ISSUER.to_string()),
            })
            .unwrap();
        settle().await;
        assert_eq!(cursor(&r.ledger).await, Some(("log-1".to_string(), 1)));
        assert!(
            r.asked.lock().unwrap().is_empty(),
            "the record is read to its end: it waits for the timer"
        );
    }

    #[tokio::test]
    async fn an_answer_from_anyone_else_or_no_one_is_not_taken() {
        let r = rig().await;
        for who in [
            Some("kube-observer.polar.serviceaccount.cluster.local".to_string()),
            None,
        ] {
            r.asker
                .send_message(RecordAskerMessage::Answered {
                    payload: answer(vec![entry(1, "forged")], true),
                    published_by: who,
                })
                .unwrap();
        }
        settle().await;
        assert_eq!(cursor(&r.ledger).await, None, "nothing taken");
        assert!(
            r.asked.lock().unwrap().is_empty(),
            "and nothing asked for on its word"
        );
    }

    #[test]
    fn cert_issuer_and_the_bootstrap_services_are_known_from_the_model() {
        let model = credentials_agent_model::Model::parse(
            &serde_json::json!({
                "format": 1,
                "workloads": [
                    { "workload": "cert-issuer", "identity": CERT_ISSUER, "issuer": "cert-issuer", "bootstrap": true },
                    { "workload": "cassini", "identity": CASSINI, "issuer": "cert-issuer", "bootstrap": true },
                    { "workload": "kube-observer", "identity": "kube-observer.polar.serviceaccount.cluster.local", "issuer": "credentials-agent", "bootstrap": false }
                ]
            })
            .to_string(),
        )
        .unwrap();
        let (who, bootstrap) = record_target(Some(&model)).unwrap();
        assert_eq!(who, CERT_ISSUER);
        assert_eq!(
            bootstrap,
            [CERT_ISSUER.to_string(), CASSINI.to_string()]
                .into_iter()
                .collect()
        );

        assert!(record_target(None).is_none(), "no model, no one to ask");
        let without = credentials_agent_model::Model::parse(
            &serde_json::json!({ "format": 1, "workloads": [
                { "workload": "cassini", "identity": CASSINI, "issuer": "cert-issuer", "bootstrap": true }
            ] })
            .to_string(),
        )
        .unwrap();
        assert!(
            record_target(Some(&without)).is_none(),
            "a model with no cert-issuer"
        );
    }

    #[test]
    fn an_answer_is_accepted_only_from_cert_issuer_and_only_when_it_reads() {
        let good = answer(vec![], false);
        assert!(accepted(&good, Some(CERT_ISSUER), CERT_ISSUER).is_ok());
        assert!(
            accepted(
                &good,
                Some("someone.polar.serviceaccount.cluster.local"),
                CERT_ISSUER
            )
            .is_err()
        );
        assert!(accepted(&good, None, CERT_ISSUER).is_err());
        assert!(accepted(b"not an answer", Some(CERT_ISSUER), CERT_ISSUER).is_err());
        let _ = RECORD_PAGE;
    }
}
