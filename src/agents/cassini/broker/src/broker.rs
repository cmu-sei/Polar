use crate::{
    control::{ControlManager, ControlManagerState},
    listener::{ListenerManager, ListenerManagerArgs},
    session::{SessionManager, SessionManagerArgs},
    subscriber::SubscriberManager,
    topic::{TopicManager, TopicManagerArgs},
    BrokerConfigError, LISTENER_MANAGER_NAME, SESSION_MANAGER_NAME, SUBSCRIBER_MANAGER_NAME,
    TOPIC_MANAGER_NAME, UNEXPECTED_MESSAGE_STR, BROKER_NAME
};
use cassini_types::{BrokerMessage, ShutdownPhase};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use ractor::concurrency::Duration;
use ractor::registry::where_is;
use tracing::{debug, error, info, trace, trace_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use std::env;
use tokio_util::sync::CancellationToken;
use cassini_tracing::try_set_parent_otel;

// ============================== Broker Supervisor Actor Definition ============================== //

pub struct Broker;

// State object containing references to worker actors
pub struct BrokerState {
    listener_mgr: Option<ActorRef<BrokerMessage>>,
    session_mgr: Option<ActorRef<BrokerMessage>>,
    topic_mgr: Option<ActorRef<BrokerMessage>>,
    subscriber_mgr: Option<ActorRef<BrokerMessage>>,
    control_mgr: Option<ActorRef<BrokerMessage>>,
    is_shutting_down: bool,
}

/// Would-be configurations for the broker actor itself, as well its workers
#[derive(Clone)]
pub struct BrokerArgs {
    /// Socket address to listen for connections on
    pub bind_addr: String,

    /// The amount of time (in seconds) before a session times out
    /// Should be between 10 and 300 seconds
    pub session_timeout: Option<u64>,

    pub server_cert_file: String,
    pub private_key_file: String,
    pub ca_cert_file: String,

    /// Authorization token for shutdown operations
    pub shutdown_auth_token: Option<String>,
}

impl BrokerArgs {
    fn required_env(var: &str) -> Result<String, BrokerConfigError> {
        env::var(var).map_err(|e| BrokerConfigError::EnvVar {
            var: var.to_string(),
            source: e,
        })
    }

    pub fn new() -> Result<Self, BrokerConfigError> {
        let server_cert_file = BrokerArgs::required_env("TLS_SERVER_CERT_CHAIN")?;
        let private_key_file = BrokerArgs::required_env("TLS_SERVER_KEY")?;
        let ca_cert_file = BrokerArgs::required_env("TLS_CA_CERT")?;
        let bind_addr =
            env::var("CASSINI_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());

        let shutdown_auth_token = env::var("BROKER_SHUTDOWN_TOKEN").ok();

        Ok(BrokerArgs {
            bind_addr,
            session_timeout: None,
            server_cert_file,
            private_key_file,
            ca_cert_file,
            shutdown_auth_token,
        })
    }
}

#[async_trait]
impl Actor for Broker {
    type Msg = BrokerMessage;
    type State = BrokerState;
    type Arguments = BrokerArgs;

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.broker.supervision_event");
        let _g = span.enter();

        match msg {
            SupervisionEvent::ActorStarted(actor_cell) => {
                debug!(
                    worker_name = ?actor_cell.get_name(),
                    worker_id = ?actor_cell.get_id(),
                    "Worker started"
                );
            }
            SupervisionEvent::ActorTerminated(actor_cell, _, _reason) => {
                // Don't claim we restart if we don't.
                warn!(
                    worker_name = ?actor_cell.get_name(),
                    worker_id = ?actor_cell.get_id(),
                    "Worker terminated (no restart strategy implemented here)"
                );

                // Remove the terminated manager from state if it's one of our core managers
                if let Some(name) = actor_cell.get_name() {
                    match name.as_str() {
                        crate::LISTENER_MANAGER_NAME => {
                            state.listener_mgr = None;
                            debug!("ListenerManager removed from state");
                        }
                        crate::SESSION_MANAGER_NAME => {
                            state.session_mgr = None;
                            debug!("SessionManager removed from state");
                        }
                        crate::TOPIC_MANAGER_NAME => {
                            state.topic_mgr = None;
                            debug!("TopicManager removed from state");
                        }
                        crate::SUBSCRIBER_MANAGER_NAME => {
                            state.subscriber_mgr = None;
                            debug!("SubscriberManager removed from state");
                        }
                        crate::CONTROL_MANAGER_NAME => {
                            state.control_mgr = None;
                            debug!("ControlManager removed from broker state");
                         }
                        _ => {}
                    }
                }

                // Check if all managers have terminated during shutdown
                if state.is_shutting_down {
                    self.check_shutdown_completion(myself.clone(), state).await?;
                }
            }

            SupervisionEvent::ActorFailed(actor_cell, e) => {
                // Today: fatal. Tomorrow: restart policy per worker type.
                error!(
                    worker_name = ?actor_cell.get_name(),
                    worker_id = ?actor_cell.get_id(),
                    error = %e,
                    "Worker failed; stopping broker"
                );
                myself.stop(Some("ACTOR_FAILED".to_string()));
            }

            SupervisionEvent::ProcessGroupChanged(_) => (),
        }

        Ok(())
    }

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: BrokerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        // Parent span for startup; don't try to "carry" this across spawned actors.
        let span = trace_span!("cassini.broker.pre_start", bind_addr = %args.bind_addr);
        let _g = span.enter();

        debug!(actor = ?myself, "Broker starting");

        let listener_manager_args = ListenerManagerArgs {
            bind_addr: args.bind_addr,
            server_cert_file: args.server_cert_file,
            private_key_file: args.private_key_file,
            ca_cert_file: args.ca_cert_file,
            shutdown_auth_token: args.shutdown_auth_token.clone(),
        };

        let (listener_mgr, _) = Actor::spawn_linked(
            Some(LISTENER_MANAGER_NAME.to_owned()),
            ListenerManager,
            listener_manager_args,
            myself.clone().into(),
        )
        .await
        .map_err(|e| {
            error!(error = ?e, "Broker startup failed: ListenerManager failed to start");
            ActorProcessingErr::from(e)
        })?;

        // SessionManagerArgs is a unit struct now (no timeout needed)
        let (session_mgr, _) = Actor::spawn_linked(
            Some(SESSION_MANAGER_NAME.to_owned()),
            SessionManager,
            SessionManagerArgs,  // unit struct
            myself.clone().into(),
        )
        .await
        .map_err(|e| {
            error!(error = ?e, "Broker startup failed: SessionManager failed to start");
            ActorProcessingErr::from(e)
        })?;

        let (subscriber_mgr, _sub_handle) = Actor::spawn_linked(
            Some(SUBSCRIBER_MANAGER_NAME.to_string()),
            SubscriberManager,
            (),
            myself.clone().into(),
        )
        .await
        .map_err(|e| {
            error!(error = ?e, "Broker startup failed: SubscriberManager failed to start");
            ActorProcessingErr::from(e)
        })?;

        let topic_mgr_args = TopicManagerArgs {
            topics: None,
            subscriber_mgr: Some(subscriber_mgr.clone()),
        };

        let (topic_mgr, _topic_handle) = Actor::spawn_linked(
            Some(TOPIC_MANAGER_NAME.to_owned()),
            TopicManager,
            topic_mgr_args,
            myself.clone().into(),
        )
        .await
        .map_err(|e| {
            error!(error = ?e, "Broker startup failed: TopicManager failed to start");
            ActorProcessingErr::from(e)
        })?;

        let control_mgr_args = ControlManagerState {
            topic_mgr: topic_mgr.clone(),
            subscriber_mgr: subscriber_mgr.clone(),
            listener_mgr: listener_mgr.clone(),
            session_mgr: session_mgr.clone(),
            shutdown_auth_token: args.shutdown_auth_token.clone(),
            is_shutting_down: false,
            current_shutdown_phase: None,
            phase_completed: false,
            phase_timeout_handle: None,
            phase_timeout_token: CancellationToken::new(),
            shutdown_timeout: Duration::from_secs(30), // 30 seconds per phase max
            shutdown_completed: false,
        };

        let (control_mgr, _) = Actor::spawn_linked(
            Some("CONTROL_MANAGER".to_owned()),
            ControlManager,
            control_mgr_args,
            myself.clone().into(),
        )
        .await
        .map_err(|e| {
            error!(error = ?e, "Broker startup failed: ControlManager failed to start");
            ActorProcessingErr::from(e)
        })?;

        info!("Broker startup complete");

        Ok(BrokerState {
            listener_mgr: Some(listener_mgr),
            session_mgr: Some(session_mgr),
            topic_mgr: Some(topic_mgr),
            subscriber_mgr: Some(subscriber_mgr),
            control_mgr: Some(control_mgr),
            is_shutting_down: false,
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!(actor = ?myself, "Broker started");
        // Start periodic shutdown check (every 1s while shutting down)
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                if let Some(broker) = where_is(BROKER_NAME.to_string()) {
                    let _ = broker.send_message(BrokerMessage::CheckShutdownComplete);
                } else {
                    break;
                }
            }
        });
        Ok(())
    }

    async fn post_stop(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!(actor = ?myself, "Broker stopped");
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {

            // Order definitely matters here. We must check for shutdown messages first and handle those.
            BrokerMessage::PrepareForShutdown { auth_token } => {
                let span = trace_span!("cassini.broker.prepare_for_shutdown");
                let _g = span.enter();
                
                // Check if shutdown is enabled and token is valid
                match &state.control_mgr {
                    Some(control_mgr) => {
                        // Forward to ControlManager for validation and orchestration
                        if let Err(e) = control_mgr.send_message(
                            BrokerMessage::PrepareForShutdown { auth_token }
                        ) {
                            error!(error = %e, "Failed to forward shutdown request to ControlManager");
                        }
                    }
                    None => error!("ControlManager missing; cannot process shutdown"),
                }
            }
            
            BrokerMessage::InitiateShutdownPhase { phase } => {
                let span = trace_span!("cassini.broker.initiate_shutdown_phase", phase = ?phase);
                let _g = span.enter();
                
                info!("Initiating shutdown phase: {:?}", phase);
                state.is_shutting_down = true;
                
                match phase {
                    ShutdownPhase::StopAcceptingNewConnections => {
                        if let Some(listener_mgr) = &state.listener_mgr {
                            info!("Sending InitiateShutdownPhase to ListenerManager");
                            listener_mgr.send_message(
                                BrokerMessage::InitiateShutdownPhase { phase }
                            )?;
                        }
                    }
                    ShutdownPhase::DrainExistingSessions => {
                        if let Some(session_mgr) = &state.session_mgr {
                            session_mgr.send_message(
                                BrokerMessage::InitiateShutdownPhase { phase }
                            )?;
                        }
                        else {
                            // Manager already gone — signal completion ourselves
                            if let Some(control_mgr) = &state.control_mgr {
                                control_mgr.send_message(BrokerMessage::ShutdownPhaseComplete { phase })?;
                            }
                        }
                    }
                    ShutdownPhase::FlushTopicQueues => {
                        if let Some(topic_mgr) = &state.topic_mgr {
                            topic_mgr.send_message(
                                BrokerMessage::InitiateShutdownPhase { phase }
                            )?;
                        }
                    }
                    ShutdownPhase::TerminateSubscribers => {
                        if let Some(subscriber_mgr) = &state.subscriber_mgr {
                            subscriber_mgr.send_message(
                                BrokerMessage::InitiateShutdownPhase { phase }
                            )?;
                        }
                    }
                    ShutdownPhase::TerminateListeners => {
                        if let Some(listener_mgr) = &state.listener_mgr {
                            info!("Sending InitiateShutdownPhase to ListenerManager for TerminateListeners");
                            listener_mgr.send_message(
                                BrokerMessage::InitiateShutdownPhase { phase }
                            )?;
                        }
                    }
                }
            }

            BrokerMessage::ShutdownPhaseComplete { phase } => {
                let span = trace_span!("cassini.broker.shutdown_phase_complete", phase = ?phase);
                let _g = span.enter();
                
                info!("Shutdown phase completed: {:?}", phase);
                
                // Forward to ControlManager
                if let Some(control_mgr) = &state.control_mgr {
                    if let Err(e) = control_mgr.send_message(
                        BrokerMessage::ShutdownPhaseComplete { phase }
                    ) {
                        error!(error = %e, "Failed to forward ShutdownPhaseComplete to ControlManager");
                    }
                } else {
                    error!("ControlManager missing; cannot process phase completion");
                }
            }

            BrokerMessage::CheckShutdownComplete => {
                let span = trace_span!("cassini.broker.check_shutdown_complete");
                let _g = span.enter();
                if state.is_shutting_down {
                    let _ = self.check_shutdown_completion(_myself.clone(), state).await;
                }
            }
    
            BrokerMessage::RegistrationRequest {
                registration_id,
                client_id,
                trace_ctx,
            } => {
                let span = trace_span!("cassini.broker.registration_request", %client_id, registration_id = ?registration_id);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();
                trace!("Broker received registration request");

                // Reject new registrations during shutdown
                if state.is_shutting_down {
                    warn!("Rejecting registration request during shutdown");
                    if let Some(session_mgr) = &state.session_mgr {
                        session_mgr.send_message(BrokerMessage::RegistrationResponse {
                            client_id,
                            result: Err("Broker is shutting down".to_string()),
                            trace_ctx: Some(span.context()),
                        })?;
                    }
                    return Ok(());
                }

                if let Some(session_mgr) = &state.session_mgr {
                    if let Err(e) = session_mgr.send_message(BrokerMessage::RegistrationRequest {
                        registration_id: registration_id.clone(),
                        client_id: client_id.clone(),
                        trace_ctx: Some(span.context()),
                    }) {
                        error!(error = %e, "Failed to forward RegistrationRequest to SessionManager");
                    }

                    if let Some(sub_mgr) = &state.subscriber_mgr {
                        if let Err(e) = sub_mgr.send_message(BrokerMessage::RegistrationRequest {
                            registration_id,
                            client_id,
                            trace_ctx: Some(span.context()),
                        }) {
                            warn!(error = %e, "Failed to notify SubscriberManager about registration");
                        }
                    } else {
                        error!("SubscriberManager ActorRef missing in broker state");
                    }
                } else {
                    error!("SessionManager ActorRef missing in broker state; cannot process registration");
                }
            }

            BrokerMessage::SubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                let span = trace_span!("cassini.broker.subscribe_request", %registration_id, %topic);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();
                trace!("Broker received subscribe request");

                // Reject new subscriptions during shutdown
                if state.is_shutting_down {
                    warn!("Rejecting subscribe request during shutdown");
                    if let Some(session_mgr) = &state.session_mgr {
                        session_mgr.send_message(BrokerMessage::SubscribeAcknowledgment {
                            registration_id,
                            topic,
                            result: Err("Broker is shutting down".to_string()),
                            trace_ctx: Some(span.context()),
                        })?;
                    }
                    return Ok(());
                }

                if let Some(topic_mgr) = &state.topic_mgr {
                    if let Err(e) = topic_mgr.send_message(BrokerMessage::SubscribeRequest {
                        registration_id,
                        topic,
                        trace_ctx: Some(span.context()),
                    }) {
                        error!(error = %e, "Failed to forward SubscribeRequest to TopicManager");
                    }
                } else {
                    error!("TopicManager ActorRef missing in broker state; cannot subscribe");
                }
            }

            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                let span = trace_span!("cassini.broker.unsubscribe_request", %registration_id, %topic);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();
                trace!("Broker received unsubscribe request");

                // I thought for a long time about whether or not we should allow unsubscribe
                // during shutdown. Ultimately, it's just one more work item we have to clear out.
                // I doubt we would get many, if any of these during the shutdown sequence, but if
                // we did, we're just deliberately allowing a client to be removed that is going to
                // shut itself down anyway (probably). So we'll let this happen, just as we're
                // going to allow disconnect requests below.

                if let Some(topic_mgr) = &state.topic_mgr {
                    if let Err(e) = topic_mgr.send_message(BrokerMessage::UnsubscribeRequest {
                        registration_id: registration_id.clone(),
                        topic: topic.clone(),
                        trace_ctx: Some(span.context()),
                    }) {
                        warn!(error = %e, "Failed to forward UnsubscribeRequest to TopicManager");
                    }
                } else {
                    warn!(%topic, "TopicManager missing while handling unsubscribe");
                }

                if let Some(subscriber_mgr) = &state.subscriber_mgr {
                    if let Err(e) = subscriber_mgr.send_message(BrokerMessage::UnsubscribeRequest {
                        registration_id,
                        topic,
                        trace_ctx: Some(span.context()),
                    }) {
                        warn!(error = %e, "Failed to forward UnsubscribeRequest to SubscriberManager");
                    }
                } else {
                    warn!(%topic, "SubscriberManager missing while handling unsubscribe");
                }
            }

            BrokerMessage::PublishRequest {
                registration_id,
                topic,
                payload,
                trace_ctx,
            } => {
                let payload_len = payload.len();
                let span = trace_span!("cassini.broker.publish_request", %registration_id, %topic, payload_bytes = payload_len);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();
                debug!("Processing publish request with span: {:?}", span.metadata().map(|m| m.name()));
                trace!("Broker received publish request");

                // Reject new publishes during shutdown
                if state.is_shutting_down {
                    warn!("Rejecting publish request during shutdown");
                    if let Some(session_mgr) = &state.session_mgr {
                        if let Err(e2) =
                            session_mgr.send_message(BrokerMessage::PublishResponse {
                                topic,
                                payload: Vec::new().into(),
                                result: Err("Broker is shutting down".to_string()),
                                trace_ctx: Some(span.context()),
                            })
                        {
                            warn!(error = %e2, "Failed to notify SessionManager of publish failure");
                        }
                    }
                    return Ok(());
                }

                if let Some(topic_mgr) = &state.topic_mgr {
                    // Low-hanging fruit: don't clone payload unless we have to.
                    // Move it into the forward message; only clone in the failure path.
                    let forward = topic_mgr.send_message(BrokerMessage::PublishRequest {
                        registration_id: registration_id.clone(),
                        topic: topic.clone(),
                        payload,
                        trace_ctx: Some(span.context()),
                    });

                    if let Err(e) = forward {
                        warn!(error = %e, "Failed to forward PublishRequest to TopicManager");

                        // If forwarding fails, we need a payload for the failure response.
                        // We only have the length now; best we can do is return an error without echoing bytes.
                        if let Some(session_mgr) = &state.session_mgr {
                            if let Err(e2) =
                                session_mgr.send_message(BrokerMessage::PublishResponse {
                                    topic,
                                    payload: Vec::new().into(),
                                    result: Err(format!("forward failed: {e}")),
                                    trace_ctx: Some(span.context()),
                                })
                            {
                                warn!(error = %e2, "Failed to notify SessionManager of publish failure");
                            }
                        } else {
                            error!(
                                payload_bytes = payload_len,
                                "SessionManager missing; publish failure can't be reported"
                            );
                        }
                    }
                } else {
                    error!("TopicManager missing; cannot publish");
                    if let Some(session_mgr) = &state.session_mgr {
                        let _ = session_mgr.send_message(BrokerMessage::PublishResponse {
                            topic,
                            payload: Vec::new().into(),
                            result: Err("topic manager unavailable".to_string()),
                            trace_ctx: Some(span.context()),
                        });
                    }
                }
            }

            BrokerMessage::PublishResponse {
                topic,
                payload,
                result,
                trace_ctx,
            } => {
                let payload_len = payload.len();
                let span = trace_span!("cassini.broker.publish_response", %topic, ok = result.is_ok(), payload_bytes = payload_len);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();

                if let Some(sub_mgr) = &state.subscriber_mgr {
                    if let Err(e) = sub_mgr.send_message(BrokerMessage::PublishResponse {
                        topic,
                        payload,
                        result,
                        trace_ctx: Some(span.context()),
                    }) {
                        error!(error = %e, "Failed to forward PublishResponse to SubscriberManager");
                    }
                } else {
                    error!("SubscriberManager missing; cannot forward PublishResponse");
                }
            }

            BrokerMessage::ErrorMessage { error, .. } => {
                warn!(%error, "Broker received error message");
            }

            BrokerMessage::DisconnectRequest {
                reason,
                client_id,
                registration_id,
                trace_ctx,
            } => {
                let span = trace_span!("cassini.broker.disconnect_request", %client_id, registration_id = ?registration_id, reason = ?reason);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();
                debug!("Broker cleaning up disconnect");

                // See not on UnsubscribeRequest above, RE: shutdown

                if let Some(subscriber_mgr) = &state.subscriber_mgr {
                    if let Err(e) = subscriber_mgr.send_message(BrokerMessage::DisconnectRequest {
                        reason: reason.clone(),
                        client_id: client_id.clone(),
                        registration_id: registration_id.clone(),
                        trace_ctx: Some(span.context()),
                    }) {
                        error!(error = %e, "Failed to forward DisconnectRequest to SubscriberManager");
                    }
                } else {
                    error!(%client_id, "SubscriberManager missing while processing disconnect");
                }

                if let Some(listener_mgr) = &state.listener_mgr {
                    if let Err(e) = listener_mgr.send_message(BrokerMessage::DisconnectRequest {
                        reason,
                        client_id,
                        registration_id,
                        trace_ctx: Some(span.context()),
                    }) {
                        error!(error = %e, "Failed to forward DisconnectRequest to ListenerManager");
                    }
                } else {
                    error!(%client_id, "ListenerManager missing while processing disconnect");
                }
            }

            BrokerMessage::TimeoutMessage {
                client_id,
                registration_id,
                error,
            } => {
                // No trace_ctx in this message variant. Still give it a searchable span.
                let span = trace_span!(
                    "cassini.broker.timeout",
                    client_id = %client_id,
                    registration_id = %registration_id,
                    error = ?error
                );
                let _g = span.enter();

                if let Some(sub_mgr) = &state.subscriber_mgr {
                    if let Err(e) = sub_mgr.send_message(BrokerMessage::TimeoutMessage {
                        client_id,
                        registration_id,
                        error,
                    }) {
                        error!(error = %e, "Failed to forward TimeoutMessage to SubscriberManager");
                    }
                } else {
                    error!("SubscriberManager missing while handling timeout");
                }
            }

            BrokerMessage::ControlRequest {
                registration_id,
                op,
                reply_to,
                trace_ctx,
            } => {
                let span = trace_span!("cassini.broker.control_request", %registration_id, op = ?op);
                try_set_parent_otel(&span, trace_ctx);
                let _g = span.enter();

                if let Some(control_mgr) = &state.control_mgr {
                    if let Err(e) = control_mgr.send_message(BrokerMessage::ControlRequest {
                        registration_id,
                        op,
                        reply_to,
                        trace_ctx: Some(span.context()),
                    }) {
                        warn!(error = %e, "Failed to forward ControlRequest to ControlManager");
                    }
                } else {
                    error!("ControlManager missing; dropping control request");
                }
            }

            other => {
                warn!(?other, "{UNEXPECTED_MESSAGE_STR}");
            }
        }

        Ok(())
    }
}

impl Broker {
    async fn check_shutdown_completion(
        &self,
        myself: ActorRef<<Broker as ractor::Actor>::Msg>,
        state: &mut BrokerState,
    ) -> Result<(), ActorProcessingErr> {
        // Check if all managers have terminated
        let all_terminated = state.listener_mgr.is_none() &&
                            state.session_mgr.is_none() &&
                            state.topic_mgr.is_none() &&
                            state.subscriber_mgr.is_none() &&
                            state.control_mgr.is_none();
        
        if all_terminated {
            info!("All managers have terminated. Broker shutdown complete.");
            myself.stop(Some("GRACEFUL_SHUTDOWN_COMPLETE".to_string()));
        }
        
        Ok(())
    }
}
