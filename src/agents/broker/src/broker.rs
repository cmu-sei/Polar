use tracing::{error, info, warn};
use crate::{get_subsciber_name, listener::{ListenerManager, ListenerManagerArgs}, session::{SessionManager, SessionManagerArgs}, subscriber::SubscriberManager, topic::{TopicManager, TopicManagerArgs}, SUBSCRIBE_REQUEST_FAILED_TXT, UNEXPECTED_MESSAGE_STR};
use crate::{BrokerMessage, LISTENER_MANAGER_NAME, SESSION_MANAGER_NAME,PUBLISH_REQ_FAILED_TXT, SUBSCRIBER_MANAGER_NAME, SUBSCRIBER_MGR_NOT_FOUND_TXT, TOPIC_MANAGER_NAME,REGISTRATION_REQ_FAILED_TXT, SESSION_NOT_FOUND_TXT, CLIENT_NOT_FOUND_TXT};
use ractor::{async_trait, registry::where_is, rpc::{call, call_and_forward}, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};


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
                // where_is(SUBSCRIBER_MANAGER_NAME.to_owned()).unwrap().send_message(BrokerMessage::SubscribeRequest { registration_id: registration_id.clone(), topic: topic.clone() }).expect("Failed to forward subscribeRequest to subscriber manager");

                //Look for existing subscriber actor.
                
                if let Some(registration_id) = registration_id {
                    
                    match where_is(get_subsciber_name(&registration_id, &topic)) {
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
                            // If no subscriber exists, forward subscribe request topic actor
                            // forward to topic actor
                            match where_is(topic.clone()) {
                                Some(actor) => {
                                    //forward request
                                    actor.send_message(BrokerMessage::SubscribeRequest { registration_id: Some(registration_id.clone()), topic: topic.clone() })
                                    .unwrap_or_else(|_| {
                                        todo!("What to do if broker can't find the topic actor when subscribing?")
                                        //TODO:What to do if broker can't find the topic actor when subscribing?
                                        // if we "find" the topic actor, but fail to send it a message
                                        // it's possible it either panicked or was killed sometime between the lookup and the send
                                        // We can either outright fail to subscribe in this case and alert the client,
                                        // OR we can start a new topic over again?
                                        

                                        //couldn't send a message to the topic actor, let know
                                        // warn!("Failed to subscribe client to topic \"{topic}\". Topic not found!")
                                        // match &where_is(registration_id.clone()) {
                                        //     Some(session) => {
                                        //         session.send_message(BrokerMessage::SubscribeAcknowledgment { registration_id: registration_id.clone(), topic: topic.clone(), result: Ok(()) })
                                        //         .unwrap_or_else(
                                        //             |e| {
                                        //                 warn!("Failed to subscribe client to topic \"{topic}\". {e}!");
                                        //             })
                                        //     }, 
                                        //     None => warn!("Failed to subscribe client to topic \"{topic}\". Session not found!")
                                        // }

                                        
                                    })
                                },
                                None => {
                                    //no topic actor exists, tell manager to make one
                                    let topic_mgr = where_is(TOPIC_MANAGER_NAME.to_owned());
                                    let sub_mgr = where_is(SUBSCRIBER_MANAGER_NAME.to_owned());

                                    let t = topic.clone();
                                    let id = registration_id.clone();

                                    if topic_mgr.is_some() && sub_mgr.is_some() {
                                        let _ = call_and_forward(&topic_mgr.unwrap(),
                                        |reply| {
                                            BrokerMessage::AddTopic { reply, registration_id: Some(id), topic: t }
                                        },
                                        sub_mgr.unwrap(),
                                        move |result| {
                                            result.map_or_else(|e| {
                                                //failed
                                                // send error to client
                                                todo!("send error to client")
                                            }, |_| {
                                                //succeeded, forward to sub manager
                                                BrokerMessage::SubscribeRequest { registration_id: Some(registration_id), topic}
                                            })
                                        },
                                        None) //TODO: Set timeouut?
                                        .map_err(|e| {
                                            error!("{}", format!("{SUBSCRIBE_REQUEST_FAILED_TXT}: {e}"));
                                        })
                                        .unwrap().await;
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
                where_is(SUBSCRIBER_MANAGER_NAME.to_owned()).unwrap().send_message(BrokerMessage::UnsubscribeRequest { registration_id: registration_id.clone(), topic: topic.clone() }).expect("Failed to forward request to subscriber manager");
            }
            BrokerMessage::SubscribeAcknowledgment { registration_id, topic, .. } => {
                where_is(registration_id.clone()).map(|session_agent_ref|{
                    session_agent_ref.send_message(BrokerMessage::SubscribeAcknowledgment { registration_id: registration_id.clone(), topic: topic.clone(), result: Ok(()) }).unwrap();
                });
            },
            BrokerMessage::PublishRequest { registration_id, topic, payload } => {
                //publish to topic, await it's response, forward to subscriber
                if let Some(registration_id) = registration_id {
                    match where_is(topic.clone()) {
                        Some(actor) => {
                            actor.send_message(BrokerMessage::PublishRequest { registration_id: Some(registration_id.clone()), topic, payload })
                            .map_err(|e| {
                                warn!("{PUBLISH_REQ_FAILED_TXT}: {e}")
                            }).unwrap();
                        },
                        None => {
                            // topic doesn't exist

                            if let Some(manager) = where_is(TOPIC_MANAGER_NAME.to_string()) {
                                let id = registration_id.clone();
                                let t = topic.clone();
                                let p = payload.clone();
                                //tell topicmgr to add one, await it to complete, 
                                call(&manager, |reply| {
                                    BrokerMessage::AddTopic { reply, registration_id: Some(id.clone()), topic }
                                    },
                                None).await
                                .expect("{PUBLISH_REQ_FAILED_TXT}: {TOPIC_MGR_NOT_FOUND_TXT}")
                                .map(|call_result| {
                                    //forward publish req to topic
                                    call_result.map(|actor| {
                                        actor.send_message(BrokerMessage::PublishRequest { registration_id: Some(id), topic: t.clone(), payload: p })
                                        .map_err(|e| { 
                                            //Failed to send message to topic
                                            let err_msg = format!("{PUBLISH_REQ_FAILED_TXT}: {e}");
                                            warn!("{err_msg}"); 
                                            where_is(registration_id.clone())
                                            .map(|session| {
                                                
                                                session.send_message(BrokerMessage::PublishResponse { topic: t, payload, result: Err(err_msg) })
                                            })

                                        }).unwrap();
                                    })
                                    .map_err(|e| {
                                        //failed to add topic, tell client
                                        warn!("{e}");
                                    })
                                });
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
