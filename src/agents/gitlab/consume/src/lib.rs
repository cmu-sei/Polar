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

use cassini_client::{TcpClient, TcpClientMessage};
use common::types::GitlabEnvelope;
use neo4rs::BoltType;
use polar::graph::{GraphController, GraphOp};
use polar::graph::{GraphControllerMsg, GraphNodeKey};
use polar::impl_graph_controller;
use ractor::registry::where_is;
use ractor::{ActorProcessingErr, ActorRef};
use std::error::Error;

// pub mod groups;
pub mod meta;
// pub mod pipelines;
pub mod projects;
// pub mod repositories;
// pub mod runners;
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
    graph_controller: GraphController<GitlabNodeKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GitlabNodeKey {
    /// A GitLab instance (SaaS or self-managed)
    GitlabInstance {
        instance_id: String, // UUIDv5 or canonical hostname hash
    },
    License {
        instance_id: String,
        license_id: String,
    },
    /// A project/repository
    Project {
        instance_id: String,
        project_id: String, // GitLab internal project ID
    },

    /// A GitLab user
    User {
        instance_id: String,
        user_id: String,
    },

    Group {
        instance_id: String,
        group_id: String,
    },

    /// A container registry repository
    ContainerRepository {
        instance_id: String,
        repository_id: String,
    },

    /// A CI pipeline run
    Pipeline {
        instance_id: String,
        project_id: String,
        pipeline_id: String,
    },

    /// A CI job
    Job {
        instance_id: String,
        project_id: String,
        job_id: String,
    },

    /// A GitLab runner
    Runner {
        instance_id: String,
        runner_id: String,
    },

    /// A pipeline artifact (logical artifact, not individual files)
    PipelineArtifact {
        instance_id: String,
        project_id: String,
        job_id: String,
        artifact_name: String,
    },
}

impl_graph_controller!(GitlabGraphController, node_key = GitlabNodeKey);

impl GraphNodeKey for GitlabNodeKey {
    fn cypher_match(&self, prefix: &str) -> (String, Vec<(String, BoltType)>) {
        match self {
            GitlabNodeKey::GitlabInstance { instance_id } => (
                format!("(:GitlabInstance {{ id: ${prefix}_id }})"),
                vec![
                    (format!("{prefix}_id"), instance_id.clone().into()),
                ],
            ),
            GitlabNodeKey::License { instance_id, license_id } => (
                format!("(:GitlabLicense {{ instance_id: ${prefix}_instance_id, license_id: ${prefix}_license_id  }})"),
                vec![
                    (format!("{prefix}_id"), instance_id.clone().into()),
                    (format!("{prefix}_license_id"), license_id.clone().into()),
                ],
            ),
            GitlabNodeKey::Project {
                instance_id,
                project_id,
            } => (
                format!(
                    "(:GitlabProject {{ instance_id: ${prefix}_instance_id, project_id: ${prefix}_project_id }})"
                ),
                vec![
                    (format!("{prefix}_instance_id"), instance_id.clone().into()),
                    (format!("{prefix}_project_id"), (project_id).to_owned().into()),
                ],
            ),

            GitlabNodeKey::User {
                instance_id,
                user_id,
            } => (
                format!(
                    "(:GitlabUser {{ instance_id: ${prefix}_instance_id, user_id: ${prefix}_user_id }})"
                ),
                vec![
                    (format!("{prefix}_instance_id"), instance_id.clone().into()),
                    (format!("{prefix}_user_id"), (user_id).to_owned().into()),
                ],
            ),
            GitlabNodeKey::Group {instance_id, group_id } =>
            (
                format!(
                    "(:GitlabGroup {{ instance_id: ${prefix}_instance_id, group_id: ${prefix}_group_id }})"
                ),
                vec![
                    (format!("{prefix}_instance_id"), instance_id.clone().into()),
                    (format!("{prefix}_group_id"), (group_id).to_owned().into()),
                ],
            ),
            GitlabNodeKey::ContainerRepository {
                instance_id,
                repository_id,
            } => (
                format!(
                    "(:GitlabContainerRepository {{ instance_id: ${prefix}_instance_id, repository_id: ${prefix}_repository_id }})"
                ),
                vec![
                    (format!("{prefix}_instance_id"), instance_id.clone().into()),
                    (format!("{prefix}_repository_id"), (repository_id).to_owned().into()),
                ],
            ),

            GitlabNodeKey::Pipeline {
                instance_id,
                project_id,
                pipeline_id,
            } => (
                format!(
                    "(:GitlabPipeline {{ instance_id: ${prefix}_instance_id, project_id: ${prefix}_project_id, pipeline_id: ${prefix}_pipeline_id }})"
                ),
                vec![
                    (format!("{prefix}_instance_id"), instance_id.clone().into()),
                    (format!("{prefix}_project_id"), project_id.to_owned().into()),
                    (format!("{prefix}_pipeline_id"), pipeline_id.to_owned().into()),
                ],
            ),

            GitlabNodeKey::Job {
                instance_id,
                project_id,
                job_id,
            } => (
                format!(
                    "(:GitlabJob {{ instance_id: ${prefix}_instance_id, project_id: ${prefix}_project_id, job_id: ${prefix}_job_id }})"
                ),
                vec![
                    (format!("{prefix}_instance_id"), instance_id.clone().into()),
                    (format!("{prefix}_project_id"), project_id.to_owned().into()),
                    (format!("{prefix}_job_id"), job_id.to_owned().into()),
                ],
            ),

            GitlabNodeKey::Runner {
                instance_id,
                runner_id,
            } => (
                format!(
                    "(:GitlabRunner {{ instance_id: ${prefix}_instance_id, runner_id: ${prefix}_runner_id }})"
                ),
                vec![
                    (format!("{prefix}_instance_id"), instance_id.clone().into()),
                    (format!("{prefix}_runner_id"), runner_id.to_owned().into()),
                ],
            ),

            GitlabNodeKey::PipelineArtifact {
                instance_id,
                project_id,
                job_id,
                artifact_name,
            } => (
                format!(
                    "(:GitlabPipelineArtifact {{ instance_id: ${prefix}_instance_id, project_id: ${prefix}_project_id, job_id: ${prefix}_job_id, name: ${prefix}_name }})"
                ),
                vec![
                    (format!("{prefix}_instance_id"), instance_id.clone().into()),
                    (format!("{prefix}_project_id"), project_id.to_owned().into()),
                    (format!("{prefix}_job_id"), job_id.to_owned().into()),
                    (format!("{prefix}_name"), artifact_name.clone().into()),
                ],
            ),
        }
    }
}

#[derive(Debug)]
pub enum CrashReason {
    CaCertReadError(String), // error when reading CA cert
    SubscriptionError(String), // error when subscribing to the topic
                             // Add other error types as necessary
}
