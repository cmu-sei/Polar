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

use crate::{GitlabConsumerState, GitlabNodeKey};
use common::types::{GitlabData, GitlabEnvelope};
use common::RUNNERS_CONSUMER_TOPIC;
use gitlab_queries::runners::CiRunner;
use polar::graph::{GraphControllerMsg, GraphOp, GraphValue, Property};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, info};

use cassini_client::TcpClientMessage;
pub struct GitlabRunnerConsumer;

impl GitlabRunnerConsumer {
    /// Handles a batch of runners observed from a GitLab instance.
    ///
    /// Semantics:
    /// - Ensures GitlabInstance node exists
    /// - Ensures each GitlabRunner node exists
    /// - Upserts runner properties
    /// - Ensures (instance)-[:OBSERVED_RUNNER]->(runner)
    ///
    /// This function performs no I/O.
    /// It emits GraphOp messages to the GraphController actor.
    /// This makes it deterministic and unit-testable.
    pub fn handle_runners(
        instance_id: String,
        runners: &[CiRunner],
        graph: &ActorRef<GraphControllerMsg<GitlabNodeKey>>,
    ) -> Result<(), ActorProcessingErr> {
        let instance_key = GitlabNodeKey::GitlabInstance {
            instance_id: instance_id.clone(),
        };

        // Ensure instance exists
        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: instance_key.clone(),
            props: vec![],
        }))?;

        for runner in runners {
            let runner_key = GitlabNodeKey::Runner {
                instance_id: instance_id.clone(),
                runner_id: runner.id.0.to_string(),
            };

            let props = vec![
                Property("paused".into(), GraphValue::Bool(runner.paused)),
                Property(
                    "runner_type".into(),
                    GraphValue::String(format!("{:?}", runner.runner_type)),
                ),
                Property(
                    "status".into(),
                    GraphValue::String(format!("{:?}", runner.status)),
                ),
                Property(
                    "access_level".into(),
                    GraphValue::String(format!("{:?}", runner.access_level)),
                ),
                Property("run_untagged".into(), GraphValue::Bool(runner.run_untagged)),
                Property(
                    "tag_list".into(),
                    GraphValue::String(runner.tag_list.clone().unwrap_or_default().join(",")),
                ),
            ];

            // Upsert runner node with properties
            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: runner_key.clone(),
                props,
            }))?;

            // Ensure relationship
            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: instance_key.clone(),
                to: runner_key,
                rel_type: "OBSERVED_RUNNER".into(),
                props: vec![],
            }))?;
        }

        Ok(())
    }
}
#[async_trait]
impl Actor for GitlabRunnerConsumer {
    type Msg = GitlabEnvelope;
    type State = GitlabConsumerState;
    type Arguments = GitlabConsumerState;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        // fire off subscribe message
        state.tcp_client.cast(TcpClientMessage::Subscribe(
            RUNNERS_CONSUMER_TOPIC.to_string(),
        ))?;

        debug!("{myself:?} starting");
        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("{:?} waiting to consume", myself.get_name());

        Ok(())
    }
    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if let GitlabData::Runners(runners) = message.data {
            Self::handle_runners(message.instance_id, &runners, &state.graph_controller)?;
        }

        Ok(())
    }
}
