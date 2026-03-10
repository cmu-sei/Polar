use crate::UNEXPECTED_MESSAGE_STR;
use crate::{
    get_subscriber_name, CLIENT_NOT_FOUND_TXT, DISCONNECTED_REASON, REGISTRATION_REQ_FAILED_TXT,
    SESSION_NOT_FOUND_TXT, SUBSCRIBE_REQUEST_FAILED_TXT, TIMEOUT_REASON,
};
use cassini_types::{BrokerMessage, ShutdownPhase};
use ractor::{
    registry::where_is, Actor, ActorProcessingErr, ActorRef, RpcReplyPort, SupervisionEvent,
    rpc::CallResult,
};
use std::collections::{VecDeque, HashMap};
use std::{sync::Arc, time::Duration};
use tracing::{debug, error, info, trace, trace_span, warn, Instrument};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use cassini_tracing::try_set_parent_otel;

pub const SUBSCRIBER_NOT_FOUND_TXT: &str = "Subscriber not found!";

/// Our supervisor for the subscribers
/// When a user subscribes to a new topic, this actor is notified inb conjunction with the topic manager.
/// A new process is started to wait and listen for new messages on that topic and forward messages.
/// Clients are only considered subscribed if an actor process exists and is managed by this actor
pub struct SubscriberManager;

pub struct SubscriberManagerArgs {
    /// Reference to the session manager, used to forward unsubscribe acknowledgments.
    pub session_mgr: ActorRef<BrokerMessage>,
}

pub struct SubscriberManagerState {
    subscribers: HashMap<String, ActorRef<BrokerMessage>>,
    session_mgr: ActorRef<BrokerMessage>,
    is_shutting_down: bool,
}

// ============================== SubscriberManager Actor ============================== //

#[ractor::async_trait]
impl Actor for SubscriberManager {
    type Msg = BrokerMessage;
    type State = SubscriberManagerState;
    type Arguments = SubscriberManagerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: SubscriberManagerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::debug!("{myself:?} starting");
        Ok(SubscriberManagerState {
            subscribers: HashMap::new(),
            session_mgr: args.session_mgr,
            is_shutting_down: false,
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::debug!("{myself:?} Started");
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<BrokerMessage>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BrokerMessage::InitiateShutdownPhase { phase } if phase == ShutdownPhase::TerminateSubscribers => {
                self.handle_initiate_shutdown_phase(myself.clone(), state).await?;
            }
            BrokerMessage::RegistrationRequest { registration_id, client_id, trace_ctx } => {
                self.handle_registration_request(myself.clone(), state, registration_id, client_id, trace_ctx).await?;
            }
            BrokerMessage::CreateSubscriber { registration_id, topic, trace_ctx, reply } => {
                self.handle_create_subscriber(myself.clone(), state, registration_id, topic, trace_ctx, reply).await?;
            }
            BrokerMessage::UnsubscribeRequest { registration_id, topic, trace_ctx } => {
                self.handle_unsubscribe_request(myself.clone(), state, registration_id, topic, trace_ctx).await?;
            }
            BrokerMessage::DisconnectRequest { client_id, registration_id, trace_ctx, .. } => {
                self.handle_disconnect_request(myself.clone(), state, client_id, registration_id, trace_ctx).await?;
            }
            BrokerMessage::TimeoutMessage { registration_id, .. } => {
                self.handle_timeout_message(myself.clone(), state, registration_id).await?;
            }
            other => warn!(?other, "{UNEXPECTED_MESSAGE_STR}"),
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<BrokerMessage>,
        msg: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                debug!(
                    "Subscription ended for session {0:?}, {reason:?}",
                    actor_cell.get_name()
                );
                self.handle_actor_terminated(myself.clone(), state).await?;
            }
            SupervisionEvent::ActorFailed(actor_cell, error) => {
                self.handle_actor_failed(actor_cell, error).await?;
            }
            SupervisionEvent::ProcessGroupChanged(..) => (),
        }
        Ok(())
    }
}

impl SubscriberManager {
    /// Removes all subscriptions for a given session
    fn forget_subscriptions(
        registration_id: String,
        myself: ActorRef<BrokerMessage>,
        reason: Option<String>,
    ) {
        let mut stopped = 0usize;
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

    // ===== Handler methods =====
    async fn handle_initiate_shutdown_phase(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
    ) -> Result<(), ActorProcessingErr> {
        info!("Starting termination of subscriber agents");
        state.is_shutting_down = true;

        let children: Vec<_> = myself.get_children();

        for subscriber in &children {
            let _ = subscriber.send_message(BrokerMessage::PrepareForShutdown { auth_token: None });
            subscriber.stop(Some("SHUTDOWN_TERMINATE".to_string()));
        }

        if children.is_empty() {
            self.signal_terminate_complete(myself.clone(), state).await?;
        }
        Ok(())
    }

    async fn handle_registration_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
        registration_id: Option<String>,
        client_id: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        // Reject new registrations during shutdown
        if state.is_shutting_down {
            warn!("Rejecting registration request during shutdown termination phase");
            return Ok(());
        }

        let span = trace_span!("cassini.subscriber_manager.handle_registration_request", %client_id, has_registration_id = registration_id.is_some());
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("Subscriber manager received registration request for client {client_id}");

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
                    if let Err(e) = subscriber.send_message(BrokerMessage::RegistrationRequest {
                        registration_id: Some(registration_id.clone()),
                        client_id: client_id.clone(),
                        trace_ctx: Some(span.context()),
                    }) {
                        warn!(error = %e, "{REGISTRATION_REQ_FAILED_TXT}: {SUBSCRIBER_NOT_FOUND_TXT}");
                    }
                }
            } else {
                let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
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
        Ok(())
    }

    async fn handle_create_subscriber(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
        reply: RpcReplyPort<Result<ActorRef<BrokerMessage>, ActorProcessingErr>>,
    ) -> Result<(), ActorProcessingErr> {
        if state.is_shutting_down {
            warn!("Rejecting subscriber creation during shutdown");
            let _ = reply.send(Err(ActorProcessingErr::from("Broker is shutting down")));
            return Ok(());
        }

        let span = trace_span!("cassini.subscriber_manager.create_subscriber", %registration_id, %topic);
        try_set_parent_otel(&span, trace_ctx);
        trace!("Subscriber manager received subscribe command");

        let subscriber_id = get_subscriber_name(&registration_id, &topic);

        // Get the session ref from the session manager
        let session_ref = match state.session_mgr
            .call(
                |reply_to| BrokerMessage::GetSessionRef {
                    registration_id: registration_id.clone(),
                    reply_to,
                },
                Some(Duration::from_millis(100)),
            )
            .await
        {
            Ok(CallResult::Success(Some(ref_))) => ref_,
            Ok(CallResult::Success(None)) => {
                let err_msg = "Session not found".to_string();
                let _ = reply.send(Err(ActorProcessingErr::from(err_msg.clone())));
                return Err(ActorProcessingErr::from(err_msg));
            }
            _ => {
                let err_msg = "Failed to get session ref".to_string();
                let _ = reply.send(Err(ActorProcessingErr::from(err_msg.clone())));
                return Err(ActorProcessingErr::from(err_msg));
            }
        };

        match Actor::spawn_linked(
            Some(subscriber_id.clone()),
            SubscriberAgent,
            SubscriberAgentArgs { session: session_ref.clone() },
            myself.clone().into(),
        )
        .instrument(span.clone())
        .await
        {
            Ok((subscriber, _)) => {
                state.subscribers.insert(subscriber_id, subscriber.clone());
                let _ = reply.send(Ok(subscriber));
            }
            Err(e) => {
                let err_msg = format!("Failed to spawn subscriber actor. {e}");
                let _ = reply.send(Err(ActorProcessingErr::from(err_msg)));
            }
        }
        Ok(())
    }

    async fn handle_unsubscribe_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.subscriber_manager.unsubscribe_request", %registration_id, %topic);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("subscriber manager received unsubscribe request");

        let subscriber_name = format!("{registration_id}:{topic}");
        if let Some(subscriber) = state.subscribers.get(&subscriber_name) {
            subscriber.stop(Some("UNSUBSCRIBED".to_string()));
            // Forward acknowledgment via session manager
            if let Err(e) = state.session_mgr.send_message(BrokerMessage::UnsubscribeAcknowledgment {
                    registration_id,
                    topic,
                    result: Ok(()),
                }) {
                warn!(error = %e, "Failed to send unsubscribe ack to session manager");
            }
        } else {
            warn!("Session agent {registration_id} not subscribed to topic {topic}");
        }
        Ok(())
    }

    async fn handle_disconnect_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
        client_id: String,
        registration_id: Option<String>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.subscriber_manager.disconnect_request", %client_id, has_registration_id = registration_id.is_some());
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();

        if let Some(registration_id) = registration_id {
            SubscriberManager::forget_subscriptions(
                registration_id,
                myself.clone(),
                Some(DISCONNECTED_REASON.to_string()),
            );

            if state.is_shutting_down {
                self.check_terminate_complete(myself.clone(), state).await?;
            }
        } else {
            warn!("Failed to process disconnect request! registration_id missing.")
        }
        Ok(())
    }

    async fn handle_timeout_message(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
        registration_id: String,
    ) -> Result<(), ActorProcessingErr> {
        SubscriberManager::forget_subscriptions(
            registration_id,
            myself.clone(),
            Some(TIMEOUT_REASON.to_string()),
        );
        if state.is_shutting_down {
            self.check_terminate_complete(myself.clone(), state).await?;
        }
        Ok(())
    }

    async fn handle_actor_terminated(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
    ) -> Result<(), ActorProcessingErr> {
        if state.is_shutting_down {
            self.check_terminate_complete(myself.clone(), state).await?;
        }
        Ok(())
    }

    async fn handle_actor_failed(
        &self,
        actor_cell: ractor::ActorCell,
        error: Box<dyn std::error::Error + Send + Sync>,
    ) -> Result<(), ActorProcessingErr> {
        error!(
            "Subscriber actor {} ({:?}) failed unexpectedly: {}",
            actor_cell.get_name().unwrap_or("<unnamed>".to_string()),
            actor_cell.get_id(),
            error
        );
        Ok(())
    }

    async fn check_terminate_complete(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
    ) -> Result<(), ActorProcessingErr> {
        if state.is_shutting_down && myself.get_children().is_empty() {
            self.signal_terminate_complete(myself.clone(), state).await?;
        }
        Ok(())
    }

    async fn signal_terminate_complete(
        &self,
        myself: ActorRef<BrokerMessage>,
        _state: &mut SubscriberManagerState,
    ) -> Result<(), ActorProcessingErr> {
        info!("All subscriber agents terminated");

        if let Some(supervisor) = myself.try_get_supervisor() {
            supervisor.send_message(BrokerMessage::ShutdownPhaseComplete {
                phase: ShutdownPhase::TerminateSubscribers,
            })?;
        }

        myself.stop(Some("SHUTDOWN_TERMINATED".to_string()));
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
    // session_ref is now stored directly
    session_ref: ActorRef<BrokerMessage>,
    topic: String,
    dead_letter_queue: VecDeque<Arc<Vec<u8>>>,
}

pub struct SubscriberAgentArgs {
    pub session: ActorRef<BrokerMessage>,
}

#[ractor::async_trait]
impl Actor for SubscriberAgent {
    type Msg = BrokerMessage;
    type State = SubscriberAgentState;
    type Arguments = SubscriberAgentArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: SubscriberAgentArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let name = myself.get_name().ok_or_else(|| {
            ActorProcessingErr::from(format!(
                "{SUBSCRIBE_REQUEST_FAILED_TXT}: Expected subscriber to have name"
            ))
        })?;

        if let Some((registration_id, topic)) = name.split_once(':') {
            Ok(SubscriberAgentState {
                registration_id: registration_id.to_string(),
                session_ref: args.session,
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
            BrokerMessage::RegistrationRequest { registration_id, client_id, trace_ctx } => {
                self.handle_registration_request(myself.clone(), state, registration_id, client_id, trace_ctx).await?;
            }
            BrokerMessage::PublishResponse { topic, payload, trace_ctx, .. } => {
                self.handle_publish_response(myself.clone(), state, topic, payload, trace_ctx).await?;
            }
            BrokerMessage::PushMessageFailed { payload } => {
                self.handle_push_message_failed(myself.clone(), state, payload).await?;
            }
            other => warn!(?other, "{UNEXPECTED_MESSAGE_STR}"),
        }
        Ok(())
    }
}

impl SubscriberAgent {
    // ===== Handler methods for SubscriberAgent =====
    async fn handle_registration_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberAgentState,
        registration_id: Option<String>,
        client_id: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.subscriber.handle_registration_request", %client_id, has_registration_id = registration_id.is_some());
        try_set_parent_otel(&span, trace_ctx);
        let _enter = span.enter();

        trace!("Subscriber actor received registration request for client {client_id}");

        if let Some(id) = registration_id {
            // Reconnection: forward missed messages to session (which we already have)
            let session = state.session_ref.clone();
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
        Ok(())
    }

    async fn handle_publish_response(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberAgentState,
        topic: String,
        payload: Arc<Vec<u8>>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.subscriber.publish_response", %topic);
        try_set_parent_otel(&span, trace_ctx);
        let _enter = span.enter();
        trace!("Subscriber actor received publish response for topic \"{topic}\"");
        debug!("New message on topic: \"{topic}\", forwarding to session: {}", state.registration_id);

        let session = state.session_ref.clone();
        if let Err(e) = session.send_message(BrokerMessage::PushMessage {
            payload: payload.clone(),
            topic,
            trace_ctx: Some(span.context()),
        }) {
            warn!("Failed to forward message to subscriber! {e} Ending subscription");
            myself.stop(None);
            return Ok(());
        }
        Ok(())
    }

    async fn handle_push_message_failed(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberAgentState,
        payload: Arc<Vec<u8>>,
    ) -> Result<(), ActorProcessingErr> {
        state.dead_letter_queue.push_back(payload.clone());
        debug!(
            "Subscriber queue has {} message(s) waiting",
            state.dead_letter_queue.len()
        );
        Ok(())
    }
}
