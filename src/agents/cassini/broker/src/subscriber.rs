use crate::UNEXPECTED_MESSAGE_STR;
use crate::{
    get_subscriber_name, CLIENT_NOT_FOUND_TXT, DISCONNECTED_REASON, REGISTRATION_REQ_FAILED_TXT,
    SESSION_NOT_FOUND_TXT, SUBSCRIBE_REQUEST_FAILED_TXT, TIMEOUT_REASON,
};
use cassini_types::BrokerMessage;
use ractor::{
    async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef, SupervisionEvent,
};
use std::collections::VecDeque;
use tracing::{debug, error, info, trace, trace_span, warn, Instrument};
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
        let mut stopped = 0usize;
        // cleanup all subscriptions for session
        for subscriber in myself.get_children().into_iter().filter(|cell| {
            let Some(sub_name) = cell.get_name() else {
                return false;
            };
            let Some((sub_id, _topic)) = sub_name.split_once(':') else {
                return false;
            };
            registration_id == sub_id
        }) {
            subscriber.stop(reason.clone());
            stopped += 1;
        }
        debug!(%registration_id, stopped, "Stopped subscriptions for registration");
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
                let span = trace_span!(
                    "cassini.subscriber_manager.handle_registration_request",
                    %client_id,
                    has_registration_id = registration_id.is_some()
                );
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
                let _g = span.enter();

                trace!(
                    "Subscriber manager for received registration request for client {client_id}"
                );

                // find all subscribers for a given registration id
                if let Some(registration_id) = registration_id {
                    info!("Gathering Subscriptions for session {registration_id}");
                    if where_is(registration_id.clone()).is_some() {
                        let cloned_session_id = registration_id.clone();

                        for subscriber in myself
                            .get_children()
                            .into_iter()
                            .filter(|cell| {
                                let Some(sub_name) = cell.get_name() else {
                                    return false;
                                };
                                let Some((sub_id, _topic)) = sub_name.split_once(':') else {
                                    return false;
                                };
                                cloned_session_id == sub_id
                            })
                        {
                            // notify so they dump unsent messages
                            if let Err(e) = subscriber.send_message(BrokerMessage::RegistrationRequest {
                                registration_id: Some(registration_id.clone()),
                                client_id: client_id.clone(),
                                trace_ctx: Some(span.context()),
                            }) {
                                warn!(error = %e, "{REGISTRATION_REQ_FAILED_TXT}: {SUBSCRIBER_NOT_FOUND_TXT}");
                            }
                        }
                    } else {
                        let err_msg =
                            format!("{REGISTRATION_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
                        warn!("{err_msg}");

                        if let Some(listener) = where_is(client_id.clone()) {
                            if let Err(e) = listener.send_message(BrokerMessage::RegistrationResponse {
                                client_id: client_id.clone(),
                                result: Err(err_msg.clone()),
                                trace_ctx: Some(span.context()),
                            }) {
                                warn!(error = %e, "{err_msg}");
                            }
                        } else {
                            error!("{err_msg}: {CLIENT_NOT_FOUND_TXT}");
                        }
                    }
                } else {
                    warn!("RegistrationRequest missing registration_id; nothing to rehydrate");
                }
            }

            BrokerMessage::CreateSubscriber {
                registration_id,
                topic,
                trace_ctx,
                reply,
            } => {
                let span = trace_span!(
                    "cassini.subscriber_manager.create_subscriber",
                    %registration_id,
                    %topic
                );
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }

                trace!("Subscriber manager received subscribe command");

                let subscriber_id = get_subscriber_name(&registration_id, &topic);

                // Instrument the spawn future instead of holding an enter-guard across `.await`.
                match Actor::spawn_linked(
                    Some(subscriber_id.clone()),
                    SubscriberAgent,
                    (),
                    myself.clone().into(),
                )
                .instrument(span.clone())
                .await
                {
                    Ok((subscriber, _)) => {
                        let _ = reply.send(Ok(subscriber));
                    }
                    Err(e) => {
                        let err_msg = format!("Failed to spawn subscriber actor. {e}");
                        let _ = reply.send(Err(ActorProcessingErr::from(err_msg)));
                    }
                }
            }

            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                let span = trace_span!(
                    "cassini.subscriber_manager.unsubscribe_request",
                    %registration_id,
                    %topic
                );
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
                let _g = span.enter();

                trace!("subscriber manager received unsubscribe request");

                let subscriber_name = format!("{registration_id}:{topic}");
                if let Some(subscriber) = where_is(subscriber_name.clone()) {
                    subscriber.stop(Some("UNSUBSCRIBED".to_string()));
                    // send ack
                    if let Some(session) = where_is(registration_id.clone()) {
                        if let Err(e) = session.send_message(BrokerMessage::UnsubscribeAcknowledgment {
                            registration_id,
                            topic,
                            result: Ok(()),
                        }) {
                            warn!(error = %e, "Failed to send unsubscribe ack to session");
                        }
                    } else {
                        warn!("Failed to lookup session {registration_id} for unsubscribe ack");
                    }
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
                let span = trace_span!(
                    "cassini.subscriber_manager.disconnect_request",
                    %client_id,
                    has_registration_id = registration_id.is_some()
                );
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
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

            BrokerMessage::TimeoutMessage { registration_id, .. } => {
                SubscriberManager::forget_subscriptions(
                    registration_id,
                    myself.clone(),
                    Some(TIMEOUT_REASON.to_string()),
                );
            }

            other => warn!(?other, "{UNEXPECTED_MESSAGE_STR}"),
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
        // parse args. if any
        let name = myself.get_name().ok_or_else(|| {
            ActorProcessingErr::from(format!(
                "{SUBSCRIBE_REQUEST_FAILED_TXT}: Expected subscriber to have name"
            ))
        })?;

        if let Some((registration_id, topic)) = name.split_once(':') {
            Ok(SubscriberAgentState {
                registration_id: registration_id.to_string(),
                topic: topic.to_string(),
                dead_letter_queue: VecDeque::new(),
            })
        } else {
            Err(ActorProcessingErr::from(format!(
                "{SUBSCRIBE_REQUEST_FAILED_TXT}: Bad name given: {name}"
            )))
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
                let span = trace_span!(
                    "cassini.subscriber.handle_registration_request",
                    %client_id,
                    has_registration_id = registration_id.is_some()
                );
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
                let _enter = span.enter();

                trace!("Subscriber actor received registration request for client {client_id}");

                // A client reconnected, push messages built up in DLQ to client
                if let Some(id) = registration_id {
                    match where_is(id.clone()) {
                        Some(session) => {
                            info!("Forwarding missed messages to session: {id}");
                            while let Some(msg) = state.dead_letter_queue.pop_front() {
                                if let Err(e) = session.send_message(BrokerMessage::PushMessage {
                                    payload: msg,
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
                let span = trace_span!("cassini.subscriber.publish_response", %topic);
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
                let _enter = span.enter();

                trace!("Subscriber actor received publish response for topic \"{topic}\"");

                debug!(
                    "New message on topic: \"{topic}\", forwarding to session: {}",
                    state.registration_id
                );

                if let Some(session) = where_is(state.registration_id.clone()) {
                    // Forward the message; if we fail to send, session is likely dead (timeout/disconnect),
                    // and message can drop (or be DLQ'd by session).
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
                // session couldn't talk to listener, add message to DLQ
                state.dead_letter_queue.push_back(payload);
                debug!(
                    "Subscriber {0} queue has {1} message(s) waiting",
                    myself
                        .get_name()
                        .expect("Expected subscriber to have been named."),
                    state.dead_letter_queue.len()
                );
            }

            _ => {
                warn!(?message, "{UNEXPECTED_MESSAGE_STR}");
            }
        }
        Ok(())
    }
}
