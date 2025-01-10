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
pub const SUBSCRIBE_REQUEST_FAILED_TXT: &str = "Failed to subscribe to topic: \"{topic}\"";
pub const PUBLISH_REQ_FAILED_TXT: &str = "Failed to publish message to topic \"{topic}\"";
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
        payload: String,
    },
    /// Publish response to the client.
    PublishResponse {
        topic: String,
        payload: String,
        result: Result<(), String>, // Ok for success, Err with error message
    },
    PublishRequestAck(String),
    PublishResponseAck,
    /// Subscribe request from the client.
    // This request originates externally, so a registration_id is not added until it is received by the session
    SubscribeRequest {
        registration_id: Option<String>, //TODO: Remove option
        topic: String,
    },
    AddTopic {
        reply: RpcReplyPort<Result<ActorRef<BrokerMessage>, String>>,
        registration_id: Option<String>,
        topic: String
        
    },
    PushMessage {
        reply: RpcReplyPort<Result<(), String>>,
        payload: String,
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
        registration_id: Option<String>, //TODO: Remove option
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
            payload: String,
            registration_id: Option<String>
        },
        /// Publish response to the client.
        PublishResponse { topic: String, payload: String, result: Result<(), String> },
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
            topic: String,
        },
        UnsubscribeAcknowledgment {
            topic: String,
            result: Result<(), String>
       },
        /// Ping message to the client to check connectivity.
        PingMessage,
        /// Pong message received from the client in response to a ping.
        PongMessage,
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
            ClientMessage::UnsubscribeRequest {  topic } => {
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

pub fn init_logging() {
    let dir = tracing_subscriber::filter::Directive::from(tracing::Level::DEBUG);

    use std::io::stderr;
    use std::io::IsTerminal;
    use tracing_glog::Glog;
    use tracing_glog::GlogFields;
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    let fmt = tracing_subscriber::fmt::Layer::default()
        .with_ansi(stderr().is_terminal())
        .with_writer(std::io::stderr)
        .event_format(Glog::default().with_timer(tracing_glog::LocalTime::default()))
        .fmt_fields(GlogFields::default().compact());

    let filter = vec![dir]
        .into_iter()
        .fold(EnvFilter::from_default_env(), |filter, directive| {
            filter.add_directive(directive)
        });

    let subscriber = Registry::default().with(filter).with(fmt);
    tracing::subscriber::set_global_default(subscriber).expect("to set global subscriber");
}

pub fn get_subsciber_name(registration_id: &str, topic: &str) -> String { format!("{0}:{1}", registration_id, topic) }

// pub fn try_get_session(registration_id: String) -> Option<ActorRef<BrokerMessage>> {
//     match &where_is(registration_id.clone()) {
//         Some(session) => {
//             Some(ActorRef::from(session.to_owned()))
//         }, 
//         None => {
//             warn!("Session {registration_id} not found!");
//             None
//         }
//     }
// }