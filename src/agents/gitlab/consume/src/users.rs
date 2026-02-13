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

use crate::GitlabConsumerState;
use cassini_client::TcpClientMessage;
use gitlab_queries::{projects::ProjectMember, users::UserCoreFragment};
use polar::graph::{GraphControllerMsg, GraphOp, GraphValue, Property};

use crate::GitlabNodeKey;
use common::{
    types::{GitlabData, GitlabEnvelope},
    USER_CONSUMER_TOPIC,
};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, info};

pub struct GitlabUserConsumer;

impl GitlabUserConsumer {
    /// Generate graph operations for GitLab user discovery.
    ///
    /// Semantics:
    /// - Ensures the GitLab instance node exists
    /// - Ensures each GitLab user node exists
    /// - Sets user attributes (non-identity)
    /// - Records that the instance has observed the user
    pub fn ops_for_gitlab_users(
        instance_id: String,
        users: &[UserCoreFragment],
    ) -> Vec<GraphOp<GitlabNodeKey>> {
        let instance_key = GitlabNodeKey::GitlabInstance {
            instance_id: instance_id.clone(),
        };

        let mut ops = Vec::new();

        // Instance node is canonical; upsert once per batch is fine
        ops.push(GraphOp::UpsertNode {
            key: instance_key.clone(),
            props: vec![],
        });

        for user in users {
            let user_key = GitlabNodeKey::User {
                instance_id: instance_id.clone(),
                user_id: user.id.to_string(),
            };

            let mut props = Vec::new();

            if let Some(username) = &user.username {
                props.push(Property(
                    "username".into(),
                    GraphValue::String(username.clone()),
                ));
            }

            props.push(Property(
                "state".into(),
                GraphValue::String(user.state.to_string()),
            ));

            props.push(Property("bot".into(), GraphValue::Bool(user.bot)));

            if let Some(created_at) = &user.created_at {
                props.push(Property(
                    "created_at".into(),
                    GraphValue::String(created_at.to_string()),
                ));
            }

            if let Some(last_activity) = &user.last_activity_on {
                props.push(Property(
                    "last_activity_on".into(),
                    GraphValue::String(last_activity.to_string()),
                ));
            }

            if let Some(location) = &user.location {
                props.push(Property(
                    "location".into(),
                    GraphValue::String(location.clone()),
                ));
            }

            if let Some(org) = &user.organization {
                props.push(Property(
                    "organization".into(),
                    GraphValue::String(org.clone()),
                ));
            }

            props.push(Property(
                "web_url".into(),
                GraphValue::String(user.web_url.clone()),
            ));

            props.push(Property(
                "web_path".into(),
                GraphValue::String(user.web_path.clone()),
            ));

            ops.push(GraphOp::UpsertNode {
                key: user_key.clone(),
                props,
            });

            ops.push(GraphOp::EnsureEdge {
                from: instance_key.clone(),
                to: user_key,
                rel_type: "OBSERVED_USER".into(),
                props: vec![],
            });
        }

        ops
    }

    /// Generate graph operations for GitLab project membership relationships.
    ///
    /// Semantics:
    /// - Ensures user and project nodes exist
    /// - Ensures MEMBER_OF relationship exists
    /// - Sets relationship attributes
    pub fn ops_for_project_memberships(
        instance_id: String,
        user_id: String,
        memberships: &[ProjectMember],
    ) -> Vec<GraphOp<GitlabNodeKey>> {
        let user_key = GitlabNodeKey::User {
            instance_id: instance_id.clone(),
            user_id,
        };

        let mut ops = Vec::new();

        // Ensure user exists (attributes handled elsewhere)
        ops.push(GraphOp::UpsertNode {
            key: user_key.clone(),
            props: vec![],
        });

        for membership in memberships {
            let Some(project) = &membership.project else {
                continue;
            };

            let project_key = GitlabNodeKey::Project {
                instance_id: instance_id.clone(),
                project_id: project.id.to_string(),
            };

            ops.push(GraphOp::UpsertNode {
                key: project_key.clone(),
                props: vec![],
            });

            let mut rel_props = Vec::new();

            if let Some(access) = &membership.access_level {
                if let Some(level) = access.integer_value {
                    rel_props.push(Property(
                        "access_level".into(),
                        GraphValue::I64(level as i64),
                    ));
                }
            }

            if let Some(created_at) = &membership.created_at {
                rel_props.push(Property(
                    "created_at".into(),
                    GraphValue::String(created_at.to_string()),
                ));
            }

            if let Some(expires_at) = &membership.expires_at {
                rel_props.push(Property(
                    "expires_at".into(),
                    GraphValue::String(expires_at.to_string()),
                ));
            }

            ops.push(GraphOp::EnsureEdge {
                from: user_key.clone(),
                to: project_key,
                rel_type: "MEMBER_OF".into(),
                props: rel_props,
            });
        }

        ops
    }
}
#[async_trait]
impl Actor for GitlabUserConsumer {
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
            .cast(TcpClientMessage::Subscribe(USER_CONSUMER_TOPIC.to_string()))?;

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
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message.data {
            GitlabData::Users(users) => {
                let ops = Self::ops_for_gitlab_users(message.instance_id.clone(), &users);
                for op in ops {
                    state.graph_controller.cast(GraphControllerMsg::Op(op))?;
                }
            }
            GitlabData::ProjectMembers(link) => {
                if let Some(memberships) = link.connection.nodes {
                    let memberships = memberships
                        .iter()
                        .map(|m| m.clone().unwrap())
                        .collect::<Vec<_>>();

                    let ops = Self::ops_for_project_memberships(
                        message.instance_id.clone(),
                        link.resource_id.to_string(),
                        &memberships,
                    );

                    for op in ops {
                        state.graph_controller.cast(GraphControllerMsg::Op(op))?;
                    }
                }
            }
            _ => todo!(),
        }

        Ok(())
    }
}
