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

//! The stable front of the Cassini client.
//!
//! [`TcpClientActor`] is the connection, and a connection is the unstable part: any transport
//! error ends it. Everything that holds a client (the supervisor, the domain actors, the crypto
//! actor) used to hold that actor, so every connection failure was also an API failure and every
//! holder had to be handed a new one.
//!
//! The facade is the address they hold instead. It speaks the same [`TcpClientMessage`] protocol,
//! supervises the transport, and brings a new one up, with backoff, whenever it dies. While there is
//! no registered transport it holds publishes in a bounded buffer and sends them, in order, once one
//! registers, so a publish made during a reconnect is no longer dropped with a warning.
//!
//! It deliberately reuses two existing messages, so the public protocol does not change:
//! `ClientRegistered(id)` is how the facade learns the transport registered, and which session it has (the transport reports
//! `ClientEvent::Registered` through its delivery, which the facade observes), and `Reconnect` means
//! "bring a transport up if there is none".
//!
//! What this does not do yet: a message already written to a connection that then dies is still
//! lost, because nothing acknowledges it. That is the next step of the design in
//! `docs/architecture/cassini-client-reliability.md`.

use crate::refused::{RefusedTopics, StandingRefusal};
use crate::{
    DeliveryMode, ReconnectMode, TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage,
};
use async_trait::async_trait;
use cassini_types::ClientEvent;
use ractor::{Actor, ActorCell, ActorProcessingErr, ActorRef, SupervisionEvent};
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::{debug, error, info, warn};

/// What an agent is about to do with a topic, as its [`TopicGate`] is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TopicUse {
    Publish,
    Subscribe,
}

impl TopicUse {
    pub fn as_str(self) -> &'static str {
        match self {
            TopicUse::Publish => "publish",
            TopicUse::Subscribe => "subscribe",
        }
    }
}

/// Decides, before a publish or a subscribe leaves the agent, whether the agent may: `Err` carries why not.
/// The facade asks it for every publish and subscribe as it arrives, in order, and a refused one is neither
/// held nor sent; the agent is told with `PublishRefused` or `SubscribeRefused`. The agent builder supplies
/// it (the agent's policy first, then its topic set: docs/architecture/policy-enforcement.md, "Decisions"),
/// so this crate decides nothing itself.
pub type TopicGate = Arc<
    dyn Fn(TopicUse, String) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>
        + Send
        + Sync,
>;

/// What the facade tells the agent builder about the topics the agent was refused
/// (docs/architecture/policy-enforcement.md, stage 5b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalNews {
    /// The broker refused what the agent's gate let through: the agent's authority may be behind the broker's, so
    /// the builder asks for the current one.
    Unpredicted {
        topic: String,
        use_: TopicUse,
        reason: String,
    },
    /// What stands refused changed: the topics the agent wants and is refused now, for its health file.
    Standing(Vec<StandingRefusal>),
}

/// Told [`RefusalNews`] as it happens. The agent builder supplies it, as it does the gate.
pub type RefusalWatch = Arc<dyn Fn(RefusalNews) + Send + Sync>;

/// How many publishes are held while there is no registered transport. The oldest is dropped
/// first when it is full.
pub const DEFAULT_OFFLINE_BUFFER: usize = 1_000;

/// How long to wait before bringing a transport up again: doubling from the first to the last.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub first: Duration,
    pub last: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            first: Duration::from_secs(1),
            last: Duration::from_secs(30),
        }
    }
}

impl Backoff {
    /// The wait before attempt number `attempt`, counting from zero.
    pub fn delay(&self, attempt: u32) -> Duration {
        let doubled = self
            .first
            .saturating_mul(1u32.checked_shl(attempt.min(30)).unwrap_or(u32::MAX));
        doubled.min(self.last)
    }
}

/// How the facade makes a transport. A seam: the real one connects over TLS, a test's does not.
#[async_trait]
pub trait TransportSpawner: Send + Sync {
    async fn spawn(
        &self,
        name: &str,
        args: TcpClientArgs,
        supervisor: ActorCell,
    ) -> Result<ActorRef<TcpClientMessage>, ActorProcessingErr>;
}

/// The real transport: a [`TcpClientActor`], linked under the facade.
pub struct TlsTransportSpawner;

#[async_trait]
impl TransportSpawner for TlsTransportSpawner {
    async fn spawn(
        &self,
        name: &str,
        mut args: TcpClientArgs,
        supervisor: ActorCell,
    ) -> Result<ActorRef<TcpClientMessage>, ActorProcessingErr> {
        // The facade replaces a transport that loses its connection; the transport must not
        // mend itself behind its back.
        args.config = args.config.with_reconnect(ReconnectMode::LeaveToSupervisor);
        let (transport, _) =
            Actor::spawn_linked(Some(name.to_string()), TcpClientActor, args, supervisor)
                .await
                .map_err(ActorProcessingErr::from)?;
        Ok(transport)
    }
}

pub struct FacadeArgs {
    pub config: TCPClientConfig,
    pub delivery: DeliveryMode,
    /// The name the transport is spawned under. The facade itself carries the name callers know.
    pub transport_name: String,
    pub spawner: Arc<dyn TransportSpawner>,
    /// True while a transport is registered with the broker. Shared with the handle, so a caller
    /// can ask without a message.
    pub online: Arc<AtomicBool>,
    pub buffer_limit: usize,
    pub backoff: Backoff,
    /// Asked before each publish and subscribe; none admits everything.
    pub gate: Option<TopicGate>,
    /// Told what the agent was refused; none tells no one.
    pub watch: Option<RefusalWatch>,
}

pub struct FacadeState {
    args: FacadeArgs,
    delivery: DeliveryMode,
    transport: Option<ActorRef<TcpClientMessage>>,
    registered: bool,
    registration_id: Option<String>,
    buffer: VecDeque<TcpClientMessage>,
    attempt: u32,
    closing: bool,
    /// What the agent was refused and still wants. It outlives every transport, as the agent's wanting does.
    refused: RefusedTopics,
}

pub struct TcpClientFacade;

/// Lets the facade see when the transport registers, without changing what the rest of the agent
/// is delivered: the transport's own event path is wrapped, not replaced.
fn observe(delivery: DeliveryMode, facade: ActorRef<TcpClientMessage>) -> DeliveryMode {
    match delivery {
        DeliveryMode::Broadcast(port) => {
            port.subscribe(facade, |event| match event {
                ClientEvent::Registered { registration_id } => {
                    Some(TcpClientMessage::ClientRegistered(registration_id))
                }
                _ => None,
            });
            DeliveryMode::Broadcast(port)
        }
        DeliveryMode::Router { router, lifecycle } => DeliveryMode::Router {
            router,
            // The facade is told before the agent's own handler runs, so by the time that
            // handler's resubscriptions reach the facade it already knows it is registered.
            lifecycle: Arc::new(move |event| {
                if let ClientEvent::Registered { registration_id } = &event {
                    let _ = facade
                        .send_message(TcpClientMessage::ClientRegistered(registration_id.clone()));
                }
                lifecycle(event)
            }),
        },
    }
}

impl TcpClientFacade {
    /// Starts a transport, if there is none. Returns whether one is now running.
    async fn bring_up(
        myself: &ActorRef<TcpClientMessage>,
        state: &mut FacadeState,
    ) -> Result<(), ActorProcessingErr> {
        let transport = state
            .args
            .spawner
            .spawn(
                &state.args.transport_name,
                TcpClientArgs {
                    config: state.args.config.clone(),
                    // Ask for the session this client last had. The broker keeps one whose client
                    // went away for a while, with what was held for it, and the transport takes
                    // whichever session it is given if that one is gone.
                    registration_id: state.registration_id.clone(),
                    delivery: state.delivery.clone(),
                    // The broker's answers come here, so what was refused is kept across transports.
                    answers: Some(myself.clone()),
                },
                myself.get_cell(),
            )
            .await?;
        state.transport = Some(transport);
        Ok(())
    }

    /// Arranges to try again after the backoff, and lengthens the next one.
    fn retry_later(myself: &ActorRef<TcpClientMessage>, state: &mut FacadeState) {
        let wait = state.args.backoff.delay(state.attempt);
        state.attempt = state.attempt.saturating_add(1);
        info!("the Cassini transport will be brought up again in {wait:?}");
        drop(myself.send_after(wait, || TcpClientMessage::Reconnect));
    }

    fn hold(state: &mut FacadeState, message: TcpClientMessage) {
        if state.buffer.len() >= state.args.buffer_limit {
            state.buffer.pop_front();
            warn!(
                "the Cassini offline buffer is full ({} publishes): the oldest was dropped",
                state.args.buffer_limit
            );
        }
        state.buffer.push_back(message);
    }

    /// Tells the watch what stands refused now.
    fn tell_standing(state: &FacadeState) {
        if let Some(watch) = &state.args.watch {
            watch(RefusalNews::Standing(state.refused.standing()));
        }
    }

    /// The agent's authority changed: asks the gate again about everything refused, and sends each wanted
    /// subscription it admits now. With no gate the broker alone decides, so each is sent again.
    async fn judge_again(state: &mut FacadeState) {
        let mut verdicts = BTreeMap::new();
        for (topic, use_) in state.refused.refused() {
            let verdict = match &state.args.gate {
                Some(gate) => gate(use_, topic.clone()).await,
                None => Ok(()),
            };
            verdicts.insert((topic, use_), verdict);
        }
        let (send, changed) = state.refused.judged(&verdicts);
        if changed {
            Self::tell_standing(state);
        }
        for topic in send {
            info!(%topic, "a subscription refused before is admitted now, and sent");
            // Not registered: the agent subscribes to everything it wants when it is, and this is no longer held
            // back.
            if let Some(transport) = &state.transport
                && state.registered
                && let Err(e) = transport.send_message(TcpClientMessage::Subscribe {
                    topic,
                    trace_ctx: None,
                })
            {
                warn!("a subscription admitted again could not be sent to the transport: {e}");
            }
        }
    }

    fn set_online(state: &mut FacadeState, online: bool) {
        state.registered = online;
        state.args.online.store(online, Ordering::SeqCst);
    }
}

#[async_trait]
impl Actor for TcpClientFacade {
    type Msg = TcpClientMessage;
    type State = FacadeState;
    type Arguments = FacadeArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: FacadeArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        let delivery = observe(args.delivery.clone(), myself.clone());
        let mut state = FacadeState {
            args,
            delivery,
            transport: None,
            registered: false,
            registration_id: None,
            buffer: VecDeque::new(),
            attempt: 0,
            closing: false,
            refused: RefusedTopics::default(),
        };
        // The first transport is part of starting: if it cannot be built (a missing certificate,
        // say) the facade does not start, as the transport did not before it. Later failures are
        // retried.
        Self::bring_up(&myself, &mut state).await?;
        Ok(state)
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // A subscription refused before is held back: the agent subscribes to everything it wants each time it
        // registers, and this one waits for its authority to change.
        if let TcpClientMessage::Subscribe { topic, .. } = &message
            && state.refused.holds_back(topic)
        {
            debug!(%topic, "a refused subscription is held back until this agent's authority changes");
            return Ok(());
        }
        if let TcpClientMessage::UnsubscribeRequest { topic, .. } = &message
            && state.refused.clear(topic, TopicUse::Subscribe)
        {
            Self::tell_standing(state);
        }
        if let Some(gate) = &state.args.gate {
            let asked = match &message {
                TcpClientMessage::Publish { topic, .. } => Some((TopicUse::Publish, topic.clone())),
                TcpClientMessage::Subscribe { topic, .. } => {
                    Some((TopicUse::Subscribe, topic.clone()))
                }
                _ => None,
            };
            if let Some((use_, topic)) = asked
                && let Err(reason) = gate(use_, topic.clone()).await
            {
                error!(%topic, %reason, "this agent may not {} on the topic; nothing was sent", use_.as_str());
                if state.refused.refuse(&topic, use_, &reason, false) {
                    Self::tell_standing(state);
                }
                let event = match use_ {
                    TopicUse::Publish => ClientEvent::PublishRefused { topic, reason },
                    TopicUse::Subscribe => ClientEvent::SubscribeRefused { topic, reason },
                };
                TcpClientActor::dispatch_event(&state.delivery, event);
                return Ok(());
            }
        }
        match message {
            TcpClientMessage::ClientRegistered(id) => {
                state.registration_id = Some(id);
                state.attempt = 0;
                Self::set_online(state, true);
                if let Some(transport) = &state.transport {
                    for held in state.buffer.drain(..) {
                        if let Err(e) = transport.send_message(held) {
                            warn!("a held publish could not be sent to the transport: {e}");
                        }
                    }
                }
            }
            TcpClientMessage::BrokerAnswered {
                topic,
                use_,
                result,
            } => match result {
                Ok(()) => {
                    if state.refused.clear(&topic, use_) {
                        Self::tell_standing(state);
                    }
                }
                Err(reason) => {
                    // The gate let it through, or it would not have been sent.
                    if let Some(watch) = &state.args.watch {
                        watch(RefusalNews::Unpredicted {
                            topic: topic.clone(),
                            use_,
                            reason: reason.clone(),
                        });
                    }
                    if state.refused.refuse(&topic, use_, &reason, true) {
                        Self::tell_standing(state);
                    }
                }
            },
            TcpClientMessage::JudgeAgain => Self::judge_again(state).await,
            TcpClientMessage::Reconnect => {
                if state.transport.is_none()
                    && !state.closing
                    && let Err(e) = Self::bring_up(&myself, state).await
                {
                    warn!("the Cassini transport could not be started: {e}");
                    Self::retry_later(&myself, state);
                }
            }
            publish @ TcpClientMessage::Publish { .. } => match &state.transport {
                Some(transport) if state.registered => {
                    // Lost the race with the transport ending: keep it for the next one.
                    if let Err(ractor::MessagingErr::SendErr(message)) =
                        transport.send_message(publish)
                    {
                        Self::hold(state, message);
                    }
                }
                _ => Self::hold(state, publish),
            },
            TcpClientMessage::GetRegistrationId(reply) => match &state.transport {
                Some(transport) if state.registered => {
                    let _ = transport.send_message(TcpClientMessage::GetRegistrationId(reply));
                }
                _ => {
                    let _ = reply.send(state.registration_id.clone());
                }
            },
            TcpClientMessage::Disconnect { trace_ctx } => {
                state.closing = true;
                if let Some(transport) = &state.transport {
                    let _ = transport.send_message(TcpClientMessage::Disconnect { trace_ctx });
                }
            }
            other => match &state.transport {
                // Subscriptions and the rest are the agent's to repeat on every registration, as
                // they always were; a request made while offline has nothing to answer it.
                Some(transport) if state.registered => {
                    let _ = transport.send_message(other);
                }
                _ => debug!("not forwarding a request while there is no registered transport"),
            },
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let cell = match &event {
            SupervisionEvent::ActorTerminated(cell, _, _) => cell,
            SupervisionEvent::ActorFailed(cell, _) => cell,
            _ => return Ok(()),
        };
        let is_ours = state
            .transport
            .as_ref()
            .is_some_and(|transport| transport.get_id() == cell.get_id());
        if !is_ours {
            return Ok(());
        }
        warn!("the Cassini transport ended; the client stays up and will bring a new one");
        state.transport = None;
        Self::set_online(state, false);
        if !state.closing {
            Self::retry_later(&myself, state);
        }
        Ok(())
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Self::set_online(state, false);
        if let Some(transport) = state.transport.take() {
            transport.stop(Some("the Cassini client is stopping".to_string()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A stand-in transport that records what it is sent.
    struct Recorder {
        received: Arc<Mutex<Vec<TcpClientMessage>>>,
    }

    #[async_trait]
    impl Actor for Recorder {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            self.received.lock().unwrap().push(message);
            Ok(())
        }
    }

    type Received = Arc<Mutex<Vec<TcpClientMessage>>>;

    /// Makes recording transports, remembering each, and can be told to fail.
    #[derive(Default)]
    struct FakeSpawner {
        spawned: Mutex<Vec<(ActorRef<TcpClientMessage>, Received)>>,
        fail_next: Mutex<u32>,
        registration_ids_asked_for: Mutex<Vec<Option<String>>>,
    }

    #[async_trait]
    impl TransportSpawner for FakeSpawner {
        async fn spawn(
            &self,
            name: &str,
            args: TcpClientArgs,
            supervisor: ActorCell,
        ) -> Result<ActorRef<TcpClientMessage>, ActorProcessingErr> {
            self.registration_ids_asked_for
                .lock()
                .unwrap()
                .push(args.registration_id.clone());
            {
                let mut fail = self.fail_next.lock().unwrap();
                if *fail > 0 {
                    *fail -= 1;
                    return Err(ActorProcessingErr::from("the fake transport cannot start"));
                }
            }
            let received = Arc::new(Mutex::new(Vec::new()));
            let (transport, _) = Actor::spawn_linked(
                Some(format!("{name}-{}", self.spawned.lock().unwrap().len())),
                Recorder {
                    received: received.clone(),
                },
                (),
                supervisor,
            )
            .await?;
            self.spawned
                .lock()
                .unwrap()
                .push((transport.clone(), received));
            Ok(transport)
        }
    }

    impl FakeSpawner {
        fn count(&self) -> usize {
            self.spawned.lock().unwrap().len()
        }
        fn latest(&self) -> (ActorRef<TcpClientMessage>, Received) {
            self.spawned
                .lock()
                .unwrap()
                .last()
                .cloned()
                .expect("a transport")
        }
    }

    fn config() -> TCPClientConfig {
        TCPClientConfig::from_values(
            "broker:1".into(),
            "broker".into(),
            "/ca".into(),
            "/cert".into(),
            "/key".into(),
        )
    }

    struct Rig {
        facade: ActorRef<TcpClientMessage>,
        spawner: Arc<FakeSpawner>,
        online: Arc<AtomicBool>,
        events: Arc<ractor::OutputPort<ClientEvent>>,
    }

    async fn rig(buffer_limit: usize) -> Rig {
        gated_rig(buffer_limit, None).await
    }

    async fn gated_rig(buffer_limit: usize, gate: Option<TopicGate>) -> Rig {
        watched_rig(buffer_limit, gate, None).await
    }

    async fn watched_rig(
        buffer_limit: usize,
        gate: Option<TopicGate>,
        watch: Option<RefusalWatch>,
    ) -> Rig {
        let spawner = Arc::new(FakeSpawner::default());
        let online = Arc::new(AtomicBool::new(false));
        let events = Arc::new(ractor::OutputPort::default());
        let (facade, _) = Actor::spawn(
            Some(format!("facade-{}", uuid::Uuid::new_v4())),
            TcpClientFacade,
            FacadeArgs {
                config: config(),
                delivery: DeliveryMode::Broadcast(events.clone()),
                transport_name: format!("transport-{}", uuid::Uuid::new_v4()),
                spawner: spawner.clone(),
                online: online.clone(),
                buffer_limit,
                backoff: Backoff::default(),
                gate,
                watch,
            },
        )
        .await
        .unwrap();
        Rig {
            facade,
            spawner,
            online,
            events,
        }
    }

    fn publish(topic: &str) -> TcpClientMessage {
        TcpClientMessage::publish(topic.to_string(), topic.as_bytes().to_vec(), None)
    }

    fn topics(received: &Arc<Mutex<Vec<TcpClientMessage>>>) -> Vec<String> {
        received
            .lock()
            .unwrap()
            .iter()
            .filter_map(|m| match m {
                TcpClientMessage::Publish { topic, .. } => Some(topic.clone()),
                _ => None,
            })
            .collect()
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    /// What the transport reports to its delivery when the broker accepts it.
    fn registered(rig: &Rig, id: &str) {
        rig.events.send(ClientEvent::Registered {
            registration_id: id.to_string(),
        });
    }

    type Asked = Arc<Mutex<Vec<(TopicUse, String)>>>;

    /// A gate that admits every topic but `refused`, and remembers what it was asked.
    fn gate_refusing(refused: &'static str) -> (TopicGate, Asked) {
        let asked: Asked = Arc::new(Mutex::new(Vec::new()));
        let noted = asked.clone();
        let gate: TopicGate = Arc::new(move |use_, topic: String| {
            noted.lock().unwrap().push((use_, topic.clone()));
            Box::pin(async move {
                if topic == refused {
                    Err(format!("{topic} is not this agent's"))
                } else {
                    Ok(())
                }
            })
        });
        (gate, asked)
    }

    type Refusals = Arc<Mutex<Vec<(&'static str, String, String)>>>;

    /// Stands for the agent: notes each refusal it is told of.
    struct Listener {
        seen: Refusals,
    }

    #[async_trait]
    impl Actor for Listener {
        type Msg = ClientEvent;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            event: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            match event {
                ClientEvent::PublishRefused { topic, reason } => {
                    self.seen.lock().unwrap().push(("publish", topic, reason))
                }
                ClientEvent::SubscribeRefused { topic, reason } => {
                    self.seen.lock().unwrap().push(("subscribe", topic, reason))
                }
                _ => {}
            }
            Ok(())
        }
    }

    /// The refusals the agent is told of, as (kind, topic, reason).
    async fn refusals(rig: &Rig) -> Refusals {
        let seen: Refusals = Arc::new(Mutex::new(Vec::new()));
        let (listener, _) = Actor::spawn(None, Listener { seen: seen.clone() }, ())
            .await
            .unwrap();
        rig.events.subscribe(listener, Some);
        seen
    }

    fn refusal(kind: &'static str, topic: &str) -> (&'static str, String, String) {
        (
            kind,
            topic.to_string(),
            format!("{topic} is not this agent's"),
        )
    }

    fn subscribe(topic: &str) -> TcpClientMessage {
        TcpClientMessage::Subscribe {
            topic: topic.to_string(),
            trace_ctx: None,
        }
    }

    fn subscribed(received: &Received) -> Vec<String> {
        received
            .lock()
            .unwrap()
            .iter()
            .filter_map(|m| match m {
                TcpClientMessage::Subscribe { topic, .. } => Some(topic.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn a_publish_the_gate_refuses_is_neither_sent_nor_held_and_the_agent_is_told() {
        let (gate, asked) = gate_refusing("not-mine");
        let rig = gated_rig(10, Some(gate)).await;
        let refused = refusals(&rig).await;
        // Offline: a refused publish is not held for later either.
        rig.facade.cast(publish("not-mine")).unwrap();
        rig.facade.cast(publish("mine")).unwrap();
        settle().await;
        registered(&rig, "reg-1");
        settle().await;
        rig.facade.cast(publish("not-mine")).unwrap();
        rig.facade.cast(publish("mine")).unwrap();
        settle().await;

        assert_eq!(topics(&rig.spawner.latest().1), ["mine", "mine"]);
        assert_eq!(
            *refused.lock().unwrap(),
            vec![refusal("publish", "not-mine"); 2]
        );
        assert!(
            asked
                .lock()
                .unwrap()
                .iter()
                .all(|(use_, _)| *use_ == TopicUse::Publish)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_subscribe_the_gate_refuses_is_not_sent_and_the_agent_is_told() {
        let (gate, asked) = gate_refusing("not-mine");
        let rig = gated_rig(10, Some(gate)).await;
        let refused = refusals(&rig).await;
        registered(&rig, "reg-1");
        settle().await;
        rig.facade.cast(subscribe("not-mine")).unwrap();
        rig.facade.cast(subscribe("mine")).unwrap();
        settle().await;

        assert_eq!(subscribed(&rig.spawner.latest().1), ["mine"]);
        assert_eq!(*refused.lock().unwrap(), [refusal("subscribe", "not-mine")]);
        assert_eq!(
            *asked.lock().unwrap(),
            [
                (TopicUse::Subscribe, "not-mine".to_string()),
                (TopicUse::Subscribe, "mine".to_string())
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn publishes_before_registration_are_held_and_sent_in_order_once_registered() {
        let rig = rig(10).await;
        rig.facade.cast(publish("a")).unwrap();
        rig.facade.cast(publish("b")).unwrap();
        settle().await;
        assert!(topics(&rig.spawner.latest().1).is_empty(), "held, not sent");

        registered(&rig, "reg-1");
        settle().await;
        assert_eq!(topics(&rig.spawner.latest().1), ["a", "b"]);
    }

    #[tokio::test(start_paused = true)]
    async fn once_registered_publishes_go_straight_to_the_transport() {
        let rig = rig(10).await;
        registered(&rig, "reg-1");
        settle().await;
        rig.facade.cast(publish("c")).unwrap();
        settle().await;
        assert_eq!(topics(&rig.spawner.latest().1), ["c"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_dead_transport_is_replaced_after_a_backoff_and_publishes_in_between_are_kept() {
        let rig = rig(10).await;
        registered(&rig, "reg-1");
        settle().await;
        assert_eq!(rig.spawner.count(), 1);

        rig.spawner.latest().0.kill();
        settle().await;
        assert!(!rig.online.load(Ordering::SeqCst), "offline once it died");
        assert_eq!(rig.spawner.count(), 1, "not replaced before the backoff");

        rig.facade.cast(publish("during")).unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(rig.spawner.count(), 2, "replaced after the first backoff");
        assert!(
            topics(&rig.spawner.latest().1).is_empty(),
            "not yet registered"
        );

        registered(&rig, "reg-2");
        settle().await;
        assert_eq!(topics(&rig.spawner.latest().1), ["during"]);
        assert!(rig.online.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn the_backoff_doubles_and_starts_over_after_a_registration() {
        let rig = rig(10).await;
        // Never registers: each death waits twice as long as the one before.
        for expected_wait_ms in [1000u64, 2000, 4000] {
            let before = rig.spawner.count();
            rig.spawner.latest().0.kill();
            tokio::time::sleep(Duration::from_millis(expected_wait_ms - 100)).await;
            assert_eq!(
                rig.spawner.count(),
                before,
                "still waiting ({expected_wait_ms} ms)"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(
                rig.spawner.count(),
                before + 1,
                "replaced after {expected_wait_ms} ms"
            );
        }
        // A registration resets it.
        registered(&rig, "reg-9");
        settle().await;
        let before = rig.spawner.count();
        rig.spawner.latest().0.kill();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(rig.spawner.count(), before + 1, "back to the first backoff");
    }

    #[tokio::test(start_paused = true)]
    async fn a_transport_that_cannot_start_is_retried_with_backoff() {
        let rig = rig(10).await;
        *rig.spawner.fail_next.lock().unwrap() = 2;
        rig.spawner.latest().0.kill();
        tokio::time::sleep(Duration::from_millis(1100)).await; // first retry fails
        tokio::time::sleep(Duration::from_millis(2100)).await; // second retry fails
        assert_eq!(rig.spawner.count(), 1, "two attempts failed");
        tokio::time::sleep(Duration::from_millis(4100)).await; // third succeeds
        assert_eq!(rig.spawner.count(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_buffer_is_bounded_and_the_oldest_goes_first() {
        let rig = rig(3).await;
        for t in ["1", "2", "3", "4", "5"] {
            rig.facade.cast(publish(t)).unwrap();
        }
        settle().await;
        registered(&rig, "reg-1");
        settle().await;
        assert_eq!(topics(&rig.spawner.latest().1), ["3", "4", "5"]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_registration_id_is_known_while_the_transport_is_down() {
        let rig = rig(10).await;
        registered(&rig, "reg-1");
        settle().await;
        rig.spawner.latest().0.kill();
        settle().await;
        let id = ractor::call!(rig.facade, TcpClientMessage::GetRegistrationId).unwrap();
        assert_eq!(id.as_deref(), Some("reg-1"));
    }

    #[tokio::test(start_paused = true)]
    async fn the_address_callers_hold_never_changes() {
        let rig = rig(10).await;
        let address = rig.facade.get_id();
        rig.spawner.latest().0.kill();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(rig.facade.get_id(), address);
        assert_eq!(rig.facade.get_status(), ractor::ActorStatus::Running);
    }

    #[tokio::test(start_paused = true)]
    async fn stopping_the_facade_stops_the_transport() {
        let rig = rig(10).await;
        let transport = rig.spawner.latest().0;
        rig.facade
            .stop_and_wait(None, Some(Duration::from_secs(5)))
            .await
            .unwrap();
        settle().await;
        assert_eq!(transport.get_status(), ractor::ActorStatus::Stopped);
    }

    #[tokio::test(start_paused = true)]
    async fn a_replacement_asks_to_resume_the_last_session() {
        // The broker keeps a session whose client went away for a while, with what was held for
        // it, so the replacement asks for that session, and gets it back if it still exists.
        let rig = rig(10).await;
        registered(&rig, "reg-1");
        settle().await;
        rig.spawner.latest().0.kill();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(
            *rig.spawner.registration_ids_asked_for.lock().unwrap(),
            vec![None, Some("reg-1".to_string())],
            "the first transport has no session to resume; the replacement does"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_session_asked_for_is_the_latest_the_broker_gave() {
        let rig = rig(10).await;
        registered(&rig, "reg-1");
        settle().await;
        rig.spawner.latest().0.kill();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        // The broker had lost it and gave a new one: that is the one to ask for next.
        registered(&rig, "reg-2");
        settle().await;
        rig.spawner.latest().0.kill();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(
            rig.spawner
                .registration_ids_asked_for
                .lock()
                .unwrap()
                .last(),
            Some(&Some("reg-2".to_string()))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_replacement_that_never_registered_keeps_asking_for_the_last_session() {
        let rig = rig(10).await;
        registered(&rig, "reg-1");
        settle().await;
        // Two deaths with no registration in between.
        rig.spawner.latest().0.kill();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        rig.spawner.latest().0.kill();
        tokio::time::sleep(Duration::from_millis(2100)).await;
        let asked = rig
            .spawner
            .registration_ids_asked_for
            .lock()
            .unwrap()
            .clone();
        assert_eq!(asked.last(), Some(&Some("reg-1".to_string())));
    }
    // -- what the agent was refused --------------------------------------------------------------------------------

    type Admitted = Arc<Mutex<std::collections::BTreeSet<String>>>;
    type Watched = Arc<Mutex<Vec<RefusalNews>>>;

    /// A gate that admits what `admitted` holds at the time it is asked, as a policy that changes would.
    fn gate_admitting(admitted: &Admitted) -> (TopicGate, Asked) {
        let asked: Asked = Arc::new(Mutex::new(Vec::new()));
        let noted = asked.clone();
        let admitted = admitted.clone();
        let gate: TopicGate = Arc::new(move |use_, topic: String| {
            noted.lock().unwrap().push((use_, topic.clone()));
            let verdict = if admitted.lock().unwrap().contains(&topic) {
                Ok(())
            } else {
                Err(format!("{topic} is not admitted"))
            };
            Box::pin(async move { verdict })
        });
        (gate, asked)
    }

    fn watch() -> (RefusalWatch, Watched) {
        let watched: Watched = Arc::new(Mutex::new(Vec::new()));
        let noted = watched.clone();
        (
            Arc::new(move |news| noted.lock().unwrap().push(news)),
            watched,
        )
    }

    /// What stands refused, as the agent was last told it.
    fn standing(watched: &Watched) -> Vec<(String, TopicUse, bool)> {
        watched
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find_map(|news| match news {
                RefusalNews::Standing(standing) => Some(
                    standing
                        .iter()
                        .map(|s| (s.topic.clone(), s.use_, s.by_broker))
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn unpredicted(watched: &Watched) -> Vec<(String, TopicUse)> {
        watched
            .lock()
            .unwrap()
            .iter()
            .filter_map(|news| match news {
                RefusalNews::Unpredicted { topic, use_, .. } => Some((topic.clone(), *use_)),
                _ => None,
            })
            .collect()
    }

    fn broker_answered(rig: &Rig, topic: &str, use_: TopicUse, result: Result<(), String>) {
        rig.facade
            .cast(TcpClientMessage::BrokerAnswered {
                topic: topic.to_string(),
                use_,
                result,
            })
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_subscription_is_held_back_when_the_agent_subscribes_again_on_registering() {
        let admitted: Admitted = Arc::default();
        let (gate, asked) = gate_admitting(&admitted);
        let (watch, watched) = watch();
        let rig = watched_rig(10, Some(gate), Some(watch)).await;
        let refused = refusals(&rig).await;
        registered(&rig, "reg-1");
        settle().await;
        rig.facade.cast(subscribe("news")).unwrap();
        settle().await;
        // The agent registers again, and subscribes again to everything it wants.
        registered(&rig, "reg-2");
        settle().await;
        rig.facade.cast(subscribe("news")).unwrap();
        settle().await;

        assert!(subscribed(&rig.spawner.latest().1).is_empty());
        assert_eq!(asked.lock().unwrap().len(), 1, "asked once, not each time");
        assert_eq!(refused.lock().unwrap().len(), 1, "told once, not each time");
        assert_eq!(
            standing(&watched),
            [("news".to_string(), TopicUse::Subscribe, false)]
        );
        // The gate said so: nothing to ask the authority about.
        assert!(unpredicted(&watched).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn once_its_authority_changes_a_held_subscription_the_gate_admits_is_sent_once() {
        let admitted: Admitted = Arc::default();
        let (gate, _) = gate_admitting(&admitted);
        let (watch, watched) = watch();
        let rig = watched_rig(10, Some(gate), Some(watch)).await;
        registered(&rig, "reg-1");
        settle().await;
        rig.facade.cast(subscribe("news")).unwrap();
        settle().await;
        // A change that does not admit it: still held back.
        rig.facade.cast(TcpClientMessage::JudgeAgain).unwrap();
        settle().await;
        assert!(subscribed(&rig.spawner.latest().1).is_empty());

        admitted.lock().unwrap().insert("news".to_string());
        rig.facade.cast(TcpClientMessage::JudgeAgain).unwrap();
        settle().await;
        rig.facade.cast(TcpClientMessage::JudgeAgain).unwrap();
        settle().await;
        assert_eq!(subscribed(&rig.spawner.latest().1), ["news"]);
        assert!(standing(&watched).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_the_gate_did_not_predict_is_told_as_such_and_held_back() {
        let admitted: Admitted =
            Arc::new(Mutex::new(["news".to_string(), "sport".to_string()].into()));
        let (gate, _) = gate_admitting(&admitted);
        let (watch, watched) = watch();
        let rig = watched_rig(10, Some(gate), Some(watch)).await;
        registered(&rig, "reg-1");
        settle().await;
        rig.facade.cast(subscribe("news")).unwrap();
        settle().await;
        broker_answered(
            &rig,
            "news",
            TopicUse::Subscribe,
            Err("the broker refused: no".into()),
        );
        broker_answered(
            &rig,
            "sport",
            TopicUse::Publish,
            Err("the broker refused: no".into()),
        );
        settle().await;
        rig.facade.cast(subscribe("news")).unwrap();
        settle().await;

        assert_eq!(subscribed(&rig.spawner.latest().1), ["news"], "sent once");
        assert_eq!(
            unpredicted(&watched),
            [
                ("news".to_string(), TopicUse::Subscribe),
                ("sport".to_string(), TopicUse::Publish)
            ]
        );
        assert_eq!(
            standing(&watched),
            [
                ("news".to_string(), TopicUse::Subscribe, true),
                ("sport".to_string(), TopicUse::Publish, true)
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_subscription_taken_or_given_up_stands_refused_no_more() {
        let (watch, watched) = watch();
        let rig = watched_rig(10, None, Some(watch)).await;
        registered(&rig, "reg-1");
        settle().await;
        broker_answered(&rig, "news", TopicUse::Subscribe, Err("no".into()));
        broker_answered(&rig, "sport", TopicUse::Subscribe, Err("no".into()));
        settle().await;
        assert_eq!(standing(&watched).len(), 2);
        broker_answered(&rig, "news", TopicUse::Subscribe, Ok(()));
        rig.facade
            .cast(TcpClientMessage::UnsubscribeRequest {
                topic: "sport".to_string(),
                trace_ctx: None,
            })
            .unwrap();
        settle().await;
        assert!(standing(&watched).is_empty());
        // The unsubscribe still reaches the broker.
        assert!(
            rig.spawner
                .latest()
                .1
                .lock()
                .unwrap()
                .iter()
                .any(|m| matches!(
                    m,
                    TcpClientMessage::UnsubscribeRequest { topic, .. } if topic == "sport"
                ))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn with_no_gate_a_change_of_authority_sends_again_what_the_broker_refused() {
        let rig = gated_rig(10, None).await;
        registered(&rig, "reg-1");
        settle().await;
        rig.facade.cast(subscribe("news")).unwrap();
        settle().await;
        broker_answered(&rig, "news", TopicUse::Subscribe, Err("no".into()));
        settle().await;
        rig.facade.cast(subscribe("news")).unwrap();
        settle().await;
        assert_eq!(subscribed(&rig.spawner.latest().1), ["news"]);
        rig.facade.cast(TcpClientMessage::JudgeAgain).unwrap();
        settle().await;
        assert_eq!(subscribed(&rig.spawner.latest().1), ["news", "news"]);
    }
}
