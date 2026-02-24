use crate::{get_subscriber_name, BROKER_NAME, UNEXPECTED_MESSAGE_STR};
use crate::{
    BROKER_NOT_FOUND_TXT, CLIENT_NOT_FOUND_TXT, PUBLISH_REQ_FAILED_TXT,
    REGISTRATION_REQ_FAILED_TXT, SUBSCRIBE_REQUEST_FAILED_TXT,
};
use cassini_types::{BrokerMessage, ControlError, ShutdownPhase};
use ractor::{
    async_trait,
    registry::where_is,
    Actor,
    ActorProcessingErr,
    ActorRef,
    SupervisionEvent,
};
use std::collections::HashMap;
use tracing::{debug, error, info, trace, trace_span, warn};
use uuid::Uuid;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use cassini_tracing::try_set_parent_otel;


/// The manager process for our concept of client sessions.
/// When the broker receives word of a new connection from the ListenerManager, it requests
/// that the client be "registered", signifying the client as legitimate.
pub struct SessionManager;

/// Private representation of a connected session.
struct Session {
    agent_ref: ActorRef<BrokerMessage>,
    pending_publishes: usize, // Number of unacknowledged publish requests
}

pub struct SessionManagerState {
    /// Map of registration_id to Session ActorRefs
    sessions: HashMap<String, Session>,

    is_shutting_down: bool,
    drain_phase_active: bool,
    drain_completion_notified: bool, // Prevent duplicate signals
    total_pending_publishes: usize,
    pub pending_drain_count: usize,
}

pub struct SessionManagerArgs;

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
        _args: SessionManagerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        Ok(SessionManagerState {
            sessions: HashMap::new(),
            is_shutting_down: false,
            drain_phase_active: false,
            drain_completion_notified: false,
            total_pending_publishes: 0,
            pending_drain_count: 0,
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
            BrokerMessage::PrepareForShutdown { .. } => {
                info!("SessionManager preparing for shutdown");
                state.is_shutting_down = true;
                
                // Notify all sessions to prepare for shutdown and stop accepting new work
                for (registration_id, session) in &mut state.sessions {
                    if let Err(e) = session.agent_ref.send_message(
                        BrokerMessage::PrepareForShutdown { auth_token: None }
                    ) {
                        warn!("Failed to notify session {} of shutdown: {}", registration_id, e);
                    }
                }
            }
            
            BrokerMessage::InitiateShutdownPhase { phase } if phase == ShutdownPhase::DrainExistingSessions => {
                info!("Starting drain of existing sessions");
                state.drain_phase_active = true;
                state.drain_completion_notified = false;
                state.total_pending_publishes = 0;

                let pending: Vec<_> = state.sessions.keys().cloned().collect();
                state.pending_drain_count = pending.len();

                for reg_id in &pending {
                    if let Some(session) = state.sessions.get(reg_id) {
                        let _ = session.agent_ref.send_message(
                            BrokerMessage::PrepareForShutdown { auth_token: None }
                        );
                        state.total_pending_publishes += session.pending_publishes;
                    }
                }

                if state.total_pending_publishes == 0 && !state.sessions.is_empty() {
                    // All work is done; stop remaining sessions immediately
                    for (_id, session) in state.sessions.drain() {
                        session.agent_ref.stop(Some("DRAIN_COMPLETE".to_string()));
                    }
                    self.signal_drain_complete(myself.clone(), state).await?;
                }

                if pending.is_empty() {
                    self.signal_drain_complete(myself.clone(), state).await?;
                    return Ok(());
                }
            }
            
            BrokerMessage::SessionPendingUpdate { registration_id, delta } => {
                if let Some(session) = state.sessions.get_mut(&registration_id) {
                    // Update session's pending count
                    let new_pending = (session.pending_publishes as i32 + delta) as usize;
                    session.pending_publishes = new_pending;
                    // Update global total
                    state.total_pending_publishes = (state.total_pending_publishes as i32 + delta) as usize;
          
                    debug!(%registration_id, pending = session.pending_publishes, "Pending publish count updated");
                    
                    // If we're in drain phase, check if we can complete
                    if state.drain_phase_active && !state.drain_completion_notified {
                        self.check_drain_complete(myself.clone(), state).await?;
                    }
                }
            }
            
            BrokerMessage::RegistrationRequest { registration_id, client_id, trace_ctx } => {
                let span = trace_span!("session_manager.registration_request", %client_id, ?registration_id);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();
                trace!("received registration request");

                if state.is_shutting_down {
                    warn!("Rejecting registration request during shutdown");
                    if let Some(listener_ref) = where_is(client_id.clone()) {
                        listener_ref.send_message(BrokerMessage::RegistrationResponse {
                            client_id,
                            result: Err("Broker is shutting down".to_string()),
                            trace_ctx: Some(span.context()),
                        })?;
                    }
                    return Ok(());
                }

                let Some(listener_ref) = where_is(client_id.clone()) else {
                    warn!("{REGISTRATION_REQ_FAILED_TXT} {CLIENT_NOT_FOUND_TXT}");
                    return Ok(());
                };

                // Try to reuse an existing session
                if let Some(existing_id) = &registration_id {
                    if let Some(session) = state.sessions.get_mut(existing_id) {
                        info!(registration_id = %existing_id, "reusing existing session");
                        if let Err(e) = session.agent_ref.send_message(BrokerMessage::ReassignClient {
                            client_ref: listener_ref.clone().into(),
                            trace_ctx: Some(span.context()),
                        }) {
                            error!("Failed to reassign client to existing session: {}", e);
                        } else {
                            listener_ref.send_message(BrokerMessage::RegistrationResponse {
                                client_id,
                                result: Ok(existing_id.clone()),
                                trace_ctx: Some(span.context()),
                            })?;
                            return Ok(());
                        }
                    } else {
                        warn!(registration_id = %existing_id, "requested session not found, creating new one");
                    }
                }

                // Create new session
                let new_id = Uuid::new_v4().to_string();
                info!(registration_id = %new_id, "starting session for client");

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
                                pending_publishes: 0,
                            },
                        );
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
            _ => {
                warn!("Received unexpected message: {message:?}");
            }
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
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                debug!(
                    "Session: {0:?}:{1:?} terminated. {reason:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );

                if let Some(name) = actor_cell.get_name() {
                    if let Some(session) = state.sessions.remove(&name) {
                        // Subtract its pending publishes from the global total
                        state.total_pending_publishes -= session.pending_publishes;
                    }
                    
                    // If we are in drain phase, check if we can complete
                    if state.drain_phase_active && !state.drain_completion_notified {
                        self.check_drain_complete(myself.clone(), state).await?;
                    }
                }
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

impl SessionManager {
    async fn check_drain_complete(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionManagerState,
    ) -> Result<(), ActorProcessingErr> {
        if !state.drain_phase_active || state.drain_completion_notified {
            return Ok(());
        }
        
        // Calculate total pending publishes
        let total_pending = state.total_pending_publishes;
        let sessions_empty = state.sessions.is_empty();
        
        if total_pending == 0 && sessions_empty {
            info!("All sessions drained and terminated");
            self.signal_drain_complete(myself.clone(), state).await?;
        } else {
            debug!(total_pending, session_count = state.sessions.len(), "Still draining");
        }
        
        Ok(())
    }
    
    async fn signal_drain_complete(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionManagerState,
    ) -> Result<(), ActorProcessingErr> {
        if state.drain_completion_notified {
            return Ok(());
        }
        state.drain_completion_notified = true;
        state.drain_phase_active = false;

        info!("All sessions drained and terminated");

        if let Some(supervisor) = myself.try_get_supervisor() {
            supervisor.send_message(BrokerMessage::ShutdownPhaseComplete {
                phase: ShutdownPhase::DrainExistingSessions,
            })?;
        }

        myself.stop(Some("SHUTDOWN_DRAINED".to_string()));
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
    registration_id: String, // store for convenience
    shutting_down: bool,
}

#[async_trait]
impl Actor for SessionAgent {
    type Msg = BrokerMessage;
    type State = SessionAgentState;
    type Arguments = SessionAgentArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let registration_id = myself.get_name().unwrap_or_default();
        Ok(SessionAgentState {
            client_ref: args.client_ref,
            registration_id,
            shutting_down: false,
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
            BrokerMessage::PrepareForShutdown { .. } => {
                debug!("Session agent entering shutdown mode – will reject new requests");
                state.shutting_down = true;
                // Do NOT disconnect – client will disconnect when finished.
            }
            BrokerMessage::InitSession { trace_ctx, client_id } => {
                let registration_id = state.registration_id.clone();
                let span = trace_span!("session.init", %client_id, %registration_id);
                try_set_parent_otel(&span, trace_ctx);
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
                let registration_id = state.registration_id.clone();
                let span = trace_span!("session.re_registration_request", %client_id, %registration_id);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();
                trace!("received re-registration request");

                match where_is(client_id.clone()) {
                    Some(listener) => {
                        state.client_ref = ActorRef::from(listener);

                        info!("re-established comms with client");
                        if let Err(e) = state.client_ref.send_message(BrokerMessage::RegistrationResponse {
                            client_id: client_id.clone(),
                            result: Ok(registration_id.clone()),
                            trace_ctx: Some(span.context()),
                        }) {
                            error!("failed to send registration ack to listener: {e}");
                        }

                        match myself.try_get_supervisor() {
                            Some(manager) => {
                                if let Err(e) = manager.send_message(BrokerMessage::RegistrationResponse {
                                    client_id,
                                    result: Ok(registration_id),
                                    trace_ctx: Some(span.context()),
                                }) {
                                    let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT} {e}!");
                                    error!("{err_msg}");
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

            BrokerMessage::ReassignClient { client_ref, trace_ctx } => {
                let span = trace_span!("session.reassign_client", registration_id = %state.registration_id);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();
                debug!("reassigning client listener");
                state.client_ref = client_ref;
                // If the session had pending publishes, they will continue using the new client.
                // No need to notify subscribers; they are still attached to this session.
            }

            BrokerMessage::PublishRequest { registration_id, topic, payload, trace_ctx } => {
                let span = trace_span!("session.publish_request", %registration_id, %topic);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();

                if state.shutting_down {
                    warn!("Rejecting publish during shutdown");
                    return Ok(());
                }

                trace!("received publish request");

                // Clone the registration_id for use in multiple places
                let reg_id_clone = registration_id.clone();

                // Notify session manager that we're about to send a publish request
                if let Some(manager) = myself.try_get_supervisor() {
                    manager.send_message(BrokerMessage::SessionPendingUpdate {
                        registration_id: reg_id_clone.clone(),
                        delta: 1,
                    }).ok();
                }

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

                    // Notify manager that this publish failed immediately (delta -1)
                    if let Some(manager) = myself.try_get_supervisor() {
                        manager.send_message(BrokerMessage::SessionPendingUpdate {
                            registration_id: reg_id_clone.clone(),
                            delta: -1,
                        }).ok();
                    }
                    return Ok(());
                };

                if let Err(e) = broker.send_message(BrokerMessage::PublishRequest {
                    registration_id: reg_id_clone.clone(),
                    topic,
                    payload,
                    trace_ctx: Some(span.context()),
                }) {
                    error!("failed to forward publish to broker: {e}");
                    // Notify manager that publish failed (delta -1)
                    if let Some(manager) = myself.try_get_supervisor() {
                        manager.send_message(BrokerMessage::SessionPendingUpdate {
                            registration_id: reg_id_clone.clone(),
                            delta: -1,
                        }).ok();
                    }
                }
            }

            BrokerMessage::PushMessage { payload, topic, trace_ctx } => {
                let registration_id = state.registration_id.clone();
                let span = trace_span!("session.push_message", %registration_id, %topic);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();
                trace!("received push directive; forwarding to client listener");

                if let Err(e) = state.client_ref.cast(BrokerMessage::PublishResponse {
                    topic: topic.clone(),
                    payload: payload.clone(),  // Changed from payload.clone().into()
                    result: Ok(()),
                    trace_ctx: Some(span.context()),
                }) {
                    warn!("{PUBLISH_REQ_FAILED_TXT}: {CLIENT_NOT_FOUND_TXT}: {e}");

                    if let Some(subscriber) = where_is(get_subscriber_name(
                        &registration_id,
                        &topic,
                    )) {
                        subscriber
                            .send_message(BrokerMessage::PushMessageFailed { 
                                payload: payload.clone()  // Changed from just payload
                            })
                            .inspect_err(|_| warn!("failed to DLQ message on topic {topic}"))
                            .ok();
                    }
                }
            }

            BrokerMessage::PublishRequestAck { topic, trace_ctx } => {
                let registration_id = state.registration_id.clone();

                let span = trace_span!("session.publish_request_ack", %registration_id, %topic);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();
                trace!("received publish request ack");

                if let Err(e) = state.client_ref.send_message(BrokerMessage::PublishRequestAck {
                    topic,
                    trace_ctx: Some(span.context()),
                }) {
                    error!("failed to forward publish ack to client: {e}");
                    let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                        client_id: state.client_ref.get_name().unwrap_or_default(),
                        registration_id: registration_id.clone(),
                        error: Some(format!("{e}")),
                    });
                }

                // Notify session manager that a pending publish has completed
                if let Some(manager) = myself.try_get_supervisor() {
                    let _ = manager.send_message(BrokerMessage::SessionPendingUpdate {
                        registration_id: registration_id.clone(),
                        delta: -1,
                    });
                }
            }

            BrokerMessage::SubscribeRequest { registration_id, topic, trace_ctx } => {
                let span = trace_span!("session.subscribe_request", %registration_id, %topic);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();

                if state.shutting_down {
                    warn!("Rejecting subscribe during shutdown");
                    return Ok(());
                }

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
                try_set_parent_otel(&span, trace_ctx);
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
                try_set_parent_otel(&span, trace_ctx);
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
                try_set_parent_otel(&span, trace_ctx);
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
                // Stop this session actor now that the client is gone
                myself.stop(Some("DISCONNECTED".to_string()));
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
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();
                trace!("received control request");

                // Look up the broker actor directly (it's not our supervisor)
                let Some(broker) = where_is(BROKER_NAME.to_string()) else {
                    error!("{BROKER_NOT_FOUND_TXT}");
                    let _ = state.client_ref.send_message(BrokerMessage::ControlResponse {
                        registration_id,
                        result: Err(ControlError::InternalError(
                            "broker not found".to_string()
                        )),
                        trace_ctx: Some(span.context()),
                    });
                    return Ok(());
                };

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
            }

            BrokerMessage::ControlResponse { registration_id, result, trace_ctx } => {
                let span = trace_span!("session.control_response", %registration_id);
                try_set_parent_otel(&span, trace_ctx);
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
