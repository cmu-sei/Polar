use cassini_types::{BrokerMessage, ControlError, ControlOp, ControlResult, ShutdownPhase};
use ractor::{concurrency::Duration, rpc::CallResult, Actor, ActorProcessingErr, ActorRef};
use std::time::Instant;
use tracing::{debug, error, trace_span, warn, info};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tokio_util::sync::CancellationToken;
use cassini_tracing::try_set_parent_otel;

use crate::UNEXPECTED_MESSAGE_STR;

// ========================
// ControlManager Actor
// ========================

use ractor::registry::where_is;

pub struct ControlManager;

pub struct ControlManagerState {
    pub listener_mgr: ActorRef<BrokerMessage>,
    pub session_mgr: ActorRef<BrokerMessage>,
    pub topic_mgr: ActorRef<BrokerMessage>,
    pub subscriber_mgr: ActorRef<BrokerMessage>,
    pub shutdown_auth_token: Option<String>,
    pub is_shutting_down: bool,
    pub current_shutdown_phase: Option<ShutdownPhase>,
    pub phase_completed: bool,
    pub shutdown_timeout: Duration,
    pub shutdown_completed: bool, // Prevent duplicate completion signals
    pub phase_timeout_handle: Option<tokio::task::AbortHandle>,
    pub phase_timeout_token: CancellationToken,
}

#[ractor::async_trait]
impl Actor for ControlManager {
    type Msg = BrokerMessage;
    type State = ControlManagerState;
    type Arguments = ControlManagerState;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        // Ensure we have a timeout set
        Ok(ControlManagerState {
            shutdown_completed: false,
            phase_timeout_handle: None,
            phase_timeout_token: CancellationToken::new(),
            ..args
        })
    }

    async fn post_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        state.phase_completed = false;
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            BrokerMessage::PrepareForShutdown { auth_token } => {
                let span = trace_span!("cassini.control_manager.prepare_for_shutdown");
                let _g = span.enter();

                // Validate shutdown token if required
                if let Some(expected_token) = &state.shutdown_auth_token {
                    if auth_token.as_ref() != Some(expected_token) {
                        error!("Invalid shutdown token provided");
                        return Ok(());
                    }
                }

                if state.is_shutting_down {
                    warn!("Shutdown already in progress");
                    return Ok(());
                }

                info!("Initiating graceful shutdown sequence");
                state.is_shutting_down = true;
                state.phase_completed = false;

                // Cancel any leftover timeout from previous run
                state.phase_timeout_token.cancel();
                state.phase_timeout_token = CancellationToken::new();

                // Start with phase 1
                state.current_shutdown_phase = Some(ShutdownPhase::StopAcceptingNewConnections);

                if let Some(supervisor) = myself.try_get_supervisor() {
                    supervisor.send_message(BrokerMessage::InitiateShutdownPhase {
                        phase: ShutdownPhase::StopAcceptingNewConnections,
                    })?;
                    // Start the global timeout for this phase
                    self.start_phase_timeout(myself.clone(), state, ShutdownPhase::StopAcceptingNewConnections)
                        .await?;
                }
            }

            BrokerMessage::ShutdownPhaseComplete { phase } => {
                let span = trace_span!("cassini.control_manager.shutdown_phase_complete", phase = ?phase);
                let _g = span.enter();

                // If shutdown already fully completed, ignore further signals
                if state.shutdown_completed {
                    return Ok(());
                }

                // Cancel the phase timeout now that we've completed
                state.phase_timeout_token.cancel();
                if let Some(handle) = state.phase_timeout_handle.take() {
                    handle.abort();
                }

                // Verify this is the expected phase
                let expected = state.current_shutdown_phase.as_ref();
                if expected != Some(&phase) {
                    debug!(?phase, current = ?expected, "Ignoring completion signal for non‑current phase");
                    return Ok(());
                }

                info!("Shutdown phase completed: {:?}", phase);
                state.phase_completed = true;

                // Proceed to next phase
                self.advance_to_next_phase(myself.clone(), state, phase).await?;
            }

            BrokerMessage::InitiateShutdownPhase { phase } => {
                let span = trace_span!("cassini.control_manager.initiate_shutdown_phase", phase = ?phase);
                let _g = span.enter();
                // Just forward to broker; the actual manager will handle it
                if let Some(supervisor) = myself.try_get_supervisor() {
                    supervisor.send_message(BrokerMessage::InitiateShutdownPhase { phase })?;
                }
            }

            BrokerMessage::ControlRequest {
                registration_id,
                op,
                trace_ctx,
                reply_to,
            } => {
                // Parent span for the whole control request; attach upstream ctx if present.
                let span = trace_span!("cassini.control_manager.handle", %registration_id, op = ?op);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();

                let resp = match op {
                    ControlOp::Ping => {
                        debug!("Control Manager Received Ping");
                        BrokerMessage::ControlResponse {
                            registration_id,
                            result: Ok(ControlResult::Pong),
                            trace_ctx: Some(span.context()),
                        }
                    }
                    ControlOp::ListSessions => {
                        // Make this span a *child* of the request span (not a new root).
                        let span = trace_span!(
                            parent: &span,
                            "cassini.control_manager.list_sessions",
                            timeout_ms = 100u64
                        );
                        let _g = span.enter();
                        let t0 = Instant::now();

                        match state
                            .session_mgr
                            .call(
                                |reply_to| BrokerMessage::GetSessions {
                                    reply_to,
                                    trace_ctx: Some(span.context()),
                                },
                                Some(Duration::from_millis(100)),
                            )
                            .await
                        {
                            Ok(result) => match result {
                                CallResult::Success(sessions) => {
                                    debug!(elapsed_ms = t0.elapsed().as_millis(), sessions = sessions.len(), "list_sessions ok");
                                    BrokerMessage::ControlResponse {
                                        registration_id,
                                        result: Ok(ControlResult::SessionList(sessions)),
                                        trace_ctx: Some(span.context()),
                                    }
                                }
                                CallResult::SenderError => {
                                    warn!(elapsed_ms = t0.elapsed().as_millis(), "list_sessions sender_error");
                                    BrokerMessage::ControlResponse {
                                        registration_id,
                                        result: Err(ControlError::InternalError(
                                            "Operation failed".to_string(),
                                        )),
                                        trace_ctx: Some(span.context()),
                                    }
                                }
                                CallResult::Timeout => {
                                    warn!(elapsed_ms = t0.elapsed().as_millis(), "list_sessions timeout");
                                    BrokerMessage::ControlResponse {
                                        registration_id,
                                        result: Err(ControlError::InternalError(
                                            "Operation timed out.".to_string(),
                                        )),
                                        trace_ctx: Some(span.context()),
                                    }
                                }
                            },
                            Err(e) => {
                                // ractor::rpc::CallError: actor stopped/mailbox closed/etc.
                                error!(elapsed_ms = t0.elapsed().as_millis(), error = %e, "session_mgr call failed");
                                BrokerMessage::ControlResponse {
                                    registration_id,
                                    result: Err(ControlError::InternalError(
                                        "Session manager unavailable".to_string(),
                                    )),
                                    trace_ctx: Some(span.context()),
                                }
                            }
                        }
                    }
                    // ControlOp::GetSessionInfo { registration_id } => {
                    //     let sessions = state.sessions.lock().await;
                    //     if let Some(s) = sessions.get(&registration_id) {
                    //         Ok(ControlResult::SessionInfo(s.clone()))
                    //     } else {
                    //         Err(ControlError::NotFound(registration_id))
                    //     }
                    // }
                    // TODO: Is this something we'd want to support? eventually, admins might want something like this so they can cut off sessions at will.
                    // ControlOp::DisconnectSession { registration_id } => {
                    //     let mut sessions = state.sessions.lock().await;
                    //     if sessions.remove(&registration_id).is_some() {
                    //         Ok(ControlResult::Disconnected)
                    //     } else {
                    //         Err(ControlError::NotFound(registration_id))
                    //     }
                    // }
                    ControlOp::ListTopics => {
                        let span = trace_span!(
                            parent: &span,
                            "cassini.control_manager.list_topics",
                            timeout_ms = 100u64
                        );
                        let _g = span.enter();
                        let t0 = Instant::now();

                        match state
                            .topic_mgr
                            .call(
                                |reply_to| BrokerMessage::GetTopics {
                                    registration_id: registration_id.clone(),
                                    reply_to,
                                    trace_ctx: Some(span.context()),
                                },
                                Some(Duration::from_millis(100)),
                            )
                            .await
                        {
                            Ok(result) => match result {
                                CallResult::Success(topics) => {
                                    debug!(elapsed_ms = t0.elapsed().as_millis(), topics = topics.len(), "list_topics ok");
                                    BrokerMessage::ControlResponse {
                                        registration_id,
                                        result: Ok(ControlResult::TopicList(topics)),
                                        trace_ctx: Some(span.context()),
                                    }
                                }
                                CallResult::SenderError => {
                                    warn!(elapsed_ms = t0.elapsed().as_millis(), "list_topics sender_error");
                                    BrokerMessage::ControlResponse {
                                        registration_id,
                                        result: Err(ControlError::InternalError(
                                            "Operation failed".to_string(),
                                        )),
                                        trace_ctx: Some(span.context()),
                                    }
                                }
                                CallResult::Timeout => {
                                    warn!(elapsed_ms = t0.elapsed().as_millis(), "list_topics timeout");
                                    BrokerMessage::ControlResponse {
                                        registration_id,
                                        result: Err(ControlError::InternalError(
                                            "Operation timed out.".to_string(),
                                        )),
                                        trace_ctx: Some(span.context()),
                                    }
                                }
                            },
                            Err(e) => {
                                error!(elapsed_ms = t0.elapsed().as_millis(), error = %e, "topic_mgr call failed");
                                BrokerMessage::ControlResponse {
                                    registration_id,
                                    result: Err(ControlError::InternalError(
                                        "Topic manager unavailable".to_string(),
                                    )),
                                    trace_ctx: Some(span.context()),
                                }
                            }
                        }
                    }
                    // ControlOp::ListSubscribers { topic } => {
                    //     let topics = state.topics.lock().await;
                    //     if let Some(subs) = topics.get(&topic) {
                    //         Ok(ControlResult::SubscriberList(subs.clone()))
                    //     } else {
                    //         Err(ControlError::NotFound(topic))
                    //     }
                    // }
                    ControlOp::PrepareForShutdown { auth_token } => {
                        // Forward to ControlManager's own shutdown handler
                        myself.send_message(BrokerMessage::PrepareForShutdown {
                            auth_token: Some(auth_token)
                        }).ok();

                        BrokerMessage::ControlResponse {
                            registration_id,
                            result: Ok(ControlResult::ShutdownInitiated),
                            trace_ctx: Some(span.context()),
                        }
                    }
                    ControlOp::GetSessionInfo { registration_id } => {
                        error!("ControlOp::GetSessionInfo not implemented");
                        BrokerMessage::ControlResponse {
                            registration_id,
                            result: Err(ControlError::InternalError("not implemented".to_string())),
                            trace_ctx: Some(span.context()),
                        }
                    }
                    ControlOp::DisconnectSession { registration_id } => {
                        error!("ControlOp::DisconnectSession not implemented");
                        BrokerMessage::ControlResponse {
                            registration_id,
                            result: Err(ControlError::InternalError("not implemented".to_string())),
                            trace_ctx: Some(span.context()),
                        }
                    }
                    ControlOp::ListSubscribers { topic: _ } => {
                        error!("ControlOp::ListSubscribers not implemented");
                        BrokerMessage::ControlResponse {
                            registration_id: registration_id.clone(),
                            result: Err(ControlError::InternalError("not implemented".to_string())),
                            trace_ctx: Some(span.context()),
                        }
                    }
                    ControlOp::GetBrokerStats => {
                        error!("ControlOp::GetBrokerStats not implemented");
                        BrokerMessage::ControlResponse {
                            registration_id,
                            result: Err(ControlError::InternalError("not implemented".to_string())),
                            trace_ctx: Some(span.context()),
                        }
                    }
                    ControlOp::ShutdownBroker { graceful: _ } => {
                        error!("ControlOp::ShutdownBroker not implemented; use PrepareForShutdown instead");
                        BrokerMessage::ControlResponse {
                            registration_id,
                            result: Err(ControlError::InternalError("use PrepareForShutdown".to_string())),
                            trace_ctx: Some(span.context()),
                        }
                    }
                };

                // forward response back to client
                if let Some(session) = reply_to {
                    if let Err(e) = session.cast(resp) {
                        warn!(error = %e, "Failed to respond to session");
                    } else {
                        // Helpful breadcrumb when correlating "request -> response" without opening every span.
                        debug!("ControlResponse forwarded to session");
                    }
                } else {
                    warn!("ControlRequest missing reply_to; dropping response");
                }
            }

            other => {
                warn!(?other, "{}", UNEXPECTED_MESSAGE_STR);
            }
        }
        Ok(())
    }
}

impl ControlManager {
    async fn advance_to_next_phase(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut ControlManagerState,
        completed_phase: ShutdownPhase,
    ) -> Result<(), ActorProcessingErr> {
        let next_phase = match completed_phase {
            ShutdownPhase::StopAcceptingNewConnections => Some(ShutdownPhase::DrainExistingSessions),
            ShutdownPhase::DrainExistingSessions => Some(ShutdownPhase::FlushTopicQueues),
            ShutdownPhase::FlushTopicQueues => Some(ShutdownPhase::TerminateSubscribers),
            ShutdownPhase::TerminateSubscribers => Some(ShutdownPhase::TerminateListeners),
            ShutdownPhase::TerminateListeners => {
                if !state.shutdown_completed {
                    state.shutdown_completed = true;
                    info!("All shutdown phases completed, terminating ControlManager");
                    if let Some(supervisor) = myself.try_get_supervisor() {
                        supervisor.send_message(BrokerMessage::ShutdownPhaseComplete {
                            phase: ShutdownPhase::TerminateListeners,
                        })?;
                    }
                }
                // Stop ourselves after a short delay (idempotent)
                let myself_clone = myself.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    myself_clone.stop(Some("SHUTDOWN_SEQUENCE_COMPLETE".to_string()));
                });
                None
            }
        };

        if let Some(next) = next_phase {
            // Cancel the previous phase's timeout
            state.phase_timeout_token.cancel();
            if let Some(handle) = state.phase_timeout_handle.take() {
                handle.abort();
            }

            state.current_shutdown_phase = Some(next.clone());
            state.phase_completed = false;
            // Create a fresh cancellation token for the new phase
            state.phase_timeout_token = CancellationToken::new();

            if let Some(supervisor) = myself.try_get_supervisor() {
                supervisor.send_message(BrokerMessage::InitiateShutdownPhase {
                    phase: next.clone(),
                })?;
            }

            // Start a timeout for this next phase
            self.start_phase_timeout(myself.clone(), state, next).await?;
        }
        Ok(())
    }

    async fn start_phase_timeout(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut ControlManagerState,
        phase: ShutdownPhase,
    ) -> Result<(), ActorProcessingErr> {
        let timeout = state.shutdown_timeout;
        let token = state.phase_timeout_token.clone();
        let myself_clone = myself.clone();
        let phase_name = format!("{:?}", phase);

        let handle = tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(timeout) => {
                    warn!("Phase {} timed out after {:.2?}, forcing manager stop", phase_name, timeout);
                    // Force‑stop the manager for this phase
                    let manager_name = Self::manager_name_for_phase(&phase);
                    if let Some(manager) = where_is(manager_name.clone()) {
                        manager.stop(Some(format!("SHUTDOWN_TIMEOUT_{}", phase_name)));
                    } else {
                        warn!("Manager {} not found, cannot force stop", manager_name);
                    }
                    // Signal completion anyway – ControlManager will proceed
                    let _ = myself_clone.send_message(BrokerMessage::ShutdownPhaseComplete { phase });
                }
                _ = token.cancelled() => {
                    debug!("Phase {} timeout cancelled", phase_name);
                }
            }
        }).abort_handle();

        state.phase_timeout_handle = Some(handle);
        Ok(())
    }

    fn manager_name_for_phase(phase: &ShutdownPhase) -> String {
        match phase {
            ShutdownPhase::StopAcceptingNewConnections => crate::LISTENER_MANAGER_NAME.to_string(),
            ShutdownPhase::DrainExistingSessions => crate::SESSION_MANAGER_NAME.to_string(),
            ShutdownPhase::FlushTopicQueues => crate::TOPIC_MANAGER_NAME.to_string(),
            ShutdownPhase::TerminateSubscribers => crate::SUBSCRIBER_MANAGER_NAME.to_string(),
            ShutdownPhase::TerminateListeners => crate::LISTENER_MANAGER_NAME.to_string(),
        }
    }
}
