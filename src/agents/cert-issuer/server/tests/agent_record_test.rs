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

//! cert-issuer run whole against a real broker that enforces, and the credentials agent's side of the record over it
//! (docs/architecture/cert-issuer-isolation.md, "Step 2"): the credentials agent's asker, with its ledger, asks for the
//! record over Cassini and is answered by cert-issuer, as the broker knows it, until the ledger holds the certificates
//! cert-issuer signed itself, as reported ones. Each side may use the record's topics because its certificate names
//! them, written from its floor entry as the catalog writes it from the scaffold; no policy is set. Anyone else is kept
//! out twice: the broker refuses a certificate that does not name the topics, and cert-issuer does not answer one that
//! does but is not the credentials agent's.

mod agent_rig;
mod helpers;
#[path = "../../../cassini/broker/tests/support/mod.rs"]
mod support;

use agent_rig::*;
use cert_issuer_common::record::{
    IssuePath, RECORD_REPLIES_TOPIC, RECORD_REQUESTS_TOPIC, RecordReply, RecordRequest,
};
use credentials_agent_consumer::ledger::{LedgerActor, LedgerActorArgs, LedgerMessage};
use credentials_agent_consumer::projection::ProjectionMessage;
use credentials_agent_consumer::record_asker::{RecordAsker, RecordAskerArgs, RecordAskerMessage};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::time::{Duration, Instant};
use support::{TestBroker, TestClient};

const CREDENTIALS_AGENT: &str = "credentials-agent.polar.serviceaccount.cluster.local";
/// An agent whose certificate does not name the record's topics.
const IMPOSTOR: &str = "observer.polar.serviceaccount.cluster.local";
/// An agent whose certificate names the requests topic, as if granted it in error.
const STRAY: &str = "scheduler.polar.serviceaccount.cluster.local";

/// The graph, which this test does not look at.
struct NoGraph;

#[ractor::async_trait]
impl Actor for NoGraph {
    type Msg = ProjectionMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(&self, _: ActorRef<Self::Msg>, _: ()) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        _: Self::Msg,
        _: &mut (),
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
}

#[test]
fn the_credentials_agent_asks_over_cassini_and_its_ledger_learns_what_cert_issuer_alone_answers() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    // No policy: what each certificate names decides, and anything else is refused.
    let broker = runtime.block_on(TestBroker::start_enforcing());
    let ca = broker.pki.ca().to_string_lossy().into_owned();
    let ca_key = broker.pki.ca_key().to_string_lossy().into_owned();
    // cert-issuer loads a CA key no one else may read, and no other.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&ca_key, std::fs::Permissions::from_mode(0o600)).unwrap();
    // Its own floor entry, with the topics its scaffold declares, which it writes into the certificate it signs itself.
    let floor = serde_json::json!([{
        "workload": WORKLOAD, "cert_type": "CLIENT", "names": [], "groups": ["cassini-client"],
        "publish": [RECORD_REPLIES_TOPIC], "subscribe": [RECORD_REQUESTS_TOPIC]
    }])
    .to_string();
    // The credentials agent's, as its floor entry names them; the others'.
    broker.pki.issue_client_carrying(
        CREDENTIALS_AGENT,
        &[
            format!("p:pub:{RECORD_REQUESTS_TOPIC}"),
            format!("p:sub:{RECORD_REPLIES_TOPIC}"),
            "g:cassini-client".to_string(),
        ],
    );
    broker
        .pki
        .issue_client_carrying(IMPOSTOR, &["g:cassini-client".to_string()]);
    broker.pki.issue_client_carrying(
        STRAY,
        &[
            format!("p:pub:{RECORD_REQUESTS_TOPIC}"),
            "g:cassini-client".to_string(),
        ],
    );
    // cert-issuer as the broker's own CA, so the broker trusts the certificate it signs itself.
    let _rig = Rig::start(&[
        ("CERT_ISSUER_BOOTSTRAP", floor.as_str()),
        ("BROKER_ADDR", broker.addr.as_str()),
        ("CASSINI_SERVER_NAME", "localhost"),
        ("CERT_ISSUER_CA_PROVISIONING", "provisioned"),
        ("CERT_ISSUER_CA_CERT_PATH", ca.as_str()),
        ("CERT_ISSUER_CA_KEY_PATH", ca_key.as_str()),
        ("CERT_ISSUER_INTERNAL_CALLERS", CREDENTIALS_AGENT),
    ]);

    // The credentials agent: its connection, its ledger and its asker.
    let credentials_agent = runtime.block_on(TestClient::connect(&broker, CREDENTIALS_AGENT));
    credentials_agent.subscribe(RECORD_REPLIES_TOPIC);
    let ledger_dir = tempfile::tempdir().unwrap();
    let (ledger, asker) = runtime.block_on(async {
        let (graph, _) = Actor::spawn(None, NoGraph, ()).await.unwrap();
        let (ledger, _) = Actor::spawn(
            None,
            LedgerActor,
            LedgerActorArgs {
                log_path: ledger_dir
                    .path()
                    .join("events.jsonl")
                    .to_string_lossy()
                    .into_owned(),
                projection_actor: graph,
            },
        )
        .await
        .unwrap();
        let (asker, _) = Actor::spawn(
            None,
            RecordAsker,
            RecordAskerArgs {
                client: credentials_agent.transport.clone(),
                ledger: ledger.clone(),
                cert_issuer: IDENTITY.to_string(),
                bootstrap: Some([IDENTITY.to_string()].into()),
            },
        )
        .await
        .unwrap();
        (ledger, asker)
    });
    let cursor = |ledger: &ActorRef<LedgerMessage>| {
        runtime
            .block_on(async {
                ractor::call!(ledger, |reply| LedgerMessage::RecordCursor { reply })
            })
            .unwrap()
    };

    // Asked until cert-issuer, once it is registered and subscribed, answers, each answer handed to the asker as the
    // credentials agent's supervisor hands it, until the ledger has read the two certificates cert-issuer signs at
    // start: its TLS server certificate, signed for itself, and its client certificate, on its own token through its
    // floor.
    let mut handed = 0;
    let began = Instant::now();
    while cursor(&ledger).map(|(_, seq)| seq) != Some(2) {
        assert!(
            began.elapsed() < Duration::from_secs(30),
            "the ledger never read cert-issuer's record: {:?}",
            cursor(&ledger)
        );
        let replies = credentials_agent.received_from(RECORD_REPLIES_TOPIC);
        if replies.len() == handed {
            asker.send_message(RecordAskerMessage::Ask).unwrap();
        }
        for (payload, published_by) in replies[handed..].iter().cloned() {
            asker
                .send_message(RecordAskerMessage::Answered {
                    payload,
                    published_by,
                })
                .unwrap();
        }
        handed = replies.len();
        std::thread::sleep(Duration::from_millis(500));
    }

    let replies = credentials_agent.received_from(RECORD_REPLIES_TOPIC);
    assert!(
        replies
            .iter()
            .all(|(_, by)| by.as_deref() == Some(IDENTITY)),
        "published by cert-issuer as the broker knows it"
    );
    let reply: RecordReply = serde_json::from_slice(&replies.last().unwrap().0).unwrap();
    let signed: Vec<_> = reply
        .entries
        .iter()
        .map(|e| (e.identity.as_str(), e.path))
        .collect();
    assert_eq!(
        signed,
        vec![
            (SERVER_IDENTITY, IssuePath::Own),
            (IDENTITY, IssuePath::TokenOnly),
        ],
        "{reply:?}"
    );
    assert_eq!(
        reply.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(!reply.more);

    // Its client certificate is the ledger's now, as a reported one, found as an issued one is.
    let client = reply
        .entries
        .iter()
        .find(|e| e.path == IssuePath::TokenOnly)
        .unwrap();
    let held = runtime
        .block_on(async {
            ractor::call!(ledger, |reply| LedgerMessage::CheckCertificate {
                identity: IDENTITY.to_string(),
                reply
            })
        })
        .unwrap()
        .expect("the ledger holds cert-issuer's client certificate");
    assert_eq!(held.serial, client.serial);
    assert_eq!(held.certificate_pem, client.certificate_pem);
    assert!(held.reported);

    // Kept out by the broker: a certificate that does not name the record's topics may neither ask nor read.
    let impostor = runtime.block_on(TestClient::connect(&broker, IMPOSTOR));
    let request = serde_json::to_vec(&RecordRequest {
        log: None,
        after: 0,
    })
    .unwrap();
    impostor.subscribe(RECORD_REPLIES_TOPIC);
    impostor.publish(RECORD_REQUESTS_TOPIC, &request);
    runtime.block_on(
        impostor.wait_for("the broker's refusals", Duration::from_secs(10), |c| {
            !c.refusals(RECORD_REQUESTS_TOPIC).is_empty()
                && !c.refusals(RECORD_REPLIES_TOPIC).is_empty()
        }),
    );

    // Kept out by cert-issuer: one the broker lets ask, which is not the credentials agent, is not answered; the
    // credentials agent, asking after, is.
    let stray = runtime.block_on(TestClient::connect(&broker, STRAY));
    let answered = credentials_agent.received_from(RECORD_REPLIES_TOPIC).len();
    stray.publish(RECORD_REQUESTS_TOPIC, &request);
    runtime.block_on(
        stray.wait_for("the broker taking it", Duration::from_secs(10), |c| {
            c.acknowledged(RECORD_REQUESTS_TOPIC) >= 1
        }),
    );
    assert!(
        stray.refusals(RECORD_REQUESTS_TOPIC).is_empty(),
        "the broker lets it ask"
    );
    asker.send_message(RecordAskerMessage::Ask).unwrap();
    let began = Instant::now();
    while credentials_agent.received_from(RECORD_REPLIES_TOPIC).len() == answered {
        assert!(
            began.elapsed() < Duration::from_secs(30),
            "cert-issuer never answered again"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(
        credentials_agent.received_from(RECORD_REPLIES_TOPIC).len(),
        answered + 1,
        "one answer, to the credentials agent alone"
    );
    assert!(impostor.received_from(RECORD_REPLIES_TOPIC).is_empty());

    asker.stop(None);
    ledger.stop(None);
    runtime.block_on(broker.stop());
}
