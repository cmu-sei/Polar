use crate::UNEXPECTED_MESSAGE_STR;
use crate::{
    get_subscriber_name, BrokerMessage, CLIENT_NOT_FOUND_TXT, DISCONNECTED_REASON,
    PUBLISH_REQ_FAILED_TXT, REGISTRATION_REQ_FAILED_TXT, SESSION_NOT_FOUND_TXT,
    SUBSCRIBE_REQUEST_FAILED_TXT, TIMEOUT_REASON,
};
use ractor::rpc::{call, CallResult};
use ractor::{
    async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef, SupervisionEvent,
};
use std::collections::VecDeque;
use tracing::{debug, error, info, warn};

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
        //cleanup all subscriptions for session
        for subscriber in myself.get_children().into_iter().filter(|cell| {
            let sub_name = cell.get_name().unwrap();
            let sub_id = sub_name.split_once(":").unwrap().0;
            registration_id.eq(sub_id)
        }) {
            subscriber.stop(reason.clone());
        }
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
            } => {
                //find all subscribers for a given registration id
                registration_id.map(|registration_id: String| {
                    tracing::info!("Gathering Subscriptions for session {registration_id}");
                    if let Some(_) = where_is(registration_id.clone()) {
                        let cloned_session_id = registration_id.clone();
                        for subscriber in myself.get_children()
                        .into_iter()
                        .filter(|cell|{
                            let sub_name = cell.get_name().unwrap();
                            let sub_id = sub_name.split_once(":").unwrap().0;
                            cloned_session_id.eq(sub_id)
                            }) {
                            //notify so they dump unsent messages
                            if let Err(e) = subscriber.send_message(BrokerMessage::RegistrationRequest { registration_id: Some(registration_id.clone()), client_id: client_id.clone()}) {
                                warn!("{REGISTRATION_REQ_FAILED_TXT}: {SUBSCRIBER_NOT_FOUND_TXT}: {e}")
                            }
                        }
                    } else {
                        let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
                        warn!("{err_msg}");

                        if let Some(listener) = where_is(client_id.clone()) {
                            listener.send_message(BrokerMessage::RegistrationResponse { registration_id: Some(registration_id.clone()), client_id: client_id.clone(), success: false, error: Some(err_msg.clone()) })
                            .map_err(|e| {
                                warn!("{err_msg}: {e}");
                            }).unwrap()
                        }
                        else { error!("{err_msg}: {CLIENT_NOT_FOUND_TXT}"); }
                    }
                });
            }
            BrokerMessage::Subscribe {
                reply,
                registration_id,
                topic,
            } => {
                let subscriber_id = get_subscriber_name(&registration_id, &topic);
                // start new subscriber actor for session
                match Actor::spawn_linked(
                    Some(subscriber_id.clone()),
                    SubscriberAgent,
                    (),
                    myself.clone().into(),
                )
                .await
                {
                    Ok(_) => {
                        if let Err(e) = reply.send(Ok(subscriber_id)) {
                            error!("{SUBSCRIBE_REQUEST_FAILED_TXT}, {e}");
                            myself.stop(None);
                        }
                    }
                    Err(e) => {
                        if let Err(e) = reply.send(Err(e.to_string())) {
                            error!("{SUBSCRIBE_REQUEST_FAILED_TXT}, Couldn't communicate with broker, {e}");
                            myself.stop(None);
                        }
                    }
                }
            }

            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
            } => {
                match registration_id {
                    Some(id) => {
                        let subscriber_name = format!("{id}:{topic}");
                        if let Some(subscriber) = where_is(subscriber_name.clone()) {
                            subscriber.stop(Some("UNSUBSCRIBED".to_string()));
                            //send ack
                            let id_clone = id.clone();
                            where_is(id).map_or_else(
                                || error!("Could not find session for client: {id_clone}"),
                                |session| {
                                    session
                                        .send_message(BrokerMessage::UnsubscribeAcknowledgment {
                                            registration_id: id_clone.clone(),
                                            topic,
                                            result: Ok(()),
                                        })
                                        .expect("expected to send ack to session");
                                },
                            );
                        } else {
                            warn!("Session agent {id} not subscribed to topic {topic}");
                            //TODO: determine naming convention for subscriber agents?
                            // session_id:topic?
                        }
                    }
                    None => todo!(),
                }
            }
            BrokerMessage::DisconnectRequest {
                registration_id, ..
            } => {
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
            BrokerMessage::TimeoutMessage {
                registration_id, ..
            } => {
                if let Some(registration_id) = registration_id {
                    SubscriberManager::forget_subscriptions(
                        registration_id,
                        myself.clone(),
                        Some(TIMEOUT_REASON.to_string()),
                    );
                } else {
                    warn!(" Failed to process timeout request! registration_id missing!")
                }
            }
            _ => warn!(UNEXPECTED_MESSAGE_STR, "{message:?}"),
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
        //parse args. if any
        let name = myself
            .get_name()
            .expect("{SUBSCRIBE_REQUEST_FAILED_TXT}: Expected subscriber to have name");
        if let Some((registration_id, topic)) = name.split_once(':') {
            Ok(SubscriberAgentState {
                registration_id: registration_id.to_string(),
                topic: topic.to_string(),
                dead_letter_queue: VecDeque::new(),
            })
        } else {
            panic!("{SUBSCRIBE_REQUEST_FAILED_TXT}: Bad name given");
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
                registration_id, ..
            } => {
                //A client reconnected, push messages built up in DLQ to client
                if let Some(id) = registration_id {
                    match where_is(id.clone()) {
                        Some(session) => {
                            info!("Forwarding missed messages to session: {id}");
                            while let Some(msg) = &state.dead_letter_queue.pop_front() {
                                match call(
                                    &session,
                                    |reply| BrokerMessage::PushMessage {
                                        reply,
                                        payload: msg.clone().to_vec(),
                                        topic: state.topic.clone(),
                                    },
                                    None,
                                )
                                .await
                                .expect("Expected to forward message to subscriber")
                                {
                                    CallResult::Success(result) => {
                                        if let Err(_) = result {
                                            //session couldn't talk to listener, add message to DLQ
                                            warn!("{REGISTRATION_REQ_FAILED_TXT} Session not available.");
                                            state.dead_letter_queue.push_back(msg.clone().to_vec());
                                            debug!("Subscriber: {myself:?} queue has {0} message(s) waiting", state.dead_letter_queue.len());
                                        }
                                    }
                                    CallResult::Timeout => {
                                        error!("{REGISTRATION_REQ_FAILED_TXT}: Session timed out");
                                        // myself.stop(Some("{PUBLISH_REQ_FAILED_TXT}: Session timed out".to_string()));
                                    }
                                    CallResult::SenderError => {
                                        error!("{REGISTRATION_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: The transmission channel was dropped without any message(s) being sent");
                                        // myself.stop(Some("{REGISTRATION_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}".to_string()));
                                    }
                                }
                            }
                        }
                        None => {
                            error!("{REGISTRATION_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
                        }
                    }
                }
            }
            BrokerMessage::PublishResponse { topic, payload, .. } => {
                let id = state.registration_id.clone();
                let t = topic.clone();
                let p = payload.clone();
                tracing::debug!("New message on topic: {topic}, forwarding to session: {id}");
                if let Some(session) = where_is(id.clone()) {
                    match call(
                        &session,
                        |reply| BrokerMessage::PushMessage {
                            reply,
                            payload: p,
                            topic,
                        },
                        None,
                    )
                    .await
                    .expect("Expected to forward message to subscriber")
                    {
                        CallResult::Success(result) => {
                            if let Err(e) = result {
                                //session couldn't talk to listener, add message to DLQ
                                state.dead_letter_queue.push_back(payload.clone());
                                debug!(
                                    "{t} queue has {0} message(s) waiting",
                                    state.dead_letter_queue.len()
                                );
                            }
                        }
                        CallResult::Timeout => {
                            error!("{PUBLISH_REQ_FAILED_TXT}: Session timed out");
                            myself.stop(Some(
                                "{PUBLISH_REQ_FAILED_TXT}: Session timed out".to_string(),
                            ));
                        }
                        CallResult::SenderError => {
                            error!("{PUBLISH_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
                            myself.stop(Some("{PUBLISH_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: The transmission channel was dropped without any message(s) being sent".to_string()));
                        }
                    }
                } else {
                    error!("{PUBLISH_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
                }
            }
            _ => warn!(UNEXPECTED_MESSAGE_STR),
        }
        Ok(())
    }
}
