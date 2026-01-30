use crate::{
    control::{ControlManager, ControlManagerState},
    listener::{ListenerManager, ListenerManagerArgs},
    session::{SessionManager, SessionManagerArgs},
    subscriber::SubscriberManager,
    topic::{TopicManager, TopicManagerArgs},
    BrokerConfigError, LISTENER_MANAGER_NAME, SESSION_MANAGER_NAME, SUBSCRIBER_MANAGER_NAME,
    TOPIC_MANAGER_NAME, UNEXPECTED_MESSAGE_STR,
};
use cassini_types::BrokerMessage;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use tracing::{debug, error, info, trace, trace_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use std::env;

// ============================== Broker Supervisor Actor Definition ============================== //

pub struct Broker;

// State object containing references to worker actors
pub struct BrokerState {
    listener_mgr: Option<ActorRef<BrokerMessage>>,
    session_mgr: Option<ActorRef<BrokerMessage>>,
    topic_mgr: Option<ActorRef<BrokerMessage>>,
    subscriber_mgr: Option<ActorRef<BrokerMessage>>,
    control_mgr: Option<ActorRef<BrokerMessage>>,
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

        Ok(BrokerArgs {
            bind_addr,
            session_timeout: None,
            server_cert_file,
            private_key_file,
            ca_cert_file,
        })
    }
}

#[async_trait]
impl Actor for Broker {
    type Msg = BrokerMessage;
    type State = BrokerState;
    type Arguments = BrokerArgs;

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

        let session_timeout = args.session_timeout.unwrap_or(90);

        let (session_mgr, _) = Actor::spawn_linked(
            Some(SESSION_MANAGER_NAME.to_owned()),
            SessionManager,
            SessionManagerArgs { session_timeout },
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
        })
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _: &mut Self::State,
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

            SupervisionEvent::ActorTerminated(actor_cell, ..) => {
                // Don't claim we restart if we don't.
                warn!(
                    worker_name = ?actor_cell.get_name(),
                    worker_id = ?actor_cell.get_id(),
                    "Worker terminated (no restart strategy implemented here)"
                );
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

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!(actor = ?myself, "Broker started");
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
            BrokerMessage::RegistrationRequest {
                registration_id,
                client_id,
                trace_ctx,
            } => {
                let span = trace_span!(
                    "cassini.broker.registration_request",
                    %client_id,
                    registration_id = ?registration_id
                );
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
                let _g = span.enter();

                trace!("Broker received registration request");

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
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
                let _g = span.enter();

                trace!("Broker received subscribe request");

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
                let span =
                    trace_span!("cassini.broker.unsubscribe_request", %registration_id, %topic);
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
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
            }

            BrokerMessage::PublishRequest {
                registration_id,
                topic,
                payload,
                trace_ctx,
            } => {
                let payload_len = payload.len();
                let span = trace_span!(
                    "cassini.broker.publish_request",
                    %registration_id,
                    %topic,
                    payload_bytes = payload_len
                );
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
                let _g = span.enter();

                trace!("Broker received publish request");

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
                                    payload: Vec::new(),
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
                            payload: Vec::new(),
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
                let span = trace_span!(
                    "cassini.broker.publish_response",
                    %topic,
                    ok = result.is_ok(),
                    payload_bytes = payload_len
                );
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
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
                let span = trace_span!(
                    "cassini.broker.disconnect_request",
                    %client_id,
                    registration_id = ?registration_id,
                    reason = ?reason
                );
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
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
                if let Some(ctx) = trace_ctx {
                    let _ = span.set_parent(ctx);
                }
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
