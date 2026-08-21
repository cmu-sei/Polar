/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

use cassini_client::TcpClientMessage;
use cassini_types::WireTraceCtx;
use polar::graph::controller::GraphController;
use ractor::registry::where_is;
use std::error::Error;

pub mod groups;
pub mod issues;
pub mod projects;
pub mod supervisor;
pub mod users;

pub const BROKER_CLIENT_NAME: &str = "JIRA_CONSUMER_CLIENT";
pub const JIRA_USER_CONSUMER: &str = "users";
pub const JIRA_ISSUE_CONSUMER: &str = "issues";

pub struct JiraConsumerState {
    pub registration_id: String,
    pub graph_controller: GraphController,
}
#[derive(Clone)]
pub struct JiraConsumerArgs {
    pub registration_id: String,
    pub graph_controller: GraphController,
}

///
/// Helper fn to set up consumer state and subscribe to a given topic.
/// No longer connects to the graph database directly -- the supervisor
/// owns the single shared GraphControllerActor and injects a reference to
/// it via JiraConsumerArgs.graph_controller instead.
pub async fn subscribe_to_topic(
    registration_id: String,
    topic: String,
    graph_controller: GraphController,
) -> Result<JiraConsumerState, Box<dyn Error>> {
    let client = where_is(BROKER_CLIENT_NAME.to_string()).expect("Expected to find TCP client.");

    client.send_message(TcpClientMessage::Subscribe {
        topic,
        trace_ctx: WireTraceCtx::from_current_span(),
    })?;

    Ok(JiraConsumerState {
        registration_id,
        graph_controller,
    })
}

#[derive(Debug)]
pub enum CrashReason {
    CaCertReadError(String), // error when reading CA cert
    SubscriptionError(String), // error when subscribing to the topic
                             // Add other error types as necessary
}
