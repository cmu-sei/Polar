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
use cassini_client::TcpClient;
use common::types::{GitlabData, GitlabEnvelope, GitlabPackageFile};
use common::REPOSITORY_CONSUMER_TOPIC;
use gitlab_queries::projects::{ContainerRepository, ContainerRepositoryTag, Package};

use polar::graph::{GraphController, GraphControllerMsg, GraphOp, GraphValue, Property};
use polar::ProvenanceEvent;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, info};

const UNKNOWN_FIELD: &str = "unknown";

pub struct GitlabRepositoryConsumer;

impl GitlabRepositoryConsumer {
    pub fn handle_container_repositories(
        instance_id: &String,
        project_id: &String,
        repos: &[ContainerRepository],
        graph: &ActorRef<GraphControllerMsg<GitlabNodeKey>>,
    ) -> Result<(), ActorProcessingErr> {
        let project_key = GitlabNodeKey::Project {
            instance_id: instance_id.into(),
            project_id: project_id.to_owned(),
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: project_key.clone(),
            props: vec![],
        }))?;

        for repo in repos {
            let repo_key = GitlabNodeKey::ContainerRepository {
                project_id: project_id.clone(),
                repository_id: repo.id.to_string(),
            };

            let props = vec![
                Property(
                    "created_at".into(),
                    GraphValue::String(repo.created_at.to_string()),
                ),
                Property(
                    "updated_at".into(),
                    GraphValue::String(repo.updated_at.to_string()),
                ),
                Property("location".into(), GraphValue::String(repo.location.clone())),
                Property("name".into(), GraphValue::String(repo.name.clone())),
                Property("path".into(), GraphValue::String(repo.path.clone())),
                Property(
                    "migration_state".into(),
                    GraphValue::String(repo.migration_state.clone()),
                ),
                Property(
                    "protection_rule_exists".into(),
                    GraphValue::Bool(repo.protection_rule_exists),
                ),
                Property("tags_count".into(), GraphValue::I64(repo.tags_count as i64)),
            ];

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: repo_key.clone(),
                props,
            }))?;

            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: repo_key.clone(),
                to: project_key.clone(),
                rel_type: "BELONGS_TO".into(),
                props: vec![],
            }))?;

            // registry host relationship
            let registry_key = GitlabNodeKey::ContainerRepository {
                project_id: project_id.clone(),
                repository_id: repo.id.to_string(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: registry_key,
                to: repo_key,
                rel_type: "HOSTS_REPOSITORY".into(),
                props: vec![],
            }))?;
        }

        Ok(())
    }

    pub fn handle_container_repository_tags(
        project_id: String,
        repository_id: String,
        tags: &[ContainerRepositoryTag],
        graph: &GraphController<GitlabNodeKey>,
        tcp_client: &TcpClient,
    ) -> Result<(), ActorProcessingErr> {
        for tag in tags {
            if tag.digest.is_none() {
                continue;
            }

            polar::emit_provenance_event(
                ProvenanceEvent::OCIArtifactDiscovered {
                    uri: tag.location.clone(),
                },
                tcp_client,
            )?;

            let tag_key = GitlabNodeKey::ContainerRepositoryTag {
                repository_id: repository_id.clone(),
                digest: tag.digest.clone().unwrap(),
            };

            let props = vec![
                Property(
                    "media_type".into(),
                    GraphValue::String(
                        tag.media_type
                            .clone()
                            .unwrap_or_else(|| UNKNOWN_FIELD.into()),
                    ),
                ),
                Property("location".into(), GraphValue::String(tag.location.clone())),
                Property(
                    "revision".into(),
                    GraphValue::String(tag.revision.clone().unwrap_or_default()),
                ),
                Property(
                    "short_revision".into(),
                    GraphValue::String(tag.short_revision.clone().unwrap_or_default()),
                ),
                Property(
                    "total_size".into(),
                    GraphValue::String(tag.total_size.clone().map(|v| v.0).unwrap_or_default()),
                ),
                Property(
                    "created_at".into(),
                    GraphValue::String(tag.created_at.clone().map(|v| v.0).unwrap_or_default()),
                ),
            ];

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: tag_key.clone(),
                props,
            }))?;

            let repo_key = GitlabNodeKey::ContainerRepository {
                project_id: project_id.clone(),
                repository_id: repository_id.clone(),
            };

            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: repo_key,
                to: tag_key,
                rel_type: "CONTAINS_TAG".into(),
                props: vec![],
            }))?;
        }

        Ok(())
    }

    pub fn handle_project_packages(
        instance_id: String,
        project_id: String,
        packages: &[Package],
        graph: &ActorRef<GraphControllerMsg<GitlabNodeKey>>,
    ) -> Result<(), ActorProcessingErr> {
        let project_key = GitlabNodeKey::Project {
            instance_id: instance_id.to_string(),
            project_id: project_id.clone(),
        };

        for pkg in packages {
            let pkg_key = GitlabNodeKey::Package {
                package_id: pkg.id.to_string(),
            };

            let props = vec![
                Property("name".into(), GraphValue::String(pkg.name.clone())),
                Property(
                    "version".into(),
                    GraphValue::String(pkg.version.clone().unwrap_or_default()),
                ),
                Property(
                    "package_type".into(),
                    GraphValue::String(pkg.package_type.to_string()),
                ),
                Property("status".into(), GraphValue::String(pkg.status.to_string())),
                Property(
                    "status_message".into(),
                    GraphValue::String(pkg.status_message.clone().unwrap_or_default()),
                ),
            ];

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: pkg_key.clone(),
                props,
            }))?;

            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: project_key.clone(),
                to: pkg_key.clone(),
                rel_type: "HAS_PACKAGE".into(),
                props: vec![],
            }))?;

            // pipelines
            if let Some(conn) = &pkg.pipelines {
                if let Some(nodes) = &conn.nodes {
                    for pipeline in nodes.iter().flatten() {
                        let pipeline_key = GitlabNodeKey::Pipeline {
                            instance_id: instance_id.to_string(),
                            pipeline_id: pipeline.id.to_string(),
                        };

                        graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                            from: pipeline_key,
                            to: pkg_key.clone(),
                            rel_type: "PRODUCED".into(),
                            props: vec![],
                        }))?;
                    }
                }
            }
        }

        Ok(())
    }

    pub fn handle_package_files(
        package_id: String,
        files: &[GitlabPackageFile],
        graph: &ActorRef<GraphControllerMsg<GitlabNodeKey>>,
    ) -> Result<(), ActorProcessingErr> {
        let pkg_key = GitlabNodeKey::Package {
            package_id: package_id.clone(),
        };

        for file in files {
            let file_key = GitlabNodeKey::PackageFile {
                package_id: package_id.clone(),
                file_id: file.id.to_string(),
            };

            let props = vec![Property(
                "file_name".into(),
                GraphValue::String(file.file_name.clone()),
            )];

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: file_key.clone(),
                props,
            }))?;

            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: pkg_key.clone(),
                to: file_key,
                rel_type: "CONTAINS_FILE".into(),
                props: vec![],
            }))?;
        }

        Ok(())
    }
}

#[async_trait]
impl Actor for GitlabRepositoryConsumer {
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
                topic: REPOSITORY_CONSUMER_TOPIC.to_string(),
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
        info!("{:?} waiting to consume", myself.get_name());
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message.data {
            GitlabData::ProjectContainerRepositories {
                project_id,
                repositories,
            } => {
                Self::handle_container_repositories(
                    &message.instance_id,
                    &project_id,
                    &repositories,
                    &state.graph_controller,
                )?;
            }

            GitlabData::ContainerRepositoryTags {
                project_id,
                repository_id,
                tags,
            } => {
                Self::handle_container_repository_tags(
                    project_id,
                    repository_id,
                    &tags,
                    &state.graph_controller,
                    &state.tcp_client,
                )?;
            }

            GitlabData::ProjectPackages {
                project_id,
                packages,
            } => {
                Self::handle_project_packages(
                    message.instance_id,
                    project_id,
                    &packages,
                    &state.graph_controller,
                )?;
            }

            GitlabData::PackageFiles { package_id, files } => {
                Self::handle_package_files(package_id, &files, &state.graph_controller)?;
            }

            _ => {}
        }

        Ok(())
    }
}
