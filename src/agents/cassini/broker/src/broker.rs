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

use crate::{
    LISTENER_MANAGER_NAME, SESSION_MANAGER_NAME, SUBSCRIBER_MANAGER_NAME, TOPIC_MANAGER_NAME,
    UNEXPECTED_MESSAGE_STR,
    control::{ControlManager, ControlManagerState},
    listener::{ListenerManager, ListenerManagerArgs},
    session::{SessionManager, SessionManagerArgs},
    subscriber::{SubscriberManager, SubscriberManagerArgs},
    topic::{TopicManager, TopicManagerArgs},
};

use cassini_tracing::try_set_parent_otel;

use cassini_types::ControlOp;
use cassini_types::DisconnectReason;
use cassini_types::{BrokerMessage, ShutdownPhase};

use polar_agent_builder::{
    Configured, InfrastructureEvent, InfrastructureSupervisorMessage, ServiceBuilder,
};

use ractor::concurrency::Duration;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, trace_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;

// ============================== Broker Supervisor Actor Definition ============================== //

pub struct Broker;

// State object containing references to worker actors
pub struct BrokerState {
    /// The builder's infrastructure: the broker asks it to begin the shutdown (which starts the
    /// budget's watchdog), and it tells the broker when something else has decided it should end.
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    listener_mgr: Option<ActorRef<BrokerMessage>>,
    session_mgr: Option<ActorRef<BrokerMessage>>,
    topic_mgr: Option<ActorRef<BrokerMessage>>,
    subscriber_mgr: Option<ActorRef<BrokerMessage>>,
    control_mgr: Option<ActorRef<BrokerMessage>>,

    is_shutting_down: bool,
}

/// What the broker is started with: where it listens and with what certificate, and the
/// configuration the builder judged, from which it builds its infrastructure.
pub struct BrokerArgs {
    /// Socket address to listen for connections on
    pub bind_addr: String,

    /// How long (in seconds) a session whose client has gone away is kept, with the messages held
    /// for it, before it is ended. `None` is `DEFAULT_SESSION_RETENTION` (ten minutes).
    pub session_timeout: Option<u64>,

    pub server_cert_file: String,
    /// The certificate's key: a file, or the custody daemon's server slot.
    pub server_key: polar_custody::client::ServerKey,
    pub ca_cert_file: String,

    /// Authorization token for shutdown operations
    pub shutdown_auth_token: Option<String>,

    /// How long a connection keeps the broker's decision about a topic; zero keeps none.
    pub decision_cache: Duration,

    /// The configuration `run` judged: the snapshot, the health settings and the shutdown budget,
    /// from which the broker builds its infrastructure (the health check, the signals, the
    /// shutdown handshake with its budget and watchdog).
    pub configured: Configured,
}

impl BrokerArgs {
    /// From the broker's settings and what `run` judged.
    pub fn from_settings(
        settings: crate::settings::BrokerSettings,
        configured: Configured,
    ) -> Self {
        Self {
            bind_addr: settings.bind_addr,
            session_timeout: None,
            server_cert_file: settings.server_cert_file,
            server_key: settings.server_key,
            ca_cert_file: settings.ca_cert_file,
            shutdown_auth_token: Some(settings.shutdown_auth_token),
            decision_cache: settings.decision_cache,
            configured,
        }
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
                let span = trace_span!("cassini.broker.supervision_event.terminated");
                let _g = span.enter();

                warn!(
                    worker_name = ?actor_cell.get_name(),
                    worker_id = ?actor_cell.get_id(),
                    "Worker terminated (no restart strategy implemented here)"
                );

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

                if state.is_shutting_down {
                    self.check_shutdown_completion(myself.clone(), state)
                        .await?;
                }
            }

            SupervisionEvent::ActorFailed(actor_cell, e) => {
                // Today: the broker ends. Tomorrow: restart policy per worker type. It ends in
                // order: the infrastructure tells the broker to begin its shutdown, and its
                // watchdog ends the process if that does not finish within the budget.
                error!(
                    worker_name = ?actor_cell.get_name(),
                    worker_id = ?actor_cell.get_id(),
                    error = %e,
                    "Worker failed; ending the broker in order"
                );
                let _ = state
                    .infrastructure
                    .cast(InfrastructureSupervisorMessage::BeginShutdown {
                        reason: format!(
                            "the broker's worker {} failed: {e}",
                            actor_cell.get_name().unwrap_or_default()
                        ),
                    });
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

        // The infrastructure: the health check, the signal listener and the shutdown handshake
        // with its budget and watchdog, as every agent has, without a Cassini client (the broker
        // is what a client connects to). It tells the broker when something has decided it
        // should end: a health verdict, SIGTERM, a worker's death.
        let shutdown_budget = args.configured.shutdown_budget();
        let publisher = args
            .configured
            .config()
            .read(&polar_agent_builder::environment::AUTHORITY_PUBLISHER)
            .map_err(|e| ActorProcessingErr::from(e.to_string()))?;
        let infrastructure = ServiceBuilder::new(myself.clone(), "cassini")
            .into_configured(args.configured)
            .build()
            .await
            .map_err(|e| {
                error!(error = ?e, "Broker startup failed: the infrastructure did not start");
                e
            })?;
        let token_for_infrastructure = args.shutdown_auth_token.clone();
        infrastructure
            .infrastructure_events
            .subscribe(myself.clone(), move |event| match event {
                InfrastructureEvent::PrepareShutdown => Some(BrokerMessage::PrepareForShutdown {
                    auth_token: token_for_infrastructure.clone(),
                }),
            });

        // Create SessionManager without listener manager (will set later)
        let (session_mgr, _) = Actor::spawn_linked(
            Some(SESSION_MANAGER_NAME.to_owned()),
            SessionManager,
            SessionManagerArgs {
                broker: myself.clone(),
                listener_mgr: None,
                retention: args
                    .session_timeout
                    .map(std::time::Duration::from_secs)
                    .unwrap_or(crate::DEFAULT_SESSION_RETENTION),
            },
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
            SubscriberManagerArgs {
                session_mgr: session_mgr.clone(),
            },
            myself.clone().into(),
        )
        .await
        .map_err(|e| {
            error!(error = ?e, "Broker startup failed: SubscriberManager failed to start");
            ActorProcessingErr::from(e)
        })?;

        // Wire the subscriber manager ref into session_mgr now that it
        // exists -- SessionManager is spawned before SubscriberManager, so
        // it can't receive this at construction time.
        let _ = session_mgr.send_message(BrokerMessage::SetSubscriberManagerRef {
            subscriber_mgr: subscriber_mgr.clone(),
        });

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

        // Wire the topic manager ref into subscriber_mgr now that it exists
        // -- SubscriberManager is spawned before TopicManager, so it can't
        // receive this at construction time.
        let _ = subscriber_mgr.send_message(BrokerMessage::SetTopicManagerRef {
            topic_mgr: topic_mgr.clone(),
        });

        // Who may publish and subscribe to what: the credentials agent's authority, kept as every agent keeps it,
        // and asked for through the topic manager. It exists before the first connection is accepted.
        let authority = crate::authority::BrokerAuthority::spawn(
            publisher,
            myself.get_cell(),
            topic_mgr.clone(),
            args.decision_cache,
        )
        .await
        .map_err(|e| {
            error!(error = ?e, "Broker startup failed: its authority did not start");
            e
        })?;

        // The ListenerManager comes last of the workers that serve clients: it accepts connections at once, and
        // each needs the session manager and the authority.
        let listener_manager_args = ListenerManagerArgs {
            bind_addr: args.bind_addr,
            server_cert_file: args.server_cert_file,
            server_key: args.server_key,
            ca_cert_file: args.ca_cert_file,
            session_mgr: session_mgr.clone(),
            shutdown_auth_token: args.shutdown_auth_token.clone(),
            authority,
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

        // Send listener manager ref to the existing session manager
        let _ = session_mgr.send_message(BrokerMessage::SetListenerManager {
            listener_mgr: listener_mgr.clone(),
        });

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
            // The budget is shared by the five phases.
            shutdown_timeout: (shutdown_budget / 5).max(Duration::from_millis(1)),
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
            infrastructure: infrastructure.infrastructure,
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
                if myself
                    .send_message(BrokerMessage::CheckShutdownComplete)
                    .is_err()
                {
                    // Broker is gone, exit loop
                    break;
                }
                // Continue loop
            }
        });
        Ok(())
    }

    async fn post_stop(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // The infrastructure is told it may come down, which also ends its watchdog.
        let _ = state
            .infrastructure
            .cast(InfrastructureSupervisorMessage::ProceedWithShutdown);
        // The managers are this actor's children and nothing else stops them: left running they
        // keep their names, and the listener keeps the address bound.
        myself
            .get_cell()
            .stop_children_and_wait(None, Some(std::time::Duration::from_secs(5)))
            .await;
        debug!(actor = ?myself, "Broker stopped");
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            // Order definitely matters here. We must check for shutdown messages first and handle those.
            BrokerMessage::PrepareForShutdown { auth_token } => {
                self.handle_prepare_for_shutdown(myself, state, auth_token)
                    .await?;
            }
            BrokerMessage::InitiateShutdownPhase { phase } => {
                self.handle_initiate_shutdown_phase(myself, state, phase)
                    .await?;
            }
            BrokerMessage::ShutdownPhaseComplete { phase } => {
                self.handle_shutdown_phase_complete(myself, state, phase)
                    .await?;
            }
            BrokerMessage::CheckShutdownComplete => {
                self.handle_check_shutdown_complete(myself, state).await?;
            }
            BrokerMessage::RegistrationRequest {
                registration_id,
                client_id,
                trace_ctx,
                peer_identity,
            } => {
                self.handle_registration_request(
                    myself,
                    state,
                    registration_id,
                    client_id,
                    trace_ctx,
                    peer_identity,
                )
                .await?;
            }
            BrokerMessage::SubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                self.handle_subscribe_request(myself, state, registration_id, topic, trace_ctx)
                    .await?;
            }
            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                self.handle_unsubscribe_request(myself, state, registration_id, topic, trace_ctx)
                    .await?;
            }
            BrokerMessage::PublishRequest {
                registration_id,
                topic,
                payload,
                reply_to,
                trace_ctx,
                published_by,
            } => {
                self.handle_publish_request(
                    myself,
                    state,
                    registration_id,
                    topic,
                    payload,
                    reply_to,
                    trace_ctx,
                    published_by,
                )
                .await?;
            }
            BrokerMessage::PublishResponse {
                topic,
                payload,
                result,
                trace_ctx,
                published_by,
            } => {
                self.handle_publish_response(
                    myself,
                    state,
                    topic,
                    payload,
                    result,
                    trace_ctx,
                    published_by,
                )
                .await?;
            }
            BrokerMessage::ErrorMessage { error, .. } => {
                self.handle_error_message(myself, state, error).await?;
            }
            BrokerMessage::DisconnectRequest {
                reason,
                client_id,
                registration_id,
                trace_ctx,
            } => {
                self.handle_disconnect_request(
                    myself,
                    state,
                    reason,
                    client_id,
                    registration_id,
                    trace_ctx,
                )
                .await?;
            }
            BrokerMessage::TimeoutMessage {
                client_id,
                registration_id,
                error,
            } => {
                self.handle_timeout_message(myself, state, client_id, registration_id, error)
                    .await?;
            }
            BrokerMessage::ControlRequest {
                registration_id,
                op,
                reply_to,
                trace_ctx,
            } => {
                self.handle_control_request(
                    myself,
                    state,
                    registration_id,
                    op,
                    reply_to,
                    trace_ctx,
                )
                .await?;
            }
            other => {
                warn!(?other, "{UNEXPECTED_MESSAGE_STR}");
            }
        }
        Ok(())
    }
}

impl Broker {
    // ===== Handler methods =====
    async fn handle_prepare_for_shutdown(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut BrokerState,
        auth_token: Option<String>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.broker.prepare_for_shutdown");
        let _g = span.enter();

        match &state.control_mgr {
            Some(control_mgr) => {
                if let Err(e) =
                    control_mgr.send_message(BrokerMessage::PrepareForShutdown { auth_token })
                {
                    error!(error = %e, "Failed to forward shutdown request to ControlManager");
                }
            }
            None => error!("ControlManager missing; cannot process shutdown"),
        }
        Ok(())
    }

    async fn handle_initiate_shutdown_phase(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut BrokerState,
        phase: ShutdownPhase,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.broker.initiate_shutdown_phase", phase = ?phase);
        let _g = span.enter();

        info!("Initiating shutdown phase: {:?}", phase);
        if !state.is_shutting_down {
            // The shutdown has begun. The infrastructure starts the budget's watchdog if nothing
            // asked for the shutdown through it (a shutdown requested over the wire did not); if
            // it has begun already this changes nothing.
            let _ = state
                .infrastructure
                .cast(InfrastructureSupervisorMessage::BeginShutdown {
                    reason: "an orderly shutdown was requested".to_string(),
                });
        }
        state.is_shutting_down = true;

        match phase {
            ShutdownPhase::StopAcceptingNewConnections => {
                if let Some(listener_mgr) = &state.listener_mgr {
                    info!("Sending InitiateShutdownPhase to ListenerManager");
                    listener_mgr.send_message(BrokerMessage::InitiateShutdownPhase { phase })?;
                }
            }
            ShutdownPhase::DrainExistingSessions => {
                if let Some(session_mgr) = &state.session_mgr {
                    session_mgr.send_message(BrokerMessage::InitiateShutdownPhase { phase })?;
                } else {
                    // Manager already gone — signal completion ourselves
                    if let Some(control_mgr) = &state.control_mgr {
                        control_mgr.send_message(BrokerMessage::ShutdownPhaseComplete { phase })?;
                    }
                }
            }
            ShutdownPhase::FlushTopicQueues => {
                if let Some(topic_mgr) = &state.topic_mgr {
                    topic_mgr.send_message(BrokerMessage::InitiateShutdownPhase { phase })?;
                }
            }
            ShutdownPhase::TerminateSubscribers => {
                if let Some(subscriber_mgr) = &state.subscriber_mgr {
                    subscriber_mgr.send_message(BrokerMessage::InitiateShutdownPhase { phase })?;
                }
            }
            ShutdownPhase::TerminateListeners => {
                if let Some(listener_mgr) = &state.listener_mgr {
                    info!(
                        "Sending InitiateShutdownPhase to ListenerManager for TerminateListeners"
                    );
                    listener_mgr.send_message(BrokerMessage::InitiateShutdownPhase { phase })?;
                }
            }
        }
        Ok(())
    }

    async fn handle_shutdown_phase_complete(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut BrokerState,
        phase: ShutdownPhase,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.broker.shutdown_phase_complete", phase = ?phase);
        let _g = span.enter();

        info!("Shutdown phase completed: {:?}", phase);

        // Forward to ControlManager
        if let Some(control_mgr) = &state.control_mgr {
            if let Err(e) = control_mgr.send_message(BrokerMessage::ShutdownPhaseComplete { phase })
            {
                error!(error = %e, "Failed to forward ShutdownPhaseComplete to ControlManager");
            }
        } else {
            error!("ControlManager missing; cannot process phase completion");
        }
        Ok(())
    }

    async fn handle_check_shutdown_complete(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut BrokerState,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.broker.check_shutdown_complete");
        let _g = span.enter();
        if state.is_shutting_down {
            self.check_shutdown_completion(myself.clone(), state)
                .await?;
        }
        Ok(())
    }

    async fn handle_registration_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut BrokerState,
        registration_id: Option<String>,
        client_id: String,
        trace_ctx: Option<opentelemetry::Context>,
        peer_identity: Option<String>,
    ) -> Result<(), ActorProcessingErr> {
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
                peer_identity: peer_identity.clone(),
            }) {
                error!(error = %e, "Failed to forward RegistrationRequest to SessionManager");
            }

            // The session manager tells the subscriber manager itself, once a resumed session has
            // its new connection. See SessionManager::handle_registration_request.
        } else {
            error!("SessionManager ActorRef missing in broker state; cannot process registration");
        }
        Ok(())
    }

    async fn handle_subscribe_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut BrokerState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.broker.subscribe_request", %registration_id, %topic);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("Broker received subscribe request");

        // Reject new subscriptions during shutdown
        if state.is_shutting_down {
            warn!("Rejecting subscribe request during shutdown");
            // To the session, which forwards it to the client (the session manager had no use for it).
            crate::topic::acknowledge_subscribe(
                &registration_id,
                &topic,
                Err("Broker is shutting down".to_string()),
                Some(span.context()),
            );
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
        Ok(())
    }

    async fn handle_unsubscribe_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut BrokerState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.broker.unsubscribe_request", %registration_id, %topic);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("Broker received unsubscribe request");

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
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_publish_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut BrokerState,
        registration_id: String,
        topic: String,
        payload: Arc<Vec<u8>>,
        reply_to: Option<ActorRef<BrokerMessage>>,
        trace_ctx: Option<opentelemetry::Context>,
        published_by: Option<String>,
    ) -> Result<(), ActorProcessingErr> {
        let payload_len = payload.len();
        let span = trace_span!("cassini.broker.publish_request", %registration_id, %topic, payload_bytes = payload_len);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        debug!(
            "Processing publish request with span: {:?}",
            span.metadata().map(|m| m.name())
        );
        trace!("Broker received publish request");

        // Reject new publishes during shutdown
        if state.is_shutting_down {
            warn!("Rejecting publish request during shutdown");
            if let Some(session_mgr) = &state.session_mgr
                && let Err(e2) = session_mgr.send_message(BrokerMessage::PublishResponse {
                    topic,
                    payload: Vec::new().into(),
                    result: Err("Broker is shutting down".to_string()),
                    trace_ctx: Some(span.context()),
                    published_by: None,
                })
            {
                warn!(error = %e2, "Failed to notify SessionManager of publish failure");
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
                reply_to, // pass through the reply_to from the incoming message
                trace_ctx: Some(span.context()),
                published_by,
            });

            if let Err(e) = forward {
                warn!(error = %e, "Failed to forward PublishRequest to TopicManager");

                // If forwarding fails, we need a payload for the failure response.
                // We only have the length now; best we can do is return an error without echoing bytes.
                if let Some(session_mgr) = &state.session_mgr {
                    if let Err(e2) = session_mgr.send_message(BrokerMessage::PublishResponse {
                        topic,
                        payload: Vec::new().into(),
                        result: Err(format!("forward failed: {e}")),
                        trace_ctx: Some(span.context()),
                        published_by: None,
                    }) {
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
                    published_by: None,
                });
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_publish_response(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut BrokerState,
        topic: String,
        payload: Arc<Vec<u8>>,
        result: Result<(), String>,
        trace_ctx: Option<opentelemetry::Context>,
        published_by: Option<String>,
    ) -> Result<(), ActorProcessingErr> {
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
                published_by,
            }) {
                error!(error = %e, "Failed to forward PublishResponse to SubscriberManager");
            }
        } else {
            error!("SubscriberManager missing; cannot forward PublishResponse");
        }
        Ok(())
    }

    async fn handle_error_message(
        &self,
        _myself: ActorRef<BrokerMessage>,
        _state: &mut BrokerState,
        error: String,
    ) -> Result<(), ActorProcessingErr> {
        warn!(%error, "Broker received error message");
        Ok(())
    }

    async fn handle_disconnect_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut BrokerState,
        reason: DisconnectReason,
        client_id: String,
        registration_id: Option<String>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.broker.disconnect_request", %client_id, registration_id = ?registration_id, reason = ?reason);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        debug!("Broker cleaning up disconnect");

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
        Ok(())
    }

    async fn handle_timeout_message(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut BrokerState,
        client_id: String,
        registration_id: String,
        error: Option<String>,
    ) -> Result<(), ActorProcessingErr> {
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
        Ok(())
    }

    async fn handle_control_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut BrokerState,
        registration_id: String,
        op: ControlOp,
        reply_to: Option<ActorRef<BrokerMessage>>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
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
        Ok(())
    }

    async fn check_shutdown_completion(
        &self,
        myself: ActorRef<<Broker as ractor::Actor>::Msg>,
        state: &mut BrokerState,
    ) -> Result<(), ActorProcessingErr> {
        // Check if all managers have terminated
        let all_terminated = state.listener_mgr.is_none()
            && state.session_mgr.is_none()
            && state.topic_mgr.is_none()
            && state.subscriber_mgr.is_none()
            && state.control_mgr.is_none();

        if all_terminated {
            info!("All managers have terminated. Broker shutdown complete.");
            myself.stop(Some("GRACEFUL_SHUTDOWN_COMPLETE".to_string()));
        }

        Ok(())
    }
}
