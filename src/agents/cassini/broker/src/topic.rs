use crate::UNEXPECTED_MESSAGE_STR;
use crate::{get_subscriber_name, BrokerMessage, PUBLISH_REQ_FAILED_TXT};
use ractor::registry::where_is;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, OutputPort};
use std::collections::{HashMap, VecDeque};
use tracing::{debug, error, info, trace, trace_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;

pub const TOPIC_ADD_FAILED_TXT: &str = "Failed to add topic \"{topic}!\"";

// ============================== Topic Supervisor definition ============================== //
/// Our supervisor for managing topics and their message queues
/// This processes is generally responsible for creating, removing, maintaining which topics
/// a client can know about
pub struct TopicManager;

pub struct TopicManagerState {
    pub topics: HashMap<String, ActorRef<BrokerMessage>>,
    pub subscriber_mgr: Option<ActorRef<BrokerMessage>>,
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
                    TopicAgentArgs { subscribers: None },
                    myself.clone().into(),
                )
                .await
                {
                    Ok((actor_ref, _handle)) => {
                        entry.insert(actor_ref.clone());
                        Ok(actor_ref)
                    }
                    Err(e) => {
                        error!("TopicManager failed to start topic actor for \"{}\": {e:?}", topic);
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
        trace_ctx: Option<tracing::SpanContext>, // adjust type to your trace ctx
        myself: ActorRef<BrokerMessage>,
        state: &mut TopicManagerState,
    ) -> Result<ActorRef<BrokerMessage>, ActorProcessingErr> {
        // 1) Ensure topic (atomic using entry API)
        let topic_ref = match state.topics.entry(topic.clone()) {
            std::collections::hash_map::Entry::Occupied(o) => o.get().clone(),
            std::collections::hash_map::Entry::Vacant(v) => {
                let (actor_ref, _handle) = Actor::spawn_linked(
                    Some(topic.clone()),
                    TopicAgent,
                    TopicAgentArgs { subscribers: None },
                    myself.clone().into(),
                )
                .await
                .map_err(|e| {
                    error!("Failed to spawn TopicAgent for {}: {e:?}", topic);
                    ActorProcessingErr::from(e)
                })?;
                v.insert(actor_ref.clone());
                actor_ref
            }
        };

        // 2) Ask SubscriberManager to create the subscriber (delegation)
        if let Some(sub_mgr) = &state.subscriber_mgr {
            sub_mgr.send_message(BrokerMessage::Subscribe {
                registration_id: registration_id.clone(),
                topic: topic.clone(),
                trace_ctx,
            }).map_err(|e| {
                error!("Failed to forward Subscribe to SubscriberManager: {e:?}");
                ActorProcessingErr::from(e)
            })?;
        } else {
            error!("TopicManager: subscriber_mgr missing while subscribing {} -> {}", registration_id, topic);
            return Err(ActorProcessingErr::from("SubscriberManager not configured for TopicManager"));
        }

        Ok(topic_ref)
    }
}

#[async_trait]
impl Actor for TopicManager {
    #[doc = " The message type for this actor"]
    type Msg = BrokerMessage;

    #[doc = " The type of state this actor manages internally"]
    type State = TopicManagerState;

    #[doc = " Initialization arguments"]
    type Arguments = TopicManagerArgs;

    #[doc = " Invoked when an actor is being started by the system."]
    #[doc = ""]
    #[doc = " Any initialization inherent to the actor\'s role should be"]
    #[doc = " performed here hence why it returns the initial state."]
    #[doc = ""]
    #[doc = " Panics in `pre_start` do not invoke the"]
    #[doc = " supervision strategy and the actor won\'t be started. [Actor]::`spawn`"]
    #[doc = " will return an error to the caller"]
    #[doc = ""]
    #[doc = " * `myself` - A handle to the [ActorCell] representing this actor"]
    #[doc = " * `args` - Arguments that are passed in the spawning of the actor which might"]
    #[doc = " be necessary to construct the initial state"]
    #[doc = ""]
    #[doc = " Returns an initial [Actor::State] to bootstrap the actor"]
    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: TopicManagerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        //
        // Try to reinit from a predermined list of topics
        debug!("Starting {myself:?}");
        let mut state = TopicManagerState {
            topics: HashMap::new(),
            subscriber_mgr: args.subscriber_mgr
        };

        if let Some(topics) = args.topics {
            debug!("Spawning topic actors...");

            for topic in topics {
                //start topic actors for that topic

                match Actor::spawn_linked(
                    Some(topic.clone()),
                    TopicAgent,
                    TopicAgentArgs { subscribers: None },
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
            BrokerMessage::PublishRequest { registration_id, topic, payload, trace_ctx } => {
                let span = trace_span!("topic_manager.handle_publish_request");
                if let Some(ctx) = trace_ctx { span.set_parent(ctx).ok(); }
                let _enter = span.enter();

                // Always go through ensure_topic
                match self.ensure_topic(topic.clone(), myself.clone(), state).await {
                    Ok(topic_actor) => {
                        // Forward the publish to the topic actor
                        if let Err(e) = topic_actor.send_message(BrokerMessage::PublishRequest {
                            registration_id,
                            topic: topic.clone(),
                            payload,
                            trace_ctx: Some(span.context()),
                        }) {
                            error!("TopicManager failed to forward publish to topic actor {:?}: {e:?}", topic);
                        }
                    }
                    Err(e) => {
                        // If we can’t create/ensure topic, send an error upstream
                        trace!(
                            "TopicManager failed to ensure topic \"{}\"; sending error to broker: {e:?}",
                            topic
                        );
                        if let Some(supervisor) = myself.try_get_supervisor() {
                            supervisor.send_message(BrokerMessage::ErrorMessage {
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
                let span = trace_span!("topic_manager.handle_publish_request");
                if let Some(ctx) = trace_ctx {
                    span.set_parent(ctx).ok();
                }
                let _enter = span.enter();

                trace!("Topic manager received publish response for \"{topic}\"");

                //forward to broker
                match myself.try_get_supervisor() {
                    Some(broker) => broker
                        .send_message(BrokerMessage::PublishResponse {
                            topic,
                            payload,
                            result: Result::Ok(()),
                            trace_ctx: Some(span.context()),
                        })
                        .expect("Expected to forward message to broker"),
                    None => todo!(),
                }
            }
            BrokerMessage::AddTopic {
                registration_id,
                topic,
                trace_ctx,
            } => {
                let span = trace_span!("topic_manager.handle_publish_request");
                if let Some(ctx) = trace_ctx {
                    span.set_parent(ctx).ok();
                }
                let _enter = span.enter();

                trace!("topic manager received add topic command for topic \"{topic}\"");

                // Just create a new topic agent

                if let Some(registration_id) = registration_id {
                    let mut subscribers = Vec::new();
                    subscribers.push(get_subscriber_name(&registration_id, &topic));
                    drop(_enter);

                    Actor::spawn_linked(
                        Some(topic.clone()),
                        TopicAgent,
                        TopicAgentArgs {
                            subscribers: Some(subscribers),
                        },
                        myself.clone().into(),
                    )
                    .await
                    .ok();
                } else {
                    // Just create a new topic agent
                    drop(_enter);
                    Actor::spawn_linked(
                        Some(topic.clone()),
                        TopicAgent,
                        TopicAgentArgs { subscribers: None },
                        myself.clone().into(),
                    )
                    .await
                    .ok();
                }
            }
            _ => warn!(UNEXPECTED_MESSAGE_STR),
        }
        Ok(())
    }
}

// ============================== Topic Worker definition ============================== //
/// Our worker process for managing message queues on a given topic
/// The broker supervisor is generally notified of incoming messages on the message queue.
struct TopicAgent;

struct TopicAgentState {
    subscribers: Vec<String>,
    queue: VecDeque<Vec<u8>>,
}

pub struct TopicAgentArgs {
    pub subscribers: Option<Vec<String>>,
}

#[async_trait]
impl Actor for TopicAgent {
    #[doc = " The message type for this actor"]
    type Msg = BrokerMessage;

    #[doc = " The type of state this actor manages internally"]
    type State = TopicAgentState;

    #[doc = " Initialization arguments"]
    type Arguments = TopicAgentArgs;

    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        args: TopicAgentArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(TopicAgentState {
            subscribers: args.subscribers.unwrap_or_default(),
            queue: VecDeque::new(),
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} Started",);
        Ok(())
    }
    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BrokerMessage::PublishRequest {
                registration_id,
                topic,
                payload,
                ..
            } => {
                let span = trace_span!("topic.dequeue_messages");
                // explicitly break publishing context and start one to trace dequeue flow.
                span.set_parent(opentelemetry::Context::new()).ok();

                let _enter = span.enter();

                trace!("Topic agent received publish request for \"{topic}\"");

                //send ACK to session that made the request
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

                //alert subscribers
                if !state.subscribers.is_empty() {
                    for subscriber in &state.subscribers {
                        where_is(subscriber.to_string()).map(|actor| {
                            if let Err(e) = actor.send_message(BrokerMessage::PublishResponse {
                                topic: topic.clone(),
                                payload: payload.clone(),
                                result: Ok(()),
                                trace_ctx: Some(span.context()),
                            }) {
                                warn!("{PUBLISH_REQ_FAILED_TXT}: {e}")
                            }
                        });
                    }
                } else {
                    //queue message
                    state.queue.push_back(payload);
                    info!(
                        "{}",
                        format!(
                            "New message on topic \"{0:?}\", queue has {1} message(s) waiting.",
                            myself.get_name(),
                            state.queue.len()
                        )
                    );
                }
            }
            BrokerMessage::Subscribe{
                registration_id,
                topic,
                trace_ctx,
            } => {
                if let Err(e) = self.ensure_topic_and_subscribe(
                    registration_id.clone(),
                    topic.clone(),
                    trace_ctx.clone(),
                    myself.clone(),
                    state,
                ).await {
                    error!("ensure_topic_and_subscribe failed: {e:?}");
                    // Optionally notify the session via SessionManager or return SubscribeAcknowledgment with Err
                }
            }
            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                let span =
                    trace_span!("topic.handle_unsubscribe_request", %registration_id, %topic);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("topic actor received unsubscribe request.");
            }
            _ => (),
        }
        Ok(())
    }
}
