use crate::{PUBLISH_REQ_FAILED_TXT};
use crate::UNEXPECTED_MESSAGE_STR;
use cassini_types::{BrokerMessage, ShutdownPhase};
use ractor::registry::where_is;
use ractor::rpc::CallResult;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;
use std::sync::Arc;
use tracing::{debug, error, info, trace, trace_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use cassini_tracing::try_set_parent_otel;

pub const TOPIC_ADD_FAILED_TXT: &str = "Failed to add topic \"{topic}!\"";

// ============================== Topic Supervisor definition ============================== //
/// Our supervisor for managing topics and their message queues
/// This processes is generally responsible for creating, removing, maintaining which topics
/// a client can know about
pub struct TopicManager;

pub struct TopicManagerState {
    pub topics: HashMap<String, ActorRef<BrokerMessage>>,
    pub subscriber_mgr: Option<ActorRef<BrokerMessage>>,
    pub is_shutting_down: bool,
}

pub struct TopicManagerArgs {
    pub topics: Option<Vec<String>>,
    // A reference to the SubscriberManager actor (injected at broker bootstrap).
    pub subscriber_mgr: Option<ActorRef<BrokerMessage>>, // or the concrete SubscriberMsg type
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
                    (),
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

        // Ask SubscriberManager to create the subscriber (delegation)
        if let Some(sub_mgr) = &state.subscriber_mgr {
            // BUGFIX: don't silently ignore call() failures (mailbox closed/actor dead/etc).
            // Also: keep the trace context consistent through the whole subscribe flow.
            let call_res = sub_mgr
                .call(
                    |reply| BrokerMessage::CreateSubscriber {
                        registration_id: registration_id.clone(),
                        topic: topic.clone(),
                        trace_ctx: trace_ctx.clone(),
                        reply,
                    },
                    Some(Duration::from_millis(200)),
                )
                .await;

            match call_res {
                Ok(call_result) => match call_result {
                    CallResult::Success(result) => match result {
                        Ok(subscriber_ref) => {
                            // Attach subscriber to topic
                            if let Err(e) = topic_actor.cast(BrokerMessage::AddSubscriber {
                                subscriber_ref,
                                // Preserve tracing into the add-subscriber/dequeue path.
                                trace_ctx: trace_ctx.clone(),
                            }) {
                                warn!(
                                    error = %e,
                                    registration_id = %registration_id,
                                    topic = %topic,
                                    "failed to cast AddSubscriber to topic actor"
                                );
                            }
                        }
                        Err(err) => {
                            // Don't leave a TODO landmine in the hot path; report and fail.
                            error!(
                                error = ?err,
                                registration_id = %registration_id,
                                topic = %topic,
                                "subscriber_mgr returned subscription error"
                            );
                            return Err(ActorProcessingErr::from(
                                "SubscriberManager returned subscription error",
                            ));
                        }
                    },
                    CallResult::SenderError => {
                        error!(
                            registration_id = %registration_id,
                            topic = %topic,
                            "SubscriberManager sender error while subscribing"
                        );
                        return Err(ActorProcessingErr::from(
                            "SubscriberManager sender error while subscribing",
                        ));
                    }
                    CallResult::Timeout => {
                        error!(
                            registration_id = %registration_id,
                            topic = %topic,
                            timeout_ms = 200u64,
                            "SubscriberManager call timed out while subscribing"
                        );
                        return Err(ActorProcessingErr::from(
                            "SubscriberManager call timed out while subscribing",
                        ));
                    }
                },
                Err(e) => {
                    // ractor::rpc::CallError: actor stopped/mailbox closed/etc.
                    error!(
                        error = %e,
                        registration_id = %registration_id,
                        topic = %topic,
                        "SubscriberManager call failed while subscribing"
                    );
                    return Err(ActorProcessingErr::from(
                        "SubscriberManager call failed while subscribing",
                    ));
                }
            }
        } else {
            error!(
                "TopicManager: subscriber_mgr missing while subscribing {} -> {}",
                registration_id, topic
            );
            return Err(ActorProcessingErr::from(
                "SubscriberManager not configured for TopicManager",
            ));
        }

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
        };

        if let Some(topics) = args.topics {
            debug!("Spawning topic actors...");

            for topic in topics {
                match Actor::spawn_linked(
                    Some(topic.clone()),
                    TopicAgent,
                    (),
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
            BrokerMessage::InitiateShutdownPhase { phase } if phase == ShutdownPhase::FlushTopicQueues => {
                info!("Starting flush of topic queues");
                state.is_shutting_down = true;

                // Ask each topic agent to flush its queue and then shut down
                for (_topic_name, topic_actor) in &state.topics {
                    let _ = topic_actor.send_message(BrokerMessage::FlushQueue {
                        reply_to: myself.clone().into(),
                    });
                    let _ = topic_actor.send_message(BrokerMessage::PrepareForShutdown { auth_token: None });
                }

                if state.topics.is_empty() {
                    self.signal_flush_complete(myself.clone(), state).await?;
                }
                // No timeout – ControlManager will handle phase timeout and force‑stop.
            }

            BrokerMessage::PrepareForShutdown { .. } => {
                info!("TopicManager preparing for shutdown");
                state.is_shutting_down = true;
            }

            BrokerMessage::PublishRequest {
                registration_id,
                topic,
                payload,
                trace_ctx,
            } => {
                let span = trace_span!("cassini.topic_manager.publish_request", %registration_id, topic = %topic, payload_len = payload.len());
                
                // Reject new publishes during shutdown
                if state.is_shutting_down {
                    warn!("Rejecting publish request during shutdown");
                    return Ok(());
                }

                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();

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
                            trace_ctx: Some(span.context()),
                        }) {
                            error!(error = %e, topic = %topic, "failed to forward publish to topic actor");
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
            }

            BrokerMessage::PublishResponse {
                topic,
                payload,
                trace_ctx,
                ..
            } => {
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
                    }) {
                        error!("Failed to forward publish response to broker: {}", e);
                    }
                } else {
                    error!("TopicManager has no supervisor – dropping PublishResponse");
                }
            }

            BrokerMessage::SubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                // Reject new subscriptions during shutdown
                if state.is_shutting_down {
                    warn!("Rejecting subscribe request during shutdown");
                    return Ok(());
                }

                let span = trace_span!("cassini.topic_manager.subscribe_request", %registration_id, topic = %topic);
                try_set_parent_otel(&span, trace_ctx.clone());
                let _g = span.enter();

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
            }

            BrokerMessage::GetTopics {
                registration_id,
                reply_to,
                trace_ctx,
            } => {
                let span = trace_span!("cassini.topic_manager.get_topics", %registration_id);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();

                trace!("Topic Manager retrieving topics.");

                let topics: HashSet<String> =
                    state.topics.keys().map(|topic| topic.to_owned()).collect();

                reply_to
                    .send(topics)
                    .map_err(|_| warn!("Failed to send topics."))
                    .ok();
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
        match msg {
            SupervisionEvent::ActorTerminated(actor_cell, _, _) => {
                if let Some(name) = actor_cell.get_name() {
                    // Remove from topics map if it exists
                    state.topics.remove(&name);
                    // If we are in shutdown (flush phase) and no topics remain, complete
                    if state.is_shutting_down && state.topics.is_empty() {
                        self.signal_flush_complete(myself.clone(), state).await?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl TopicManager {
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
struct TopicAgent;

struct TopicAgentState {
    subscribers: Vec<ActorRef<BrokerMessage>>,
    queue: VecDeque<Arc<Vec<u8>>>,  // Changed from VecDeque<Vec<u8>>
    flushing: bool, // whether we are in flush phase
}

#[async_trait]
impl Actor for TopicAgent {
    type Msg = BrokerMessage;
    type State = TopicAgentState;
    type Arguments = ();

    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(TopicAgentState {
            subscribers: Vec::new(),
            queue: VecDeque::new(),
            flushing: false,
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
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
            BrokerMessage::FlushQueue { reply_to } => {
                debug!("Topic agent flushing queue");
                state.flushing = true;

                // If there are no subscribers and no queue, we're done immediately
                if state.subscribers.is_empty() && state.queue.is_empty() {
                    reply_to.send_message(BrokerMessage::FlushQueueComplete {
                        topic: myself.get_name().unwrap_or_default(),
                    })?;
                    return Ok(());
                }

                // If there are no subscribers but queue has messages, we cannot deliver them.
                // According to the design, we should still consider the queue flushed? 
                // Actually, during shutdown, there are no active subscribers (they are terminated in phase 4).
                // But phase 3 (flush) happens before phase 4 (terminate subscribers). So subscribers might still exist.
                // We must attempt to deliver all queued messages to existing subscribers.
                while let Some(msg) = state.queue.pop_front() {
                    if state.subscribers.is_empty() {
                        // No one to deliver to; we discard the message (but log it).
                        warn!("Discarding queued message on topic {} during flush: no subscribers", myself.get_name().unwrap_or_default());
                        continue;
                    }
                    for subscriber in &state.subscribers {
                        if let Err(e) = subscriber.send_message(BrokerMessage::PublishResponse {
                            topic: myself.get_name().unwrap_or_default(),
                            payload: msg.clone(),
                            result: Ok(()),
                            trace_ctx: None,
                        }) {
                            warn!("Failed to deliver queued message to subscriber: {}", e);
                        }
                    }
                }

                // After delivering, we signal completion.
                // Note: We don't wait for acknowledgments from subscribers; that's the subscriber's responsibility.
                reply_to.send_message(BrokerMessage::FlushQueueComplete {
                    topic: myself.get_name().unwrap_or_default(),
                })?;
                
                state.flushing = false;
            }

            // BrokerMessage::PrepareForShutdown { .. } => {
            //     // Mark as shutting down; we may still need to deliver queued messages.
            //     debug!("Topic agent preparing for shutdown");
            //     // No immediate action; flush will be triggered later.
            // }
            BrokerMessage::PrepareForShutdown { .. } => {
                debug!("Topic agent received shutdown notification, stopping");
                // No ongoing work besides queue flush – that’s handled by FlushQueue.
                // Just stop.
                myself.stop(Some("SHUTDOWN_GRACEFUL".to_string()));
            }

            BrokerMessage::PublishRequest {
                registration_id,
                topic,
                payload,
                trace_ctx,
            } => {
                let span = trace_span!(
                    "cassini.topic_agent.publish",
                    %registration_id,
                    topic = %topic,
                    payload_len = payload.len()
                );
                
                // If we are in flush phase, we should still accept publishes? Actually no, 
                // the TopicManager rejects new publishes during shutdown. But if we are here,
                // it's because the message was already accepted before shutdown.
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();

                trace!("Topic agent received publish request for \"{topic}\"");

                // alert subscribers
                if !state.subscribers.is_empty() {
                    // If we are in flush phase, we still deliver normally.
                    for subscriber in &state.subscribers {
                        // Clone payload for each subscriber
                        let send_result = if state.flushing {
                            // During flush, we may want to be more aggressive, but it's the same.
                            subscriber.send_message(BrokerMessage::PublishResponse {
                                topic: topic.clone(),
                                payload: payload.clone(),
                                result: Ok(()),
                                trace_ctx: Some(span.context()),
                            })
                        } else {
                            subscriber.send_message(BrokerMessage::PublishResponse {
                                topic: topic.clone(),
                                payload: payload.clone(),
                                result: Ok(()),
                                trace_ctx: Some(span.context()),
                            })
                        };
                        if let Err(e) = send_result {
                            warn!("{PUBLISH_REQ_FAILED_TXT}: {e}")
                        }
                    }
                } else {
                    // No subscribers, queue the message
                    state.queue.push_back(payload.clone());
                    info!(
                        "{}",
                        format!(
                            "New message on topic \"{0:?}\", queue has {1} message(s) waiting.",
                            myself.get_name(),
                            state.queue.len()
                        )
                    );
                }

                // send ACK to session that made the request
                match where_is(registration_id.clone()) {
                    Some(session) => {
                        if let Err(e) = session.send_message(BrokerMessage::PublishRequestAck {
                            topic: topic.clone(),
                            trace_ctx: Some(span.context()),
                        }) {
                            warn!("Failed to send publish Ack to session! {e}")
                        }
                    }
                    None => warn!("Failed to lookup session {registration_id}"),
                }
            }

            BrokerMessage::AddSubscriber {
                subscriber_ref,
                trace_ctx,
            } => {
                // If we are in flush phase and already delivered all queued messages, we may still get new subscribers?
                // During shutdown, we shouldn't get new subscriptions, but just in case, we still add them.
                if state.flushing {
                    debug!("Adding subscriber during flush phase");
                }

                let span = trace_span!("cassini.topic_agent.add_subscriber");
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();

                trace!("Received subscribe session directive");

                state.subscribers.push(subscriber_ref.clone());

                // Drain queued messages now that someone is listening.
                // This is the normal drain, not the shutdown flush.
                while let Some(msg) = state.queue.pop_front() {
                    // if this were to fail, user probably unsubscribed while we were dequeuing
                    // It should be ok to ignore this case.
                    subscriber_ref
                        .send_message(BrokerMessage::PublishResponse {
                            topic: myself.get_name().unwrap(),
                            payload: msg.clone(),
                            result: Ok(()),
                            trace_ctx: Some(span.context()),
                        })
                        .ok();
                }
            }

            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                let span = trace_span!("cassini.topic_agent.unsubscribe_request", %registration_id, topic = %topic);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();

                trace!("topic actor received unsubscribe request.");
            }

            _ => (),
        }
        Ok(())
    }
}
