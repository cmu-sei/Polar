use std::collections::HashMap;
use ractor::registry::where_is;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, error, warn};
use crate::{BrokerMessage, PUBLISH_REQ_FAILED_TXT, SUBSCRIBE_REQUEST_FAILED_TXT};
use crate::UNEXPECTED_MESSAGE_STR;

pub const TOPIC_ADD_FAILED_TXT: &str = "Failed to add topic \"{topic}!\"";



// ============================== Topic Supervisor definition ============================== //
/// Our supervisor for managing topics and their message queues
/// This processes is generally responsible for creating, removing, maintaining which topics
/// a client can know about
pub struct TopicManager;

pub struct TopicManagerState {
    topics: HashMap<String, ActorRef<BrokerMessage>> // Map of topic name to Topic addresses
}

pub struct  TopicManagerArgs {
    pub topics: Option<Vec<String>>,
}


#[async_trait]
impl Actor for TopicManager {
    #[doc = " The message type for this actor"]
    type Msg = BrokerMessage;

    #[doc = " The type of state this actor manages internally"]
    type State = TopicManagerState;

    #[doc = " Initialization arguments"]
    type Arguments = TopicManagerArgs;

    #[doc = " Invoked when an actor is being started by the system."]
    #[doc = ""]
    #[doc = " Any initialization inherent to the actor\'s role should be"]
    #[doc = " performed here hence why it returns the initial state."]
    #[doc = ""]
    #[doc = " Panics in `pre_start` do not invoke the"]
    #[doc = " supervision strategy and the actor won\'t be started. [Actor]::`spawn`"]
    #[doc = " will return an error to the caller"]
    #[doc = ""]
    #[doc = " * `myself` - A handle to the [ActorCell] representing this actor"]
    #[doc = " * `args` - Arguments that are passed in the spawning of the actor which might"]
    #[doc = " be necessary to construct the initial state"]
    #[doc = ""]
    #[doc = " Returns an initial [Actor::State] to bootstrap the actor"]
    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: TopicManagerArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        //
        // Try to reinit from a predermined list of topics
        let mut state  = TopicManagerState{
            topics: HashMap::new()            
        };
        
        if let Some(topics) = args.topics {
            for topic in topics {
                //start topic actors for that topic
                
                match Actor::spawn_linked(Some(topic.clone()), TopicAgent, TopicAgentArgs {subscribers: None}, myself.clone().into()).await {
                    Ok((actor, _)) => {
                        state.topics.insert(topic.clone(), actor.clone());
                    },
                    Err(_) => error!("Failed to start actor for topic {topic}"),
                }

            }
        }        
        debug!("Starting {myself:?}");
        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
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
            BrokerMessage::PublishRequest { registration_id, topic, payload } => {
                match registration_id {
                    Some(registration_id) => {
                 //forward request to topic agent
                 if let Some(topic_actor) = state.topics.get(&topic.clone()) {
                    topic_actor.send_message(BrokerMessage::PublishRequest { registration_id: Some(registration_id), topic , payload}).expect("Failed for forward publish request")
                } else {
                    warn!("No agent set to handle topic: {topic}, starting new agent...");    
                    

                    match Actor::spawn_linked(Some(topic.clone()), TopicAgent, TopicAgentArgs {subscribers: None }, myself.clone().into()).await {
                    
                    Ok((actor, _)) => {
                        state.topics.insert(topic.clone(), actor.clone());
                    },
                    Err(e) => {
                        tracing::error!("Failed to start actor for topic: {topic}");
                        //TOOD: send error message here
                        match myself.try_get_supervisor() {
                            Some(broker) => broker.send_message(BrokerMessage::ErrorMessage{ client_id: registration_id.clone(), error: e.to_string()}).unwrap(),
                            None => todo!()
                        }
                    }
                    }
                }
                    },
                None => warn!("Received publish request from unknown session. {payload}")
                }
            },
            BrokerMessage::PublishResponse { topic, payload, .. } => {
                //forward to broker
                match myself.try_get_supervisor(){
                    Some(broker) => broker.send_message(BrokerMessage::PublishResponse { topic, payload, result: Result::Ok(()) })
                    .expect("Expected to forward message to broker"),
                    None => todo!()
                }
            }
            BrokerMessage::AddTopic {reply, registration_id, topic } => {
                // add some topic, optionally on behalf of a session
                if let Some(registration_id) = registration_id {
                    let subscribers = vec![format!("{}:{}", registration_id.clone(), topic.clone())];
                    
                    match Actor::spawn_linked(Some(
                        topic.clone()),
                        TopicAgent,
                        TopicAgentArgs{ subscribers: Some(subscribers) },
                        myself.clone().into()).await {
                            Ok((actor, _)) => reply.send(Ok(actor.clone())).expect("{BROKER_NOT_FOUND_TXT}"),
                            Err(e) => reply.send(Err(format!("{TOPIC_ADD_FAILED_TXT}: {e}"))).expect("{BROKER_NOT_FOUND_TXT}")
                        }                     
                } else { 
                    // Just create a new topic agent
                    match Actor::spawn_linked(Some(
                        topic.clone()),
                        TopicAgent,
                        TopicAgentArgs{ subscribers: None },
                        myself.clone().into()).await {
                            Ok((actor,_)) => reply.send(Ok(actor.clone())).expect("{BROKER_NOT_FOUND_TXT}"),
                            Err(e) => reply.send(Err(format!("{TOPIC_ADD_FAILED_TXT}: {e}"))).expect("{BROKER_NOT_FOUND_TXT}")
                        }
                 }
            }
            _ => warn!(UNEXPECTED_MESSAGE_STR)
        }
        Ok(())
    }
}

// ============================== Topic Worker definition ============================== //
/// Our worker process for managing message queues on a given topic
/// The broker supervisor is generally notified of incoming messages on the message queue.
struct TopicAgent;

struct TopicAgentState {
    subscribers: Vec<String>,
}

pub struct TopicAgentArgs {
    /// Mapping of subscribers to 
    pub subscribers: Option<Vec<String>>
}


#[async_trait]
impl Actor for TopicAgent {

    #[doc = " The message type for this actor"]
    type Msg = BrokerMessage;

    #[doc = " The type of state this actor manages internally"]
    type State = TopicAgentState;

    #[doc = " Initialization arguments"]
    type Arguments = TopicAgentArgs;


    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: TopicAgentArgs
    ) -> Result<Self::State, ActorProcessingErr> {

        let subscribers = args.subscribers.unwrap_or_default();

        let state: TopicAgentState  = TopicAgentState {subscribers};
        
        debug!("Starting... {myself:?}");

        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
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
            BrokerMessage::PublishRequest{registration_id,topic,payload} => {
                match registration_id {
                    Some(registration_id) => {
                        debug!("{myself:?}: New message from {0}: {1}", registration_id, payload);
                        
                        //alert subscribers
                        for subscriber in &state.subscribers {
                            if let Some(actor) = where_is(subscriber.to_string()) {
                                actor.send_message(BrokerMessage::PublishResponse {
                                    topic: topic.clone(),
                                    payload: payload.clone(),
                                    result: Ok(())
                                })
                                .map_err(|e| {
                                    warn!("{PUBLISH_REQ_FAILED_TXT}: {e}")
                                }).unwrap();
                            } else { warn!("{PUBLISH_REQ_FAILED_TXT}: failed to lookup subscriber for {registration_id}") }
                        }

                        //send ACK to session that made the request
                        match where_is(registration_id.clone()) {
                            Some(session) => {
                                session.send_message(BrokerMessage::PublishRequestAck(topic))
                                .map_err(|e| { warn!("Failed to send publish Ack to session! {e}") })
                                .unwrap();
                            }
                            None => warn!("Failed to lookup session {registration_id}")
                        }
                    }, 
                    None => {
                        warn!("Received publish request from unknown session: {payload}");
                        //TODO: send error message
                    }
                }
            }
            BrokerMessage::SubscribeRequest { registration_id, topic } => {
                if let Some(registration_id) = registration_id {
                    
                    match where_is(registration_id.clone()) {
                        Some(session) => {
                            state.subscribers.push(registration_id.clone());
                            //send ack
                            if let Err(e) = session.send_message(BrokerMessage::SubscribeAcknowledgment { registration_id, topic, result: Ok(()) }) {
                                warn!("{SUBSCRIBE_REQUEST_FAILED_TXT}: {e}");
                            }
                            
                        },
                        None => todo!()
                    }
                }
        }
            _ => {
                warn!("{}", format!("{UNEXPECTED_MESSAGE_STR}: {message:?}"))
            }
        }
        Ok(())
    }
}