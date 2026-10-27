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

//! The daemon watches for its own revocation (docs/architecture/key-custody.md, step 5).
//!
//! Once its client slot holds a certificate, it connects to Cassini as the pod, its handshake signed in process
//! (`polar_custody::own`), and registers a *watcher* session: one the broker keeps beside the workload's own, which
//! hears the credentials agent's authority and nothing else, and which the broker leaves open under a revoked
//! certificate. It keeps that authority by the rules every agent keeps (`AuthorityCore`): believed only from the
//! credentials agent's identity, under the key pinned from its first snapshot. When a set it believes names a
//! certificate it holds, every key is destroyed and the daemon is to exit; the kubelet restarts it empty, and the
//! workload's health actor rejuvenates the pod (step 4, item 6).
//!
//! It signs for the workload meanwhile, before its session is up and while Cassini is away: otherwise the credentials
//! agent's own pod could never start, its daemon waiting on the credentials agent. A revocation published while it
//! was away is in the snapshot it asks for when it is back.

use cassini_client::{
    DeliveryMode, ReconnectMode, TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage,
};
use cassini_types::ClientEvent;
use credentials_agent_common::authority_leaf::{AuthorityCore, Effect, Refusal};
use credentials_agent_common::authority_update::{
    AUTHORITY_SNAPSHOT_REPLIES_TOPIC, AUTHORITY_SNAPSHOT_REQUESTS_TOPIC, AUTHORITY_UPDATES_TOPIC,
    AuthoritySnapshot, AuthoritySnapshotRequest, AuthorityUpdate,
};
use polar_custody::custody::Custody;
use polar_custody::protocol::Slot;
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

/// What came of something heard on an authority topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Heard {
    /// Nothing for the keys held.
    Nothing,
    /// An update could not be checked, or one was missed: ask for the current authority.
    Ask,
    /// A set believed names these slots' certificates: every key is destroyed.
    Revoked(Vec<Slot>),
}

/// The credentials agent's authority as the daemon keeps it, and what it means for the keys held. No actors: given
/// what arrived on which topic and who the broker says published it, it says what to do.
pub struct Watch {
    core: AuthorityCore,
}

impl Watch {
    /// `publisher` is the identity the broker gives the credentials agent.
    pub fn new(publisher: impl Into<String>) -> Self {
        Self {
            core: AuthorityCore::new(publisher),
        }
    }

    /// Whether the credentials agent's key is pinned.
    pub fn has_key(&self) -> bool {
        self.core.has_key()
    }

    /// What `payload`, published on `topic` by `published_by`, means for `custody`; a revoked certificate's keys are
    /// destroyed here.
    pub fn hear(
        &mut self,
        topic: &str,
        payload: &[u8],
        published_by: Option<&str>,
        custody: &RwLock<Custody>,
    ) -> Heard {
        let handled = match topic {
            AUTHORITY_UPDATES_TOPIC => {
                match rkyv::from_bytes::<AuthorityUpdate, rkyv::rancor::Error>(payload) {
                    Ok(update) => self.core.on_update(&update, published_by),
                    Err(e) => {
                        warn!("a message on the authority updates topic is not an update: {e}");
                        return Heard::Nothing;
                    }
                }
            }
            AUTHORITY_SNAPSHOT_REPLIES_TOPIC => {
                match rkyv::from_bytes::<AuthoritySnapshot, rkyv::rancor::Error>(payload) {
                    Ok(snapshot) => self.core.on_snapshot(&snapshot, published_by),
                    Err(e) => {
                        warn!("a message on the authority snapshot topic is not a snapshot: {e}");
                        return Heard::Nothing;
                    }
                }
            }
            _ => return Heard::Nothing,
        };
        let handled = match handled {
            Ok(handled) => handled,
            Err(Refusal::NoKeyYet) => return Heard::Ask,
            Err(Refusal::WrongPublisher { got }) => {
                debug!(
                    ?got,
                    "authority from someone other than the credentials agent, ignored"
                );
                return Heard::Nothing;
            }
            Err(refusal) => {
                warn!(?refusal, "authority refused");
                return Heard::Nothing;
            }
        };
        for refused in &handled.refused {
            warn!("an authority update in a snapshot was refused: {refused}");
        }
        let mut heard = Heard::Nothing;
        for effect in handled.effects {
            match effect {
                Effect::SetRevoked(serials) => {
                    let revoked = custody
                        .write()
                        .expect("the custody is never held across a panic")
                        .revoke(&serials);
                    if !revoked.is_empty() {
                        return Heard::Revoked(revoked);
                    }
                }
                Effect::AskForSnapshot => heard = Heard::Ask,
                _ => {}
            }
        }
        heard
    }
}

/// The variable naming the credentials agent's identity, and its default: the same as every agent's
/// (`polar_agent_builder::environment::AUTHORITY_PUBLISHER`; a test holds the two together).
pub const PUBLISHER_VARIABLE: &str = "POLAR_AUTHORITY_PUBLISHER";
pub const DEFAULT_PUBLISHER: &str = "credentials-agent.polar.serviceaccount.cluster.local";

/// How often the actor looks whether its client slot holds a certificate yet, and whether its connection is up.
const TICK: Duration = Duration::from_secs(1);
/// The least time between two requests for the current authority, as an agent keeps it.
const ASK_INTERVAL: Duration = Duration::from_secs(5);
/// How often it asks again while it has no key.
const ASK_WITHOUT_KEY: Duration = Duration::from_secs(30);

pub struct WatcherArgs {
    pub custody: Arc<RwLock<Custody>>,
    /// The pinned CA, which Cassini's certificate is verified under.
    pub ca_pem: String,
    /// Cassini's address and the name on its certificate.
    pub broker: String,
    pub server_name: String,
    /// The identity the broker gives the credentials agent.
    pub publisher: String,
    /// Called once the keys are destroyed, with the slots whose certificates were revoked. The daemon exits.
    pub on_revoked: Arc<dyn Fn(Vec<Slot>) + Send + Sync>,
}

pub enum WatcherMessage {
    Tick,
    Event(ClientEvent),
}

pub struct WatcherState {
    args: WatcherArgs,
    watch: Watch,
    client: Option<ActorRef<TcpClientMessage>>,
    last_ask: Option<Instant>,
    revoked: bool,
}

pub struct Watcher;

impl WatcherState {
    fn ask(&mut self, interval: Duration) {
        let Some(client) = &self.client else { return };
        let now = Instant::now();
        if self
            .last_ask
            .is_some_and(|at| now.duration_since(at) < interval)
        {
            return;
        }
        self.last_ask = Some(now);
        match rkyv::to_bytes::<rkyv::rancor::Error>(&AuthoritySnapshotRequest) {
            Ok(request) => {
                let _ = client.cast(TcpClientMessage::publish(
                    AUTHORITY_SNAPSHOT_REQUESTS_TOPIC.to_string(),
                    request.to_vec(),
                    None,
                ));
            }
            Err(e) => error!("the request for the authority does not encode: {e}"),
        }
    }

    async fn connect(&mut self, myself: &ActorRef<WatcherMessage>) {
        let tls = match polar_custody::own::own_client_config(
            self.args.custody.clone(),
            &self.args.ca_pem,
        ) {
            Ok(tls) => Arc::new(tls),
            Err(e) => {
                debug!("not watching yet: {e}");
                return;
            }
        };
        let config = TCPClientConfig::from_values(
            self.args.broker.clone(),
            self.args.server_name.clone(),
            String::new(),
            String::new(),
            String::new(),
        )
        .with_tls(tls)
        .as_watcher()
        // Its supervisor is this actor: a lost connection ends the transport, and the next tick makes another.
        .with_reconnect(ReconnectMode::LeaveToSupervisor);
        let events = Arc::new(OutputPort::default());
        events.subscribe(myself.clone(), |event| Some(WatcherMessage::Event(event)));
        match Actor::spawn_linked(
            None,
            TcpClientActor,
            TcpClientArgs {
                config,
                registration_id: None,
                delivery: DeliveryMode::Broadcast(events),
                answers: None,
            },
            myself.get_cell(),
        )
        .await
        {
            Ok((client, _)) => self.client = Some(client),
            Err(e) => warn!("the watcher's connection could not start: {e}"),
        }
    }
}

#[ractor::async_trait]
impl Actor for Watcher {
    type Msg = WatcherMessage;
    type State = WatcherState;
    type Arguments = WatcherArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: WatcherArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        drop(myself.send_interval(TICK, || WatcherMessage::Tick));
        Ok(WatcherState {
            watch: Watch::new(args.publisher.clone()),
            args,
            client: None,
            last_ask: None,
            revoked: false,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if state.revoked {
            return Ok(());
        }
        match message {
            WatcherMessage::Tick => {
                if state.client.is_none() {
                    state.connect(&myself).await;
                } else if !state.watch.has_key() {
                    state.ask(ASK_WITHOUT_KEY);
                }
            }
            WatcherMessage::Event(ClientEvent::Registered { registration_id }) => {
                info!(%registration_id, "watching for this pod's revocation");
                if let Some(client) = &state.client {
                    for topic in [AUTHORITY_UPDATES_TOPIC, AUTHORITY_SNAPSHOT_REPLIES_TOPIC] {
                        let _ = client.cast(TcpClientMessage::Subscribe {
                            topic: topic.to_string(),
                            trace_ctx: None,
                        });
                    }
                }
                // What was published while it was away is in the current authority.
                state.last_ask = None;
                state.ask(ASK_INTERVAL);
            }
            WatcherMessage::Event(ClientEvent::MessagePublished {
                topic,
                payload,
                published_by,
                ..
            }) => match state.watch.hear(
                &topic,
                &payload,
                published_by.as_deref(),
                &state.args.custody,
            ) {
                Heard::Nothing => {}
                Heard::Ask => state.ask(ASK_INTERVAL),
                Heard::Revoked(slots) => {
                    error!(
                        ?slots,
                        "this pod's certificate is revoked: every key it held is destroyed"
                    );
                    state.revoked = true;
                    if let Some(client) = state.client.take() {
                        client.stop(None);
                    }
                    (state.args.on_revoked)(slots);
                }
            },
            WatcherMessage::Event(_) => {}
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorTerminated(cell, _, _)
            | SupervisionEvent::ActorFailed(cell, _)
                if state
                    .client
                    .as_ref()
                    .is_some_and(|client| client.get_id() == cell.get_id()) =>
            {
                debug!("the watcher's connection ended; the next tick makes another");
                state.client = None;
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use credentials_agent_common::authority_update::{AuthorityBody, AuthorityPayload, sign};
    use polar_custody::custody::Roles;
    use polar_custody::protocol::{Reply, Request};
    use rsa::RsaPrivateKey;
    use rsa::pkcs8::EncodePublicKey;

    pub(crate) const CREDENTIALS: &str = "credentials-agent.polar.serviceaccount.cluster.local";
    const POD: &str = "git-repo-observer.polar.serviceaccount.cluster.local";

    fn key(n: usize) -> RsaPrivateKey {
        static KEYS: std::sync::OnceLock<[RsaPrivateKey; 2]> = std::sync::OnceLock::new();
        KEYS.get_or_init(|| {
            [
                RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap(),
                RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap(),
            ]
        })[n]
            .clone()
    }

    fn revoked(sequence: u64, serials: &[String]) -> AuthorityPayload {
        AuthorityPayload {
            sequence,
            version: sequence,
            body: AuthorityBody::Revoked {
                serials: serials.to_vec(),
            },
            authored_by: None,
        }
    }

    fn snapshot(signer: &RsaPrivateKey, payloads: &[AuthorityPayload]) -> Vec<u8> {
        let snapshot = AuthoritySnapshot {
            public_key_pem: signer
                .to_public_key()
                .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
                .unwrap(),
            updates: payloads.iter().map(|p| sign(p, signer).unwrap()).collect(),
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&snapshot)
            .unwrap()
            .to_vec()
    }

    fn update(signer: &RsaPrivateKey, payload: &AuthorityPayload) -> Vec<u8> {
        rkyv::to_bytes::<rkyv::rancor::Error>(&sign(payload, signer).unwrap())
            .unwrap()
            .to_vec()
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// A custody whose client slot holds a certificate, and that certificate's serial.
    fn installed() -> (RwLock<Custody>, String) {
        static CA: std::sync::OnceLock<(rcgen::Issuer<'static, rcgen::KeyPair>, String)> =
            std::sync::OnceLock::new();
        let (issuer, ca_pem) = CA.get_or_init(|| {
            let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            let key = rcgen::KeyPair::generate().unwrap();
            let pem = params.self_signed(&key).unwrap().pem();
            (rcgen::Issuer::new(params, key), pem)
        });
        let installer = Roles {
            installer: true,
            workload: false,
        };
        let mut custody = Custody::new(ca_pem).unwrap();
        let Reply::Csr(csr) = custody.handle(
            installer,
            Request::MakeCsr {
                slot: Slot::Client,
                identity: POD.to_string(),
                names: vec![],
            },
            now(),
        ) else {
            panic!("a CSR")
        };
        let mut request = rcgen::CertificateSigningRequestParams::from_pem(&csr).unwrap();
        request.params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        request.params.not_after = rcgen::date_time_ymd(2100, 1, 1);
        let certificate = request.signed_by(issuer).unwrap().pem();
        assert_eq!(
            custody.handle(
                installer,
                Request::Install {
                    slot: Slot::Client,
                    chain_pem: vec![certificate.clone()],
                },
                now(),
            ),
            Reply::Installed
        );
        let serial = custody.serial(Slot::Client).unwrap();
        (RwLock::new(custody), serial)
    }

    fn signs(custody: &RwLock<Custody>) -> bool {
        let message = {
            let mut m = vec![0x20; 64];
            m.extend_from_slice(b"TLS 1.3, client CertificateVerify\0");
            m.extend_from_slice(&[9; 32]);
            m
        };
        matches!(
            custody.read().unwrap().handle_read(
                Roles {
                    installer: false,
                    workload: true
                },
                Request::Sign {
                    slot: Slot::Client,
                    message
                }
            ),
            Reply::Signature(_)
        )
    }

    #[test]
    fn its_own_serial_in_the_credentials_agent_s_snapshot_destroys_its_keys() {
        let (custody, own) = installed();
        let mut watch = Watch::new(CREDENTIALS);
        let heard = watch.hear(
            AUTHORITY_SNAPSHOT_REPLIES_TOPIC,
            &snapshot(&key(0), &[revoked(1, &["00ff".to_string(), own])]),
            Some(CREDENTIALS),
            &custody,
        );
        assert_eq!(heard, Heard::Revoked(vec![Slot::Client]));
        assert!(custody.read().unwrap().is_revoked());
        assert!(!signs(&custody));
    }

    #[test]
    fn its_own_serial_in_a_later_update_destroys_its_keys() {
        let (custody, own) = installed();
        let mut watch = Watch::new(CREDENTIALS);
        assert_eq!(
            watch.hear(
                AUTHORITY_SNAPSHOT_REPLIES_TOPIC,
                &snapshot(&key(0), &[revoked(1, &[])]),
                Some(CREDENTIALS),
                &custody
            ),
            Heard::Nothing
        );
        assert!(signs(&custody));
        assert_eq!(
            watch.hear(
                AUTHORITY_UPDATES_TOPIC,
                &update(&key(0), &revoked(2, &[own])),
                Some(CREDENTIALS),
                &custody
            ),
            Heard::Revoked(vec![Slot::Client])
        );
        assert!(!signs(&custody));
    }

    #[test]
    fn other_serials_change_nothing() {
        let (custody, _) = installed();
        let mut watch = Watch::new(CREDENTIALS);
        let heard = watch.hear(
            AUTHORITY_SNAPSHOT_REPLIES_TOPIC,
            &snapshot(&key(0), &[revoked(1, &["00ff".to_string()])]),
            Some(CREDENTIALS),
            &custody,
        );
        assert_eq!(heard, Heard::Nothing);
        assert!(signs(&custody));
    }

    #[test]
    fn a_set_from_anyone_but_the_credentials_agent_or_under_another_key_is_not_believed() {
        let (custody, own) = installed();
        let mut watch = Watch::new(CREDENTIALS);
        let forged = snapshot(&key(1), &[revoked(1, std::slice::from_ref(&own))]);
        assert_eq!(
            watch.hear(
                AUTHORITY_SNAPSHOT_REPLIES_TOPIC,
                &forged,
                Some("impostor.polar.serviceaccount.cluster.local"),
                &custody
            ),
            Heard::Nothing
        );
        assert!(!watch.has_key(), "nothing pinned from an impostor");
        // The credentials agent's key pinned, a snapshot under another is refused whoever it claims to be.
        watch.hear(
            AUTHORITY_SNAPSHOT_REPLIES_TOPIC,
            &snapshot(&key(0), &[revoked(1, &[])]),
            Some(CREDENTIALS),
            &custody,
        );
        let forged = snapshot(&key(1), &[revoked(2, std::slice::from_ref(&own))]);
        assert_eq!(
            watch.hear(
                AUTHORITY_SNAPSHOT_REPLIES_TOPIC,
                &forged,
                Some(CREDENTIALS),
                &custody
            ),
            Heard::Nothing
        );
        let forged = update(&key(1), &revoked(2, &[own]));
        assert_eq!(
            watch.hear(
                AUTHORITY_UPDATES_TOPIC,
                &forged,
                Some(CREDENTIALS),
                &custody
            ),
            Heard::Nothing
        );
        assert!(signs(&custody));
    }

    #[test]
    fn an_update_before_any_snapshot_asks_for_one() {
        let (custody, own) = installed();
        let mut watch = Watch::new(CREDENTIALS);
        assert_eq!(
            watch.hear(
                AUTHORITY_UPDATES_TOPIC,
                &update(&key(0), &revoked(1, &[own])),
                Some(CREDENTIALS),
                &custody
            ),
            Heard::Ask
        );
        assert!(signs(&custody), "not believed before a key is pinned");
    }

    #[test]
    fn the_daemon_believes_the_publisher_every_agent_believes_by_default() {
        let default =
            polar_env::Config::builder(&std::collections::HashMap::<String, String>::new())
                .register(&polar_agent_builder::environment::BUILDER)
                .build();
        assert_eq!(
            default
                .read(&polar_agent_builder::environment::AUTHORITY_PUBLISHER)
                .unwrap(),
            crate::watch::DEFAULT_PUBLISHER
        );
        assert_eq!(
            polar_agent_builder::environment::AUTHORITY_PUBLISHER.name(),
            crate::watch::PUBLISHER_VARIABLE
        );
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// Something heard on an authority topic: from the credentials agent (its one key) or from another identity
        /// (either key), a snapshot or a live update, naming the daemon's own serial or not.
        #[derive(Debug, Clone)]
        struct Event {
            from_the_credentials_agent: bool,
            other_key: bool,
            snapshot: bool,
            own: bool,
        }

        fn event() -> impl Strategy<Value = Event> {
            (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>()).prop_map(
                |(from_the_credentials_agent, other_key, snapshot, own)| Event {
                    from_the_credentials_agent,
                    other_key,
                    snapshot,
                    own,
                },
            )
        }

        proptest! {
            #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

            /// For any sequence heard, the daemon signs exactly while no set it believed (the credentials agent's,
            /// a snapshot or an update after a snapshot pinned its key) has named its own certificate.
            #[test]
            fn it_signs_exactly_until_a_believed_set_names_it(events in prop::collection::vec(event(), 0..10)) {
                let (custody, own) = installed();
                let mut watch = Watch::new(CREDENTIALS);
                let mut pinned = false;
                let mut revoked_seen = false;
                for (n, e) in events.iter().enumerate() {
                    let sequence = n as u64 + 1;
                    let serials = if e.own { vec![own.clone()] } else { vec!["00ff".to_string()] };
                    let payload = revoked(sequence, &serials);
                    // The credentials agent has one key; another identity may sign with any.
                    let signer = if e.from_the_credentials_agent { key(0) } else { key(e.other_key as usize) };
                    let (topic, bytes) = if e.snapshot {
                        (AUTHORITY_SNAPSHOT_REPLIES_TOPIC, snapshot(&signer, &[payload]))
                    } else {
                        (AUTHORITY_UPDATES_TOPIC, update(&signer, &payload))
                    };
                    let publisher = if e.from_the_credentials_agent { CREDENTIALS } else { "impostor.polar.serviceaccount.cluster.local" };
                    watch.hear(topic, &bytes, Some(publisher), &custody);
                    let believed = e.from_the_credentials_agent && (e.snapshot || pinned);
                    pinned |= e.from_the_credentials_agent && e.snapshot;
                    revoked_seen |= believed && e.own;
                    prop_assert_eq!(signs(&custody), !revoked_seen);
                }
            }
        }
    }
}
