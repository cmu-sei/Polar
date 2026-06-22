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

use cassini_client::TcpClient;
use common::types::GitlabEnvelope;
use neo4rs::BoltType;
use polar::graph::controller::GraphController;
use ractor::ActorRef;

pub mod groups;
pub mod meta;
pub mod pipelines;
pub mod projects;
pub mod repositories;
pub mod runners;
pub mod supervisor;
pub mod users;
pub type GitlabConsumer = ActorRef<GitlabEnvelope>;
pub const BROKER_CLIENT_NAME: &str = "GITLAB_CONSUMER_CLIENT";
pub const GITLAB_USER_CONSUMER: &str = "users";

// in case we unwrap optional fields
pub const UNKNOWN_FILED: &str = "unknown";

#[derive(Clone)]
pub struct GitlabConsumerState {
    tcp_client: TcpClient,
    graph_controller: GraphController,
}

#[derive(Debug)]
pub enum CrashReason {
    CaCertReadError(String), // error when reading CA cert
    SubscriptionError(String), // error when subscribing to the topic
                             // Add other error types as necessary
}
