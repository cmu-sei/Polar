use ractor::{ActorRef, RpcReplyPort};
use serde::{Deserialize, Serialize};

pub mod topic;
pub mod listener;
pub mod broker;
pub mod session;
pub mod subscriber;
pub mod client;

/// Constant names of core supervisors, should remain constant
/// as they are used to conduct lookups in the ractor registry
pub const BROKER_NAME: &str = "BROKER_SUPERVISOR";
pub const LISTENER_MANAGER_NAME: &str = "LISTENER_MANAGER";
pub const SESSION_MANAGER_NAME: &str = "SESSION_MANAGER";
pub const TOPIC_MANAGER_NAME: &str = "TOPIC_MANAGER";
pub const SUBSCRIBER_MANAGER_NAME: &str = "SUBSCRIBER_MANAGER";

pub const ACTOR_STARTUP_MSG: &str =  "Started {myself:?}";
pub const UNEXPECTED_MESSAGE_STR: &str = "Received unexpected message!";

pub const SESSION_MISSING_REASON_STR: &str = "SESSION_MISSING";
pub const SESSION_NOT_FOUND_TXT: &str = "Session not found!";
pub const CLIENT_NOT_FOUND_TXT: &str = "Listener not found!";
pub const TOPIC_MGR_NOT_FOUND_TXT: &str = "Topic Manager not found!";
pub const SUBSCRIBER_MGR_NOT_FOUND_TXT: &str = "Subscription Manager not found!";
pub const BROKER_NOT_FOUND_TXT: &str = "Broker not found!";
pub const SUBSCRIBE_REQUEST_FAILED_TXT: &str = "Failed to subscribe to topic";
pub const PUBLISH_REQ_FAILED_TXT: &str = "Failed to publish message to topic";
pub const REGISTRATION_REQ_FAILED_TXT: &str = "Failed to register session!";
pub const LISTENER_MGR_NOT_FOUND_TXT: & str = "Listener Manager not found!";
pub const TIMEOUT_REASON: &str = "SESSION_TIMEDOUT";
pub const DISCONNECTED_REASON: &str = "CLIENT_DISCONNECTED";
/// Internal messagetypes for the Broker.
/// 
#[derive(Debug)]
pub enum BrokerMessage {
    /// Registration request from the client.
    /// When a client connects over TCP, it cannot send messages until it receives a registrationID and a session has been created for it
    /// In the event of a disconnect, a client should be able to either resume their session by providing that registration ID, or
    /// have a new one assigned to it by sending an empty registration request
    RegistrationRequest {
    //Id for a new, potentially unauthenticated/unauthorized client client
    registration_id: Option<String>,
    client_id: String
    },
    /// Registration response to the client after attempting registration
    RegistrationResponse {
        registration_id: Option<String>, //new and final id for a client successfully registered
        client_id: String,
        success: bool,
        error: Option<String>, // Optional error message if registration failed
    },
    /// Publish request from the client.
    PublishRequest {
        registration_id: Option<String>, //TODO: Reemove option, listener checks for registration_id before forwarding
        topic: String,
        payload: Vec<u8>,
    },
    /// Publish response to the client.
    PublishResponse {
        topic: String,
        payload: Vec<u8>,
        result: Result<(), String>, // Ok for success, Err with error message
    },
    PublishRequestAck(String),
    PublishResponseAck,
    /// Subscribe request from the client.
    SubscribeRequest {
        registration_id: Option<String>,
        topic: String,
    },
    /// Sent to the subscriber manager to create a new subscriber actor to handle pushing messages to the client.
    /// If successful, the associated topic actor is notified, adding the id of the new actor to it's subscriber list
    Subscribe {
        reply: RpcReplyPort<Result<String, String>>,
        topic: String,
        registration_id: String
    },
    AddTopic {
        reply: RpcReplyPort<Result<ActorRef<BrokerMessage>, String>>,
        registration_id: Option<String>,
        topic: String
        
    }, 
    /// Sent to session actors to forward messages to their clients.
    /// Messages that fail to be delivered for some reason are kept in their queues.
    PushMessage {
        reply: RpcReplyPort<Result<(), String>>,
        payload: Vec<u8>,
        topic: String,
    },
    /// Subscribe acknowledgment to the client.
    SubscribeAcknowledgment {
        registration_id: String,
        topic: String,
        result: Result<(), String>, // Ok for success, Err with error message
    },
    /// Unsubscribe request from the client.
    UnsubscribeRequest {
        registration_id: Option<String>,
        topic: String,
    },
    /// Unsubscribe acknowledgment to the client.
    UnsubscribeAcknowledgment {
        registration_id: String,
        topic: String,
        result: Result<(), String>, // Ok for success, Err with error message
    },
    /// Disconnect request from the client.
    DisconnectRequest {
        client_id: String,
        registration_id: Option<String>
    },
    /// Error message to the client.
    ErrorMessage {
        client_id: String,
        error: String,
    },
    /// Ping message to the client to check connectivity.
    PingMessage {
        registration_id : String,
        client_id: String
    },
    /// Pong message received from the client in response to a ping.
    PongMessage {
        registration_id: String,
    },
    TimeoutMessage {
        client_id: String,
        registration_id: Option<String>, //name of the session agent that died
        error: Option<String>
    }
}

///External Messages for client comms
/// These messages are serialized/deserialized to/from JSON
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
        RegistrationRequest {
            registration_id: Option<String>,
        },
        RegistrationResponse {
            registration_id: String, //new and final id for a client successfully registered
            success: bool,
            error: Option<String>, // Optional error message if registration failed
        },
        /// Publish request from the client.
        PublishRequest {
            topic: String,
            payload: Vec<u8>,
            registration_id: Option<String>
        },
        /// Publish response to the client.
        PublishResponse { topic: String, payload: Vec<u8>, result: Result<(), String> },
        /// Sent back to actor that made initial publish request
        PublishRequestAck(String),
        SubscribeRequest {
            registration_id: Option<String>,
            topic: String
        },
        /// Subscribe acknowledgment to the client.
        SubscribeAcknowledgment {
            topic: String,
            result: Result<(), String>, // Ok for success, Err with error message
        },
        /// Unsubscribe request from the client.
        UnsubscribeRequest {
            registration_id: Option<String>,
            topic: String,
        },
        UnsubscribeAcknowledgment {
            topic: String,
            result: Result<(), String>
       },
        ///Disconnect, sending a session id to end, if any
        DisconnectRequest(Option<String>),
        ///Mostly for testing purposes, intentional timeout message with a client_id
        TimeoutMessage(Option<String>),
        ErrorMessage(String)
}

impl BrokerMessage {
    pub fn from_client_message(msg: ClientMessage, client_id: String, registration_id: Option<String>) -> Self {
        match msg {
            ClientMessage::RegistrationRequest { registration_id } => {
                BrokerMessage::RegistrationRequest {
                    registration_id,
                    client_id,
                }
            },
            ClientMessage::PublishRequest { topic, payload, registration_id } => {
                BrokerMessage::PublishRequest {
                    registration_id,
                    topic,
                    payload,
                }
            },
            ClientMessage::SubscribeRequest{topic, registration_id} => {
                BrokerMessage::SubscribeRequest {
                    registration_id,
                    topic,
                }
            },
            ClientMessage::UnsubscribeRequest {  registration_id, topic } => {
                BrokerMessage::UnsubscribeRequest {
                    registration_id,
                    topic,
                }
            },
            
            ClientMessage::DisconnectRequest(registration_id) => {
                BrokerMessage::DisconnectRequest {
                    client_id,
                    registration_id
                }
            },
            ClientMessage::TimeoutMessage(registration_id) => BrokerMessage::TimeoutMessage { client_id, registration_id, error: None },
            
            // ClientMessage::PingMessage { client_id } => {
            //     BrokerMessage::PingMessage { client_id }
            // },
            // Handle unexpected messages
            _ => {
                todo!()
            }
        }
    }
}

///TODO: Consider a different naming convention for the subscribers, right now they're named directly after the session they represent and the topic they subscribe to
/// IF we wanted to support topics subscribing to topics e.g overloading the type of subscriber topics can have, we will want to reconsider this approach.
pub fn get_subscriber_name(registration_id: &str, topic: &str) -> String { format!("{0}:{1}", registration_id, topic) }

//TODO: Helper fn to send an error message back to the client
// pub fn raise_error(msg) {
// if let Some(session) = where_is(registration_id.clone()) {
// if let Err(e) = session.send_message(BrokerMessage::SubscribeAcknowledgment { registration_id: registration_id, topic: topic.clone(), result: Err(e) }) {
//     warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {SESSION_NOT_FOUND_TXT} {e}");
// }
// } else { warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {SESSION_NOT_FOUND_TXT} {e}"); }
// }
