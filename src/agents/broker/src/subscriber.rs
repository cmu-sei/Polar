use std::collections::{HashMap, VecDeque};
use ractor::rpc::{call, CallResult};
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use tracing::{debug, error, info, warn};
use crate::{BrokerMessage, CLIENT_NOT_FOUND_TXT, DISCONNECTED_REASON, PUBLISH_REQ_FAILED_TXT, REGISTRATION_REQ_FAILED_TXT, SESSION_NOT_FOUND_TXT, SUBSCRIBE_REQUEST_FAILED_TXT, TIMEOUT_REASON};
use crate::UNEXPECTED_MESSAGE_STR;


pub const SUBSCRIBER_NOT_FOUND_TXT: &str = "Subscriber not found!";

/// Our supervisor for the subscribers
/// When a user subscribes to a new topic, this actor is notified inb conjunction with the topic manager.
/// A new process is started to wait and listen for new messages on that topic and forward messages.
/// Clients are only considered subscribed if an actor process exists and is managed by this actor
pub struct SubscriberManager;


/// Define the state for the actor
pub struct SubscriberManagerState {
    subscriptions: HashMap<String, Vec<String>> // Map of topics to list subscriber ids
}

impl SubscriberManager {
    /// Removes all subscriptions for a given session
    fn forget_subscriptions(registration_id: String, myself: ActorRef<BrokerMessage>, reason: Option<String> ) {
        //cleanup all subscriptions for session
        for subscriber in myself.get_children()
        .into_iter()
        .filter(|cell|{ 
            let sub_name = cell.get_name().unwrap();
            let sub_id = sub_name.split_once(":").unwrap().0;
            registration_id.eq(sub_id)
            }) {subscriber.stop(reason.clone()); }
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
        _: ()
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::debug!("{myself:?} starting");

        //parse args. if any
        let state = SubscriberManagerState { subscriptions: HashMap::new()};
        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
            tracing::debug!("{myself:?} Started");
            Ok(())

        }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr>  {
        match message {
            BrokerMessage::RegistrationRequest { registration_id, client_id } => {
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
            BrokerMessage::SubscribeRequest { registration_id, topic } => {
                match registration_id {
                    Some(registration_id) => {
                        
                        let subscriber_id = format!("{registration_id}:{topic}");

                        // start new subscriber actor for session
                        Actor::spawn_linked(Some(subscriber_id), SubscriberAgent, (), myself.clone().into()).await
                        .map_err(|e| {
                            let err_msg = format!("{SUBSCRIBE_REQUEST_FAILED_TXT}, {e}");
                            warn!("{err_msg}");
                            //send error message to session
                            let cloned_msg = err_msg.clone();
                            if let Some(session) = where_is(registration_id.clone()) {
                                session.send_message(BrokerMessage::SubscribeAcknowledgment { registration_id, topic, result: Err(err_msg) })
                                .map_err(|e| {
                                    warn!("{}", format!("{cloned_msg}: {SESSION_NOT_FOUND_TXT} {e}"));
                                }).unwrap();
                            } else {
                                warn!("{SUBSCRIBE_REQUEST_FAILED_TXT}, {SESSION_NOT_FOUND_TXT}");
                            }
                        }).unwrap();
                    } 
                    None => warn!("{SUBSCRIBE_REQUEST_FAILED_TXT}, No registration_id provided!")
                }
            },
            BrokerMessage::UnsubscribeRequest { registration_id, topic } => {
                match registration_id {
                    Some(id) => {         
                        if let Some(subscribers) = state.subscriptions.get_mut(&topic) {
                            let subscriber_name = format!("{id}:{topic}");

                            where_is(subscriber_name.clone()).map_or_else(
                                || { warn!("Could not find subscriber for client {id}. They may not be subscribed to the topic: {topic}")},
                                |subscriber| {
                                    subscriber.kill();
                                });
                            //TODO: Monitor this approach for effectiveness
                            match subscribers.binary_search(&subscriber_name) {
                                Ok(index) => subscribers.remove(index),
                                Err(_) => todo!("Expected to find index of the subscriber name for client but couldn't, send error message")
                            };


                            //send ack
                            let id_clone = id.clone();
                            where_is(id).map_or_else(|| { error!("Could not find session for client: {id_clone}")}, |session| {
                                session.send_message(BrokerMessage::UnsubscribeAcknowledgment { registration_id: id_clone.clone(), topic, result: Ok(()) }).expect("expected to send ack to session");
                            });
                        } else {
                            warn!("Session agent {id} not subscribed to topic {topic}");
                            //TODO: determine naming convention for subscriber agents?
                            // session_id:topic?
                        
                        }
                    },
                    None => todo!()
                }
            },
            BrokerMessage::DisconnectRequest { registration_id , ..} => {
                if let Some(registration_id) = registration_id {
                    SubscriberManager::forget_subscriptions(registration_id, myself.clone(), Some(DISCONNECTED_REASON.to_string()) );
                }
                else { warn!("Failed to process disconnect request! registration_id missing.") }
            }
            BrokerMessage::TimeoutMessage { registration_id, .. } => {
                if let Some(registration_id) = registration_id {
                    SubscriberManager::forget_subscriptions(registration_id, myself.clone(), Some(TIMEOUT_REASON.to_string()) );
                }
                else { warn!(" Failed to process timeout request! registration_id missing!") }
            }
            _ => warn!(UNEXPECTED_MESSAGE_STR)

        }
        Ok(())
    }


    async fn handle_supervisor_evt(&self, _: ActorRef<Self::Msg>, msg: SupervisionEvent, _: &mut Self::State) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _ ,reason) => { debug!("Subscription ended for session {0:?}, {reason:?}", actor_cell.get_name()); }
            SupervisionEvent::ActorFailed(..) => todo!("Subscriber failed unexpectedly, restart subscription and update state"),
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
    dead_letter_queue: VecDeque<String>,
}

#[async_trait]
impl Actor for SubscriberAgent {
    type Msg = BrokerMessage; // Messages this actor handles
    type State = SubscriberAgentState; // Internal state
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: ()
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::debug!("{myself:?} starting");
        //parse args. if any
        let name = myself.get_name().expect("{SUBSCRIBE_REQUEST_FAILED_TXT}: Expected subscriber to have name");
        if let Some((registration_id, topic)) = name.split_once(':') {
            
            Ok(SubscriberAgentState {
                registration_id: registration_id.to_string(),
                topic: topic.to_string(),
                dead_letter_queue: VecDeque::new()
            })    
        } else { panic!("{SUBSCRIBE_REQUEST_FAILED_TXT}: Bad name given"); }        
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
            tracing::debug!("{myself:?} Started");
            
            //send ACK to session so they know they're subscribed
            if let Some(session) = where_is(state.registration_id.clone()) {
                session.send_message(BrokerMessage::SubscribeAcknowledgment {
                    registration_id: state.registration_id.clone(),
                    topic: state.topic.clone(),
                    result: Ok(())
                })
                .map_err(|e| {
                    // if we can't ack to the session, it's probably dead
                    myself.stop(Some(format!("SESSION_MISSING {e}")));
                }).unwrap();
            }
            Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr>  {
        match message {
            BrokerMessage::RegistrationRequest { registration_id, .. } => {
                //A client reconnected, push messages built up in DLQ to client
                if let Some(id) = registration_id {

                    match where_is(id.clone()) {
                        Some(session) => {
                            info!("Forwarding missed messages to session: {id}");
                            while let Some(msg) = &state.dead_letter_queue.pop_front() {
                                match call(&session,
                                    |reply| {
                                        BrokerMessage::PushMessage { reply, payload: msg.to_string(), topic: state.topic.clone()}
                                    }, None)
                                .await
                                .expect("Expected to forward message to subscriber") {
                                    CallResult::Success(result) => {
                                        if let Err(message) = result {
                                            //session couldn't talk to listener, add message to DLQ
                                            warn!("{REGISTRATION_REQ_FAILED_TXT} Session not available.");
                                            state.dead_letter_queue.push_back(message);
                                            debug!("Subscriber: {myself:?} queue has {0} message(s) waiting", state.dead_letter_queue.len());
                                        }
                                    },
                                    CallResult::Timeout => {
                                        error!("{REGISTRATION_REQ_FAILED_TXT}: Session timed out");
                                        // myself.stop(Some("{PUBLISH_REQ_FAILED_TXT}: Session timed out".to_string()));
                                    },
                                    CallResult::SenderError => {
                                        error!("{REGISTRATION_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: The transmission channel was dropped without any message(s) being sent");
                                        // myself.stop(Some("{REGISTRATION_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}".to_string()));
                                    },
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

                    match call(&session,
                        |reply| {
                            BrokerMessage::PushMessage { reply, payload: p, topic}
                        }, None)
                    .await
                    .expect("Expected to forward message to subscriber") {
                        CallResult::Success(result) => {
                            if let Err(e) = result {
                                //session couldn't talk to listener, add message to DLQ
                                warn!("{}", format!("{PUBLISH_REQ_FAILED_TXT}: Client not available."));
                                state.dead_letter_queue.push_back(payload.clone());
                                debug!("{t} queue has {0} message(s) waiting", state.dead_letter_queue.len());
                            }
                        },
                        CallResult::Timeout => {
                            error!("{PUBLISH_REQ_FAILED_TXT}: Session timed out");
                            myself.stop(Some("{PUBLISH_REQ_FAILED_TXT}: Session timed out".to_string()));
                        },
                        CallResult::SenderError => {
                            error!("{PUBLISH_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
                            myself.stop(Some("{PUBLISH_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: The transmission channel was dropped without any message(s) being sent".to_string()));
                        },
                    }
                } else {
                    error!("{PUBLISH_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
                }
               
            },
            _ => warn!(UNEXPECTED_MESSAGE_STR)

        }
        Ok(())
    }
}
