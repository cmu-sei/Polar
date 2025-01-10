use std::collections::HashMap;
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;
use crate::{listener, BrokerMessage, BROKER_NOT_FOUND_TXT, CLIENT_NOT_FOUND_TXT, PUBLISH_REQ_FAILED_TXT, REGISTRATION_REQ_FAILED_TXT, SUBSCRIBE_REQUEST_FAILED_TXT, TIMEOUT_REASON};
use crate::UNEXPECTED_MESSAGE_STR;

/// The manager process for our concept of client sessions.
/// When thje broker receives word of a new connection from the listenerManager, it requests
/// that the client be "registered". Signifying the client as legitimate.
pub struct SessionManager;

/// Our representation of a connected session, and how close it is to timing out
/// TODO: What other fields might we want to have here?
pub struct Session {
    agent_ref: ActorRef<BrokerMessage>,
}


/// Define the state for the actor
pub struct SessionManagerState {
    /// Map of registration_id to Session ActorRefes
    sessions: HashMap<String, Session>,           
    session_timeout: u64,
    cancellation_tokens: HashMap<String, CancellationToken>,
}

pub struct SessionManagerArgs {
    pub session_timeout: u64
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
        args: SessionManagerArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        //parse args. if any
        let state = SessionManagerState {
            sessions: HashMap::new(),
            cancellation_tokens: HashMap::new(),
            session_timeout: args.session_timeout
        };

        Ok(state)
    }
    async fn post_start(&self, myself: ActorRef<Self::Msg>, _state: &mut Self::State) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} started");
  
        Ok(())
    }
    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr>  {
        match message {
            BrokerMessage::RegistrationRequest { client_id, ..} => {
                
                // Start brand new session
                let new_id = Uuid::new_v4().to_string();

                info!("SessionManager: Stating session for client: {client_id} with registration ID {new_id}");

                match myself.try_get_supervisor() {
                    Some(broker_ref) => {

                        if let Some(listener_ref) = where_is(client_id.clone()) {
                            let args = SessionAgentArgs { registration_id: new_id.clone(), client_ref: listener_ref.clone().into() , broker_ref: broker_ref.clone().into()};
                            
                            if let Ok((session_agent, _)) = Actor::spawn_linked(Some(new_id.clone()), SessionAgent, args, myself.clone().into()).await {
                                state.sessions.insert(new_id.clone(), Session {
                                    agent_ref: session_agent                            
                                });
                            } else { 
                                let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT} Couldn't start session agent!");
                                warn!("{err_msg}");
                                if let Err(e) = listener_ref.send_message(BrokerMessage::RegistrationResponse { registration_id: None, client_id: client_id.clone(), success: false, error: Some(err_msg.clone()) }) {
                                    error!("{err_msg}: {e}")
                                }
                            }
                        } else {
                            warn!("{REGISTRATION_REQ_FAILED_TXT} {CLIENT_NOT_FOUND_TXT}")
                        }
                        
                    } None => {
                        error!("Couldn't lookup root broker actor!");
                        
                    }
                }   
            }
            BrokerMessage::RegistrationResponse { registration_id, client_id, .. } => {
                //A client has (re)registered. Cancel timeout thread
                // debug!("SessionManager: Alerted of session registration");
                if let Some(registration_id) = registration_id {
                    match where_is(registration_id.clone()) {
                        Some(_) => {
                            if let Some(token) = &state.cancellation_tokens.get(&registration_id) {
                                debug!("Aborting session cleanup for {registration_id}");
                                token.cancel();
                                state.cancellation_tokens.remove(&registration_id);
                            }
                        }, None => {
                            warn!("No session found for id {registration_id}");   
                        }
                    }
                } else {
                    warn!("Received registration response from unknown client: {client_id}");
                }
            }
            BrokerMessage::DisconnectRequest { client_id, registration_id } => {
                //forward to broker, kill session
                if let Some(registration_id) = registration_id {
                    match where_is(registration_id.clone()) {
                        Some(session) => session.stop(Some("CLIENT_DISCONNECTED".to_owned())),
                        None => warn!("Failed to find session {registration_id}")
                    }

                    match myself.try_get_supervisor() {
                        Some(broker) => broker.send_message(
                            BrokerMessage::DisconnectRequest { client_id, registration_id: Some(registration_id) })
                            .expect("Expected to forward message"),

                        None => warn!("Failed to find supervisor")
                    }
                    
                }
            }
            BrokerMessage::TimeoutMessage { client_id, registration_id, error } => {
                if let Some(registration_id) = registration_id {
                    
                    if let Some(session) = state.sessions.get(&registration_id) {
                        warn!("Session {registration_id} timing out, waiting for reconnect...");
                        let ref_clone = session.agent_ref.clone();    
                        let token = CancellationToken::new();

                        state.cancellation_tokens.insert(registration_id.clone(), token.clone());
                        let timeout = state.session_timeout.clone();
                        let _ = tokio::spawn(async move {
                            tokio::select! {
                                // Use cloned token to listen to cancellation requests
                                _ = token.cancelled() => {
                                    // The timer was cancelled, task can shut down
                                }
                                // wait before killing session
                                _ = tokio::time::sleep(std::time::Duration::from_secs(timeout)) => {
                                    match myself.try_get_supervisor() {
                                        Some(manager) => {
                                            info!("Ending session {ref_clone:?}");
                                            manager.send_message(BrokerMessage::TimeoutMessage {
                                                client_id,
                                                registration_id: Some(registration_id),
                                                error })
                                                .expect("Expected to forward to manager")
                                        }
    
                                        None => warn!("Could not find broker supervisor!")
                                    }
                                    ref_clone.stop(Some(TIMEOUT_REASON.to_string()));
                                
                                }
                            }
                        });
                    }
                }
            }, _ => warn!("Received unexpected message: {message:?}")
        }
        Ok(())
    }

    async fn handle_supervisor_evt(&self, _: ActorRef<Self::Msg>, msg: SupervisionEvent, _: &mut Self::State) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                debug!("Session: {0:?}:{1:?} terminated. {reason:?}", actor_cell.get_name(), actor_cell.get_id());
            },
            SupervisionEvent:: ActorFailed(actor_cell, error) => warn!("Worker agent: {0:?}:{1:?} failed! {error}", actor_cell.get_name(), actor_cell.get_id()),
            SupervisionEvent::ProcessGroupChanged(_) => (),
        }
        Ok(())
    }

}

/// Worker process for handling client sessions.
pub struct SessionAgent;
pub struct SessionAgentArgs {
    pub registration_id: String,
    pub client_ref: ActorRef<BrokerMessage>,
    pub broker_ref: ActorRef<BrokerMessage>
}
pub struct SessionAgentState {
    pub client_ref: ActorRef<BrokerMessage>,     ///id of client listener to connect to
    pub broker: ActorRef<BrokerMessage>
}

#[async_trait]
impl Actor for SessionAgent {
    type Msg = BrokerMessage; // Messages this actor handles
    type State = SessionAgentState; // Internal state
    type Arguments = SessionAgentArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: SessionAgentArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        info!("{myself:?} starting");
        //parse args. if any
        let state = SessionAgentState { client_ref: args.client_ref.clone(), broker: args.broker_ref};

        Ok(state)
    }

    async fn post_start(&self, myself: ActorRef<Self::Msg>, state: &mut Self::State) -> Result<(), ActorProcessingErr> {
        
        let client_id = state.client_ref.get_name().unwrap_or_default();
        let registration_id = myself.get_name().unwrap_or_default();
        info!("Started session {myself:?} for client {client_id}. Contacting");      

        //send ack to client listener
       state.client_ref.send_message(BrokerMessage::RegistrationResponse { 
            registration_id: Some(registration_id),
            client_id,
            success: true,
            error: None 
        }).expect("Expected to send message to client listener");

        Ok(())

    }

    // TODO: Harden, don't expect message forwarding to always succeed, treat as a timeout if we can't forward to listener
    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr>  {
        
        match message {
            BrokerMessage::RegistrationRequest { registration_id, client_id, .. } => {
                //A a new connection has been established, update state to send messages to new listener actor
                match where_is(client_id.clone()) {
                    Some(listener) => {     
                        state.client_ref = ActorRef::from(listener);
                        //ack
                        info!("Established comms with client {client_id}");
                        state.client_ref.send_message(BrokerMessage::RegistrationResponse {
                            registration_id: registration_id.clone(),
                            client_id: client_id.clone(),
                            success: true,
                            error: None })
                        .expect("Expected to send ack to listener");
                        // Alert session manager we've got our listener so it doesn't potentially kill it
                        match myself.try_get_supervisor() {
                            Some(manager) => {
                                //continue registration
                                if let Err(manager_send_err) = manager.send_message(BrokerMessage::RegistrationResponse { registration_id: registration_id.clone(), client_id: client_id.clone(), success: true , error: None }) {
                                    let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT} {manager_send_err}!");
                                    error!("{err_msg}");
                                    // if we fail to contact manager, send failure
                                    if let Err(listener_err) = state.client_ref.send_message(BrokerMessage::RegistrationResponse { registration_id: None, client_id: client_id.clone(), success: false, error: Some(err_msg.clone()) }){
                                        error!("{err_msg}: {listener_err}");
                                        // no listener? start timeout logic
                                        if let Err(fwd_err) = myself.send_message(BrokerMessage::TimeoutMessage { client_id, registration_id, error: Some(format!("{err_msg} {listener_err}")) }) {
                                            error!("{err_msg}: {listener_err}: {fwd_err}");
                                            myself.stop(Some("{err_msg}: {listener_err}: {fwd_err}".to_string()));
                                        }
                                    }
                                }
                            }
                            None => warn!("Couldn't find supervisor for session agent!")
                        }
                    } None => {
                        warn!("Could not find listener for client: {client_id}");
                    }
                }
                
            }
            BrokerMessage::PublishRequest { registration_id, topic, payload } => {
                //forward to broker
                if let Err(e) = state.broker.send_message(BrokerMessage::PublishRequest { registration_id: registration_id.clone(), topic: topic.clone(), payload: payload.clone() }) {
                    let err_msg = format!("{PUBLISH_REQ_FAILED_TXT} {BROKER_NOT_FOUND_TXT}");
                    if let Err(listener_err) = state.client_ref.send_message(BrokerMessage::PublishResponse { topic, payload, result: Err(err_msg.clone()) }){
                        error!("{err_msg}: {listener_err}");
                        // no listener? start timeout logic
                        if let Err(fwd_err) = myself.send_message(BrokerMessage::TimeoutMessage { client_id: state.client_ref.get_name().unwrap_or_default(), registration_id, error: Some(format!("{err_msg} {listener_err}")) }) {
                            error!("{err_msg}: {listener_err}: {fwd_err}");
                            myself.stop(Some("{err_msg}: {listener_err}: {fwd_err}".to_string()));
                        }
                    }
                }
            },
            BrokerMessage::PushMessage { reply, payload, topic } => {
                //push to client
                if let Ok(_) = state.client_ref.send_message(BrokerMessage::PublishResponse { topic, payload: payload.clone(), result: Ok(()) }) {
                    if let Err(e) = reply.send(Ok(())) {
                        warn!("Failed to reply to subscriber! Subscriber not available: {e} Client may need to resubscribe")
                    }
                    
                } else {
                    warn!("{}", format!("{PUBLISH_REQ_FAILED_TXT}: {CLIENT_NOT_FOUND_TXT}"));
                    if let Err(e) = reply.send(Err(payload)) {
                        warn!("{}", format!("{PUBLISH_REQ_FAILED_TXT}: {CLIENT_NOT_FOUND_TXT}: {e}"))
                    }
                }
            },
            BrokerMessage::PublishRequestAck(topic) => {
                if let Err(e) = state.client_ref.send_message(BrokerMessage::PublishRequestAck(topic)) {
                    // no listener? start timeout logic
                    if let Err(fwd_err) = myself.send_message(BrokerMessage::TimeoutMessage { client_id: state.client_ref.get_name().unwrap_or_default(), registration_id: Some(myself.get_name().unwrap_or_default()), error: Some(format!("{e}")) }) {
                        error!("{e}: {fwd_err}");
                        myself.stop(Some("{err_msg}: {listener_err}: {fwd_err}".to_string()));
                    }
                }
            },
            BrokerMessage::SubscribeRequest { registration_id, topic } => {
                if let Err(e) = state.broker.send_message(BrokerMessage::SubscribeRequest { registration_id, topic}) {
                    error!("{SUBSCRIBE_REQUEST_FAILED_TXT} {BROKER_NOT_FOUND_TXT}");

                    if let Err(fwd_err) = myself.send_message(BrokerMessage::TimeoutMessage { client_id: state.client_ref.get_name().unwrap_or_default(), registration_id: Some(myself.get_name().unwrap_or_default()), error: Some(format!("{e}")) }) {
                        error!("{e}: {fwd_err}");
                        myself.stop(Some("{err_msg}: {listener_err}: {fwd_err}".to_string()));
                    }
                }
                
            },
            BrokerMessage::UnsubscribeRequest { registration_id, topic } => {
                if let Err(e) = state.broker.send_message(BrokerMessage::UnsubscribeRequest { registration_id, topic}) {
                    // error!("{SUBSCRIBE_REQUEST_FAILED_TXT} {BROKER_NOT_FOUND_TXT}");

                    if let Err(fwd_err) = myself.send_message(BrokerMessage::TimeoutMessage { client_id: state.client_ref.get_name().unwrap_or_default(), registration_id: Some(myself.get_name().unwrap_or_default()), error: Some(format!("{e}")) }) {
                        error!("{e}: {fwd_err}");
                        myself.stop(Some("{err_msg}: {listener_err}: {fwd_err}".to_string()));
                    }
                }
            },
            BrokerMessage::UnsubscribeAcknowledgment { registration_id, topic, result } => {
                if let Err(e) = state.client_ref.send_message(BrokerMessage::UnsubscribeAcknowledgment { registration_id, topic, result }) {
                    error!("Failed to forward message to client");

                    if let Err(fwd_err) = myself.send_message(BrokerMessage::TimeoutMessage { client_id: state.client_ref.get_name().unwrap_or_default(), registration_id: Some(myself.get_name().unwrap_or_default()), error: Some(format!("{e}")) }) {
                        error!("{e}: {fwd_err}");
                        myself.stop(Some("{err_msg}: {listener_err}: {fwd_err}".to_string()));
                    }
                }
            }
            BrokerMessage::SubscribeAcknowledgment { registration_id, topic, result } => {        
                if let Err(e) = state.client_ref.send_message(BrokerMessage::SubscribeAcknowledgment { registration_id, topic, result }) {
                    if let Err(fwd_err) = myself.send_message(BrokerMessage::TimeoutMessage { client_id: state.client_ref.get_name().unwrap_or_default(), registration_id: Some(myself.get_name().unwrap_or_default()), error: Some(format!("{e}")) }) {
                        error!("{e}: {fwd_err}");
                        myself.stop(Some("{err_msg}: {listener_err}: {fwd_err}".to_string()));
                    }
                }
            }
            BrokerMessage::DisconnectRequest { client_id,registration_id } => {
                //client disconnected, clean up after it then die with honor
                debug!("client {client_id} disconnected");
                match myself.try_get_supervisor() {
                    Some(manager) => manager.send_message(BrokerMessage::DisconnectRequest { client_id, registration_id }).expect("Expected to forward to manager"),
                    None=> tracing::error!("Couldn't find supervisor.")
                }
                
            }
            BrokerMessage::TimeoutMessage { client_id, registration_id, error } => {
                match myself.try_get_supervisor() {
                    Some(manager) => manager.send_message(BrokerMessage::TimeoutMessage { client_id, registration_id, error }).expect("Expected to forward to manager"),
                    None=> error!("Couldn't find supervisor.")
                }
            }
            _ => {
                warn!("{}",format!("{UNEXPECTED_MESSAGE_STR}: {message:?}"));
            }
        }
    
        Ok(())
    }


}