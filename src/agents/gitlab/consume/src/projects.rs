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

use crate::{GitlabConsumerState, GitlabNodeKey, UNKNOWN_FILED};
use cassini_client::{TcpClient, TcpClientMessage};
use polar::{
    graph::{GraphController, GraphControllerMsg, GraphOp, GraphValue, Property},
    GitRepositoryDiscoveredEvent, GIT_REPO_DISCOGERY_TOPIC,
};
use tracing::trace;

use common::{
    types::{GitlabData, GitlabEnvelope},
    PROJECTS_CONSUMER_TOPIC,
};
use gitlab_queries::projects::Project;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use rkyv::to_bytes;
use tracing::{debug, info};

pub struct GitlabProjectConsumer;

impl GitlabProjectConsumer {
    fn handle_projects(
        tcp_client: &TcpClient,
        graph_controller: &GraphController<GitlabNodeKey>,
        instance_id: String,
        projects: &[Project],
    ) -> Result<(), ActorProcessingErr> {
        let instance_k = GitlabNodeKey::GitlabInstance {
            instance_id: instance_id.clone(),
        };

        for project in projects {
            Self::emit_repo_discovered(tcp_client, project)?;

            let project_k = GitlabNodeKey::Project {
                instance_id: instance_id.clone(),
                project_id: project.id.to_string(),
            };

            let created_at = project
                .created_at
                .clone()
                .map_or(GraphValue::String(UNKNOWN_FILED.to_string()), |d| {
                    GraphValue::String(d.to_string())
                });
            let last_activity_at = project
                .last_activity_at
                .clone()
                .map_or(GraphValue::String(UNKNOWN_FILED.to_string()), |d| {
                    GraphValue::String(d.to_string())
                });

            graph_controller.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: project_k.clone(),
                props: vec![
                    Property("name".into(), GraphValue::String(project.name.clone())),
                    Property(
                        "full_path".into(),
                        GraphValue::String(project.full_path.to_string()),
                    ),
                    Property("created_at".into(), created_at),
                    Property("last_activity_at".into(), last_activity_at),
                    Property(
                        "http_url_to_repo".into(),
                        GraphValue::String(project.http_url_to_repo.clone().unwrap_or_default()),
                    ),
                    Property(
                        "ssh_url_to_repo".into(),
                        GraphValue::String(project.ssh_url_to_repo.clone().unwrap_or_default()),
                    ),
                ],
            }))?;

            graph_controller.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: instance_k.clone(),
                to: project_k,
                rel_type: "OBSERVED_PROJECT".into(),
                props: Vec::default(),
            }))?;
        }
        Ok(())
    }

    fn emit_repo_discovered(
        tcp_client: &TcpClient,
        project: &Project,
    ) -> Result<(), ActorProcessingErr> {
        if project.http_url_to_repo.is_none() && project.ssh_url_to_repo.is_none() {
            return Ok(());
        }

        let event = GitRepositoryDiscoveredEvent {
            http_url: project.http_url_to_repo.clone(),
            ssh_url: project.ssh_url_to_repo.clone(),
        };

        let payload = to_bytes::<rkyv::rancor::Error>(&event)?.to_vec();
        trace!("emitting event {event:?}");
        tcp_client.cast(TcpClientMessage::Publish {
            topic: GIT_REPO_DISCOGERY_TOPIC.to_string(),
            payload,
            trace_ctx: None,
        })?;

        Ok(())
    }
}

#[async_trait]
impl Actor for GitlabProjectConsumer {
    type Msg = GitlabEnvelope;
    type State = GitlabConsumerState;
    type Arguments = GitlabConsumerState;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        state.tcp_client.cast(TcpClientMessage::Subscribe {
            topic: PROJECTS_CONSUMER_TOPIC.to_string(),
            trace_ctx: None,
        })?;
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
        if let GitlabData::Projects(projects) = message.data {
            debug!("Handling projects {:?}", projects.len());
            Self::handle_projects(
                &state.tcp_client,
                &state.graph_controller,
                message.instance_id.clone(),
                &projects,
            )?;
        }

        Ok(())
    }
}
