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

use crate::PUBLISH_REQ_FAILED_TXT;
use crate::UNEXPECTED_MESSAGE_STR;
use crate::budget::Bounds;
use cassini_tracing::try_set_parent_otel;
use cassini_types::{Backlog, BrokerMessage, Hold, MessageId, Payload, ShutdownPhase};
use ractor::rpc::CallResult;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tracing::{Instrument, debug, error, info, trace, trace_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;

pub const TOPIC_ADD_FAILED_TXT: &str = "Failed to add topic \"{topic}!\"";
const QUEUE_TICK_SECS: u64 = 60;
/// How long a subscription waits for the subscriber manager before the client is told it was not made. It is waited
/// for in a task of its own, so this holds up nothing else.
const SUBSCRIBE_WAIT: Duration = Duration::from_secs(10);
const QUEUE_STALE_AFTER_SECS: u64 = QUEUE_TICK_SECS * 3; // ~3 minutes

// ============================== Topic Supervisor definition ============================== //
/// Our supervisor for managing topics and their message queues
/// This processes is generally responsible for creating, removing, maintaining which topics
/// a client can know about
pub struct TopicManager;

pub struct TopicManagerState {
    pub topics: HashMap<String, ActorRef<BrokerMessage>>,
    pub subscriber_mgr: Option<ActorRef<BrokerMessage>>,
    pub is_shutting_down: bool,
    pub bounds: Bounds,
}

pub struct TopicManagerArgs {
    pub topics: Option<Vec<String>>,
    // A reference to the SubscriberManager actor (injected at broker bootstrap).
    pub subscriber_mgr: Option<ActorRef<BrokerMessage>>, // or the concrete SubscriberMsg type
    /// The memory budget and the stall bound, which every topic applies.
    pub bounds: Bounds,
}

impl TopicManager {
    /// Ensure that a topic exists; if not, create it.
    /// Returns the topic actor ref.
    async fn ensure_topic(
        &self,
        topic: String,
        myself: ActorRef<BrokerMessage>,
        state: &mut TopicManagerState,
    ) -> Result<ActorRef<BrokerMessage>, ActorProcessingErr> {
        if state.is_shutting_down {
            return Err(ActorProcessingErr::from("TopicManager is shutting down"));
        }
        match state.topics.entry(topic.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                // Return the existing actor
                Ok(entry.get().clone())
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                // Create and store a new actor
                match Actor::spawn_linked(
                    Some(topic.clone()),
                    TopicAgent,
                    state.bounds.clone(),
                    myself.clone().into(),
                )
                .await
                {
                    Ok((actor_ref, _handle)) => {
                        entry.insert(actor_ref.clone());
                        Ok(actor_ref)
                    }
                    Err(e) => {
                        error!(
                            "TopicManager failed to start topic actor for \"{}\": {e:?}",
                            topic
                        );
                        Err(ActorProcessingErr::from(e))
                    }
                }
            }
        }
    }

    /// Ensure topic exists (idempotent) and ask the SubscriberManager to create/attach
    /// a subscriber for `registration_id`.
    async fn ensure_topic_and_subscribe(
        &self,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
        myself: ActorRef<BrokerMessage>,
        state: &mut TopicManagerState,
    ) -> Result<ActorRef<BrokerMessage>, ActorProcessingErr> {
        let topic_actor = self
            .ensure_topic(topic.clone(), myself.clone(), state)
            .await?;

        // The subscriber manager is asked in a task of its own: this actor routes every publish, so it must not wait
        // on a subscription (it used to, for up to 200 ms each, and a burst of subscriptions held up the broker's
        // publishing). Whatever comes of it, the client is told: a subscription it is never told about is one it
        // waits on for good.
        let Some(sub_mgr) = state.subscriber_mgr.clone() else {
            error!(%registration_id, %topic, "TopicManager: subscriber_mgr missing while subscribing");
            acknowledge_subscribe(
                &registration_id,
                &topic,
                Err("the broker cannot make subscriptions now".to_string()),
                trace_ctx,
            );
            return Err(ActorProcessingErr::from(
                "SubscriberManager not configured for TopicManager",
            ));
        };
        let attach_to = topic_actor.clone();
        tokio::spawn(async move {
            let asked = sub_mgr
                .call(
                    |reply| BrokerMessage::CreateSubscriber {
                        registration_id: registration_id.clone(),
                        topic: topic.clone(),
                        trace_ctx: trace_ctx.clone(),
                        reply,
                    },
                    Some(SUBSCRIBE_WAIT),
                )
                .await;
            let outcome = match asked {
                Ok(CallResult::Success(Ok((subscriber_ref, backlog)))) => attach_to
                    .cast(BrokerMessage::AddSubscriber {
                        registration_id: registration_id.clone(),
                        subscriber_ref,
                        backlog,
                        trace_ctx: trace_ctx.clone(),
                    })
                    .map_err(|e| format!("the topic could not take the subscriber: {e}")),
                Ok(CallResult::Success(Err(e))) => {
                    Err(format!("the subscriber could not be created: {e}"))
                }
                Ok(CallResult::Timeout) => Err(format!(
                    "the subscriber manager did not answer within {SUBSCRIBE_WAIT:?}"
                )),
                Ok(CallResult::SenderError) => {
                    Err("the subscriber manager did not answer: it dropped the request".to_string())
                }
                Err(e) => Err(format!("the subscriber manager did not answer: {e}")),
            };
            if let Err(why) = &outcome {
                warn!(%registration_id, %topic, "subscription refused: {why}");
            }
            acknowledge_subscribe(&registration_id, &topic, outcome, trace_ctx);
        });

        Ok(topic_actor)
    }
}

#[async_trait]
impl Actor for TopicManager {
    type Msg = BrokerMessage;
    type State = TopicManagerState;
    type Arguments = TopicManagerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: TopicManagerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        // Try to reinit from a predetermined list of topics
        debug!("Starting {myself:?}");
        let mut state = TopicManagerState {
            topics: HashMap::new(),
            subscriber_mgr: args.subscriber_mgr,
            is_shutting_down: false,
            bounds: args.bounds,
        };

        if let Some(topics) = args.topics {
            debug!("Spawning topic actors...");

            for topic in topics {
                match Actor::spawn_linked(
                    Some(topic.clone()),
                    TopicAgent,
                    state.bounds.clone(),
                    myself.clone().into(),
                )
                .await
                {
                    Ok((actor, _)) => {
                        state.topics.insert(topic.clone(), actor.clone());
                    }
                    Err(_) => error!("Failed to start actor for topic {topic}"),
                }
            }
        }

        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} Started");
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BrokerMessage::InitiateShutdownPhase {
                phase: ShutdownPhase::FlushTopicQueues,
            } => {
                self.handle_initiate_shutdown_phase(myself.clone(), state)
                    .await?;
            }
            BrokerMessage::PrepareForShutdown { .. } => {
                self.handle_prepare_for_shutdown(state).await?;
            }
            BrokerMessage::PublishRequest {
                registration_id,
                topic,
                payload,
                reply_to,
                trace_ctx,
                published_by,
                id,
                window,
            } => {
                self.handle_publish_request(
                    myself.clone(),
                    state,
                    registration_id,
                    topic,
                    payload,
                    reply_to,
                    trace_ctx,
                    published_by,
                    id,
                    window,
                )
                .await?;
            }
            BrokerMessage::PublishResponse {
                topic,
                payload,
                trace_ctx,
                published_by,
                ..
            } => {
                self.handle_publish_response(
                    myself.clone(),
                    state,
                    topic,
                    payload,
                    trace_ctx,
                    published_by,
                )
                .await?;
            }
            BrokerMessage::SubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                self.handle_subscribe_request(
                    myself.clone(),
                    state,
                    registration_id,
                    topic,
                    trace_ctx,
                )
                .await?;
            }
            BrokerMessage::GetTopics {
                registration_id,
                reply_to,
                trace_ctx,
            } => {
                self.handle_get_topics(myself.clone(), state, registration_id, reply_to, trace_ctx)
                    .await?;
            }
            _ => warn!(UNEXPECTED_MESSAGE_STR),
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if let SupervisionEvent::ActorTerminated(actor_cell, _, _) = msg
            && let Some(name) = actor_cell.get_name()
        {
            // Remove from topics map if it exists
            state.topics.remove(&name);
            // If we are in shutdown (flush phase) and no topics remain, complete
            if state.is_shutting_down && state.topics.is_empty() {
                self.signal_flush_complete(myself.clone(), state).await?;
            }
        }
        Ok(())
    }
}

impl TopicManager {
    // ===== Handler methods =====
    async fn handle_initiate_shutdown_phase(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut TopicManagerState,
    ) -> Result<(), ActorProcessingErr> {
        info!("Starting flush of topic queues");
        state.is_shutting_down = true;

        // Ask each topic agent to flush its queue and then shut down
        for topic_actor in state.topics.values() {
            let _ = topic_actor.send_message(BrokerMessage::FlushQueue {
                reply_to: myself.clone(),
            });
            let _ =
                topic_actor.send_message(BrokerMessage::PrepareForShutdown { auth_token: None });
        }

        if state.topics.is_empty() {
            self.signal_flush_complete(myself.clone(), state).await?;
        }
        // No timeout – ControlManager will handle phase timeout and force‑stop.
        Ok(())
    }

    async fn handle_prepare_for_shutdown(
        &self,
        state: &mut TopicManagerState,
    ) -> Result<(), ActorProcessingErr> {
        info!("TopicManager preparing for shutdown");
        state.is_shutting_down = true;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_publish_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut TopicManagerState,
        registration_id: String,
        topic: String,
        payload: Payload,
        reply_to: Option<ActorRef<BrokerMessage>>,
        trace_ctx: Option<opentelemetry::Context>,
        published_by: Option<String>,
        id: MessageId,
        window: Hold,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.topic_manager.publish_request", %registration_id, topic = %topic, payload_len = payload.len());

        // Reject new publishes during shutdown
        if state.is_shutting_down {
            warn!("Rejecting publish request during shutdown");
            return Ok(());
        }

        try_set_parent_otel(&span, trace_ctx);
        async {
            // Always go through ensure_topic
            match self
                .ensure_topic(topic.clone(), myself.clone(), state)
                .await
            {
                Ok(topic_actor) => {
                    // Forward the publish to the topic actor
                    if let Err(e) = topic_actor.send_message(BrokerMessage::PublishRequest {
                        registration_id,
                        topic: topic.clone(),
                        payload,
                        reply_to, // forward to topic agent
                        trace_ctx: Some(span.context()),
                        published_by,
                        id,
                        window,
                    }) {
                        error!(error = %e, topic = %topic, "failed to forward publish");
                    }
                }
                Err(e) => {
                    error!(error = %e, topic = %topic, "failed to ensure topic; notifying broker");
                    if let Some(supervisor) = myself.try_get_supervisor() {
                        supervisor
                            .send_message(BrokerMessage::ErrorMessage {
                                client_id: registration_id.clone(),
                                error: format!("Failed to ensure topic {topic}: {e:?}"),
                            })
                            .ok();
                    }
                }
            }
            Ok::<(), ActorProcessingErr>(())
        }
        .instrument(span.clone())
        .await
    }

    async fn handle_publish_response(
        &self,
        myself: ActorRef<BrokerMessage>,
        _state: &mut TopicManagerState,
        topic: String,
        payload: Payload,
        trace_ctx: Option<opentelemetry::Context>,
        published_by: Option<String>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.topic_manager.publish_response", topic = %topic, payload_len = payload.len());
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("Topic manager received publish response for \"{topic}\"");
        // forward to broker
        if let Some(broker) = myself.try_get_supervisor() {
            if let Err(e) = broker.send_message(BrokerMessage::PublishResponse {
                topic,
                payload,
                result: Ok(()),
                trace_ctx: Some(span.context()),
                published_by,
                origin: None,
                delivery_id: None,
                refusal: None,
            }) {
                error!("Failed to forward publish response to broker: {}", e);
            }
        } else {
            error!("TopicManager has no supervisor – dropping PublishResponse");
        }
        Ok(())
    }

    async fn handle_subscribe_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut TopicManagerState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        // Reject new subscriptions during shutdown
        if state.is_shutting_down {
            warn!("Rejecting subscribe request during shutdown");
            return Ok(());
        }

        let span = trace_span!("cassini.topic_manager.subscribe_request", %registration_id, topic = %topic);
        try_set_parent_otel(&span, trace_ctx.clone());
        async {
            if let Err(e) = self
                .ensure_topic_and_subscribe(
                    registration_id.clone(),
                    topic.clone(),
                    trace_ctx,
                    myself.clone(),
                    state,
                )
                .await
            {
                error!(error = %e, "ensure_topic_and_subscribe failed");
            }
            Ok::<(), ActorProcessingErr>(())
        }
        .instrument(span)
        .await
    }

    async fn handle_get_topics(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut TopicManagerState,
        registration_id: String,
        reply_to: ractor::RpcReplyPort<HashSet<String>>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.topic_manager.get_topics", %registration_id);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();

        trace!("Topic Manager retrieving topics.");

        let topics: HashSet<String> = state.topics.keys().map(|topic| topic.to_owned()).collect();

        reply_to
            .send(topics)
            .map_err(|_| warn!("Failed to send topics."))
            .ok();
        Ok(())
    }

    async fn signal_flush_complete(
        &self,
        myself: ActorRef<BrokerMessage>,
        _state: &mut TopicManagerState,
    ) -> Result<(), ActorProcessingErr> {
        info!("All topic queues flushed");

        if let Some(supervisor) = myself.try_get_supervisor() {
            supervisor.send_message(BrokerMessage::ShutdownPhaseComplete {
                phase: ShutdownPhase::FlushTopicQueues,
            })?;
        }

        // Stop the manager – no children left
        myself.stop(Some("SHUTDOWN_FLUSHED".to_string()));
        Ok(())
    }
}

// ============================== Topic Worker definition ============================== //
/// Our worker process for managing message queues on a given topic
/// The broker supervisor is generally notified of incoming messages on the message queue.
///
/// A topic accepts a publish (fans it out to its subscribers, or queues it when it has none, and acknowledges it)
/// only while none of its connected subscribers is above its high water of unacknowledged deliveries
/// (docs/architecture/cassini-backpressure.md, phase 1). Until then the publish waits here, in order, holding its
/// connection's window, so a publisher that outruns the slowest subscriber is slowed by TCP. A subscriber that holds
/// the topic back for longer than the stall bound has its connection ended, and the topic moves again.
struct TopicAgent;

/// A message waiting in a topic with no subscriber: when it came, its payload, who published it and their number for it.
type Queued = (
    std::time::Instant,
    Payload,
    Option<String>,
    Option<MessageId>,
);

/// A publish the topic has not accepted yet.
struct Pending {
    registration_id: String,
    payload: Payload,
    reply_to: Option<ActorRef<BrokerMessage>>,
    trace_ctx: Option<opentelemetry::Context>,
    published_by: Option<String>,
    id: MessageId,
    /// The publisher's connection's credit for it, released when it is accepted.
    window: Hold,
}

struct TopicAgentState {
    name: String,
    subscribers: HashMap<String, (ActorRef<BrokerMessage>, Arc<Backlog>)>,
    queue: VecDeque<Queued>,
    /// Publishes waiting for the subscribers to catch up, oldest first.
    waiting: VecDeque<Pending>,
    /// The bytes this topic holds, queued or waiting, as the budget's report names its holders.
    held: Arc<Backlog>,
    bounds: Bounds,
    /// A stall check is due.
    stall_armed: bool,
    flushing: bool,
}

#[async_trait]
impl Actor for TopicAgent {
    type Msg = BrokerMessage;
    type State = TopicAgentState;
    type Arguments = Bounds;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        bounds: Bounds,
    ) -> Result<Self::State, ActorProcessingErr> {
        let name = myself.get_name().unwrap_or_default();
        // Never above its high water: a topic holds nothing back, it is only named in the budget's report.
        let held = Arc::new(Backlog::new(format!("topic {name}"), usize::MAX));
        bounds.budget.register(&held);
        Ok(TopicAgentState {
            name,
            subscribers: HashMap::new(),
            queue: VecDeque::new(),
            waiting: VecDeque::new(),
            held,
            bounds,
            stall_armed: false,
            flushing: false,
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} Started");
        drop(
            myself.send_interval(std::time::Duration::from_secs(QUEUE_TICK_SECS), || {
                BrokerMessage::QueueExpiryTick
            }),
        );
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BrokerMessage::FlushQueue { reply_to } => {
                self.handle_flush_queue(myself.clone(), state, reply_to)
                    .await?;
            }
            BrokerMessage::PrepareForShutdown { .. } => {
                self.handle_prepare_for_shutdown(myself.clone(), state)
                    .await?;
            }
            BrokerMessage::PublishRequest {
                registration_id,
                payload,
                reply_to,
                trace_ctx,
                published_by,
                id,
                window,
                ..
            } => {
                trace!(topic = %state.name, %registration_id, "Topic agent received publish request");
                state.held.add(payload.weight());
                state.waiting.push_back(Pending {
                    registration_id,
                    payload,
                    reply_to,
                    trace_ctx,
                    published_by,
                    id,
                    window,
                });
                Self::pump(&myself, state);
            }
            BrokerMessage::AddSubscriber {
                registration_id,
                subscriber_ref,
                backlog,
                trace_ctx,
            } => {
                self.handle_add_subscriber(
                    myself.clone(),
                    state,
                    registration_id,
                    subscriber_ref,
                    backlog,
                    trace_ctx,
                )
                .await?;
            }
            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                self.handle_unsubscribe_request(
                    myself.clone(),
                    state,
                    registration_id,
                    topic,
                    trace_ctx,
                )
                .await?;
                Self::pump(&myself, state);
            }
            BrokerMessage::TopicResume => Self::pump(&myself, state),
            BrokerMessage::StallCheck => {
                state.stall_armed = false;
                Self::end_stalled(state);
                Self::pump(&myself, state);
            }
            BrokerMessage::QueueExpiryTick => {
                self.handle_queue_expiry_tick(myself.clone(), state).await?;
            }
            _ => (),
        }
        Ok(())
    }
}

impl TopicAgent {
    /// The subscriber holding the topic back, if one is: connected, and above its high water.
    fn holder_back(state: &TopicAgentState) -> Option<&Arc<Backlog>> {
        state
            .subscribers
            .values()
            .map(|(_, backlog)| backlog)
            .find(|backlog| backlog.holds_back())
    }

    /// Accepts what waits, oldest first, for as long as no subscriber holds the topic back. Held back, it arranges to
    /// look again whether a subscriber has stalled. While the broker flushes for its shutdown, everything is accepted.
    fn pump(myself: &ActorRef<BrokerMessage>, state: &mut TopicAgentState) {
        while !state.waiting.is_empty() {
            if !state.flushing
                && let Some(backlog) = Self::holder_back(state)
            {
                debug!(
                    topic = %state.name,
                    subscriber = backlog.name(),
                    held = backlog.bytes(),
                    waiting = state.waiting.len(),
                    "a subscriber above its high water holds the topic back"
                );
                if !state.stall_armed {
                    state.stall_armed = true;
                    drop(
                        myself.send_after(state.bounds.stall_check(), || BrokerMessage::StallCheck),
                    );
                }
                return;
            }
            if let Some(pending) = state.waiting.pop_front() {
                state.held.remove(pending.payload.weight());
                Self::accept(state, pending);
            }
        }
    }

    /// Ends the connection of each subscriber that has held the topic back above its high water for longer than the
    /// stall bound (docs/architecture/cassini-backpressure.md, "Decided" 1). It no longer holds the topic back; its
    /// session detaches for the retention window, keeping what it is owed while the budget allows.
    fn end_stalled(state: &mut TopicAgentState) {
        let stall = state.bounds.stall;
        for (registration_id, (_, backlog)) in &state.subscribers {
            let Some(above_for) = backlog.above_for() else {
                continue;
            };
            if !backlog.holds_back() || above_for < stall {
                continue;
            }
            // Its reader waits for room, so its acknowledgements may be what the broker is not reading: its
            // publishes are refused for now instead, and its reader reads on. It is ended at a later check if it is
            // still above its high water then (cassini-backpressure.md, second review, A).
            if let Some(connection) = crate::listener::connection_of(registration_id)
                && connection.spare(stall)
            {
                // It is given the stall bound again to drain, now that what it acknowledged is read.
                backlog.restart_stall_clock();
                info!(
                    topic = %state.name,
                    subscriber = backlog.name(),
                    held = backlog.bytes(),
                    "a subscriber holding the topic back waits for room to publish: its publishes are refused for now"
                );
                continue;
            }
            backlog.set_connected(false);
            state.bounds.budget.count_slow_disconnection();
            warn!(
                topic = %state.name,
                subscriber = backlog.name(),
                held = backlog.bytes(),
                ?above_for,
                ?stall,
                "a subscriber has held the topic back past the stall bound: its connection is ended"
            );
            if let Some(sessions) =
                ractor::registry::where_is(crate::SESSION_MANAGER_NAME.to_string())
            {
                let _ = sessions.send_message(BrokerMessage::SlowSubscriber {
                    registration_id: registration_id.clone(),
                    topic: state.name.clone(),
                    held: backlog.bytes(),
                    above_for,
                });
            }
        }
    }

    /// Fans a publish out to every subscriber, or queues it when there is none, and acknowledges it. A subscriber that
    /// cannot take a message has ended (its session is gone): it is dropped here, once, instead of being sent every
    /// later message and logging each loss. What reached no subscriber is kept for the next one, as when there is none,
    /// unless the budget is exhausted: then the queue refuses it (counted, and logged) rather than grow.
    fn accept(state: &mut TopicAgentState, pending: Pending) {
        let topic = state.name.clone();
        let span = trace_span!(
            "cassini.topic_agent.publish",
            registration_id = %pending.registration_id,
            topic = %topic,
            payload_len = pending.payload.len()
        );
        try_set_parent_otel(&span, pending.trace_ctx);
        let _g = span.enter();

        let mut delivered = 0usize;
        let mut ended = Vec::new();
        for (registration_id, (subscriber, _)) in &state.subscribers {
            match subscriber.send_message(BrokerMessage::PublishResponse {
                topic: topic.clone(),
                payload: pending.payload.clone(),
                result: Ok(()),
                trace_ctx: Some(span.context()),
                published_by: pending.published_by.clone(),
                origin: Some(pending.id),
                delivery_id: None,
                refusal: None,
            }) {
                Ok(()) => delivered += 1,
                Err(_) => ended.push(registration_id.clone()),
            }
        }
        if !ended.is_empty() {
            for registration_id in &ended {
                state.subscribers.remove(registration_id);
            }
            warn!(%topic, ended = ?ended, "{PUBLISH_REQ_FAILED_TXT}: these subscribers have ended and are dropped");
        }
        let mut refused = None;
        if delivered == 0 {
            if state.bounds.budget.is_exhausted() {
                state.bounds.budget.count_refused(1);
                warn!(
                    %topic,
                    bytes = pending.payload.len(),
                    queued = state.queue.len(),
                    "the broker's memory budget is exhausted: a message for a topic with no subscriber is refused, \
                     not queued"
                );
                refused = Some(
                    "the broker's memory budget is exhausted and the topic has no subscriber to take the message"
                        .to_string(),
                );
            } else {
                state.held.add(pending.payload.weight());
                state.queue.push_back((
                    std::time::Instant::now(),
                    pending.payload.clone(),
                    pending.published_by.clone(),
                    Some(pending.id),
                ));
                info!(
                    "New message on topic \"{topic}\", queue has {} message(s) waiting.",
                    state.queue.len()
                );
            }
        }

        if let Some(session) = pending.reply_to {
            // Refused for want of memory, it is not acknowledged: the publisher is told it was refused for now, keeps
            // it and sends it again (docs/architecture/cassini-backpressure.md, review finding 4).
            let answer = match refused {
                Some(reason) => BrokerMessage::PublishResponse {
                    topic: topic.clone(),
                    payload: Payload::uncharged(Arc::new(Vec::new())),
                    result: Err(reason),
                    trace_ctx: Some(span.context()),
                    published_by: None,
                    origin: Some(pending.id),
                    delivery_id: None,
                    refusal: Some(cassini_types::PublishRefusal::Unavailable),
                },
                None => BrokerMessage::PublishRequestAck {
                    topic: topic.clone(),
                    id: pending.id,
                    trace_ctx: Some(span.context()),
                },
            };
            if let Err(e) = session.send_message(answer) {
                warn!("Failed to send publish Ack to session! {e}")
            }
        } else {
            warn!("PublishRequest missing reply_to – cannot send ack");
        }
        // Accepted: the publisher's connection may read again what this frame took of its window.
        pending.window.release();
    }

    // ===== Handler methods for TopicAgent =====
    async fn handle_flush_queue(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut TopicAgentState,
        reply_to: ActorRef<BrokerMessage>,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Topic agent flushing queue");
        state.flushing = true;
        // What waits is accepted now, whatever the subscribers hold: the broker is shutting down.
        Self::pump(&myself, state);

        // If there are no subscribers and no queue, we're done immediately
        if state.subscribers.is_empty() && state.queue.is_empty() {
            reply_to.send_message(BrokerMessage::FlushQueueComplete {
                topic: state.name.clone(),
            })?;
            return Ok(());
        }

        // Phase 3 (flush) happens before phase 4 (terminate subscribers), so subscribers might still exist: queued
        // messages are delivered to them.
        while let Some((_queued_at, msg, published_by, origin)) = state.queue.pop_front() {
            state.held.remove(msg.weight());
            if state.subscribers.is_empty() {
                warn!(
                    "Discarding queued message on topic {} during flush: no subscribers",
                    state.name
                );
                continue;
            }
            for (subscriber, _) in state.subscribers.values() {
                if let Err(e) = subscriber.send_message(BrokerMessage::PublishResponse {
                    topic: state.name.clone(),
                    payload: msg.clone(),
                    result: Ok(()),
                    trace_ctx: None,
                    published_by: published_by.clone(),
                    origin,
                    delivery_id: None,
                    refusal: None,
                }) {
                    warn!("Failed to deliver queued message to subscriber: {}", e);
                }
            }
        }

        // After delivering, we signal completion.
        // Note: We don't wait for acknowledgments from subscribers; that's the subscriber's responsibility.
        reply_to.send_message(BrokerMessage::FlushQueueComplete {
            topic: state.name.clone(),
        })?;

        state.flushing = false;
        Ok(())
    }

    async fn handle_prepare_for_shutdown(
        &self,
        myself: ActorRef<BrokerMessage>,
        _state: &mut TopicAgentState,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Topic agent received shutdown notification, stopping");
        // No ongoing work besides queue flush – that’s handled by FlushQueue.
        // Just stop.
        myself.stop(Some("SHUTDOWN_GRACEFUL".to_string()));
        Ok(())
    }

    async fn handle_add_subscriber(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut TopicAgentState,
        registration_id: String,
        subscriber_ref: ActorRef<BrokerMessage>,
        backlog: Arc<Backlog>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        if state.flushing {
            debug!("Adding subscriber during flush phase");
        }
        let span = trace_span!("cassini.topic_agent.add_subscriber", %registration_id);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("Received subscribe session directive");

        // registration_id is the key -- inserting for an already-present
        // registration replaces the old entry rather than accumulating a
        // second, stale ActorRef alongside it. This is the fix for the
        // structural one-to-many mismatch between TopicAgent's own
        // tracking and SubscriberManager's already-enforced one-to-one
        // (registration_id, topic) -> SubscriberAgent relationship.
        state
            .subscribers
            .insert(registration_id, (subscriber_ref.clone(), backlog));

        // Drain queued messages now that someone is listening.
        while let Some((_queued_at, msg, published_by, origin)) = state.queue.pop_front() {
            state.held.remove(msg.weight());
            subscriber_ref
                .send_message(BrokerMessage::PublishResponse {
                    topic: state.name.clone(),
                    payload: msg,
                    result: Ok(()),
                    trace_ctx: Some(span.context()),
                    published_by,
                    origin,
                    delivery_id: None,
                    refusal: None,
                })
                .ok();
        }
        Ok(())
    }

    async fn handle_unsubscribe_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut TopicAgentState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.topic_agent.unsubscribe_request", %registration_id, topic = %topic);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();

        if state.subscribers.remove(&registration_id).is_some() {
            trace!("Removed subscriber for registration {registration_id}");
        } else {
            trace!("No subscriber for registration {registration_id} on this topic (already gone)");
        }
        Ok(())
    }

    /// Drop messages that have sat in this topic's zero-subscriber queue
    /// for too long. Messages are pushed in arrival order, so the oldest
    /// is always at the front -- once we hit one that isn't stale yet,
    /// everything behind it is guaranteed not stale either.
    ///
    /// We deliberately don't persist these to disk; that's a real,
    /// separate future capability, not something to improvise here. For
    /// now: log clearly, then drop, so an abandoned topic doesn't hold
    /// memory forever.
    async fn handle_queue_expiry_tick(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut TopicAgentState,
    ) -> Result<(), ActorProcessingErr> {
        let threshold = std::time::Duration::from_secs(QUEUE_STALE_AFTER_SECS);
        let mut dropped = 0usize;

        while let Some((queued_at, _, _, _)) = state.queue.front() {
            if queued_at.elapsed() < threshold {
                break;
            }
            if let Some((_, payload, _, _)) = state.queue.pop_front() {
                state.held.remove(payload.weight());
            }
            dropped += 1;
        }

        if dropped > 0 {
            warn!(
                "Dropped {dropped} stale message(s) on topic \"{0:?}\" -- no subscriber \
                 appeared within {QUEUE_STALE_AFTER_SECS}s of the oldest message arriving",
                state.name
            );
        }
        Ok(())
    }
}

/// Tells the client, through its session (registered under its registration id), whether its subscription to `topic`
/// was taken. A session that has gone has no client to tell.
pub(crate) fn acknowledge_subscribe(
    registration_id: &str,
    topic: &str,
    result: Result<(), String>,
    trace_ctx: Option<opentelemetry::Context>,
) {
    if let Some(session) = ractor::registry::where_is(registration_id.to_string()) {
        let _ = session.send_message(BrokerMessage::SubscribeAcknowledgment {
            registration_id: registration_id.to_string(),
            topic: topic.to_string(),
            result,
            trace_ctx,
        });
    }
}
