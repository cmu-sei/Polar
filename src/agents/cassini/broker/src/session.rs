use crate::{get_subscriber_name, BROKER_NAME, UNEXPECTED_MESSAGE_STR};
use crate::{
    BROKER_NOT_FOUND_TXT, CLIENT_NOT_FOUND_TXT, PUBLISH_REQ_FAILED_TXT, REGISTRATION_REQ_FAILED_TXT,
    SUBSCRIBE_REQUEST_FAILED_TXT, TIMEOUT_REASON,
};
use cassini_types::{BrokerMessage, ControlError, SessionDetails};
use ractor::{
    async_trait,
    registry::where_is,
    Actor,
    ActorProcessingErr,
    ActorRef,
    SupervisionEvent,
};
use std::collections::{HashMap, HashSet};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, trace_span, warn, Instrument};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use uuid::Uuid;

/// The manager process for our concept of client sessions.
/// When the broker receives word of a new connection from the ListenerManager, it requests
/// that the client be "registered", signifying the client as legitimate.
pub struct SessionManager;

/// Private representation of a connected session.
struct Session {
    agent_ref: ActorRef<BrokerMessage>,
    subscriptions: HashSet<String>,
}

pub struct SessionManagerState {
    /// Map of registration_id to Session ActorRefs
    sessions: HashMap<String, Session>,
    /// Amount of time (in seconds) that can pass before a session counts as expired
    session_timeout: u64,
    /// Tokens used to cancel the thread that cleans up expired sessions.
    cancellation_tokens: HashMap<String, CancellationToken>,
}

pub struct SessionManagerArgs {
    pub session_timeout: u64,
}

#[derive(Debug)]
pub struct SetTopicManagerRef(pub ActorRef<BrokerMessage>);

#[derive(Debug)]
pub struct SetListenerManagerActorRef(pub ActorRef<BrokerMessage>);

#[async_trait]
impl Actor for SessionManager {
    type Msg = BrokerMessage;
    type State = SessionManagerState;
    type Arguments = SessionManagerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: SessionManagerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        Ok(SessionManagerState {
            sessions: HashMap::new(),
            cancellation_tokens: HashMap::new(),
            session_timeout: args.session_timeout,
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} started");
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BrokerMessage::RegistrationRequest { client_id, trace_ctx, .. } => {
                let span =
                    trace_span!("session_manager.registration_request", %client_id);
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                trace!("received registration request");

                let new_id = Uuid::new_v4().to_string();
                info!(registration_id = %new_id, "starting session for client");

                let Some(listener_ref) = where_is(client_id.clone()) else {
                    warn!("{REGISTRATION_REQ_FAILED_TXT} {CLIENT_NOT_FOUND_TXT}");
                    return Ok(());
                };

                // Anti-pattern fix: do NOT drop span guards to cross an await.
                // The span is per-message and the await is part of the message handling.
                match Actor::spawn_linked(
                    Some(new_id.clone()),
                    SessionAgent,
                    SessionAgentArgs {
                        client_ref: listener_ref.clone().into(),
                    },
                    myself.clone().into(),
                )
                .await
                {
                    Ok((session_agent, _)) => {
                        state.sessions.insert(
                            new_id.clone(),
                            Session {
                                agent_ref: session_agent.clone(),
                                subscriptions: HashSet::new(),
                            },
                        );

                        // Initialize the session and propagate trace context
                        session_agent
                            .cast(BrokerMessage::InitSession {
                                client_id,
                                trace_ctx: Some(span.context()),
                            })
                            .ok();
                    }
                    Err(e) => {
                        error!(registration_id = %new_id, "failed to spawn session agent: {e}");
                    }
                }
            }

            BrokerMessage::RegistrationResponse { result, trace_ctx, .. } => {
                let span = trace_span!("session_manager.registration_response");
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                trace!("received registration response");

                if let Ok(registration_id) = result {
                    // Cancel any pending timeout cleanup for this session.
                    if let Some(token) = state.cancellation_tokens.remove(&registration_id) {
                        debug!(%registration_id, "cancelling pending session cleanup");
                        token.cancel();
                    }
                }
            }

            BrokerMessage::GetSessions { reply_to, trace_ctx } => {
                let span = trace_span!("session_manager.get_sessions");
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                let mut sessions = HashMap::new();
                for (registration_id, session) in &state.sessions {
                    sessions.insert(
                        registration_id.to_owned(),
                        SessionDetails {
                            registration_id: registration_id.clone(),
                            subscriptions: session.subscriptions.clone(),
                        },
                    );
                }

                if let Err(e) = reply_to.send(sessions) {
                    warn!("failed to send sessions to controller: {e}");
                }
            }

            BrokerMessage::DisconnectRequest {
                reason,
                client_id,
                registration_id,
                trace_ctx,
            } => {
                let span = trace_span!("session_manager.disconnect_request", %client_id);
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                trace!("received disconnect request");

                let Some(registration_id) = registration_id else {
                    warn!("disconnect request missing registration_id");
                    return Ok(());
                };

                // Stop the session actor if we can find it.
                match where_is(registration_id.clone()) {
                    Some(session) => session.stop(Some("CLIENT_DISCONNECTED".to_owned())),
                    None => warn!(%registration_id, "failed to find session"),
                }

                // Forward to broker supervisor.
                match myself.try_get_supervisor() {
                    Some(broker) => {
                        if let Err(e) = broker.send_message(BrokerMessage::DisconnectRequest {
                            reason,
                            client_id,
                            registration_id: Some(registration_id),
                            trace_ctx: Some(span.context()),
                        }) {
                            error!("failed to forward disconnect to broker: {e}");
                        }
                    }
                    None => {
                        error!("failed to find supervisor");
                        myself.stop(None);
                    }
                }
            }

            BrokerMessage::TimeoutMessage { client_id, registration_id, error } => {
                // This is where your trace graph was going to die:
                // spawned tasks had no parent span, and you had no trace_ctx in the message.
                let span = trace_span!("session_manager.timeout_message", %client_id, %registration_id);
                let _g = span.enter();

                trace!("received timeout message");

                let Some(session) = state.sessions.get(&registration_id) else {
                    warn!("timeout for unknown session (already removed?)");
                    return Ok(());
                };

                warn!("session timing out; waiting for reconnect grace period");

                let ref_clone = session.agent_ref.clone();
                let token = CancellationToken::new();
                state
                    .cancellation_tokens
                    .insert(registration_id.clone(), token.clone());

                let timeout = state.session_timeout;

                // Capture what we need for the task. ActorRef is cheap to clone.
                let manager_ref = myself.try_get_supervisor();

                // IMPORTANT: instrument the spawned task with this span so the trace links up.
                tokio::spawn({
                    let registration_id = registration_id.clone();
                    let client_id = client_id.clone();
                    let error = error.clone();
                    let span = span.clone();
                    async move {
                        tokio::select! {
                            _ = token.cancelled() => {
                                trace!(%registration_id, "timeout cancelled; client re-registered");
                            }
                            _ = tokio::time::sleep(std::time::Duration::from_secs(timeout)) => {
                                warn!(%registration_id, "grace period elapsed; expiring session");

                                if let Some(manager) = manager_ref {
                                    if let Err(e) = manager.send_message(BrokerMessage::TimeoutMessage {
                                        client_id,
                                        registration_id: registration_id.clone(),
                                        error,
                                    }) {
                                        error!(%registration_id, "failed to forward timeout to manager: {e}");
                                    }
                                } else {
                                    warn!("could not find broker supervisor");
                                }

                                ref_clone.stop(Some(TIMEOUT_REASON.to_string()));
                            }
                        }
                    }
                    .instrument(span)
                });
            }

            BrokerMessage::SubscribeAcknowledgment {
                registration_id,
                topic,
                trace_ctx,
                ..
            } => {
                let span = trace_span!("session_manager.subscribe_ack", %registration_id, %topic);
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                if let Some(session) = state.sessions.get_mut(&registration_id) {
                    session.subscriptions.insert(topic.clone());
                    if let Err(e) = session.agent_ref.send_message(BrokerMessage::SubscribeAcknowledgment {
                        registration_id,
                        topic,
                        trace_ctx: Some(span.context()),
                        result: Ok(()),
                    }) {
                        warn!("failed to forward subscribe ack to session agent: {e}");
                    }
                } else {
                    warn!("could not find session for registration ID: {registration_id}");
                }
            }

            _ => {
                warn!("Received unexpected message: {message:?}");
            }
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
                    "Session: {0:?}:{1:?} terminated. {reason:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorFailed(actor_cell, error) => warn!(
                "Worker agent: {0:?}:{1:?} failed! {error}",
                actor_cell.get_name(),
                actor_cell.get_id()
            ),
            SupervisionEvent::ProcessGroupChanged(_) => (),
        }
        Ok(())
    }
}

/// Worker process for handling client sessions.
pub struct SessionAgent;

pub struct SessionAgentArgs {
    pub client_ref: ActorRef<BrokerMessage>,
}

pub struct SessionAgentState {
    pub client_ref: ActorRef<BrokerMessage>,
}

#[async_trait]
impl Actor for SessionAgent {
    type Msg = BrokerMessage;
    type State = SessionAgentState;
    type Arguments = SessionAgentArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(SessionAgentState {
            client_ref: args.client_ref,
        })
    }

    async fn post_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BrokerMessage::InitSession { trace_ctx, client_id } => {
                let registration_id = myself.get_name().unwrap_or_default();

                let span = trace_span!("session.init", %client_id, %registration_id);
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                trace!("received init");

                state
                    .client_ref
                    .cast(BrokerMessage::RegistrationResponse {
                        client_id,
                        result: Ok(registration_id),
                        trace_ctx: Some(span.context()),
                    })
                    .ok();
            }

            BrokerMessage::RegistrationRequest { client_id, trace_ctx, .. } => {
                let registration_id = myself.get_name().unwrap_or_default();

                let span = trace_span!("session.re_registration_request", %client_id, %registration_id);
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                trace!("received re-registration request");

                match where_is(client_id.clone()) {
                    Some(listener) => {
                        state.client_ref = ActorRef::from(listener);

                        info!("re-established comms with client");
                        // Ack to listener
                        if let Err(e) = state.client_ref.send_message(BrokerMessage::RegistrationResponse {
                            client_id: client_id.clone(),
                            result: Ok(registration_id.clone()),
                            trace_ctx: Some(span.context()),
                        }) {
                            error!("failed to send registration ack to listener: {e}");
                        }

                        // Tell manager to cancel any pending timeout cleanup
                        match myself.try_get_supervisor() {
                            Some(manager) => {
                                if let Err(e) = manager.send_message(BrokerMessage::RegistrationResponse {
                                    client_id,
                                    result: Ok(registration_id),
                                    trace_ctx: Some(span.context()),
                                }) {
                                    let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT} {e}!");
                                    error!("{err_msg}");
                                    // Best-effort tell the client and stop.
                                    let _ = state.client_ref.send_message(BrokerMessage::RegistrationResponse {
                                        client_id: state.client_ref.get_name().unwrap_or_default(),
                                        result: Err(err_msg),
                                        trace_ctx: Some(span.context()),
                                    });
                                    myself.stop(Some(e.to_string()));
                                }
                            }
                            None => warn!("couldn't find supervisor for session agent"),
                        }
                    }
                    None => {
                        warn!("could not find listener for client: {client_id}");
                    }
                }
            }

            BrokerMessage::PublishRequest { registration_id, topic, payload, trace_ctx } => {
                let span = trace_span!("session.publish_request", %registration_id, %topic);
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                trace!("received publish request");

                let Some(broker) = where_is(BROKER_NAME.to_string()) else {
                    let err_msg = format!("{PUBLISH_REQ_FAILED_TXT} {BROKER_NOT_FOUND_TXT}");
                    error!("{err_msg}");
                    let _ = state.client_ref.send_message(BrokerMessage::PublishResponse {
                        topic,
                        payload,
                        result: Err(err_msg),
                        trace_ctx: Some(span.context()),
                    });
                    // no broker -> timeout path (client may be fine; broker is not)
                    let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                        client_id: state.client_ref.get_name().unwrap_or_default(),
                        registration_id,
                        error: Some("broker not found".to_owned()),
                    });
                    return Ok(());
                };

                if let Err(e) = broker.send_message(BrokerMessage::PublishRequest {
                    registration_id,
                    topic,
                    payload,
                    trace_ctx: Some(span.context()),
                }) {
                    error!("failed to forward publish to broker: {e}");
                }
            }

            BrokerMessage::PushMessage { payload, topic, trace_ctx } => {
                let registration_id = myself.get_name().unwrap_or_default();

                let span = trace_span!("session.push_message", %registration_id, %topic);
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                trace!("received push directive; forwarding to client listener");

                if let Err(e) = state.client_ref.cast(BrokerMessage::PublishResponse {
                    topic: topic.clone(),
                    payload: payload.clone(),
                    result: Ok(()),
                    trace_ctx: Some(span.context()),
                }) {
                    warn!("{PUBLISH_REQ_FAILED_TXT}: {CLIENT_NOT_FOUND_TXT}: {e}");

                    if let Some(subscriber) = where_is(get_subscriber_name(
                        &registration_id,
                        &topic,
                    )) {
                        subscriber
                            .send_message(BrokerMessage::PushMessageFailed { payload })
                            .inspect_err(|_| warn!("failed to DLQ message on topic {topic}"))
                            .ok();
                    }
                }
            }

            BrokerMessage::PublishRequestAck { topic, trace_ctx } => {
                let registration_id = myself.get_name().unwrap_or_default();

                let span = trace_span!("session.publish_request_ack", %registration_id, %topic);
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                trace!("received publish request ack");

                if let Err(e) = state.client_ref.send_message(BrokerMessage::PublishRequestAck {
                    topic,
                    trace_ctx: Some(span.context()),
                }) {
                    error!("failed to forward publish ack to client: {e}");
                    let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                        client_id: state.client_ref.get_name().unwrap_or_default(),
                        registration_id,
                        error: Some(format!("{e}")),
                    });
                }
            }

            BrokerMessage::SubscribeRequest { registration_id, topic, trace_ctx } => {
                let span = trace_span!("session.subscribe_request", %registration_id, %topic);
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                trace!("received subscribe request");

                let Some(broker) = where_is(BROKER_NAME.to_string()) else {
                    error!("{SUBSCRIBE_REQUEST_FAILED_TXT} {BROKER_NOT_FOUND_TXT}");
                    let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                        client_id: state.client_ref.get_name().unwrap_or_default(),
                        registration_id,
                        error: Some("broker not found".to_owned()),
                    });
                    return Ok(());
                };

                if let Err(e) = broker.send_message(BrokerMessage::SubscribeRequest {
                    registration_id,
                    topic,
                    trace_ctx: Some(span.context()),
                }) {
                    error!("failed to forward subscribe to broker: {e}");
                }
            }

            BrokerMessage::UnsubscribeRequest { registration_id, topic, trace_ctx } => {
                let span = trace_span!("session.unsubscribe_request", %registration_id, %topic);
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                trace!("received unsubscribe request");

                let Some(broker) = where_is(BROKER_NAME.to_string()) else {
                    error!("{BROKER_NOT_FOUND_TXT}");
                    let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                        client_id: state.client_ref.get_name().unwrap_or_default(),
                        registration_id,
                        error: Some("broker not found".to_owned()),
                    });
                    return Ok(());
                };

                if let Err(e) = broker.send_message(BrokerMessage::UnsubscribeRequest {
                    registration_id,
                    topic,
                    trace_ctx: Some(span.context()),
                }) {
                    error!("failed to forward unsubscribe to broker: {e}");
                }
            }

            BrokerMessage::UnsubscribeAcknowledgment { registration_id, topic, result } => {
                let span = trace_span!("session.unsubscribe_ack", %registration_id, %topic);
                let _g = span.enter();

                trace!("received unsubscribe acknowledgment");

                if let Err(e) = state.client_ref.send_message(BrokerMessage::UnsubscribeAcknowledgment {
                    registration_id,
                    topic,
                    result,
                }) {
                    error!("failed to forward unsubscribe ack to client: {e}");
                    let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                        client_id: state.client_ref.get_name().unwrap_or_default(),
                        registration_id: myself.get_name().unwrap_or_default(),
                        error: Some(format!("{e}")),
                    });
                }
            }

            BrokerMessage::SubscribeAcknowledgment { registration_id, topic, result, trace_ctx } => {
                let span = trace_span!("session.subscribe_ack", %registration_id, %topic);
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                trace!("received subscribe acknowledgment");

                match myself.try_get_supervisor() {
                    Some(manager) => {
                        let _ = manager.send_message(BrokerMessage::SubscribeAcknowledgment {
                            registration_id: registration_id.clone(),
                            topic: topic.clone(),
                            result: result.clone(),
                            trace_ctx: Some(span.context()),
                        });

                        if let Err(e) = state.client_ref.send_message(BrokerMessage::SubscribeAcknowledgment {
                            registration_id,
                            topic,
                            result,
                            trace_ctx: Some(span.context()),
                        }) {
                            error!("failed to forward subscribe ack to client: {e}");
                            let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                                client_id: state.client_ref.get_name().unwrap_or_default(),
                                registration_id: myself.get_name().unwrap_or_default(),
                                error: Some(format!("{e}")),
                            });
                        }
                    }
                    None => return Err(ActorProcessingErr::from("Failed to find supervisor")),
                }
            }

            BrokerMessage::DisconnectRequest { reason, client_id, registration_id, trace_ctx } => {
                let span = trace_span!("session.disconnect_request", %client_id);
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                trace!("received disconnect request");
                info!("client disconnected");

                if let Some(manager) = myself.try_get_supervisor() {
                    let _ = manager.send_message(BrokerMessage::DisconnectRequest {
                        reason,
                        client_id,
                        registration_id,
                        trace_ctx: Some(span.context()),
                    });
                } else {
                    error!("couldn't find supervisor");
                }
            }

            BrokerMessage::TimeoutMessage { client_id, registration_id, error } => {
                let span = trace_span!("session.forward_timeout", %client_id, %registration_id);
                let _g = span.enter();

                trace!("forwarding timeout to manager");

                if let Some(manager) = myself.try_get_supervisor() {
                    let _ = manager.send_message(BrokerMessage::TimeoutMessage {
                        client_id,
                        registration_id,
                        error,
                    });
                } else {
                    error!("couldn't find supervisor");
                }
            }

            BrokerMessage::ControlRequest { registration_id, op, trace_ctx, .. } => {
                let span = trace_span!("session.control_request", %registration_id);
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                trace!("received control request");

                if let Some(broker) = myself.try_get_supervisor() {
                    if let Err(e) = broker.send_message(BrokerMessage::ControlRequest {
                        registration_id: registration_id.clone(),
                        op,
                        reply_to: Some(myself.clone()),
                        trace_ctx: Some(span.context()),
                    }) {
                        error!("failed to forward control request to broker: {e}");
                        let _ = state.client_ref.send_message(BrokerMessage::ControlResponse {
                            registration_id,
                            result: Err(ControlError::InternalError(format!(
                                "Failed to process message. {e}"
                            ))),
                            trace_ctx: Some(span.context()),
                        });
                    }
                } else {
                    error!("no supervisor available for control request");
                }
            }

            BrokerMessage::ControlResponse { registration_id, result, trace_ctx } => {
                let span = trace_span!("session.control_response", %registration_id);
                if let Some(ctx) = trace_ctx.as_ref() {
                    let _ = span.set_parent(ctx.clone());
                }
                let _g = span.enter();

                trace!("received control response");

                let _ = state.client_ref.send_message(BrokerMessage::ControlResponse {
                    registration_id,
                    result,
                    trace_ctx: Some(span.context()),
                });
            }

            _ => {
                warn!("{}", format!("{UNEXPECTED_MESSAGE_STR}: {message:?}"));
            }
        }

        Ok(())
    }
}
