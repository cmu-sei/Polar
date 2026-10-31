/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

#![deny(clippy::disallowed_methods)]
pub mod agent;
pub mod authority;
pub mod broker;
pub mod budget;
pub mod budget_report;
pub mod control;
pub mod deliveries;
pub mod listener;
pub mod pressure;
pub mod session;
pub mod settings;
pub mod subscriber;
pub mod topic;

/// Constant names of core supervisors, should remain constant
/// as they are used to conduct lookups in the ractor registry
/// How long a session whose client has gone away is kept, with what was held for it, before it is
/// ended. A client that returns inside this window resumes its session and is replayed what it
/// missed.
pub const DEFAULT_SESSION_RETENTION: std::time::Duration = std::time::Duration::from_secs(600);

pub const BROKER_NAME: &str = "BROKER_SUPERVISOR";
pub const LISTENER_MANAGER_NAME: &str = "LISTENER_MANAGER";
pub const SESSION_MANAGER_NAME: &str = "SESSION_MANAGER";
pub const TOPIC_MANAGER_NAME: &str = "TOPIC_MANAGER";
pub const SUBSCRIBER_MANAGER_NAME: &str = "SUBSCRIBER_MANAGER";
pub const CONTROL_MANAGER_NAME: &str = "CONTROL_MANAGER";
pub const PRESSURE_MANAGER_NAME: &str = "PRESSURE_MANAGER";
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
