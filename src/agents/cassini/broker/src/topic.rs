use crate::broker::Broker;
use crate::UNEXPECTED_MESSAGE_STR;
use crate::{get_subscriber_name, BrokerMessage, PUBLISH_REQ_FAILED_TXT};
use ractor::registry::where_is;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, OutputPort};
use rkyv::DeserializeUnsized;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use tracing::{debug, error, info, trace, trace_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;

pub const TOPIC_ADD_FAILED_TXT: &str = "Failed to add topic \"{topic}!\"";

// ============================== Topic Supervisor definition ============================== //
/// Our supervisor for managing topics and their message queues
/// This processes is generally responsible for creating, removing, maintaining which topics
/// a client can know about
pub struct TopicManager;

pub struct TopicManagerState {
    /// TODO: Not entirely sure this is needed since we can lookup actors by name any time, but acessing this may be faster?
    topics: HashMap<String, ActorRef<BrokerMessage>>, // Map of topic name to Topic addresses
}

pub struct TopicManagerArgs {
    pub topics: Option<Vec<String>>,
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
            BrokerMessage::PublishRequest {
                registration_id,
                topic,
                payload,
                trace_ctx,
            } => {
                let span = trace_span!("topic_manager.handle_publish_request");
                if let Some(ctx) = trace_ctx {
                    span.set_parent(ctx).ok();
                }
                let _enter = span.enter();

                trace!("Topic manager received publish request for \"{topic}\"");

                //forward request to topic agent
                if let Some(topic_actor) = state.topics.get(&topic.clone()) {
                    topic_actor
                        .send_message(BrokerMessage::PublishRequest {
                            registration_id,
                            topic,
                            payload,
                            trace_ctx: Some(span.context()),
                        })
                        .expect("Failed for forward publish request")
                } else {
                    info!("Creating topic: \"{topic}\".");

                    drop(_enter);

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
                        Err(e) => {
                            tracing::error!("Failed to start actor for topic: {topic}");
                            //TOOD: send error message here
                            match myself.try_get_supervisor() {
                                Some(broker) => broker
                                    .send_message(BrokerMessage::ErrorMessage {
                                        client_id: registration_id.clone(),
                                        error: e.to_string(),
                                    })
                                    .unwrap(),
                                None => todo!(),
                            }
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
                let topic_output = OutputPort::<Vec<u8>>::default();

                // subscribe subscribe the subscriber to the topic port.
                // \I admit this is a little crazy, but I think thats just life with a borrow checker.
                let cloned_topic = topic.clone();
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
                trace_ctx,
            } => {
                let span = trace_span!("topic.dequeue_messages");
                if let Some(ctx) = trace_ctx {
                    span.set_parent(ctx).ok();
                }
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
                debug!("Forwarding message to subscribers.");

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
                // state.topic_output.send(payload);
                //
            }
            // TODO: I think I also killed this code, the Topic manager handles this.
            BrokerMessage::Subscribe {
                registration_id,
                topic,
                trace_ctx,
            } => {
                let span = trace_span!("topic.handle_subscribe_request", %registration_id);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _ = span.enter();

                trace!("Topic agent \"{topic}\" received subscribe directive");

                debug!("Subscribing session to topic \"{topic}\"");

                let sub_id = get_subscriber_name(&registration_id, &topic);

                debug!("Adding {sub_id} to subscriber list");

                state.subscribers.push(sub_id.clone());
                //send any waiting messages
                where_is(sub_id.clone()).map(|subscriber| {
                    // TODO: I don't think this is wise in hindsight, we don't want to dump the queue upon getting every new subscriber.

                    while let Some(msg) = &state.queue.pop_front() {
                        // if this were to fail, user probably unsubscribed while we were dequeuing
                        // It should be ok to ignore this case.

                        subscriber
                            .send_message(BrokerMessage::PublishResponse {
                                topic: topic.clone(),
                                payload: msg.to_vec(),
                                result: Ok(()),
                                trace_ctx: Some(span.context()),
                            })
                            .ok();
                    }
                });

                //TODO: I don't know what it is about this code that isn't working well. We want to use outputports, but the messages
                // simply aren't going through.
                // where_is(get_subscriber_name(&registration_id, &topic)).map(|subscriber| {
                //     state
                //         .topic_output
                //         .subscribe(subscriber.into(), move |payload| {
                //             Some(BrokerMessage::PublishResponse {
                //                 topic: topic.clone(),
                //                 payload,
                //                 result: Ok(()),
                //                 // we'll start a new "deque_message" flow at the subscriber.
                //                 // The span doesn't live long enough to be accessed here anyway, so we can say we're done subscribing.
                //                 trace_ctx: Some(span.context()),
                //             })
                //         })
                // });
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
