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

//! cert-issuer run whole against a real broker that is restarted again and again, and the credentials agent's side of
//! the held requests over it (docs/architecture/core-restarts.md, "3 in detail"; docs/architecture/broker-baseline.md,
//! "The fifth set", "Found on the way": issuance stopped after the broker's tenth restart in a row, until cert-issuer
//! was restarted).
//!
//! Each restart, orderly or at once, comes as a bootstrap service's new pod has just been held and its offer has reached
//! the credentials agent, so the answer is on its way with no session to carry it: the pod, asking again as polar-init
//! does, is signed, and so is a pod that comes after the restart. Neither side is restarted. The credentials agent's
//! side is its own record asker, ledger and voucher on the client facade every agent's client is, doing on each
//! registration and each event what its supervisor does. Before the voucher kept its answers, the pod held across the
//! restart was refused as "too soon" (core-restarts.md, "One answer to a request").

mod agent_rig;
mod helpers;
#[path = "../../../cassini/broker/tests/support/mod.rs"]
mod support;

use agent_rig::*;
use cassini_client::facade::{Backoff, FacadeArgs, TcpClientFacade, TlsTransportSpawner};
use cassini_client::{DeliveryMode, TCPClientConfig, TcpClientMessage};
use cassini_types::{BrokerMessage, ClientEvent};
use cert_client::handshake::{HandshakeClient, HandshakeError};
use cert_issuer_common::CertType;
use cert_issuer_common::held::{HELD_REPLIES_TOPIC, HELD_REQUESTS_TOPIC};
use cert_issuer_common::keypair::{KeyAlgorithm, generate_csr};
use cert_issuer_common::record::{RECORD_REPLIES_TOPIC, RECORD_REQUESTS_TOPIC};
use cert_issuer_common::ticket::SUCCESSOR_TICKETS_TOPIC;
use credentials_agent_consumer::held_voucher::{HeldVoucher, HeldVoucherArgs, HeldVoucherMessage};
use credentials_agent_consumer::ledger::{LedgerActor, LedgerActorArgs};
use credentials_agent_consumer::projection::ProjectionMessage;
use credentials_agent_consumer::record_asker::{RecordAsker, RecordAskerArgs, RecordAskerMessage};
use helpers::*;
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use support::TestBroker;

const CREDENTIALS_AGENT: &str = "credentials-agent.polar.serviceaccount.cluster.local";
const CASSINI_WORKLOAD: &str = "system:serviceaccount:polar:cassini";
const CASSINI: &str = "cassini.polar.serviceaccount.cluster.local";
const CASSINI_NAME: &str = "cassini-ip-svc.polar.svc.cluster.local";

/// The broker's restarts in a row: more than the ten the fifth set saw issuance stop at.
const RESTARTS: usize = 12;

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

/// What the credentials agent's supervisor does with its client's events, for the record and the held requests: the
/// builder subscribes again on each registration; the voucher is told the session is up or lost; the record is asked
/// for at once and on an interval (here a second, where the agent's is 30 s, and its mTLS poll 10 s).
///
/// While the test says, an offer that arrives is kept from the voucher until the test releases it: an offer delivered
/// as the broker goes, whose answer is then on its way with no session to carry it.
struct CredentialsAgentSide;

enum SideMessage {
    Event(ClientEvent),
    /// The client may have registered before this side was told of its events.
    IfOnline,
    Tick,
    /// The offers kept, to the voucher.
    Release,
}

struct SideArgs {
    client: ActorRef<TcpClientMessage>,
    online: Arc<AtomicBool>,
    asker: ActorRef<RecordAskerMessage>,
    voucher: ActorRef<HeldVoucherMessage>,
    registrations: Arc<AtomicUsize>,
    /// Offers that arrived over Cassini.
    offers: Arc<AtomicUsize>,
    /// Whether an offer that arrives is kept until `Release`.
    keep: Arc<AtomicBool>,
    kept: Vec<(Vec<u8>, Option<String>)>,
}

impl SideArgs {
    fn registered(&self) {
        self.registrations.fetch_add(1, Ordering::SeqCst);
        for topic in [HELD_REQUESTS_TOPIC, RECORD_REPLIES_TOPIC] {
            let _ = self.client.send_message(TcpClientMessage::Subscribe {
                topic: topic.to_string(),
                trace_ctx: None,
            });
        }
        let _ = self.voucher.send_message(HeldVoucherMessage::CassiniUp);
        let _ = self.asker.send_message(RecordAskerMessage::Ask);
    }
}

#[ractor::async_trait]
impl Actor for CredentialsAgentSide {
    type Msg = SideMessage;
    type State = SideArgs;
    type Arguments = SideArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: SideArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        myself.send_interval(Duration::from_secs(1), || SideMessage::Tick);
        Ok(args)
    }

    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SideMessage::IfOnline if state.online.load(Ordering::SeqCst) => state.registered(),
            SideMessage::IfOnline => {}
            SideMessage::Event(ClientEvent::Registered { .. }) => state.registered(),
            SideMessage::Event(ClientEvent::TransportError { .. }) => {
                let _ = state.voucher.send_message(HeldVoucherMessage::CassiniLost);
            }
            SideMessage::Event(ClientEvent::MessagePublished {
                topic,
                payload,
                published_by,
                ..
            }) => {
                if topic == RECORD_REPLIES_TOPIC {
                    let _ = state.asker.send_message(RecordAskerMessage::Answered {
                        payload,
                        published_by,
                    });
                } else if topic == HELD_REQUESTS_TOPIC {
                    state.offers.fetch_add(1, Ordering::SeqCst);
                    if state.keep.load(Ordering::SeqCst) {
                        state.kept.push((payload, published_by));
                    } else {
                        let _ = state.voucher.send_message(HeldVoucherMessage::Offered {
                            payload,
                            published_by,
                        });
                    }
                }
            }
            SideMessage::Event(_) => {}
            SideMessage::Release => {
                state.keep.store(false, Ordering::SeqCst);
                for (payload, published_by) in state.kept.drain(..) {
                    let _ = state.voucher.send_message(HeldVoucherMessage::Offered {
                        payload,
                        published_by,
                    });
                }
            }
            SideMessage::Tick => {
                let _ = state.asker.send_message(RecordAskerMessage::Ask);
                let _ = state.voucher.send_message(HeldVoucherMessage::Poll);
            }
        }
        Ok(())
    }
}

/// A new pod of the bootstrap service, numbered `pod`, asking on its token alone, with one CSR however often it asks.
struct Pod {
    token: String,
    csr_pem: String,
}

/// What one ask came to.
#[derive(Debug, PartialEq, Eq)]
enum Asked {
    Signed,
    Held,
    Unreachable,
    Refused(String),
}

impl Pod {
    fn new(pod: usize) -> Self {
        let token = mint_token(
            TEST_KID,
            primary_keypair(),
            serde_json::json!({
                "iss": TEST_ISSUER,
                "aud": TEST_AUDIENCE,
                "sub": CASSINI_WORKLOAD,
                "exp": now_plus(3600),
                "kubernetes.io": {
                    "namespace": "polar",
                    "pod": { "name": format!("cassini-{pod}"), "uid": format!("00000000-0000-0000-0000-{pod:012}") }
                }
            }),
        );
        let csr = generate_csr(
            CASSINI,
            &KeyAlgorithm::EcdsaP256,
            &[CASSINI_NAME.to_string()],
        )
        .expect("a CSR");
        Self {
            token,
            csr_pem: csr.csr_pem,
        }
    }

    async fn ask(&self, cert_issuer: &HandshakeClient) -> Asked {
        match cert_issuer
            .issue(
                &self.token,
                &self.csr_pem,
                CertType::Server,
                vec![CASSINI_NAME.to_string()],
                None,
            )
            .await
        {
            Ok(_) => Asked::Signed,
            Err(HandshakeError::Held(_)) => Asked::Held,
            Err(HandshakeError::Unreachable(_)) => Asked::Unreachable,
            Err(other) => Asked::Refused(other.to_string()),
        }
    }

    /// Asked until it is held: `Err` if it is signed at once (nothing held: the floor state had its entry open) or
    /// refused.
    async fn held(&self, cert_issuer: &HandshakeClient) -> Result<(), String> {
        let began = Instant::now();
        loop {
            match self.ask(cert_issuer).await {
                Asked::Held => return Ok(()),
                Asked::Unreachable if began.elapsed() < Duration::from_secs(30) => {}
                other => return Err(format!("not held: {other:?}")),
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Asked again until it is signed, as polar-init asks: `Err` if it is refused or still held after `within`.
    async fn signed(&self, cert_issuer: &HandshakeClient, within: Duration) -> Result<(), String> {
        let began = Instant::now();
        loop {
            match self.ask(cert_issuer).await {
                Asked::Signed => return Ok(()),
                Asked::Refused(why) => return Err(format!("refused: {why}")),
                Asked::Held | Asked::Unreachable if began.elapsed() > within => {
                    return Err(format!("held, and not signed after {within:?}"));
                }
                Asked::Held | Asked::Unreachable => {}
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

#[test]
fn issuance_through_held_requests_survives_the_broker_restarting_again_and_again() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let broker = runtime.block_on(TestBroker::start_enforcing());
    let ca = broker.pki.ca().to_string_lossy().into_owned();
    let ca_key = broker.pki.ca_key().to_string_lossy().into_owned();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&ca_key, std::fs::Permissions::from_mode(0o600)).unwrap();
    // cert-issuer's floor: its own entry with the topics its scaffold declares, and a bootstrap service's.
    let floor = serde_json::json!([
        {
            "workload": WORKLOAD, "cert_type": "CLIENT", "names": [], "groups": ["cassini-client"],
            "publish": [RECORD_REPLIES_TOPIC, HELD_REQUESTS_TOPIC],
            "subscribe": [RECORD_REQUESTS_TOPIC, HELD_REPLIES_TOPIC, SUCCESSOR_TICKETS_TOPIC]
        },
        { "workload": CASSINI_WORKLOAD, "cert_type": "SERVER", "names": [CASSINI_NAME], "groups": [] }
    ])
    .to_string();
    broker.pki.issue_client_carrying(
        CREDENTIALS_AGENT,
        &[
            format!("p:pub:{RECORD_REQUESTS_TOPIC}"),
            format!("p:sub:{RECORD_REPLIES_TOPIC}"),
            format!("p:sub:{HELD_REQUESTS_TOPIC}"),
            format!("p:pub:{HELD_REPLIES_TOPIC}"),
            "g:cassini-client".to_string(),
        ],
    );
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    let _rig = Rig::start(&[
        ("CERT_ISSUER_BOOTSTRAP", floor.as_str()),
        ("CERT_ISSUER_BIND_ADDR", bind.as_str()),
        ("CERT_ISSUER_SERVER_IDENTITY", "localhost"),
        ("BROKER_ADDR", broker.addr.as_str()),
        ("CASSINI_SERVER_NAME", "localhost"),
        ("CERT_ISSUER_CA_PROVISIONING", "provisioned"),
        ("CERT_ISSUER_CA_CERT_PATH", ca.as_str()),
        ("CERT_ISSUER_CA_KEY_PATH", ca_key.as_str()),
        ("CERT_ISSUER_INTERNAL_CALLERS", CREDENTIALS_AGENT),
    ]);
    let url = format!("https://localhost:{port}");
    let ca_pem = std::fs::read(&ca).unwrap();
    let pod_side = HandshakeClient::with_root_ca(url.clone(), &ca_pem).unwrap();
    let (cert, key) = broker.pki.client(CREDENTIALS_AGENT);
    let identity = [std::fs::read(&cert).unwrap(), std::fs::read(&key).unwrap()].concat();
    let mtls = HandshakeClient::with_root_ca_and_identity(url, &ca_pem, &identity).unwrap();

    // The credentials agent's side.
    let registrations = Arc::new(AtomicUsize::new(0));
    let offers = Arc::new(AtomicUsize::new(0));
    let keep = Arc::new(AtomicBool::new(false));
    let online = Arc::new(AtomicBool::new(false));
    let ledger_dir = tempfile::tempdir().unwrap();
    let side = runtime.block_on(async {
        let events = Arc::new(OutputPort::default());
        let (client, _) = Actor::spawn(
            None,
            TcpClientFacade,
            FacadeArgs {
                config: TCPClientConfig::from_values(
                    broker.addr.clone(),
                    "localhost".to_string(),
                    ca.clone(),
                    cert.to_string_lossy().into_owned(),
                    key.to_string_lossy().into_owned(),
                ),
                delivery: DeliveryMode::Broadcast(events.clone()),
                transport_name: "credentials-agent-transport".to_string(),
                spawner: Arc::new(TlsTransportSpawner),
                online: online.clone(),
                gate: None,
                watch: None,
                credit: Arc::new(cassini_client::credit::Credit::default()),
                backoff: Backoff {
                    first: Duration::from_secs(1),
                    last: Duration::from_secs(30),
                },
            },
        )
        .await
        .unwrap();
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
                client: client.clone(),
                ledger: ledger.clone(),
                cert_issuer: IDENTITY.to_string(),
                bootstrap: Some([CASSINI.to_string()].into()),
            },
        )
        .await
        .unwrap();
        let (voucher, _) = Actor::spawn(
            None,
            HeldVoucher,
            HeldVoucherArgs {
                client: client.clone(),
                ledger,
                cert_issuer: IDENTITY.to_string(),
                bootstrap: [CASSINI.to_string()].into(),
                own_identity: CREDENTIALS_AGENT.to_string(),
                cert_issuer_http: mtls,
            },
        )
        .await
        .unwrap();
        let (side, _) = Actor::spawn(
            None,
            CredentialsAgentSide,
            SideArgs {
                client,
                online: online.clone(),
                asker,
                voucher,
                registrations: registrations.clone(),
                offers: offers.clone(),
                keep: keep.clone(),
                kept: Vec::new(),
            },
        )
        .await
        .unwrap();
        events.subscribe(side.clone(), |event| Some(SideMessage::Event(event)));
        side.send_message(SideMessage::IfOnline).unwrap();
        side
    });

    // Before any restart: the held path works.
    runtime.block_on(async {
        let pod = Pod::new(0);
        pod.held(&pod_side)
            .await
            .unwrap_or_else(|why| panic!("before any restart: {why}"));
        pod.signed(&pod_side, Duration::from_secs(60))
            .await
            .unwrap_or_else(|why| panic!("before any restart: {why}"));
    });

    // Each restart comes as a new pod's request has just been held and offered: the offer reaches the credentials agent
    // as the broker goes, and its answer is on its way with no session to carry it. The pod asks again until it is
    // signed, as polar-init does. Then a pod that comes after the restart is held and signed as before.
    for restart in 1..=RESTARTS {
        let pod = Pod::new(restart);
        let offered = offers.load(Ordering::SeqCst);
        keep.store(true, Ordering::SeqCst);
        runtime
            .block_on(pod.held(&pod_side))
            .unwrap_or_else(|why| panic!("before the broker's restart {restart}: {why}"));
        let began = Instant::now();
        while offers.load(Ordering::SeqCst) == offered {
            assert!(
                began.elapsed() < Duration::from_secs(30),
                "restart {restart}: the held request was never offered"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let registered = registrations.load(Ordering::SeqCst);
        runtime.block_on(async {
            // Every other one in order, as a broker ends by its health verdict or a rollout, the rest at once.
            if restart % 2 == 0 {
                broker
                    .actor()
                    .cast(BrokerMessage::PrepareForShutdown { auth_token: None })
                    .unwrap();
                assert!(
                    broker.ends_by_itself(Duration::from_secs(20)).await,
                    "restart {restart}: the orderly shutdown never finished"
                );
            }
            broker.stop().await;
            while online.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            side.send_message(SideMessage::Release).unwrap();
            broker.restart(Duration::from_secs(3)).await;
        });
        let began = Instant::now();
        while registrations.load(Ordering::SeqCst) == registered {
            assert!(
                began.elapsed() < Duration::from_secs(30),
                "restart {restart}: the credentials agent never registered again"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        runtime
            .block_on(pod.signed(&pod_side, Duration::from_secs(60)))
            .unwrap_or_else(|why| {
                panic!("the pod held across the broker's restart {restart} in a row: {why}")
            });
        let after = Pod::new(100 + restart);
        runtime.block_on(async {
            after
                .held(&pod_side)
                .await
                .unwrap_or_else(|why| panic!("after the broker's restart {restart}: {why}"));
            after
                .signed(&pod_side, Duration::from_secs(60))
                .await
                .unwrap_or_else(|why| panic!("after the broker's restart {restart}: {why}"));
        });
    }
    runtime.block_on(broker.stop());
}
