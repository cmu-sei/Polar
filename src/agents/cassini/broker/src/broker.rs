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
        use std::env;

        let server_cert_file = BrokerArgs::required_env("TLS_SERVER_CERT_CHAIN")?;
        let private_key_file = BrokerArgs::required_env("TLS_SERVER_KEY")?;
        let ca_cert_file = BrokerArgs::required_env("TLS_CA_CERT")?;
        let bind_addr = env::var("CASSINI_BIND_ADDR").unwrap_or(String::from("0.0.0.0:8080"));

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
        debug!("Broker: starting {:?}", myself);

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
            error!("Broker startup failed: ListenerManager failed to start: {e:?}");
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
            error!("Broker startup failed: SessionManager failed to start: {e:?}");
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
            error!("failed to spawn SubscriberManager: {e:?}");
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
            error!("failed to spawn TopicManager: {e:?}");
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
            error!("failed to spawn ControlManager: {e:?}");
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
        _myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(actor_cell) => {
                debug!(
                    "Worker agent: {0:?}:{1:?} started",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                )
            }

            SupervisionEvent::ActorTerminated(actor_cell, ..) => {
                debug!(
                    "Worker {0:?}:{1:?} terminated, restarting..",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                )
            }

            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!(
                    "Worker agent: {0:?}:{1:?} failed! {e}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );

                // Determine type of actor that failed and restart

                // NOTE: "Remote" actors can't have their types checked? But
                // they do send serializable messages. If we can deserialize
                // them to a datatype here, that may be another acceptable means
                // of determining type.

                // TODO: Figure out what panics/failures we can/can't recover
                // from Missing certificate files and the inability to forward
                // some messages count as bad states
                _myself.stop(Some("ACTOR_FAILED".to_string()));

                // Vaughn, this sounds like a whiteboarding session. These are
                // top-level supervisors, so if they fail we definitely should
                // _try_ to recover them, but each one may have a fairly hefty
                // amount of stuff to recover. This may be akin to a full
                // restart / re-init of the broker, if one of these fails.
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
        debug!("Broker: Started {myself:?}");
        Ok(())
    }

    async fn post_stop(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Broker: Stopped {myself:?}");
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
                let span = trace_span!("broker.handle_registration_request", client_id = client_id);
                if let Some(ctx) = trace_ctx.clone() {
                    span.set_parent(ctx.clone()).ok();
                }

                trace!("Broker actor received registration request");

                // Forward the registration request to the session manager.
                // The SessionManager is responsible for locating/creating the session actor.
                if let Some(session_mgr) = &state.session_mgr {
                    if let Err(e) = session_mgr.send_message(BrokerMessage::RegistrationRequest {
                        registration_id: registration_id.clone(),
                        client_id: client_id.clone(),
                        trace_ctx: Some(span.context()),
                    }) {
                        error!("Failed to forward RegistrationRequest to SessionManager: {e:?}");
                    }

                    // Also notify subscriber manager so any queued messages can be flushed.
                    if let Some(sub_mgr) = &state.subscriber_mgr {
                        if let Err(e) = sub_mgr.send_message(BrokerMessage::RegistrationRequest {
                            registration_id: registration_id.clone(),
                            client_id: client_id.clone(),
                            trace_ctx: Some(span.context()),
                        }) {
                            warn!("Failed to notify SubscriberManager about registration: {e:?}");
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
                let span = trace_span!("broker.handle_subscribe_request", %registration_id, %topic);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("Broker received subscribe request message");

                // Delegate subscription/creation logic to TopicManager + SubscriberManager.
                // TopicManager should ensure topic exists (idempotent) and create or forward to topic actor.
                if let Some(topic_mgr) = &state.topic_mgr {
                    if let Err(e) = topic_mgr.send_message(BrokerMessage::SubscribeRequest {
                        registration_id: registration_id.clone(),
                        topic: topic.clone(),
                        trace_ctx: Some(span.context()),
                    }) {
                        error!("Failed to forward SubscribeRequest to TopicManager: {e:?}");
                    }
                } else {
                    error!("TopicManager ActorRef missing in broker state; cannot subscribe session {registration_id} to {topic}");
                }

                // Forward acknowledgment to the SessionManager; let it route to the appropriate session.
                if let Some(session_mgr) = &state.session_mgr {
                    if let Err(e) =
                        session_mgr.send_message(BrokerMessage::SubscribeAcknowledgment {
                            registration_id: registration_id.clone(),
                            topic: topic.clone(),
                            result: Ok(()),
                            trace_ctx: Some(span.context()),
                        })
                    {
                        warn!("Failed to forward SubscribeAcknowledgment to SessionManager: {e:?}");
                    }
                } else {
                    warn!("SessionManager missing when trying to deliver SubscribeAcknowledgment for {registration_id}");
                }
            }

            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                let span =
                    trace_span!("broker.handle_unsubscribe_request", %registration_id, %topic);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("Broker received unsubscribe request");

                // Ask TopicManager to remove the session from the topic (if exists).
                if let Some(topic_mgr) = &state.topic_mgr {
                    if let Err(e) = topic_mgr.send_message(BrokerMessage::UnsubscribeRequest {
                        registration_id: registration_id.clone(),
                        topic: topic.clone(),
                        trace_ctx: Some(span.context()),
                    }) {
                        warn!("Failed to forward UnsubscribeRequest to TopicManager: {e:?}");
                    }
                } else {
                    warn!("TopicManager missing while handling unsubscribe for {topic}");
                }

                // Tell SubscriberManager to clean up subscriber resources.
                if let Some(subscriber_mgr) = &state.subscriber_mgr {
                    if let Err(e) = subscriber_mgr.send_message(BrokerMessage::UnsubscribeRequest {
                        registration_id: registration_id.clone(),
                        topic: topic.clone(),
                        trace_ctx: Some(span.context()),
                    }) {
                        warn!("Failed to forward UnsubscribeRequest to SubscriberManager: {e:?}");
                    }
                } else {
                    warn!("SubscriberManager missing while handling unsubscribe for {topic}");
                }
            }
            BrokerMessage::PublishRequest {
                registration_id,
                topic,
                payload,
                trace_ctx,
            } => {
                let span = trace_span!("broker.handle_publish_request");
                if let Some(ctx) = trace_ctx {
                    span.set_parent(ctx).ok();
                }
                let _enter = span.enter();

                trace!(
                    "Broker received publish request from {registration_id} for topic \"{topic}\""
                );

                // Forward publish intent to TopicManager. TopicManager is responsible for ensuring the
                // topic exists and routing the message into the topic actor (idempotently).
                if let Some(topic_mgr) = &state.topic_mgr {
                    if let Err(e) = topic_mgr.send_message(BrokerMessage::PublishRequest {
                        registration_id: registration_id.clone(),
                        topic: topic.clone(),
                        payload: payload.clone(),
                        trace_ctx: Some(span.context()),
                    }) {
                        warn!("Failed to forward PublishRequest to TopicManager: {e:?}");
                        // Let session manager know publish failed so it can respond to client.
                        if let Some(session_mgr) = &state.session_mgr {
                            if let Err(e2) =
                                session_mgr.send_message(BrokerMessage::PublishResponse {
                                    topic: topic.clone(),
                                    payload: payload.clone(),
                                    result: Err(format!("forward failed: {e:?}")),
                                    trace_ctx: Some(span.context()),
                                })
                            {
                                warn!("Failed to notify SessionManager of publish failure: {e2:?}");
                            }
                        }
                    }
                } else {
                    error!("TopicManager missing; cannot publish to topic \"{topic}\"");
                    if let Some(session_mgr) = &state.session_mgr {
                        let _ = session_mgr.send_message(BrokerMessage::PublishResponse {
                            topic: topic.clone(),
                            payload: payload.clone(),
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
                let span = trace_span!("broker.handle_publish_response");
                if let Some(ctx) = trace_ctx {
                    span.set_parent(ctx).ok();
                }
                let _enter = span.enter();

                // Forward publish responses into the subscriber manager which handles fanout.
                if let Some(sub_mgr) = &state.subscriber_mgr {
                    if let Err(e) = sub_mgr.send_message(BrokerMessage::PublishResponse {
                        topic,
                        payload,
                        result,
                        trace_ctx: Some(span.context()),
                    }) {
                        error!("Failed to forward PublishResponse to SubscriberManager: {e:?}");
                    }
                } else {
                    error!("SubscriberManager missing; cannot forward PublishResponse");
                }
            }

            BrokerMessage::ErrorMessage { error, .. } => {
                warn!("Error Received: {error}");
            }

            BrokerMessage::DisconnectRequest {
                reason,
                client_id,
                registration_id,
                trace_ctx,
            } => {
                let span = trace_span!("broker.handle_disconnect_request", %client_id);
                if let Some(ctx) = trace_ctx {
                    span.set_parent(ctx).ok();
                }
                let _enter = span.enter();

                debug!("Cleaning up session {registration_id:?}");

                if let Some(subscriber_mgr) = &state.subscriber_mgr {
                    if let Err(e) = subscriber_mgr.send_message(BrokerMessage::DisconnectRequest {
                        reason: reason.clone(),
                        client_id: client_id.clone(),
                        registration_id: registration_id.clone(),
                        trace_ctx: Some(span.context()),
                    }) {
                        error!("Failed to forward DisconnectRequest to SubscriberManager: {e:?}");
                    }
                } else {
                    error!("SubscriberManager missing while processing disconnect for {client_id}");
                }

                if let Some(listener_mgr) = &state.listener_mgr {
                    if let Err(e) = listener_mgr.send_message(BrokerMessage::DisconnectRequest {
                        reason,
                        client_id: client_id.clone(),
                        registration_id,
                        trace_ctx: Some(span.context()),
                    }) {
                        error!("Failed to forward DisconnectRequest to ListenerManager: {e:?}");
                    }
                } else {
                    error!("ListenerManager missing while processing disconnect for {client_id}");
                }
            }

            BrokerMessage::TimeoutMessage {
                client_id,
                registration_id,
                error,
            } => {
                // Route timeout cleanup to subscriber manager which owns subscriber lifecycle.
                if let Some(sub_mgr) = &state.subscriber_mgr {
                    if let Err(e) = sub_mgr.send_message(BrokerMessage::TimeoutMessage {
                        client_id: client_id.clone(),
                        registration_id: registration_id.clone(),
                        error: error.clone(),
                    }) {
                        error!("Failed to forward TimeoutMessage to SubscriberManager: {e:?}");
                    }
                } else {
                    error!(
                        "SubscriberManager missing while handling timeout for {registration_id:?}"
                    );
                }
            }
            BrokerMessage::ControlRequest {
                registration_id,
                op,
                reply_to,
                trace_ctx,
            } => {
                let span = trace_span!("broker.handle_control_request", %registration_id);
                if let Some(ctx) = trace_ctx {
                    span.set_parent(ctx).ok();
                }
                let _enter = span.enter();
                trace!("Handling control request");
                if let Some(control_mgr) = &state.control_mgr {
                    control_mgr
                        .send_message(BrokerMessage::ControlRequest {
                            registration_id,
                            op,
                            reply_to,
                            trace_ctx: Some(span.context()),
                        })
                        .ok();
                }
            }
            other => {
                warn!(UNEXPECTED_MESSAGE_STR);
                debug!("Dropped broker message: {:?}", other);
            }
        }

        Ok(())
    }
}
