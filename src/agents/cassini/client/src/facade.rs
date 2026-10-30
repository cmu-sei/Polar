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
//! Every publish it is given is numbered `(epoch, seq)` and kept in its outbox (`outbox.rs`) until the
//! broker acknowledges that message, so a message written to a connection that then dies is sent
//! again on the next registration (build step 2 of `docs/architecture/cassini-client-reliability.md`).
//! What is published while the outbox is full waits in the buffer, in order. There is no offline
//! file (step 3) and no deduplication (step 4): a message sent again may be delivered twice.

use crate::credit::{Credit, Slot};
use crate::outbox::{Entry, Outbox};
use crate::refused::{RefusedTopics, StandingRefusal};
use crate::{
    DeliveryMode, ReconnectMode, TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage,
};
use async_trait::async_trait;
use cassini_types::{ClientEvent, MessageId};
use ractor::{Actor, ActorCell, ActorProcessingErr, ActorRef, SupervisionEvent};
use std::collections::{BTreeMap, HashMap, VecDeque};
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
    /// What the outbox and the buffer hold, and their bounds, shared with whoever publishes through the facade
    /// (`credit.rs`).
    pub credit: Arc<Credit>,
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
    /// What was published and not yet acknowledged, numbered in this facade's epoch.
    outbox: Outbox,
    attempt: u32,
    closing: bool,
    /// What the agent was refused and still wants. It outlives every transport, as the agent's wanting does.
    refused: RefusedTopics,
    /// The publishes the broker could not take for now, each with how often it was deferred and the one timer that
    /// sends it again (none while it is in flight), until it is acknowledged or refused.
    deferred: HashMap<MessageId, Deferral>,
    /// The last resend timer armed: each has its own number, so one replaced or cancelled does nothing when it fires.
    timers: u64,
}

/// A publish the broker could not take for now.
#[derive(Debug, Default)]
struct Deferral {
    /// How often it was deferred: its next wait is the backoff's for that many attempts.
    times: u32,
    /// The timer that sends it again, if one is armed.
    timer: Option<u64>,
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

    /// Sends an outbox entry to the transport, if one is registered. Otherwise it waits in the outbox for the next.
    fn transmit(state: &FacadeState, entry: &Entry) {
        if let Some(transport) = &state.transport
            && state.registered
        {
            let _ = transport.send_message(TcpClientMessage::Transmit {
                topic: entry.topic.clone(),
                payload: entry.payload.clone(),
                trace_ctx: entry.trace_ctx,
                share_identity: entry.share_identity,
                id: entry.id,
            });
        }
    }

    /// Takes a publish that took no room before it came (an agent that casts to the facade): into the outbox if
    /// there is room and nothing waits before it, otherwise into the buffer; when the buffer is full too the newest,
    /// this one, is refused, and the agent is told.
    fn admit(state: &mut FacadeState, publish: TcpClientMessage) {
        let TcpClientMessage::Publish {
            topic,
            payload,
            trace_ctx,
            share_identity,
        } = publish
        else {
            return;
        };
        match state.args.credit.take(payload.len(), true) {
            Ok(slot) => Self::keep(state, slot, topic, payload, trace_ctx, share_identity),
            Err(_) => Self::refuse_newest(state, topic),
        }
    }

    /// Keeps a publish where it took room: numbered into the outbox and sent at once if a transport is registered, or
    /// into the buffer, behind what already waits.
    fn keep(
        state: &mut FacadeState,
        slot: Slot,
        topic: String,
        payload: Vec<u8>,
        trace_ctx: Option<cassini_types::WireTraceCtx>,
        share_identity: bool,
    ) {
        match slot {
            Slot::Outbox => {
                let entry = state
                    .outbox
                    .push(topic, Arc::new(payload), trace_ctx, share_identity)
                    .clone();
                Self::transmit(state, &entry);
            }
            Slot::Buffer => {
                state.buffer.push_back(TcpClientMessage::Publish {
                    topic,
                    payload,
                    trace_ctx,
                    share_identity,
                });
                // The outbox may have emptied since this took its room (its acknowledgements came first): nothing
                // else would move it, and its count in the buffer would hold back every caller after it.
                Self::promote(state);
            }
        }
    }

    /// The buffer is full: the newest publish is refused, never sent, and the agent is told
    /// (`ClientEvent::PublishDropped`), instead of the oldest being dropped with only a warning.
    fn refuse_newest(state: &FacadeState, topic: String) {
        let reason = format!(
            "the Cassini client's outbox and buffer are full ({} publishes wait): the newest is refused",
            state.buffer.len()
        );
        warn!(%topic, "{reason}");
        TcpClientActor::dispatch_event(
            &state.delivery,
            ClientEvent::PublishDropped { topic, reason },
        );
    }

    /// Moves what waits in the buffer into the outbox, oldest first, while it has room.
    fn promote(state: &mut FacadeState) {
        while let Some(TcpClientMessage::Publish { payload, .. }) = state.buffer.front() {
            if !state.args.credit.promote(payload.len()) {
                break;
            }
            let Some(TcpClientMessage::Publish {
                topic,
                payload,
                trace_ctx,
                share_identity,
            }) = state.buffer.pop_front()
            else {
                break;
            };
            let entry = state
                .outbox
                .push(topic, Arc::new(payload), trace_ctx, share_identity)
                .clone();
            Self::transmit(state, &entry);
        }
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
        let outbox = Outbox::new(crate::new_epoch(), args.credit.outbox_limits());
        let mut state = FacadeState {
            args,
            delivery,
            transport: None,
            registered: false,
            registration_id: None,
            buffer: VecDeque::new(),
            outbox,
            attempt: 0,
            closing: false,
            refused: RefusedTopics::default(),
            deferred: HashMap::new(),
            timers: 0,
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
                TcpClientMessage::Publish { topic, .. }
                | TcpClientMessage::PublishWith { topic, .. } => {
                    Some((TopicUse::Publish, topic.clone()))
                }
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
                // Refused before it was kept: the room it took is given back.
                if let TcpClientMessage::PublishWith { slot, payload, .. } = &message {
                    state.args.credit.release(*slot, payload.len());
                }
                return Ok(());
            }
        }
        match message {
            TcpClientMessage::ClientRegistered(id) => {
                state.registration_id = Some(id);
                state.attempt = 0;
                Self::set_online(state, true);
                // Everything not acknowledged is sent again, in order, with the number it had: the connection it
                // was written to may have died with it. Then what waits in the buffer, as there is room. A deferred
                // publish is among them, so its timer is cancelled: one resend at a time.
                for deferral in state.deferred.values_mut() {
                    deferral.timer = None;
                }
                for entry in state.outbox.entries() {
                    Self::transmit(state, entry);
                }
                Self::promote(state);
            }
            TcpClientMessage::Acknowledged { id } | TcpClientMessage::Refused { id } => {
                state.deferred.remove(&id);
                if let Some(entry) = state.outbox.settle(id) {
                    state.args.credit.settle(entry.payload.len());
                    Self::promote(state);
                }
            }
            // The broker could not take it now: it stays in the outbox, sent again after a backoff that doubles each
            // time it is deferred, up to the last (and on the next registration, as everything unacknowledged is).
            // One timer at a time: a later deferral replaces it.
            TcpClientMessage::Deferred { id } => {
                if state.outbox.get(id).is_some() {
                    state.timers += 1;
                    let timer = state.timers;
                    let deferral = state.deferred.entry(id).or_default();
                    let wait = state.args.backoff.delay(deferral.times);
                    deferral.times = deferral.times.saturating_add(1);
                    deferral.timer = Some(timer);
                    drop(myself.send_after(wait, move || TcpClientMessage::Resend { id, timer }));
                }
            }
            TcpClientMessage::Resend { id, timer } => {
                let armed = state
                    .deferred
                    .get_mut(&id)
                    .filter(|deferral| deferral.timer == Some(timer));
                if let Some(deferral) = armed {
                    deferral.timer = None;
                    if let Some(entry) = state.outbox.get(id).cloned() {
                        Self::transmit(state, &entry);
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
            publish @ TcpClientMessage::Publish { .. } => Self::admit(state, publish),
            TcpClientMessage::PublishWith {
                topic,
                payload,
                trace_ctx,
                share_identity,
                slot,
            } => Self::keep(state, slot, topic, payload, trace_ctx, share_identity),
            TcpClientMessage::NotSent { topic, reason } => {
                TcpClientActor::dispatch_event(
                    &state.delivery,
                    ClientEvent::PublishDropped { topic, reason },
                );
            }
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
    use crate::outbox::OutboxLimits;
    use cassini_types::MessageId;
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

    impl Rig {
        fn credit(&self) -> Arc<Credit> {
            self.credit.clone()
        }
    }

    struct Rig {
        credit: Arc<Credit>,
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
        bounded_rig(buffer_limit, OutboxLimits::default(), gate, watch).await
    }

    async fn bounded_rig(
        buffer_limit: usize,
        outbox: OutboxLimits,
        gate: Option<TopicGate>,
        watch: Option<RefusalWatch>,
    ) -> Rig {
        let spawner = Arc::new(FakeSpawner::default());
        let online = Arc::new(AtomicBool::new(false));
        let events = Arc::new(ractor::OutputPort::default());
        let credit = Arc::new(Credit::new(
            outbox,
            OutboxLimits {
                messages: buffer_limit,
                bytes: usize::MAX,
            },
        ));
        let (facade, _) = Actor::spawn(
            Some(format!("facade-{}", uuid::Uuid::new_v4())),
            TcpClientFacade,
            FacadeArgs {
                config: config(),
                delivery: DeliveryMode::Broadcast(events.clone()),
                transport_name: format!("transport-{}", uuid::Uuid::new_v4()),
                spawner: spawner.clone(),
                online: online.clone(),
                credit: credit.clone(),
                backoff: Backoff::default(),
                gate,
                watch,
            },
        )
        .await
        .unwrap();
        Rig {
            credit,
            facade,
            spawner,
            online,
            events,
        }
    }

    fn publish(topic: &str) -> TcpClientMessage {
        TcpClientMessage::publish(topic.to_string(), topic.as_bytes().to_vec(), None)
    }

    /// The topics of the publishes the transport was sent, in order (the facade sends each as its outbox numbered
    /// it).
    fn topics(received: &Arc<Mutex<Vec<TcpClientMessage>>>) -> Vec<String> {
        sent(received).into_iter().map(|(topic, _)| topic).collect()
    }

    /// The publishes the transport was sent, in order, each with its number.
    fn sent(received: &Arc<Mutex<Vec<TcpClientMessage>>>) -> Vec<(String, MessageId)> {
        received
            .lock()
            .unwrap()
            .iter()
            .filter_map(|m| match m {
                TcpClientMessage::Transmit { topic, id, .. } => Some((topic.clone(), *id)),
                _ => None,
            })
            .collect()
    }

    fn acknowledge(rig: &Rig, id: MessageId) {
        rig.facade
            .cast(TcpClientMessage::Acknowledged { id })
            .unwrap();
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

    /// The buffer is bounded, and when it is full the newest is refused, and the agent told: nothing already waiting
    /// is dropped (until 2026-10-30 the oldest was, with only a warning).
    #[tokio::test(start_paused = true)]
    async fn the_buffer_is_bounded_and_refuses_the_newest_and_says_so() {
        // An outbox of one, so everything after the first waits in the buffer.
        let rig = bounded_rig(
            3,
            OutboxLimits {
                messages: 1,
                bytes: usize::MAX,
            },
            None,
            None,
        )
        .await;
        let dropped = dropped(&rig).await;
        for t in ["1", "2", "3", "4", "5"] {
            rig.facade.cast(publish(t)).unwrap();
        }
        settle().await;
        registered(&rig, "reg-1");
        settle().await;
        for _ in 0..3 {
            let (_, id) = *sent(&rig.spawner.latest().1).last().unwrap();
            acknowledge(&rig, id);
            settle().await;
        }
        assert_eq!(topics(&rig.spawner.latest().1), ["1", "2", "3", "4"]);
        assert_eq!(
            *dropped.lock().unwrap(),
            ["5"],
            "the newest, refused, and the agent told"
        );
    }

    type Dropped = Arc<Mutex<Vec<String>>>;

    /// Stands for the agent: notes each publish it is told was refused before it was sent.
    struct DropListener {
        seen: Dropped,
    }

    #[async_trait]
    impl Actor for DropListener {
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
            if let ClientEvent::PublishDropped { topic, .. } = event {
                self.seen.lock().unwrap().push(topic);
            }
            Ok(())
        }
    }

    async fn dropped(rig: &Rig) -> Dropped {
        let seen: Dropped = Arc::default();
        let (listener, _) = Actor::spawn(None, DropListener { seen: seen.clone() }, ())
            .await
            .unwrap();
        rig.events.subscribe(listener, Some);
        seen
    }

    /// A publish the broker could not take now is kept: sent again after a wait, and on the next registration, with
    /// its number; one it denied is let go.
    #[tokio::test(start_paused = true)]
    async fn a_deferred_publish_is_kept_and_sent_again_and_a_refused_one_is_not() {
        let rig = rig(10).await;
        registered(&rig, "reg-1");
        settle().await;
        rig.facade.cast(publish("later")).unwrap();
        rig.facade.cast(publish("never")).unwrap();
        settle().await;
        let first = sent(&rig.spawner.latest().1);
        rig.facade
            .cast(TcpClientMessage::Deferred { id: first[0].1 })
            .unwrap();
        rig.facade
            .cast(TcpClientMessage::Refused { id: first[1].1 })
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(
            sent(&rig.spawner.latest().1),
            [first[0].clone(), first[1].clone(), first[0].clone()],
            "the deferred one is sent again after a wait"
        );
        rig.spawner.latest().0.kill();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        registered(&rig, "reg-2");
        settle().await;
        assert_eq!(
            sent(&rig.spawner.latest().1),
            [first[0].clone()],
            "and on the next registration, while it is not acknowledged"
        );
    }

    /// A deferred publish is sent again once at a time: a registration before its timer sends it once (the timer then
    /// does nothing), and a second deferral before its timer replaces the timer rather than adding a chain
    /// (cassini-backpressure.md, second review, finding C).
    #[tokio::test(start_paused = true)]
    async fn a_deferred_publish_is_sent_again_once_however_deferrals_and_registrations_fall() {
        let rig = rig(10).await;
        registered(&rig, "reg-1");
        settle().await;
        rig.facade.cast(publish("later")).unwrap();
        settle().await;
        let a = sent(&rig.spawner.latest().1)[0].clone();
        rig.facade
            .cast(TcpClientMessage::Deferred { id: a.1 })
            .unwrap();
        settle().await;
        registered(&rig, "reg-1");
        settle().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(
            sent(&rig.spawner.latest().1),
            [a.clone(), a.clone()],
            "sent again by the registration, and not again by the deferral's timer"
        );
        for _ in 0..2 {
            rig.facade
                .cast(TcpClientMessage::Deferred { id: a.1 })
                .unwrap();
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(
            sent(&rig.spawner.latest().1).len(),
            3,
            "two deferrals before the timer: one resend"
        );
    }

    /// A publish deferred again waits longer each time, doubling from the first backoff up to the last; once it is
    /// acknowledged its deferrals are forgotten (finding C).
    #[tokio::test(start_paused = true)]
    async fn a_publish_deferred_again_waits_longer_each_time_up_to_the_last_backoff() {
        let rig = rig(10).await;
        registered(&rig, "reg-1");
        settle().await;
        rig.facade.cast(publish("later")).unwrap();
        settle().await;
        let a = sent(&rig.spawner.latest().1)[0].clone();
        let count = || sent(&rig.spawner.latest().1).len();
        for (n, wait) in [(1, 1), (2, 2), (3, 4), (4, 8), (5, 16), (6, 30), (7, 30)] {
            rig.facade
                .cast(TcpClientMessage::Deferred { id: a.1 })
                .unwrap();
            tokio::time::sleep(Duration::from_secs(wait) - Duration::from_millis(100)).await;
            assert_eq!(count(), n, "deferral {n}: not sent again before {wait} s");
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(count(), n + 1, "deferral {n}: sent again after {wait} s");
        }
        rig.facade
            .cast(TcpClientMessage::Acknowledged { id: a.1 })
            .unwrap();
        rig.facade.cast(publish("next")).unwrap();
        settle().await;
        let b = sent(&rig.spawner.latest().1).last().unwrap().clone();
        rig.facade
            .cast(TcpClientMessage::Deferred { id: b.1 })
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(
            sent(&rig.spawner.latest().1).last(),
            Some(&b),
            "a new publish starts from the first backoff"
        );
        assert_eq!(count(), 10);
    }

    /// A publish that took buffer room, but reaches the facade after the acknowledgements emptied the outbox, is sent:
    /// it is not left in the buffer with nothing to move it, the buffer's count wedging every later caller.
    #[tokio::test(start_paused = true)]
    async fn a_buffered_publish_that_arrives_after_the_outbox_emptied_is_sent() {
        let rig = bounded_rig(
            10,
            OutboxLimits {
                messages: 1,
                bytes: usize::MAX,
            },
            None,
            None,
        )
        .await;
        registered(&rig, "reg-1");
        settle().await;
        let credit = rig.credit();
        let with = |topic: &str, slot| TcpClientMessage::PublishWith {
            topic: topic.to_string(),
            payload: b"x".to_vec(),
            trace_ctx: None,
            share_identity: true,
            slot,
        };
        assert_eq!(credit.take(1, true), Ok(Slot::Outbox));
        rig.facade.cast(with("a", Slot::Outbox)).unwrap();
        settle().await;
        let (_, a) = sent(&rig.spawner.latest().1)[0].clone();
        // The caller takes buffer room while "a" holds the outbox...
        assert_eq!(credit.take(1, true), Ok(Slot::Buffer));
        // ... and "a" is acknowledged before its publish reaches the facade.
        acknowledge(&rig, a);
        settle().await;
        rig.facade.cast(with("b", Slot::Buffer)).unwrap();
        settle().await;
        assert_eq!(topics(&rig.spawner.latest().1), ["a", "b"]);
        assert_eq!(credit.held(), (1, 1, 0, 0), "nothing wedged in the buffer");
    }

    #[tokio::test(start_paused = true)]
    async fn a_publish_that_took_its_room_is_kept_where_it_took_it_and_its_room_given_back_when_settled()
     {
        let rig = bounded_rig(
            10,
            OutboxLimits {
                messages: 1,
                bytes: usize::MAX,
            },
            None,
            None,
        )
        .await;
        registered(&rig, "reg-1");
        settle().await;
        let credit = rig.credit();
        for (topic, slot) in [("a", Slot::Outbox), ("b", Slot::Buffer)] {
            assert_eq!(credit.take(1, true), Ok(slot));
            rig.facade
                .cast(TcpClientMessage::PublishWith {
                    topic: topic.to_string(),
                    payload: b"x".to_vec(),
                    trace_ctx: None,
                    share_identity: true,
                    slot,
                })
                .unwrap();
        }
        settle().await;
        assert_eq!(topics(&rig.spawner.latest().1), ["a"]);
        assert_eq!(credit.held(), (1, 1, 1, 1));
        let (_, a) = sent(&rig.spawner.latest().1)[0].clone();
        acknowledge(&rig, a);
        settle().await;
        assert_eq!(topics(&rig.spawner.latest().1), ["a", "b"]);
        assert_eq!(credit.held(), (1, 1, 0, 0), "b moved into the room a left");
    }

    // -- the outbox (docs/architecture/cassini-client-reliability.md, build step 2) ---------------------------------

    #[tokio::test(start_paused = true)]
    async fn each_publish_is_numbered_one_two_three_in_the_facade_s_epoch() {
        let rig = rig(10).await;
        registered(&rig, "reg-1");
        settle().await;
        for t in ["a", "b", "c"] {
            rig.facade.cast(publish(t)).unwrap();
        }
        settle().await;
        let ids: Vec<MessageId> = sent(&rig.spawner.latest().1)
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        assert_eq!(ids.iter().map(|id| id.seq).collect::<Vec<_>>(), [1, 2, 3]);
        assert!(ids.iter().all(|id| id.epoch == ids[0].epoch));
    }

    #[tokio::test(start_paused = true)]
    async fn what_was_not_acknowledged_is_sent_again_with_its_number_by_the_next_transport() {
        let rig = rig(10).await;
        registered(&rig, "reg-1");
        settle().await;
        for t in ["a", "b", "c"] {
            rig.facade.cast(publish(t)).unwrap();
        }
        settle().await;
        let first = sent(&rig.spawner.latest().1);
        // Out of order: the last before the first, and the middle one never.
        acknowledge(&rig, first[2].1);
        acknowledge(&rig, first[0].1);
        settle().await;
        rig.spawner.latest().0.kill();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        registered(&rig, "reg-2");
        settle().await;
        assert_eq!(sent(&rig.spawner.latest().1), [first[1].clone()]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_outbox_holds_what_follows_until_an_acknowledgement_makes_room() {
        let rig = bounded_rig(
            10,
            OutboxLimits {
                messages: 2,
                bytes: usize::MAX,
            },
            None,
            None,
        )
        .await;
        registered(&rig, "reg-1");
        settle().await;
        for t in ["a", "b", "c"] {
            rig.facade.cast(publish(t)).unwrap();
        }
        settle().await;
        assert_eq!(topics(&rig.spawner.latest().1), ["a", "b"]);
        let (_, b) = sent(&rig.spawner.latest().1)[1].clone();
        acknowledge(&rig, b);
        settle().await;
        assert_eq!(topics(&rig.spawner.latest().1), ["a", "b", "c"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_publish_leaves_the_outbox_and_is_not_sent_again() {
        let rig = rig(10).await;
        registered(&rig, "reg-1");
        settle().await;
        rig.facade.cast(publish("a")).unwrap();
        settle().await;
        let (_, a) = sent(&rig.spawner.latest().1)[0].clone();
        rig.facade
            .cast(TcpClientMessage::Refused { id: a })
            .unwrap();
        settle().await;
        rig.spawner.latest().0.kill();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        registered(&rig, "reg-2");
        settle().await;
        assert!(sent(&rig.spawner.latest().1).is_empty());
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
