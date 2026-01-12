use crate::{get_subscriber_name, BROKER_NAME, UNEXPECTED_MESSAGE_STR};
use crate::{
    BROKER_NOT_FOUND_TXT, CLIENT_NOT_FOUND_TXT, PUBLISH_REQ_FAILED_TXT,
    REGISTRATION_REQ_FAILED_TXT, SUBSCRIBE_REQUEST_FAILED_TXT, TIMEOUT_REASON,
};
use cassini_types::{BrokerMessage, ControlError, SessionDetails};
use ractor::{
    async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef, SupervisionEvent,
};
use std::collections::{HashMap, HashSet};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, trace_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use uuid::Uuid;

/// The manager process for our concept of client sessions.
/// When thje broker receives word of a new connection from the listenerManager, it requests
/// that the client be "registered". Signifying the client as legitimate.
pub struct SessionManager;

///Private representation of a connected session, and how close it is to timing out
/// Exposes actor references, so we keep that to ourselves here
/// Consider stuff like last_activity_time, last_message_received_time, etc.
struct Session {
    agent_ref: ActorRef<BrokerMessage>,
    subscriptions: HashSet<String>,
    // last_activity: u64,
}

/// Define the state for the actor
pub struct SessionManagerState {
    /// Map of registration_id to Session ActorRefes
    sessions: HashMap<String, Session>,
    /// Amount of time (in seconds) that can pass before a session counts as expired
    session_timeout: u64,
    /// Collection of tokens used to cancel the thread that cleans up expired sessions.
    cancellation_tokens: HashMap<String, CancellationToken>,
}

pub struct SessionManagerArgs {
    pub session_timeout: u64,
}

/// Message to set the TopicManager ref in SessionManager or other actors.
#[derive(Debug)]
pub struct SetTopicManagerRef(pub ActorRef<BrokerMessage>);

/// Message to set the ListenerManager ref in other actors.
#[derive(Debug)]
pub struct SetListenerManagerActorRef(pub ActorRef<BrokerMessage>);

#[async_trait]
impl Actor for SessionManager {
    type Msg = BrokerMessage; // Messages this actor handles
    type State = SessionManagerState; // Internal state
    type Arguments = SessionManagerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: SessionManagerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        //parse args. if any
        let state = SessionManagerState {
            sessions: HashMap::new(),
            cancellation_tokens: HashMap::new(),
            session_timeout: args.session_timeout,
        };

        Ok(state)
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
            BrokerMessage::RegistrationRequest {
                client_id,
                trace_ctx,
                ..
            } => {
                let span =
                    tracing::trace_span!("session_manager.handle_registration_request", %client_id);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("Session manager received registration request");

                // Start a brand new session: Create an ID. Spawn a new session
                // to store the ID. If for some reason we can't create a new
                // session, we kick the failure back to the client's listener
                // directly, so that they can handle the error.
                let new_id = Uuid::new_v4().to_string();

                info!("Starting session for client: {client_id} with registration ID {new_id}");

                //find client, attach its reference to new session args
                if let Some(listener_ref) = where_is(client_id.clone()) {
                    // drop the span guard before await call
                    drop(_g);
                    //start new session
                    if let Ok((session_agent, _)) = Actor::spawn_linked(
                        Some(new_id.clone()),
                        SessionAgent,
                        SessionAgentArgs {
                            client_ref: listener_ref.clone().into(),
                        },
                        myself.clone().into(),
                    )
                    .await
                    {
                        let success_span = tracing::trace_span!(
                            "session_manager.handle_registration_request",
                            client_id = client_id
                        );
                        success_span.set_parent(span.context()).ok();
                        let _g = success_span.enter();

                        trace!("initializing session.");
                        // trace!("Session manager successfully started new session actor.");

                        state.sessions.insert(
                            new_id.clone(),
                            Session {
                                agent_ref: session_agent.clone(),
                                subscriptions: HashSet::new(),
                            },
                        );

                        // send init
                        //
                        session_agent
                            .cast(BrokerMessage::InitSession {
                                client_id: client_id,
                                trace_ctx: Some(success_span.context()),
                            })
                            .ok();
                    }
                } else {
                    // LOL they didn't stick around very long!
                    warn!("{REGISTRATION_REQ_FAILED_TXT} {CLIENT_NOT_FOUND_TXT}")
                }
            }
            BrokerMessage::RegistrationResponse {
                result, trace_ctx, ..
            } => {
                //A client has (re)registered. Cancel timeout thread
                let span = trace_span!("Session manager received registration response.");
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _ = span.enter();

                trace!("Session manager received registration response message.");

                result
                    .map(|registration_id| {
                        // Find the given session by its id and cancel the cleanup task for it
                        where_is(registration_id.clone()).map(|_| {
                            if let Some(token) = &state.cancellation_tokens.get(&registration_id) {
                                debug!("Aborting session cleanup for {registration_id}");
                                token.cancel();
                                state.cancellation_tokens.remove(&registration_id);
                            }
                        });
                    })
                    .ok();
            }
            BrokerMessage::GetSessions {
                reply_to,
                trace_ctx,
            } => {
                //A client has (re)registered. Cancel timeout thread
                let span = trace_span!("Session manager received GetSessions directive.");
                trace_ctx.map(|ctx| span.set_parent(ctx).ok());
                let _ = span.enter();

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
                    warn!("Failed to send session data to controller.");
                }
            }
            BrokerMessage::DisconnectRequest {
                client_id,
                registration_id,
                trace_ctx,
            } => {
                let span = trace_span!("session_manager.handle_disconnect_request", %client_id);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("session manager recieved disconnect request");
                //forward to broker, kill session
                // if we can't find the sesion for some reason, all the better.
                registration_id.map(|registration_id| {
                    match where_is(registration_id.clone()) {
                        Some(session) => session.stop(Some("CLIENT_DISCONNECTED".to_owned())),
                        None => warn!("Failed to find session {registration_id}"),
                    }

                    match myself.try_get_supervisor() {
                        Some(broker) => broker
                            .send_message(BrokerMessage::DisconnectRequest {
                                client_id,
                                registration_id: Some(registration_id),
                                trace_ctx: Some(span.context()),
                            })
                            .expect("Expected to forward message"),

                        None => {
                            error!("Failed to find supervisor");
                            myself.stop(None);
                        }
                    }
                });
            }
            BrokerMessage::TimeoutMessage {
                client_id,
                registration_id,
                error,
            } => {
                // let span = trace_span!("session_manager.handle_timeout", %client_id);
                // trace_ctx.map(|ctx| span.set_parent(ctx));
                // let _g = span.enter();

                trace!("session manager recoieved disconnect request");

                if let Some(session) = state.sessions.get(&registration_id) {
                    warn!("Session {registration_id} timing out, waiting for reconnect...");
                    let ref_clone = session.agent_ref.clone();
                    let token = CancellationToken::new();

                    state
                        .cancellation_tokens
                        .insert(registration_id.clone(), token.clone());
                    let timeout = state.session_timeout.clone();
                    let _ = tokio::spawn(async move {
                        tokio::select! {
                            // Use cloned token to listen to cancellation requests
                            _ = token.cancelled() => {
                                // The timer was cancelled, task can shut down
                            }
                            // wait before killing session
                            _ = tokio::time::sleep(std::time::Duration::from_secs(timeout)) => {
                                //tell the broker a session has timed out so it let's other actors know
                                match myself.try_get_supervisor() {
                                    Some(manager) => {
                                        info!("Ending session {ref_clone:?}");
                                        manager.send_message(BrokerMessage::TimeoutMessage {
                                            client_id,
                                            registration_id,
                                            error })
                                            .expect("Expected to forward to manager")
                                    }

                                    None => warn!("Could not find broker supervisor!")
                                }
                                // stop session
                                ref_clone.stop(Some(TIMEOUT_REASON.to_string()));

                            }
                        }
                    });
                }
            }
            BrokerMessage::SubscribeAcknowledgment {
                registration_id,
                topic,
                trace_ctx,
                ..
            } => {
                let span = trace_span!("session.handle_subscribe_ack", %registration_id);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _ = span.enter();

                if let Some(session) = state.sessions.get_mut(&registration_id) {
                    session.subscriptions.insert(topic.clone());
                    session
                        .agent_ref
                        .send_message(BrokerMessage::SubscribeAcknowledgment {
                            registration_id,
                            topic,
                            trace_ctx: Some(span.context()),
                            result: Ok(()),
                        })
                        .map_err(|error| {
                            warn!("Failed to send subscribe acknowledgment: {error}");
                        })
                        .ok();
                } else {
                    warn!("Could not find session for registration ID: {registration_id}");
                }
            }
            _ => {
                warn!("Received unexpected message: {message:?}")
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
    /// Reference to the listener for the session
    pub client_ref: ActorRef<BrokerMessage>,
}
pub struct SessionAgentState {
    pub client_ref: ActorRef<BrokerMessage>,
}

#[async_trait]
impl Actor for SessionAgent {
    type Msg = BrokerMessage; // Messages this actor handles
    type State = SessionAgentState; // Internal state
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
            BrokerMessage::InitSession {
                trace_ctx,
                client_id,
            } => {
                let span = trace_span!("session.init", client_id = client_id);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _ = span.enter();

                trace!("Session {myself:?} received init message.");

                //send ack to client listener
                state
                    .client_ref
                    .cast(BrokerMessage::RegistrationResponse {
                        client_id,
                        result: Ok(myself
                            .get_name()
                            .expect("Expected session to have been named with a registration ID")),
                        trace_ctx: Some(span.context()),
                    })
                    .ok();
            }
            BrokerMessage::RegistrationRequest {
                client_id,
                trace_ctx,
                ..
            } => {
                let span = trace_span!("session.handle_registration_request", %client_id);
                if let Some(ctx) = trace_ctx.clone() {
                    span.set_parent(ctx.clone()).ok();
                };
                let _ = span.enter();

                trace!(
                    "Session {} received registration request from client {client_id}",
                    myself.get_name().unwrap()
                );

                // A client reestablished their session, update state to send messages to new listener actor
                match where_is(client_id.clone()) {
                    Some(listener) => {
                        state.client_ref = ActorRef::from(listener);
                        //ack
                        //
                        info!("Re-established comms with client {client_id}");
                        state
                            .client_ref
                            .send_message(BrokerMessage::RegistrationResponse {
                                client_id: client_id.clone(),
                                result: Ok(myself.get_name().unwrap()),
                                trace_ctx: trace_ctx.clone(),
                            })
                            .expect("Expected to send ack to listener");
                        // Alert session manager we've got our listener so it doesn't potentially kill it
                        match myself.try_get_supervisor() {
                            Some(manager) => {
                                //continue registration

                                manager
                                    .send_message(BrokerMessage::RegistrationResponse {
                                        client_id: client_id.clone(),
                                        result: Ok(myself.get_name().unwrap()),
                                        trace_ctx: trace_ctx.clone(),
                                    })
                                    .map_err(|e| {
                                        let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT} {e}!");
                                        error!("{err_msg}");

                                        // write back to session and die with honor. No manager, no life
                                        // If this fails, so be it, we're already in a bad state.
                                        state
                                            .client_ref
                                            .send_message(BrokerMessage::RegistrationResponse {
                                                client_id: client_id.clone(),
                                                result: Err(err_msg.clone()),
                                                trace_ctx: trace_ctx.clone(),
                                            })
                                            .ok();

                                        myself.stop(Some(e.to_string()));
                                    })
                                    .ok();
                            }
                            None => warn!("Couldn't find supervisor for session agent!"),
                        }
                    }
                    None => {
                        warn!("Could not find listener for client: {client_id}");
                    }
                }
            }
            BrokerMessage::PublishRequest {
                registration_id,
                topic,
                payload,
                trace_ctx,
            } => {
                //forward to broker
                let span = trace_span!("session.handle_publish_request", %registration_id);
                trace_ctx.map(|ctx| span.set_parent(ctx).ok());
                let _enter = span.enter();

                trace!("Session {registration_id} received publish request for topic {topic}");

                where_is(BROKER_NAME.to_string()).map(|broker| {
                    if let Err(_e) = broker.send_message(BrokerMessage::PublishRequest {
                        registration_id: registration_id.clone(),
                        topic: topic.clone(),
                        payload: payload.clone(),
                        trace_ctx: Some(span.context()),
                    }) {
                        let err_msg = format!("{PUBLISH_REQ_FAILED_TXT} {BROKER_NOT_FOUND_TXT}");
                        if let Err(listener_err) =
                            state
                                .client_ref
                                .send_message(BrokerMessage::PublishResponse {
                                    topic,
                                    payload,
                                    result: Err(err_msg.clone()),
                                    trace_ctx: Some(span.context()),
                                })
                        {
                            error!("{err_msg}: {listener_err}");
                            // no listener? start timeout logic
                            if let Err(fwd_err) =
                                myself.send_message(BrokerMessage::TimeoutMessage {
                                    client_id: state.client_ref.get_name().unwrap_or_default(),
                                    registration_id,
                                    error: Some(format!("{err_msg} {listener_err}")),
                                })
                            {
                                error!("{err_msg}: {listener_err}: {fwd_err}");

                                drop(_enter);

                                myself
                                    .stop(Some("{err_msg}: {listener_err}: {fwd_err}".to_string()));
                            }
                        }
                    }
                });
            }
            BrokerMessage::PushMessage {
                payload,
                topic,
                trace_ctx,
            } => {
                let span = trace_span!("session.dequeue_messages");
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _enter = span.enter();

                trace!("Session actor received push message directive for session {} for topic \"{topic}\"", myself.get_name().unwrap());
                //push to client
                if let Err(_e) = state.client_ref.cast(BrokerMessage::PublishResponse {
                    topic: topic.clone(),
                    payload: payload.clone(),
                    result: Ok(()),
                    trace_ctx: Some(span.context()),
                }) {
                    warn!("{PUBLISH_REQ_FAILED_TXT}: {CLIENT_NOT_FOUND_TXT}");
                    if let Some(subscriber) = where_is(get_subscriber_name(
                        &myself.get_name().expect("Expected session to be named."),
                        &topic,
                    )) {
                        // if we can't DLQ the message because the subscriber died, a client either unsubscribed, or it crashed.
                        // Either way, nothing we can do about it.

                        subscriber
                            .send_message(BrokerMessage::PushMessageFailed { payload })
                            .inspect_err(|_| warn!("Failed to DLQ message on topic {topic}"))
                            .ok();
                    }
                }
            }
            BrokerMessage::PublishRequestAck { topic, trace_ctx } => {
                let span = trace_span!("session.handle_publish_request");
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _enter = span.enter();

                trace!("Session received Publish request acknowledgement for topic \"{topic}\"");

                if let Err(e) = state
                    .client_ref
                    .send_message(BrokerMessage::PublishRequestAck {
                        topic,
                        trace_ctx: Some(span.context()),
                    })
                {
                    // no listener? start timeout logic
                    if let Err(fwd_err) = myself.send_message(BrokerMessage::TimeoutMessage {
                        client_id: state.client_ref.get_name().unwrap(),
                        registration_id: myself.get_name().unwrap(),
                        error: Some(format!("{e}")),
                    }) {
                        error!("{e}: {fwd_err}");
                        myself.stop(Some("{err_msg}: {listener_err}: {fwd_err}".to_string()));
                    }
                }
            }
            BrokerMessage::SubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                let span =
                    trace_span!("session.handle_subscribe_request", %registration_id ,%topic);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("Session agent recevied subscribe request.");

                where_is(BROKER_NAME.to_string()).inspect(|broker| {
                    if let Err(e) = broker.send_message(BrokerMessage::SubscribeRequest {
                        registration_id,
                        topic,
                        trace_ctx: Some(span.context()),
                    }) {
                        error!("{SUBSCRIBE_REQUEST_FAILED_TXT} {BROKER_NOT_FOUND_TXT}");

                        if let Err(fwd_err) = myself.send_message(BrokerMessage::TimeoutMessage {
                            client_id: state
                                .client_ref
                                .get_name()
                                .expect("Expected client to have been named with a client ID"),
                            registration_id: myself
                                .get_name()
                                .expect("Expected to have been named with a registration ID"),

                            error: Some(format!("{e}")),
                        }) {
                            error!("{e}: {fwd_err}");
                            myself.stop(Some("{err_msg}: {listener_err}: {fwd_err}".to_string()));
                        }
                    }
                });
            }

            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                let span =
                    trace_span!("session.handle_unsubscribe_request", %registration_id ,%topic);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("session received unsubscribe request");

                where_is(BROKER_NAME.to_string()).map(|broker| {
                    if let Err(e) = broker.send_message(BrokerMessage::UnsubscribeRequest {
                        registration_id,
                        topic,
                        trace_ctx: Some(span.context()),
                    }) {
                        if let Err(fwd_err) = myself.send_message(BrokerMessage::TimeoutMessage {
                            client_id: state.client_ref.get_name().unwrap_or_default(),
                            registration_id: myself
                                .get_name()
                                .expect("Expected to have been named with a registration ID"),

                            error: Some(format!("{e}")),
                        }) {
                            error!("{e}: {fwd_err}");
                            myself.stop(Some("{err_msg}: {listener_err}: {fwd_err}".to_string()));
                        }
                    }
                });
            }
            BrokerMessage::UnsubscribeAcknowledgment {
                registration_id,
                topic,
                result,
            } => {
                if let Err(e) =
                    state
                        .client_ref
                        .send_message(BrokerMessage::UnsubscribeAcknowledgment {
                            registration_id,
                            topic,
                            result,
                        })
                {
                    error!("Failed to forward message to client");

                    if let Err(fwd_err) = myself.send_message(BrokerMessage::TimeoutMessage {
                        client_id: state.client_ref.get_name().unwrap_or_default(),
                        registration_id: myself.get_name().unwrap(),
                        error: Some(format!("{e}")),
                    }) {
                        error!("{e}: {fwd_err}");
                        myself.stop(Some("{err_msg}: {listener_err}: {fwd_err}".to_string()));
                    }
                }
            }
            BrokerMessage::SubscribeAcknowledgment {
                registration_id,
                topic,
                result,
                trace_ctx,
            } => {
                let span =
                    trace_span!("session.handle_subscribe_request", %registration_id, %topic);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("Session received a subscribe acknowledgement message");

                // fwd a copy to the supervisor
                match myself.try_get_supervisor() {
                    Some(manager) => {
                        manager
                            .send_message(BrokerMessage::SubscribeAcknowledgment {
                                registration_id: registration_id.clone(),
                                topic: topic.clone(),
                                result: result.clone(),
                                trace_ctx: Some(span.context()),
                            })
                            .ok();

                        if let Err(e) =
                            state
                                .client_ref
                                .send_message(BrokerMessage::SubscribeAcknowledgment {
                                    registration_id,
                                    topic,
                                    result,
                                    trace_ctx: Some(span.context()),
                                })
                        {
                            if let Err(fwd_err) =
                                myself.send_message(BrokerMessage::TimeoutMessage {
                                    client_id: state.client_ref.get_name().expect(
                                        "Expected client to have been named with a client ID",
                                    ),
                                    registration_id: myself.get_name().unwrap_or_default(),
                                    error: Some(format!("{e}")),
                                })
                            {
                                error!("{e}: {fwd_err}");
                                myself
                                    .stop(Some("{err_msg}: {listener_err}: {fwd_err}".to_string()));
                            }
                        }
                    }
                    None => {
                        // orphaned
                        return Err(ActorProcessingErr::from("Failed to find supervisor "));
                    }
                }
            }
            BrokerMessage::DisconnectRequest {
                client_id,
                registration_id,
                trace_ctx,
            } => {
                //client disconnected, clean up after it then die with honor
                let span = trace_span!("session.handle_disconnect_request", %client_id);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("session received disconnect request.");
                info!("client {client_id} disconnected");
                match myself.try_get_supervisor() {
                    Some(manager) => manager
                        .send_message(BrokerMessage::DisconnectRequest {
                            client_id,
                            registration_id,
                            trace_ctx: Some(span.context()),
                        })
                        .expect("Expected to forward to manager"),
                    None => tracing::error!("Couldn't find supervisor."),
                }
            }
            BrokerMessage::TimeoutMessage {
                client_id,
                registration_id,
                error,
            } => match myself.try_get_supervisor() {
                Some(manager) => manager
                    .send_message(BrokerMessage::TimeoutMessage {
                        client_id,
                        registration_id,
                        error,
                    })
                    .expect("Expected to forward to manager"),
                None => error!("Couldn't find supervisor."),
            },

            BrokerMessage::ControlRequest {
                registration_id,
                op,
                reply_to,
                trace_ctx,
            } => {
                let span = trace_span!("session.handle_control_request", %registration_id);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("Received Control Request");
                if let Some(broker) = myself.try_get_supervisor() {
                    if let Err(e) = broker.send_message(BrokerMessage::ControlRequest {
                        registration_id: registration_id.clone(),
                        op,
                        reply_to: Some(myself.clone()), //pass in a ref to self so we can get a response back
                        trace_ctx: Some(span.context()),
                    }) {
                        error!("{e}");
                        state
                            .client_ref
                            .send_message(BrokerMessage::ControlResponse {
                                registration_id,
                                result: Err(ControlError::InternalError(format!(
                                    "Failed to process message. {e}"
                                ))),
                                trace_ctx: Some(span.context()),
                            })
                            .ok();
                    }
                }
            }
            BrokerMessage::ControlResponse {
                registration_id,
                result,
                trace_ctx,
            } => {
                let span = trace_span!("session.handle_control_request", %registration_id);
                trace_ctx.map(|ctx| span.set_parent(ctx));
                let _g = span.enter();

                trace!("recieved ControlResponse");
                state
                    .client_ref
                    .send_message(BrokerMessage::ControlResponse {
                        registration_id,
                        result,
                        trace_ctx: Some(span.context()),
                    })
                    .ok();
            }
            _ => {
                warn!("{}", format!("{UNEXPECTED_MESSAGE_STR}: {message:?}"));
            }
        }
        Ok(())
    }
}
