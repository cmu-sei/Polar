use std::{iter::Successors, result};

use tracing::{error, info, warn};
use crate::{get_subscriber_name, listener::{ListenerManager, ListenerManagerArgs}, session::{SessionManager, SessionManagerArgs}, subscriber::SubscriberManager, topic::{TopicManager, TopicManagerArgs}, SUBSCRIBE_REQUEST_FAILED_TXT, UNEXPECTED_MESSAGE_STR};
use crate::{BrokerMessage, LISTENER_MANAGER_NAME, SESSION_MANAGER_NAME,PUBLISH_REQ_FAILED_TXT, SUBSCRIBER_MANAGER_NAME, SUBSCRIBER_MGR_NOT_FOUND_TXT, TOPIC_MANAGER_NAME,REGISTRATION_REQ_FAILED_TXT, SESSION_NOT_FOUND_TXT, CLIENT_NOT_FOUND_TXT};
use ractor::{async_trait, registry::where_is, rpc::{call, call_and_forward}, Actor, ActorProcessingErr, ActorRef, SupervisionEvent, rpc::CallResult::Success};


// ============================== Broker Supervisor Actor Definition ============================== //

pub struct Broker;

//State object containing references to worker actors.
pub struct BrokerState;
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
    pub ca_cert_file: String
}

#[async_trait]
impl Actor for Broker {
    type Msg = BrokerMessage;
    type State = BrokerState;
    type Arguments = BrokerArgs;
    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: BrokerArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("Broker: Starting {myself:?}");
        
        let listener_manager_args =  ListenerManagerArgs {
            bind_addr: args.bind_addr,
            server_cert_file: args.server_cert_file,
            private_key_file: args.private_key_file,
            ca_cert_file: args.ca_cert_file
        };

        Actor::spawn_linked(Some(LISTENER_MANAGER_NAME.to_owned()), ListenerManager, listener_manager_args, myself.clone().into()).await.expect("Expected to start Listener Manager");


        //set default timeout for sessions, or use args
        let mut session_timeout: u64 = 90;
        if let Some(timeout) = args.session_timeout {
            session_timeout = timeout;
        }

        Actor::spawn_linked(Some(SESSION_MANAGER_NAME.to_string()), SessionManager, SessionManagerArgs { session_timeout: session_timeout }, myself.clone().into()).await.expect("Expected Session Manager to start");

        //TODO: Read some topics from configuration based on services we want to observer/consumer messages for
        let topic_mgr_args = TopicManagerArgs {topics: None};

        Actor::spawn_linked(Some(TOPIC_MANAGER_NAME.to_owned()), TopicManager, topic_mgr_args, myself.clone().into()).await.expect("Expected to start Topic Manager");    

        Actor::spawn_linked(Some(SUBSCRIBER_MANAGER_NAME.to_string()), SubscriberManager, (), myself.clone().into()).await.expect("Expected to start Subscriber Manager");

        let state = BrokerState;
        Ok(state)
    }

    async fn handle_supervisor_evt(&self, _myself: ActorRef<Self::Msg>, msg: SupervisionEvent, _: &mut Self::State) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(actor_cell) => info!("Worker agent: {0:?}:{1:?} started", actor_cell.get_name(), actor_cell.get_id()),
            
            SupervisionEvent::ActorTerminated(actor_cell, ..) => info!("Worker {0:?}:{1:?} terminated, restarting..", actor_cell.get_name(), actor_cell.get_id()),
            
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!("Worker agent: {0:?}:{1:?} failed! {e}", actor_cell.get_name(), actor_cell.get_id());
                //determine type of actor that failed and restart
                //NOTE: "Remote" actors can't have their types checked? But they do send serializable messages
                // If we can deserialize them to a datatype here, that may be another acceptable means of determining type
                //TODO: Figure out what panics/failures we can/can't recover from
                // Missing certificate files and the inability to forward some messages count as bad states
                _myself.stop(Some("ACTOR_FAILED".to_string()));

            },
            SupervisionEvent::ProcessGroupChanged(_) => (),
        }

        Ok(())
    }
    
    async fn post_start(&self, myself: ActorRef<Self::Msg>, _: &mut Self::State) -> Result<(), ActorProcessingErr> {
        info!("Broker: Started {myself:?}");
        Ok(())
    }

    async fn post_stop(&self, myself: ActorRef<Self::Msg>, _: &mut Self::State) -> Result<(), ActorProcessingErr> {
        info!("Broker: Stopped {myself:?}");
        Ok(())
    }
    
    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            
            BrokerMessage::RegistrationRequest { registration_id, client_id } => {                         
                info!("Received Registration Request from client: {client_id:?}");
                match registration_id {
                    Some(id) => {
                        //forward RegistrationRequest to the session and subscriber mgrs
                        let session_option = where_is(id.clone());
                        let sub_mgr_option = where_is(SUBSCRIBER_MANAGER_NAME.to_string());
                        

                        if session_option.is_some() && sub_mgr_option.is_some() {
                            let sub_mgr = sub_mgr_option.unwrap();
                            let session = session_option.unwrap();
                            
                            let client_id_clone = client_id.clone();
                            
                            if let Err(e) = session.send_message(BrokerMessage::RegistrationRequest { registration_id: Some(id.clone()), client_id }){
                                let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: {e}");
                                if let Some(listener) = where_is(id.clone()) {
                                    listener.send_message(BrokerMessage::RegistrationResponse { registration_id: Some(id.clone()), client_id: id.clone(), success: false, error: Some(err_msg) })
                                    .map_err(|e| {
                                        warn!("{e}");
                                    }).unwrap()
                                }
                            }

                           
                            if let Err(e) = sub_mgr.send_message(BrokerMessage::RegistrationRequest { registration_id: Some(id.clone()), client_id: client_id_clone }){
                                let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {SUBSCRIBER_MGR_NOT_FOUND_TXT}, messages may have been lost! {e}");
                                error!("{err_msg}");
                                if let Some(listener) = where_is(id.clone()) {
                                    listener.send_message(BrokerMessage::RegistrationResponse { registration_id: Some(id.clone()), client_id: id.clone(), success: false, error: Some(err_msg) })
                                    .map_err(|e| {
                                        warn!("{e}");
                                    }).unwrap()
                                }
                            }
                        } else {
                            let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: Couldn't locate session or subscribers!");
                            warn!("{err_msg}");  
                            match where_is(client_id.clone()) {
                                Some(listener) => {
                                    
                                    listener.send_message(BrokerMessage::RegistrationResponse {
                                        registration_id: Some(id.clone()),
                                        client_id: id.clone(),
                                        success: false,
                                        error: Some(err_msg.clone())
                                    })
                                    .map_err(|e| {
                                        warn!("{err_msg}: {CLIENT_NOT_FOUND_TXT}: {e}");
                                    }).unwrap()
                                } None => warn!("{err_msg}: {CLIENT_NOT_FOUND_TXT}")
                            }
                        }
                    }
                    None => {
                        //start new session
                        match where_is(SESSION_MANAGER_NAME.to_string()) {
                            Some(manager) => {
                                if let Err(e) = manager.send_message(BrokerMessage::RegistrationRequest { registration_id: registration_id.clone(), client_id: client_id.clone() }) {
                                    let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {SUBSCRIBER_MGR_NOT_FOUND_TXT}: {e}");
                                    if let Some(listener) = where_is(client_id.clone()) {
                                        listener.send_message(BrokerMessage::RegistrationResponse { registration_id: registration_id.clone(), client_id: client_id.clone(), success: false, error: Some(err_msg) })
                                        .map_err(|e| {
                                            warn!("{e}");
                                        }).unwrap()
                                    }
                                    else {
                                        error!("{err_msg}: {CLIENT_NOT_FOUND_TXT}");
                                    }
                                }
                                
                            } None => {
                                let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {SUBSCRIBER_MGR_NOT_FOUND_TXT}");
                                if let Some(listener) = where_is(client_id.clone()) {
                                    listener.send_message(BrokerMessage::RegistrationResponse { registration_id: registration_id.clone(), client_id: client_id.clone(), success: false, error: Some(err_msg) })
                                    .map_err(|e| {
                                        warn!("{e}");
                                    }).unwrap()
                                }
                                else {
                                    error!("{err_msg}: {CLIENT_NOT_FOUND_TXT}");
                                }
                            }
                        }
                    }
                }
                // match &where_is(SESSION_MANAGER_NAME.to_string())
                // {
                //     Some(session_mgr) => {
                //         session_mgr.send_message(BrokerMessage::RegistrationRequest { registration_id, client_id }).expect("Failed to forward registration response to listener manager");
                //     }, 
                //     None => todo!()
                // }
                
            },
            BrokerMessage::SubscribeRequest { registration_id, topic } => {

                //Look for existing subscriber actor.
                
                if let Some(registration_id) = registration_id {

                    match where_is(get_subscriber_name(&registration_id, &topic)) {
                        Some(_) => {
                            //Send success message, session already subscribed
                            match &where_is(registration_id.clone()) {
                                //session's probably dead for one reason or another if we fail here,
                                //log and move on
                                Some(session) => {
                                    session.send_message(BrokerMessage::SubscribeAcknowledgment { registration_id: registration_id.clone(), topic: topic.clone(), result: Ok(()) })
                                    .unwrap_or_else(
                                        |e| {
                                            warn!("Session {registration_id} not found! {e}!");
                                        })
                                }, 
                                None => warn!("Session {registration_id} not found!")
                            }
                        },
                        None => {
                            match where_is(topic.clone()) {
                                Some(actor) => {
                                    if let Some(manager) = where_is(SUBSCRIBER_MANAGER_NAME.to_string()) {
                                        let t = topic.clone();
                                        let id = registration_id .clone();
                                        
                                        // If no subscriber exists,
                                        match call(&manager, |reply| { BrokerMessage::Subscribe { reply: reply, registration_id: id.clone(), topic: t.clone() } }, None)
                                        .await.expect("Expected to call Subscriber Manager") { //TODO: Don't panic with expect, call .map_err and stop cleanly
                                            Success(add_subscribeer) => {
                                                match add_subscribeer {
                                                    Ok(_) => {
                                                        //  create one, then notify topic actor, subscribe it on behalf
                                                        match call(&actor,|reply| { 
                                                            BrokerMessage::Subscribe { reply, registration_id: registration_id.clone(), topic: topic.clone() } }, None)
                                                            .await.expect("Expected to call topic actor") {
                                                            Success(result) => {
                                                                match result {
                                                                    Ok(_) => {
                                                                        //send ack to session
                                                                        if let Some(session) = where_is(registration_id.clone()) {
                                                                            if let Err(e) = session.send_message(BrokerMessage::SubscribeAcknowledgment { registration_id: registration_id.clone(), topic: topic.clone(), result: Ok(()) }) {
                                                                                warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {e}");
                                                                            }
                                                                        } else {
                                                                            warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {SESSION_NOT_FOUND_TXT}");
                                                                            todo!("Send error message to client")
                                                                        }
                                                                    }
                                                                    Err(e) => {
                                                                        warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {e}");
                                                                        //send error to client
                                                                    }
                                                                }
                                                            }
                                                            _ => todo!()
                                                        }
                                                    }
                                                    Err(e) => {
                                                        warn!("{e}");
                                                        todo!("Send error message")
                                                    }
                                                }
                                            }
                                            _ => {
                                                // failed
                                                todo!("Send error message")
                                                //TODO: Stop broker we fail to communicate with critical actor
                                            }
                                        }
                                    } else {
                                        // failed
                                        todo!("Send error message")
                                        //TODO: Stop broker we fail to communicate with critical actor
                                    }

                                },
                                None => {
                                    //no topic actor exists, tell manager to make one
                                    
                                
                                    let topic_mgr = where_is(TOPIC_MANAGER_NAME.to_owned());
                                    let sub_mgr = where_is(SUBSCRIBER_MANAGER_NAME.to_owned());

                                    let t = topic.clone();
                                    let id = registration_id.clone();

                                    //TODO: Remove this if/else case, if we don't have either of these actor's present, the broker isn't in a sane state, just panic/fail
                                    if topic_mgr.is_some() && sub_mgr.is_some() {

                                        // create a topic on the session's behalf
                                        match call(&topic_mgr.unwrap(), |reply| { BrokerMessage::AddTopic { reply: reply, registration_id: Some(registration_id.clone()), topic: t.clone() } }, None)
                                        .await.expect("Expected to call Topic Manager") {
                                            Success(result) => {
                                                match result {
                                                    Ok(_) => {
                                                        // create subscriber
                                                        match call(&sub_mgr.unwrap(), |reply| {
                                                            BrokerMessage::Subscribe { reply, registration_id: registration_id.clone(), topic: topic.clone() }
                                                        }, None).await.expect("Expected to communicate with subscriber manager")
                                                        {
                                                            Success(result) => {
                                                                match result{
                                                                    Ok(_) => {
                                                                        //send ack to session
                                                                        if let Some(session) = where_is(registration_id.clone()) {
                                                                            if let Err(e) = session.send_message(BrokerMessage::SubscribeAcknowledgment { registration_id: registration_id, topic: topic.clone(), result: Ok(()) }) {
                                                                                warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {SESSION_NOT_FOUND_TXT}");
                                                                            }
                                                                        } else { warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {SESSION_NOT_FOUND_TXT}"); }
                                                                    }
                                                                    Err(e) => {
                                                                        //send err to session
                                                                        if let Some(session) = where_is(registration_id.clone()) {
                                                                            if let Err(e) = session.send_message(BrokerMessage::SubscribeAcknowledgment { registration_id: registration_id, topic: topic.clone(), result: Err(e) }) {
                                                                                warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {SESSION_NOT_FOUND_TXT} {e}");
                                                                            }
                                                                        } else { warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {SESSION_NOT_FOUND_TXT} {e}"); }
                                                                    }
                                                                }
                                                            },
                                                            _ => todo!("Failed to communicate with critical worker")
                                                        }      
                                                    }
                                                    Err(e) => {
                                                        warn!("{e}");
                                                        todo!("Send error message")
                                                    }
                                                }
                                            }
                                            _ => {
                                                // failed
                                                todo!("Send error message")
                                                //TODO: Stop broker we fail to communicate with critical actor
                                            }
                                        }

                                        
                                    } else {
                                        error!("{}", format!("{SUBSCRIBE_REQUEST_FAILED_TXT}: Failed to locate one or more managers!"));
                                        todo!("What to do here?");
                                    }
                                },
                            }
                        },
                    }
                }

            },
            BrokerMessage::UnsubscribeRequest { registration_id, topic } => {
                if let Some(registration_id) = registration_id {

                    //find topic actor, tell it to forget session
                    if let Some(topic_actor) = where_is(topic.clone()) {
                        if let Err(e) = topic_actor.send_message(BrokerMessage::UnsubscribeRequest { registration_id: Some(registration_id.clone()), topic: topic.clone() }) {
                            warn!("Failed to find topic to unsubscribe to")
                        }
                        // tell subscriber manager to cleanup subscribers
                        if let Some(subscriber_mgr) = where_is(SUBSCRIBER_MANAGER_NAME.to_string()) {
                            if let Err(e) = subscriber_mgr.send_message(BrokerMessage::UnsubscribeRequest { registration_id: Some(registration_id.clone()), topic: topic.clone() }) {
                                error!("{e}");
                                if let Some(session) = where_is(registration_id.clone()) {
                                    if let Err(e) = session.send_message(BrokerMessage::UnsubscribeAcknowledgment { registration_id, topic: topic.clone(), result: Err(e.to_string()) }) {
                                        error!("{e}");
                                    }
                                }
                                //TODO: Stop self here?
                            }
                        } else {
                            let msg = "Failed to unsubscribe from topic".to_string();
    
                            error!("{msg}");
                            if let Some(session) = where_is(registration_id.clone()) {
                                if let Err(e) = session.send_message(BrokerMessage::UnsubscribeAcknowledgment { registration_id, topic: topic.clone(), result: Err(msg) }) {
                                    error!("{e}");
                                }
                            }
                            //TODO: Stop self here?
                        }
                    }
                }
                                
            }
            BrokerMessage::SubscribeAcknowledgment { registration_id, topic, .. } => {
                where_is(registration_id.clone()).map(|session_agent_ref|{
                    session_agent_ref.send_message(BrokerMessage::SubscribeAcknowledgment { registration_id: registration_id.clone(), topic: topic.clone(), result: Ok(()) }).unwrap();
                });
            },
            BrokerMessage::PublishRequest { registration_id, topic, payload } => {
                //publish to topic
                if let Some(registration_id) = registration_id {
                    match where_is(topic.clone()) {
                        Some(actor) => {
                            if let Err(e) = actor.send_message(BrokerMessage::PublishRequest { registration_id: Some(registration_id.clone()), topic: topic.clone(), payload: payload.clone() }) {
                                warn!("{PUBLISH_REQ_FAILED_TXT}: {e}");
                                //send err to session
                                if let Some(session) = where_is(registration_id.clone()) {
                                    if let Err(e) = session.send_message(BrokerMessage::PublishResponse { topic, payload: payload.clone(), result: Err(e.to_string()) }) {
                                        warn!("{PUBLISH_REQ_FAILED_TXT} {e}");
                                    }
                                }
                            }
                        },
                        None => {
                            // topic doesn't exist

                            if let Some(manager) = where_is(TOPIC_MANAGER_NAME.to_string()) {
                                //tell topicmgr to add one, await it to complete, 
                                match call(&manager, |reply| {
                                    BrokerMessage::AddTopic { reply, registration_id: None, topic: topic.clone() }
                                    },
                                None).await
                                .expect("{PUBLISH_REQ_FAILED_TXT}: {TOPIC_MGR_NOT_FOUND_TXT}") {
                                    Success(add_topic_result) => {
                                        match add_topic_result {
                                            Ok(topic_actor) => {
                                                //push message to that topic
                                                if let Err(e) = topic_actor.send_message(BrokerMessage::PublishRequest { registration_id: Some(registration_id.clone()), topic: topic.clone(), payload: payload.clone() }) {
                                                    warn!("{PUBLISH_REQ_FAILED_TXT}: {e}");
                                                    //send err to session
                                                    if let Some(session) = where_is(registration_id.clone()) {
                                                        if let Err(e) = session.send_message(BrokerMessage::PublishResponse { topic, payload: payload.clone(), result: Err(e.to_string()) }) {
                                                            warn!("{PUBLISH_REQ_FAILED_TXT} {e}");
                                                        }
                                                    }
                                                }
                                            } Err(e) => {
                                                //send err to session
                                                if let Some(session) = where_is(registration_id.clone()) {
                                                    if let Err(e) = session.send_message(BrokerMessage::PublishResponse { topic, payload: payload.clone(), result: Err(e.to_string()) }) {
                                                        warn!("{PUBLISH_REQ_FAILED_TXT} {e}");
                                                    }
                                                }
                                            }
                                        }
                                    },
                                    _ => {
                                        todo!("Failed to communicate with critical actor for some reason, respond")
                                    }
                                }
                                    
                                
                            }
                        },
                    }
                }

            }// end publish req
            BrokerMessage::PublishResponse { topic, payload, result } => {
                match where_is(SUBSCRIBER_MANAGER_NAME.to_owned()) {
                    Some(actor) => actor.send_message(BrokerMessage::PublishResponse {topic, payload, result }).expect("Failed to forward notification to subscriber manager"),
                    None => tracing::error!("Failed to lookup subscriber manager!")
                } 
                
            },
            BrokerMessage::ErrorMessage { error, .. } => {
                warn!("Error Received: {error}");
            },
            BrokerMessage::DisconnectRequest { client_id, registration_id } => {
                //start cleanup
                info!("Cleaning up session {registration_id:?}");
                if let Some(manager) = where_is(SUBSCRIBER_MANAGER_NAME.to_string()) {
                    manager.send_message(BrokerMessage::DisconnectRequest {
                        client_id: client_id.clone(),
                        registration_id: registration_id.clone(),
                    }).expect("Expected to forward message");
                    
                }
                // Tell listener manager to kill listener, it's not coming back
                if let Some(manager) = where_is(LISTENER_MANAGER_NAME.to_string()) {
                    manager.send_message(BrokerMessage::DisconnectRequest {
                        client_id: client_id.clone(),
                        registration_id,
                    }).expect("Expected to forward message");
                }
            }
            BrokerMessage::TimeoutMessage { client_id, registration_id, error } => {
                //cleanup subscribers
                
                if let Some(manager) = where_is(SUBSCRIBER_MANAGER_NAME.to_string()) {
                    manager.send_message(BrokerMessage::TimeoutMessage {
                        client_id: client_id.clone(),
                        registration_id: registration_id.clone(),
                        error: error.clone()
                    }).expect("Expected to forward message");
                    
                }
            }
            _ => warn!(UNEXPECTED_MESSAGE_STR)
        }
        Ok(())
    }
}   
