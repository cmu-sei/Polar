use rkyv::{Archive, Deserialize, Serialize};

/// External Messages for client comms
/// These messages are serialized/deserialized to/from JSON
#[derive(Serialize, Deserialize, Archive, Debug, Clone)]
// #[serde(tag = "type", content = "data")]
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
}
