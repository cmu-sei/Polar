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
use crate::GitlabNodeKey;
use cassini_client::TcpClient;
use cassini_client::TcpClientMessage;
use chrono::Utc;
use common::types::GitlabData;
use common::types::GitlabEnvelope;
use common::PIPELINE_CONSUMER_TOPIC;
use gitlab_queries::projects::CiJobArtifact;
use gitlab_queries::projects::GitlabCiJob;
use polar::graph::GraphController;
use polar::graph::{GraphControllerMsg, GraphOp, GraphValue, Property};
use polar::ProvenanceEvent;
use polar::PROVENANCE_DISCOVERY_TOPIC;
use polar::{QUERY_COMMIT_FAILED, TRANSACTION_FAILED_ERROR};
use ractor::{async_trait, rpc::cast, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, error, info};
use cassini_types::WireTraceCtx;

fn fmt_download_path(base_url: &str, download_url: &str) -> String {
    format!("{base_url}{download_url}")
}

pub struct GitlabPipelineConsumer;

impl GitlabPipelineConsumer {
    fn handle_artifacts(
        instance_id: &str,
        base_url: &str,
        job_key: &GitlabNodeKey,
        artifacts: Vec<&CiJobArtifact>,
        graph: &ActorRef<GraphControllerMsg<GitlabNodeKey>>,
        tcp_client: &TcpClient,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Handling artifacts for job {job_key:?}");
        for artifact in artifacts {
            if let Some(p) = &artifact.download_path {
                let download_path = fmt_download_path(base_url, p);

                let name = artifact
                    .name
                    .clone()
                    .unwrap_or_else(|| artifact.id.0.clone());

                let ev = ProvenanceEvent::ArtifactDiscovered {
                    name,
                    url: download_path.to_string(),
                };

                polar::emit_provenance_event(ev, tcp_client)?;

                let artifact_key = GitlabNodeKey::PipelineArtifact {
                    instance_id: instance_id.into(),
                    artifact_id: artifact.id.0.clone(),
                };

                let full_download_url = artifact
                    .download_path
                    .as_ref()
                    .map(|p| format!("{base_url}{p}"))
                    .unwrap_or_default();

                let expire_at = match &artifact.expire_at {
                    Some(d) => d.0.clone(),
                    None => "null".into(),
                };

                let props = vec![
                    Property(
                        "name".into(),
                        GraphValue::String(artifact.name.clone().unwrap_or_default()),
                    ),
                    Property("size".into(), GraphValue::String(artifact.size.to_string())),
                    Property("expire_at".into(), GraphValue::String(expire_at)),
                    Property(
                        "download_path".into(),
                        GraphValue::String(full_download_url),
                    ),
                ];

                graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: artifact_key.clone(),
                    props,
                }))?;

                graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                    from: job_key.clone(),
                    to: artifact_key,
                    rel_type: "PRODUCED".into(),
                    props: vec![],
                }))?;
            }
        }

        Ok(())
    }

    pub fn handle_jobs(
        base_url: &str,
        instance_id: &str,
        pipeline_id: &str,
        jobs: &[GitlabCiJob],
        graph: &GraphController<GitlabNodeKey>,
        tcp_client: &TcpClient,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Handling jobs for pipeline {pipeline_id}");
        let pipeline_key = GitlabNodeKey::Pipeline {
            instance_id: instance_id.into(),
            pipeline_id: pipeline_id.into(),
        };

        for job in jobs {
            if job.id.is_none() {
                continue;
            }

            let job_key = GitlabNodeKey::Job {
                instance_id: instance_id.into(),
                job_id: job.id.clone().unwrap().to_string(),
            };

            let props = vec![
                Property(
                    "status".into(),
                    GraphValue::String(
                        job.status
                            .as_ref()
                            .map(|s| s.to_string())
                            .unwrap_or_default(),
                    ),
                ),
                Property(
                    "name".into(),
                    GraphValue::String(job.name.clone().unwrap_or_default()),
                ),
                Property(
                    "created_at".into(),
                    GraphValue::String(job.created_at.clone().unwrap_or_default().0),
                ),
                Property(
                    "short_sha".into(),
                    GraphValue::String(job.short_sha.clone()),
                ),
                Property(
                    "duration".into(),
                    GraphValue::I64(job.duration.unwrap_or(0) as i64),
                ),
                Property(
                    "failure_message".into(),
                    GraphValue::String(job.failure_message.clone().unwrap_or_default()),
                ),
                Property(
                    "observed_at".into(),
                    GraphValue::String(Utc::now().to_rfc3339()),
                ),
            ];

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: job_key.clone(),
                props,
            }))?;

            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: pipeline_key.clone(),
                to: job_key.clone(),
                rel_type: "HAS_JOB".into(),
                props: vec![],
            }))?;

            if let Some(runner) = &job.runner {
                let runner_key = GitlabNodeKey::Runner {
                    instance_id: instance_id.into(),
                    runner_id: runner.id.0.clone(),
                };

                graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                    from: job_key.clone(),
                    to: runner_key,
                    rel_type: "EXECUTED_BY".into(),
                    props: vec![],
                }))?;
            }

            if let Some(conn) = &job.artifacts {
                if let Some(nodes) = &conn.nodes {
                    let artifacts = nodes.iter().flatten().collect::<Vec<_>>();
                    Self::handle_artifacts(
                        instance_id,
                        base_url,
                        &job_key,
                        artifacts,
                        graph,
                        tcp_client,
                    )?;
                }
            }
        }

        Ok(())
    }

    pub fn handle_pipelines(
        instance_id: String,
        project_id: String,
        pipelines: &[gitlab_queries::projects::Pipeline],
        graph: &GraphController<GitlabNodeKey>,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Handling pipeline runs for project {project_id}");
        let project_key = GitlabNodeKey::Project {
            instance_id: instance_id.to_string(),
            project_id: project_id.clone(),
        };

        graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: project_key.clone(),
            props: vec![],
        }))?;

        for pipeline in pipelines {
            let pipeline_key = GitlabNodeKey::Pipeline {
                instance_id: instance_id.to_string(),
                pipeline_id: pipeline.id.0.clone(),
            };

            let props = vec![
                Property("active".into(), GraphValue::Bool(pipeline.active)),
                Property(
                    "created_at".into(),
                    GraphValue::String(pipeline.created_at.to_string()),
                ),
                Property(
                    "finished_at".into(),
                    GraphValue::String(pipeline.finished_at.clone().unwrap_or_default().0),
                ),
                Property(
                    "sha".into(),
                    GraphValue::String(pipeline.sha.clone().unwrap_or_default()),
                ),
                Property("child".into(), GraphValue::Bool(pipeline.child)),
                Property("complete".into(), GraphValue::Bool(pipeline.complete)),
                Property(
                    "duration".into(),
                    GraphValue::I64(pipeline.duration.unwrap_or(0) as i64),
                ),
                Property(
                    "total_jobs".into(),
                    GraphValue::I64(pipeline.total_jobs as i64),
                ),
                Property(
                    "compute_minutes".into(),
                    GraphValue::F64(pipeline.compute_minutes.unwrap_or(0.0)),
                ),
                Property(
                    "failure_reason".into(),
                    GraphValue::String(pipeline.failure_reason.clone().unwrap_or_default()),
                ),
                Property(
                    "source".into(),
                    GraphValue::String(pipeline.source.clone().unwrap_or_default()),
                ),
                Property("trigger".into(), GraphValue::Bool(pipeline.trigger)),
                Property("latest".into(), GraphValue::Bool(pipeline.latest)),
                Property(
                    "observed_at".into(),
                    GraphValue::String(Utc::now().to_rfc3339()),
                ),
            ];

            graph.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: pipeline_key.clone(),
                props,
            }))?;

            graph.cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                from: project_key.clone(),
                to: pipeline_key.clone(),
                rel_type: "HAS_PIPELINE".into(),
                props: vec![],
            }))?;
        }

        Ok(())
    }
}

#[async_trait]
impl Actor for GitlabPipelineConsumer {
    type Msg = GitlabEnvelope;
    type State = GitlabConsumerState;
    type Arguments = GitlabConsumerState;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: GitlabConsumerState,
    ) -> Result<Self::State, ActorProcessingErr> {
        // fire off subscribe message
        state.tcp_client.cast(TcpClientMessage::Subscribe {
            topic: PIPELINE_CONSUMER_TOPIC.to_string(),
            trace_ctx: None,
        })?;

        debug!("{myself:?} starting");
        Ok(state)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message.data {
            GitlabData::Pipelines {
                project_id,
                pipelines,
            } => {
                Self::handle_pipelines(
                    message.instance_id,
                    project_id,
                    &pipelines,
                    &state.graph_controller,
                )?;
            }

            GitlabData::Jobs { pipeline_id, jobs } => {
                Self::handle_jobs(
                    &message.base_url,
                    &message.instance_id,
                    &pipeline_id,
                    &jobs,
                    &state.graph_controller,
                    &state.tcp_client,
                )?;
            }

            _ => {}
        }

        Ok(())
    }
}
