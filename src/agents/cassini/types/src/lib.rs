use opentelemetry::Context;
use ractor::{ActorProcessingErr, ActorRef, RpcReplyPort};
use rkyv::{Archive, Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub type SessionMap = HashMap<String, SessionDetails>;

/// Our representation of a connected session, and how close it is to timing out
/// TODO: Eventually, we might want to add metadata to the session struct to track additional information.
/// Consider stuff like last_activity_time, last_message_received_time, etc.
#[derive(Debug, Clone, Serialize, Deserialize, Archive)]
pub struct SessionDetails {
    pub registration_id: String,
    pub subscriptions: HashSet<String>,
    // last_activity: u64,
}

/// Internal messagetypes for the Broker.
/// Activities that flow from an actor will also be traced leveraging Contexts,
/// These are optional because they aren't initialzied until the listener begins to handle the message
/// Because of this, they will be often left out of the listener's handler at first
#[derive(Debug)]
pub enum BrokerMessage {
    /// Registration request from the client.
    /// When a client connects over TCP, it cannot send messages until it receives a registrationID and a session has been created for it
    /// In the event of a disconnect, a client should be able to either resume their session by providing that registration ID, or
    /// have a new one assigned to it by sending an empty registration request
    RegistrationRequest {
        //Id for a new, potentially unauthenticated/unauthorized client client
        registration_id: Option<String>,
        client_id: String,
        trace_ctx: Option<Context>,
    },
    /// Helper variant to start a session and re/initialize it with a new client connection + tracing context
    InitSession {
        client_id: String,
        trace_ctx: Option<Context>,
    },
    GetSessions {
        reply_to: RpcReplyPort<SessionMap>,
        trace_ctx: Option<Context>,
    },
    /// A heartbeat tick messgae sent by sessions to track uptime
    HeartbeatTick,
    /// Registration response to the client after attempting registration
    /// Ok result contains new registration id,
    /// Err shoudl contain an error message
    RegistrationResponse {
        client_id: String,
        result: Result<String, String>,
        trace_ctx: Option<Context>,
    },
    /// Publish request from the client.
    PublishRequest {
        registration_id: String,
        topic: String,
        payload: Vec<u8>,
        trace_ctx: Option<Context>,
    },
    /// Publish response to the client.
    PublishResponse {
        topic: String,
        payload: Vec<u8>,
        result: Result<(), String>,
        trace_ctx: Option<Context>,
    },
    /// Message sent to the client to let them know they successfully published a message
    PublishRequestAck {
        topic: String,
        trace_ctx: Option<Context>,
    },
    PublishResponseAck,
    /// Subscribe request from the client.
    SubscribeRequest {
        registration_id: String,
        topic: String,
        trace_ctx: Option<Context>,
    },
    /// Sent to the subscriber manager to create a new subscriber actor to handle pushing messages to the client.
    /// If successful, the associated topic actor is notified, adding the id of the new actor to it's subscriber list
    CreateSubscriber {
        topic: String,
        registration_id: String,
        trace_ctx: Option<Context>,
        reply: RpcReplyPort<Result<ActorRef<BrokerMessage>, ActorProcessingErr>>,
    },
    AddSubscriber {
        subscriber_ref: ActorRef<BrokerMessage>,
        trace_ctx: Option<Context>,
    },
    /// instructs the topic manager to create a new topic actor,
    /// optionally at the behest of a session client during the processing of a SubscribeRequest
    /// which would also prompt the creation of a subscriber agent for that topic.
    AddTopic {
        registration_id: Option<String>,
        topic: String,
        trace_ctx: Option<Context>,
    },
    GetTopics {
        registration_id: String,
        reply_to: RpcReplyPort<HashSet<String>>,
        trace_ctx: Option<Context>,
    },
    /// Sent to session actors to forward messages to their clients.
    /// Messages that fail to be delivered for some reason are kept in their queues.
    PushMessage {
        // reply: RpcReplyPort<Result<(), String>>,
        payload: Vec<u8>,
        topic: String,
        trace_ctx: Option<Context>,
    },
    /// Sent back to subscription actors if sessions fail to forward messages to the client for requeueing
    PushMessageFailed {
        payload: Vec<u8>,
    },
    /// Subscribe acknowledgment to the client.
    SubscribeAcknowledgment {
        registration_id: String,
        topic: String,
        result: Result<(), String>, // Ok for success, Err with error message
        trace_ctx: Option<Context>,
    },
    /// Unsubscribe request from the client.
    UnsubscribeRequest {
        registration_id: String,
        topic: String,
        trace_ctx: Option<Context>,
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
        registration_id: Option<String>,
        trace_ctx: Option<Context>,
    },
    /// Error message to the client.
    ErrorMessage {
        client_id: String,
        error: String,
    },
    TimeoutMessage {
        client_id: String,
        ///name of the session agent that died
        registration_id: String,
        error: Option<String>,
        // trace_ctx: Option<Context>,
    },
    /// control-plane message, reply_to is session ActorRef
    ControlRequest {
        registration_id: String,
        op: ControlOp,
        reply_to: Option<ActorRef<BrokerMessage>>, // session actor ref
        trace_ctx: Option<Context>,
    },
    // Control response forwarded back across broker actor layers (or directly by ControlManager)
    ControlResponse {
        registration_id: String,
        result: Result<ControlResult, ControlError>,
        trace_ctx: Option<Context>,
    },
}

impl BrokerMessage {
    pub fn from_client_message(msg: ClientMessage, client_id: String) -> Self {
        match msg {
            ClientMessage::RegistrationRequest { registration_id } => {
                BrokerMessage::RegistrationRequest {
                    registration_id,
                    client_id,
                    trace_ctx: None,
                }
            }
            ClientMessage::PublishRequest {
                topic,
                payload,
                registration_id,
            } => BrokerMessage::PublishRequest {
                registration_id,
                topic,
                payload,
                trace_ctx: None,
            },
            ClientMessage::SubscribeRequest {
                topic,
                registration_id,
            } => BrokerMessage::SubscribeRequest {
                registration_id,
                topic,
                trace_ctx: None,
            },
            ClientMessage::UnsubscribeRequest {
                registration_id,
                topic,
            } => BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                trace_ctx: None,
            },

            ClientMessage::DisconnectRequest(registration_id) => BrokerMessage::DisconnectRequest {
                client_id,
                registration_id,
                trace_ctx: None,
            },
            ClientMessage::ControlRequest {
                registration_id,
                op,
            } => BrokerMessage::ControlRequest {
                registration_id,
                op,
                reply_to: None,
                trace_ctx: None,
            },
            // Handle unexpected messages
            _ => {
                todo!("Handle a conversion case between broker messages and client messages")
            }
        }
    }
}

// ========================
// Control Operation Types
// ========================
#[derive(Debug, Clone, Serialize, Archive, Deserialize)]
pub enum ControlOp {
    // Session Management
    GetSessionInfo { registration_id: String },
    DisconnectSession { registration_id: String },
    ListSessions,

    // Topic / Subscription Management
    ListTopics,
    ListSubscribers { topic: String },

    // Broker / System
    GetBrokerStats,
    ShutdownBroker { graceful: bool },

    // Diagnostics
    Ping,
}

#[derive(Debug, Clone, Serialize, Archive, Deserialize)]
pub enum ControlResult {
    SessionInfo(SessionDetails),
    SessionList(SessionMap),
    SubscriberList(Vec<String>),
    TopicList(HashSet<String>),
    // BrokerStats(BrokerStats),
    Pong,
    Disconnected,
    ShutdownInitiated,
}

#[derive(Debug, Clone, Serialize, Archive, Deserialize)]
pub enum ControlError {
    NotFound(String),
    PermissionDenied(String),
    InternalError(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Archive)]
pub struct SessionSummary {
    pub registration_id: String,
}

/// External Messages for client comms
/// These messages are serialized/deserialized to/from JSON
#[derive(Serialize, Deserialize, Archive, Debug, Clone)]
pub enum ClientMessage {
    RegistrationRequest {
        registration_id: Option<String>,
    },
    /// Response to an attempt to register.
    /// Ok contains the value of the registration id,
    /// Err should contain an error message
    RegistrationResponse {
        result: Result<String, String>,
    },
    /// Publish request from the client.
    PublishRequest {
        topic: String,
        payload: Vec<u8>,
        registration_id: String,
    },
    /// Publish response to the client.
    PublishResponse {
        topic: String,
        payload: Vec<u8>,
        result: Result<(), String>,
    },
    /// Sent back to actor that made initial publish request
    PublishRequestAck(String),
    SubscribeRequest {
        registration_id: String,
        topic: String,
    },
    /// Subscribe acknowledgment to the client.
    SubscribeAcknowledgment {
        topic: String,
        result: Result<(), String>, // Ok for success, Err with error message
    },
    /// Unsubscribe request from the client.
    UnsubscribeRequest {
        registration_id: String,
        topic: String,
    },
    UnsubscribeAcknowledgment {
        topic: String,
        result: Result<(), String>,
    },
    ///Disconnect, sending a session id to end, if any
    DisconnectRequest(Option<String>),
    ErrorMessage(String),
    ControlRequest {
        registration_id: String,
        op: ControlOp,
    },
    ControlResponse {
        registration_id: String,
        result: Result<ControlResult, ControlError>,
    },
}
