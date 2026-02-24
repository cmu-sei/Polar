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
use common::GROUPS_CONSUMER_TOPIC;
use gitlab_queries::groups::{GroupData, GroupMember};
use gitlab_queries::projects::ProjectCoreFragment;
use gitlab_queries::runners::CiRunnerIdFragment;
use polar::graph::{GraphController, GraphControllerMsg, GraphOp, GraphValue, Property};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use tracing::debug;

pub struct GitlabGroupConsumer;

impl GitlabGroupConsumer {
    pub fn handle_groups(
        instance_id: String,
        groups: &[GroupData],
        graph: &GraphController<GitlabNodeKey>,
    ) -> Result<(), ActorProcessingErr> {
        let instance_key = GitlabNodeKey::GitlabInstance {
            instance_id: instance_id.clone(),
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: instance_key.clone(),
            props: vec![],
        }))?;

        for group in groups {
            let group_key = GitlabNodeKey::Group {
                instance_id: instance_id.clone(),
                group_id: group.id.to_string(),
            };

            let mut props = vec![
                Property(
                    "full_name".into(),
                    GraphValue::String(group.full_name.to_string()),
                ),
                Property(
                    "full_path".into(),
                    GraphValue::String(group.full_path.to_string()),
                ),
            ];

            if let Some(created_at) = &group.created_at {
                props.push(Property(
                    "created_at".into(),
                    GraphValue::String(created_at.to_string()),
                ));
            }

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: group_key.clone(),
                props,
            }))?;

            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: instance_key.clone(),
                to: group_key,
                rel_type: "OBSERVED_GROUP".into(),
                props: vec![],
            }))?;
        }

        Ok(())
    }

    pub fn handle_group_members(
        instance_id: String,
        group_id: String,
        memberships: &[GroupMember],
        graph: &GraphController<GitlabNodeKey>,
    ) -> Result<(), ActorProcessingErr> {
        let group_key = GitlabNodeKey::Group {
            instance_id: instance_id.clone(),
            group_id,
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: group_key.clone(),
            props: vec![],
        }))?;

        for membership in memberships {
            let Some(user) = &membership.user else {
                continue;
            };

            let user_key = GitlabNodeKey::User {
                instance_id: instance_id.clone(),
                user_id: user.id.to_string(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: user_key.clone(),
                props: vec![],
            }))?;

            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: user_key,
                to: group_key.clone(),
                rel_type: "IN_GROUP".into(),
                props: vec![],
            }))?;
        }

        Ok(())
    }

    /// Handle projects associated with a group.
    ///
    /// Semantics:
    /// - Ensures group node exists
    /// - Ensures each project node exists
    /// - Ensures (project)-[:IN_GROUP]->(group)
    pub fn handle_group_projects(
        instance_id: String,
        group_id: String,
        projects: &[ProjectCoreFragment],
        graph: &ActorRef<GraphControllerMsg<GitlabNodeKey>>,
    ) -> Result<(), ActorProcessingErr> {
        let group_key = GitlabNodeKey::Group {
            instance_id: instance_id.clone(),
            group_id,
        };

        // Ensure group exists
        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: group_key.clone(),
            props: vec![],
        }))?;

        for project in projects {
            let project_key = GitlabNodeKey::Project {
                instance_id: instance_id.clone(),
                project_id: project.id.to_string(),
            };

            // Ensure project exists
            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: project_key.clone(),
                props: vec![],
            }))?;

            // Ensure relationship
            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: project_key,
                to: group_key.clone(),
                rel_type: "IN_GROUP".into(),
                props: vec![],
            }))?;
        }

        Ok(())
    }
    /// Handle runners associated with a group.
    ///
    /// Semantics:
    /// - Ensures group node exists
    /// - Ensures runner node exists
    /// - Sets runner attributes
    /// - Ensures (runner)-[:IN_GROUP]->(group)
    pub fn handle_group_runners(
        instance_id: String,
        group_id: String,
        runners: &[CiRunnerIdFragment],
        graph: &ActorRef<GraphControllerMsg<GitlabNodeKey>>,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Handling runners for group {group_id}");

        let group_key = GitlabNodeKey::Group {
            instance_id: instance_id.clone(),
            group_id,
        };

        // Ensure group exists
        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: group_key.clone(),
            props: vec![],
        }))?;

        for runner in runners {
            let runner_key = GitlabNodeKey::Runner {
                instance_id: instance_id.clone(),
                runner_id: runner.id.0.to_string(),
            };

            // Runner properties
            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: runner_key.clone(),
                props: vec![],
            }))?;

            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: runner_key,
                to: group_key.clone(),
                rel_type: "IN_GROUP".into(),
                props: vec![],
            }))?;
        }

        Ok(())
    }
}

#[async_trait]
impl Actor for GitlabGroupConsumer {
    type Msg = GitlabEnvelope;
    type State = GitlabConsumerState;
    type Arguments = GitlabConsumerState;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        // fire off subscribe message
        state
            .tcp_client
            .cast(cassini_client::TcpClientMessage::Subscribe {
                topic: GROUPS_CONSUMER_TOPIC.to_string(),
                trace_ctx: None,
            })?;

        debug!("{myself:?} starting");
        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} started. Waitng to consume.");
        Ok(())
    }
    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message.data {
            GitlabData::Groups(groups) => {
                Self::handle_groups(message.instance_id, &groups, &state.graph_controller)?
            }
            GitlabData::GroupMembers(link) => {
                if let Some(memberships) = link.connection.nodes {
                    // we have to strip out the optional values.
                    let memberships = memberships.into_iter().flatten().collect::<Vec<_>>();

                    Self::handle_group_members(
                        message.instance_id,
                        link.resource_id.to_string(),
                        &memberships,
                        &state.graph_controller,
                    )?;
                }
            }
            GitlabData::GroupProjects(link) => {
                if let Some(nodes) = link.connection.nodes {
                    let projects = nodes.into_iter().flatten().collect::<Vec<_>>();

                    Self::handle_group_projects(
                        message.instance_id.clone(),
                        link.resource_id.to_string(),
                        &projects,
                        &state.graph_controller,
                    )?;
                }
            }

            GitlabData::GroupRunners(link) => {
                if let Some(nodes) = link.connection.nodes {
                    let runners = nodes.into_iter().flatten().collect::<Vec<_>>();

                    Self::handle_group_runners(
                        message.instance_id.clone(),
                        link.resource_id.to_string(),
                        &runners,
                        &state.graph_controller,
                    )?;
                }
            }
            _ => (),
        }

        Ok(())
    }
}
