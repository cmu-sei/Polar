use crate::UNEXPECTED_MESSAGE_STR;
use crate::{
    get_subscriber_name, BrokerMessage, CLIENT_NOT_FOUND_TXT, DISCONNECTED_REASON,
    PUBLISH_REQ_FAILED_TXT, REGISTRATION_REQ_FAILED_TXT, SESSION_NOT_FOUND_TXT,
    SUBSCRIBE_REQUEST_FAILED_TXT, TIMEOUT_REASON,
};
use ractor::{
    async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef, SupervisionEvent,
};
use std::collections::VecDeque;
use tracing::{debug, error, info, trace, trace_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;

pub const SUBSCRIBER_NOT_FOUND_TXT: &str = "Subscriber not found!";

/// Our supervisor for the subscribers
/// When a user subscribes to a new topic, this actor is notified inb conjunction with the topic manager.
/// A new process is started to wait and listen for new messages on that topic and forward messages.
/// Clients are only considered subscribed if an actor process exists and is managed by this actor
pub struct SubscriberManager;

/// Define the state for the actor
pub struct SubscriberManagerState;

impl SubscriberManager {
    /// Removes all subscriptions for a given session
    fn forget_subscriptions(
        registration_id: String,
        myself: ActorRef<BrokerMessage>,
        reason: Option<String>,
    ) {
        //cleanup all subscriptions for session
        for subscriber in myself.get_children().into_iter().filter(|cell| {
            let sub_name = cell.get_name().unwrap();
            let sub_id = sub_name.split_once(":").unwrap().0;
            registration_id.eq(sub_id)
        }) {
            subscriber.stop(reason.clone());
        }
    }
}
#[async_trait]
impl Actor for SubscriberManager {
    type Msg = BrokerMessage; // Messages this actor handles
    type State = SubscriberManagerState; // Internal state
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::debug!("{myself:?} starting");

        Ok(SubscriberManagerState)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::debug!("{myself:?} Started");
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BrokerMessage::RegistrationRequest {
                registration_id,
                client_id,
                trace_ctx,
            } => {
                let span =
                    trace_span!("subscriber_manager.handle_client_reregistration", %client_id);
                if let Some(ctx) = trace_ctx {
                    span.set_parent(ctx.clone()).ok();
                }
                let _g = span.enter();

                trace!(
                    "Subscriber manager for received registration request for client {client_id}"
                );

                //find all subscribers for a given registration id
                registration_id.map(|registration_id: String| {
                    info!("Gathering Subscriptions for session {registration_id}");
                    if let Some(_) = where_is(registration_id.clone()) {
                        let cloned_session_id = registration_id.clone();
                        for subscriber in myself.get_children()
                        .into_iter()
                        .filter(|cell|{
                            let sub_name = cell.get_name().unwrap();
                            let sub_id = sub_name.split_once(":").unwrap().0;
                            cloned_session_id.eq(sub_id)
                            }) {
                            //notify so they dump unsent messages
                            if let Err(e) = subscriber.send_message(BrokerMessage::RegistrationRequest { registration_id: Some(registration_id.clone()), client_id: client_id.clone(), trace_ctx: Some(span.context())}) {
                                warn!("{REGISTRATION_REQ_FAILED_TXT}: {SUBSCRIBER_NOT_FOUND_TXT}: {e}")
                            }
                        }
                    } else {
                        let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
                        warn!("{err_msg}");

                        if let Some(listener) = where_is(client_id.clone()) {
                            listener.send_message(BrokerMessage::RegistrationResponse { client_id: client_id.clone(), result: Err(err_msg.clone()), trace_ctx: Some(span.context())})
                            .map_err(|e| {
                                warn!("{err_msg}: {e}");
                            }).unwrap()
                        }
                        else { error!("{err_msg}: {CLIENT_NOT_FOUND_TXT}"); }
                    }
                });
            }
            BrokerMessage::Subscribe {
                registration_id,
                topic,
                trace_ctx,
            } => {
                let span =
                    trace_span!("subscriber_manager.handle_subscribe", %registration_id, %topic);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("Subscriber manager received subscribe command.");

                let subscriber_id = get_subscriber_name(&registration_id, &topic);
                // start new subscriber actor for session

                // drop the span guard here. We're through
                drop(_g);
                Actor::spawn_linked(
                    Some(subscriber_id.clone()),
                    SubscriberAgent,
                    (),
                    myself.clone().into(),
                )
                .await
                .ok();
            }

            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                let span = trace_span!("subscriber_manager.handle_unsubscribe_request", %registration_id, %topic);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("subscriber maanger received unsubscribe request.");

                let subscriber_name = format!("{registration_id}:{topic}");
                if let Some(subscriber) = where_is(subscriber_name.clone()) {
                    subscriber.stop(Some("UNSUBSCRIBED".to_string()));
                    //send ack

                    where_is(registration_id.clone()).map(|session| {
                        session
                            .send_message(BrokerMessage::UnsubscribeAcknowledgment {
                                registration_id,
                                topic,
                                result: Ok(()),
                            })
                            .expect("expected to send ack to session");
                    });
                } else {
                    warn!("Session agent {registration_id} not subscribed to topic {topic}");
                }
            }
            BrokerMessage::DisconnectRequest {
                client_id,
                registration_id,
                trace_ctx,
                ..
            } => {
                let span = trace_span!("subscriber_manager.handle_disconnect_request", %client_id );
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                if let Some(registration_id) = registration_id {
                    SubscriberManager::forget_subscriptions(
                        registration_id,
                        myself.clone(),
                        Some(DISCONNECTED_REASON.to_string()),
                    );
                } else {
                    warn!("Failed to process disconnect request! registration_id missing.")
                }
            }
            BrokerMessage::TimeoutMessage {
                registration_id, ..
            } => {
                SubscriberManager::forget_subscriptions(
                    registration_id,
                    myself.clone(),
                    Some(TIMEOUT_REASON.to_string()),
                );
            }
            _ => warn!(UNEXPECTED_MESSAGE_STR, "{message:?}"),
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                debug!(
                    "Subscription ended for session {0:?}, {reason:?}",
                    actor_cell.get_name()
                );
            }
            SupervisionEvent::ActorFailed(..) => {
                todo!("Subscriber failed unexpectedly, restart subscription and update state")
            }
            SupervisionEvent::ProcessGroupChanged(..) => (),
        }
        Ok(())
    }
}

/// Our subscriber actor.
/// The existence of a running "Subscriber" signifies a clients subscription
/// it is responsible for forwarding new messages received on its given topic
pub struct SubscriberAgent;

/// Define the state for the actor
pub struct SubscriberAgentState {
    registration_id: String,
    topic: String,
    dead_letter_queue: VecDeque<Vec<u8>>,
}

#[async_trait]
impl Actor for SubscriberAgent {
    type Msg = BrokerMessage; // Messages this actor handles
    type State = SubscriberAgentState; // Internal state
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::debug!("{myself:?} starting");
        //parse args. if any
        let name = myself
            .get_name()
            .expect("{SUBSCRIBE_REQUEST_FAILED_TXT}: Expected subscriber to have name");
        if let Some((registration_id, topic)) = name.split_once(':') {
            Ok(SubscriberAgentState {
                registration_id: registration_id.to_string(),
                topic: topic.to_string(),
                dead_letter_queue: VecDeque::new(),
            })
        } else {
            panic!("{SUBSCRIBE_REQUEST_FAILED_TXT}: Bad name given");
        }
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::debug!("{myself:?} Started");
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BrokerMessage::RegistrationRequest {
                registration_id,
                client_id,
                trace_ctx,
            } => {
                let span = trace_span!("subscriber.handle_registration_request", %client_id);
                if let Some(ctx) = trace_ctx {
                    span.set_parent(ctx).ok();
                }
                let _enter = span.enter();

                trace!("Subscriber actor received registration request for client {client_id}");

                //A client reconnected, push messages built up in DLQ to client
                if let Some(id) = registration_id {
                    match where_is(id.clone()) {
                        Some(session) => {
                            info!("Forwarding missed messages to session: {id}");
                            while let Some(msg) = &state.dead_letter_queue.pop_front() {
                                if let Err(e) = session.send_message(BrokerMessage::PushMessage {
                                    payload: msg.to_owned(),
                                    topic: state.topic.clone(),
                                    trace_ctx: Some(span.context()),
                                }) {
                                    warn!("Failed to forward message to subscriber! {e} Ending subscription");
                                    myself.stop(None);
                                }
                            }
                        }
                        None => {
                            error!("{REGISTRATION_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
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
                let span = trace_span!("subscriber.dequeue_messages" ,%topic);
                if let Some(ctx) = trace_ctx {
                    span.set_parent(ctx).ok();
                }
                let _enter = span.enter();

                trace!("Subscriber actor received publish response for topic \"{topic}\"");

                debug!(
                    "New message on topic: \"{topic}\", forwarding to session: {}",
                    state.registration_id
                );

                if let Some(session) = where_is(state.registration_id.clone()) {
                    //  Forward the message, if we get an error message back from the session later.
                    //  DLQ the message for later
                    //  if we fail to send a message to a session, it's probably dead, either because of a timeout or disconnect, either way, message can drop
                    if let Err(e) = session.send_message(BrokerMessage::PushMessage {
                        payload,
                        topic,
                        trace_ctx: Some(span.context()),
                    }) {
                        warn!("Failed to forward message to subscirber! {e} Ending subscription");
                        myself.stop(None);
                    }
                } else {
                    warn!("{SESSION_NOT_FOUND_TXT} Ending subscription");
                    myself.stop(None);
                }
            }
            BrokerMessage::PushMessageFailed { payload } => {
                //session couldn't talk to listener, add message to DLQ
                state.dead_letter_queue.push_back(payload.clone());
                debug!(
                    "Subscriber {0} queue has {1} message(s) waiting",
                    myself
                        .get_name()
                        .expect("Expected subscriber to have been named."),
                    state.dead_letter_queue.len()
                );
            }
            _ => {
                warn!(UNEXPECTED_MESSAGE_STR);
                warn!("{message:?}");
            }
        }
        Ok(())
    }
}
