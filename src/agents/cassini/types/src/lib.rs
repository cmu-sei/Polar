use rkyv::{Archive, Deserialize, Serialize};

/// External Messages for client comms
/// These messages are serialized/deserialized to/from JSON
#[derive(Serialize, Deserialize, Archive, Debug, Clone)]
// #[serde(tag = "type", content = "data")]
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
        registration_id: Option<String>,
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
        registration_id: Option<String>,
        topic: String,
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
        result: Result<(), String>,
    },
    ///Disconnect, sending a session id to end, if any
    DisconnectRequest(Option<String>),
    ///Mostly for testing purposes, intentional timeout message with a client_id
    TimeoutMessage(Option<String>),
    ErrorMessage(String),
}
