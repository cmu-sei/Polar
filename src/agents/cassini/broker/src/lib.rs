use std::{env, fmt};
pub mod broker;
pub mod control;
pub mod listener;
pub mod session;
pub mod subscriber;
pub mod topic;

/// Constant names of core supervisors, should remain constant
/// as they are used to conduct lookups in the ractor registry
pub const BROKER_NAME: &str = "BROKER_SUPERVISOR";
pub const LISTENER_MANAGER_NAME: &str = "LISTENER_MANAGER";
pub const SESSION_MANAGER_NAME: &str = "SESSION_MANAGER";
pub const TOPIC_MANAGER_NAME: &str = "TOPIC_MANAGER";
pub const SUBSCRIBER_MANAGER_NAME: &str = "SUBSCRIBER_MANAGER";
pub const CONTROL_MANAGER_NAME: &str = "CONTROL_MANAGER";
pub const ACTOR_STARTUP_MSG: &str = "Started {myself:?}";
pub const UNEXPECTED_MESSAGE_STR: &str = "Received unexpected message!";
pub const SESSION_NOT_NAMED: &str = "Expected session to have been named.";
pub const SESSION_MISSING_REASON_STR: &str = "SESSION_MISSING";
pub const SESSION_NOT_FOUND_TXT: &str = "Session not found!";
pub const CLIENT_NOT_FOUND_TXT: &str = "Listener not found!";
pub const TOPIC_MGR_NOT_FOUND_TXT: &str = "Topic Manager not found!";
pub const SUBSCRIBER_MGR_NOT_FOUND_TXT: &str = "Subscription Manager not found!";
pub const SESSION_MGR_NOT_FOUND_TXT: &str = "Session Manager not found!";
pub const BROKER_NOT_FOUND_TXT: &str = "Broker not found!";
pub const SUBSCRIBE_REQUEST_FAILED_TXT: &str = "Failed to subscribe to topic";
pub const PUBLISH_REQ_FAILED_TXT: &str = "Failed to publish message to topic";
pub const REGISTRATION_REQ_FAILED_TXT: &str = "Failed to register session!";
pub const LISTENER_MGR_NOT_FOUND_TXT: &str = "Listener Manager not found!";
pub const TIMEOUT_REASON: &str = "SESSION_TIMEDOUT";
pub const DISCONNECTED_REASON: &str = "CLIENT_DISCONNECTED";
pub const SHUTDOWN_AUTH_TOKEN: &str = "BROKER_SHUTDOWN_TOKEN";

#[derive(Debug, Clone)]
pub enum BrokerConfigError {
    EnvVar { var: String, source: env::VarError },
}

impl fmt::Display for BrokerConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use crate::BrokerConfigError::EnvVar;
        match self {
            EnvVar { var, source } => {
                write!(f, "{source}. Pleaes provide a value for {var}")
            }
        }
    }
}

///Consider a different naming convention for the subscribers, right now they're named directly after the session they represent and the topic they subscribe to
/// IF we wanted to support topics subscribing to topics e.g overloading the type of subscriber topics can have, we will want to reconsider this approach.
pub fn get_subscriber_name(registration_id: &str, topic: &str) -> String {
    format!("{0}:{1}", registration_id, topic)
}

pub fn parse_host_and_port(endpoint: &str) -> Result<(String, u16), String> {
    // Add scheme if missing so Url::parse works
    let formatted = if endpoint.contains("://") {
        endpoint.to_string()
    } else {
        format!("https://{}", endpoint) // dummy scheme
    };

    let url = url::Url::parse(&formatted).map_err(|e| format!("Invalid endpoint URL: {}", e))?;

    let host = url
        .host_str()
        .ok_or_else(|| "No host found in endpoint".to_string())?
        .to_string();

    let port = url
        .port_or_known_default()
        .ok_or_else(|| "No port found and no default for scheme".to_string())?;

    Ok((host, port))
}
