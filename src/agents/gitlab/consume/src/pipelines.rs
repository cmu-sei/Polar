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

use crate::BROKER_CLIENT_NAME;
use crate::{subscribe_to_topic, GitlabConsumerArgs, GitlabConsumerState};
use cassini_client::TcpClientMessage;
use common::types::GitlabData;
use common::types::GitlabEnvelope;
use common::PIPELINE_CONSUMER_TOPIC;
use gitlab_queries::projects::CiJobArtifact;
use gitlab_queries::projects::GitlabCiJob;
use gitlab_schema::DateTimeString;
use neo4rs::Query;
use polar::ProvenanceEvent;
use polar::PROVENANCE_DISCOVERY_TOPIC;
use polar::{QUERY_COMMIT_FAILED, TRANSACTION_FAILED_ERROR};
use ractor::{async_trait, rpc::cast, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, error, info};

fn dowwnload_path(base_url: &str, download_url: &str) -> String {
    format!("{base_url}{download_url}")
}

pub struct GitlabPipelineConsumer;

impl GitlabPipelineConsumer {
    /// Determine if an artifact should be emitted to the resolver
    /// Here, we rely somewhat on convention, but we also check for specific file extensions.
    fn artifact_should_emit(name: &str) -> bool {
        name.ends_with(".cdx.json")
            || name.ends_with(".sbom.json")
            || name.ends_with("flake.lock")
            || name.ends_with("cargo.lock")
    }

    fn format_artifacts(base_url: &str, artifacts: &[CiJobArtifact]) -> String {
        artifacts
            .iter()
            .map(|artifact| {
                // while we're at it, we shoud emit an event to the discovery topic that we found an artifact
                // IFF that artifact is one we care about.

                //handle whether the artifact can actually be resolved...if it can't then there's no download path, nothing we can do.
                // TODO: It makes better sense to just do this work here, and pass it to the linker directly, and save ourselves the latency
                if let Some(name) = &artifact.name {
                    if GitlabPipelineConsumer::artifact_should_emit(name) {
                        if let Some(download_path) = &artifact.download_path {
                            if let Some(client) = ractor::registry::where_is(BROKER_CLIENT_NAME.to_string()) {
                                let event = ProvenanceEvent::SbomDiscovered { artifact_id: artifact.id.to_string(), uri: download_path.clone() };

                                match rkyv::to_bytes::<rkyv::rancor::Error>(&event) {
                                    Ok(payload) => {
                                        let message = TcpClientMessage::Publish {
                                            topic: PROVENANCE_DISCOVERY_TOPIC.to_string(),
                                            payload: payload.into(),
                                        };
                                        cast(&client, message).ok();
                                    }
                                    Err(e) => {
                                        error!("Failed to serialize event: {}", e);
                                    }
                                }
                            }
                        } else {
                            debug!("Skipping artifact {} because it has no download path", artifact.id);
                        }
                    }
                }

                // Here we format the base_url of the gitlab instance onto the download_url,
                // gitlab doesn't proivde the instance url as part of it so we do it oursleves here.
                // This will help us later when we seek to scrape artifact data like job logs, sboms, test results, etc.
                format!(
                    r#"{{ artifact_id: "{}", name: "{}", size: "{}", expire_at: "{}", download_path: "{download_path}" }}"#,
                    artifact.id,
                    artifact.name.clone().unwrap_or_default(),
                    // TODO: implement display for the enum artifact.file_type,
                    artifact.size,
                    artifact.expire_at.clone().unwrap_or_default(),
                    download_path = dowwnload_path(base_url, artifact.download_path.as_ref().unwrap_or(&String::new()))

                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn format_jobs(jobs: &Vec<GitlabCiJob>) -> String {
        jobs.iter()
            .map(|job| {
                let id = job.id.as_ref().map_or(String::new(), |v| v.0.clone());
                let status = job
                    .status
                    .as_ref()
                    .map_or(String::new(), |v| format!("{v}"));
                let runner = match &job.runner {
                    Some(runner) => &runner.id.0,
                    None => &String::default(),
                };
                let name = job.name.clone().unwrap_or_default();
                let short_sha = &job.short_sha;
                let tags = job
                    .tags
                    .as_ref()
                    .map(|tags| {
                        format!(
                            "[{}]",
                            tags.iter()
                                .map(|tag| format!(r#""{}""#, tag))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })
                    .unwrap_or_else(|| "[]".to_string());
                let created_at = job.created_at.clone().unwrap_or_default();
                let started_at = job.started_at.clone().unwrap_or_default();
                let finished_at = job.finished_at.clone().unwrap_or_default();
                let duration = job.duration.unwrap_or(0).to_string();
                let failure_message = job.failure_message.clone().unwrap_or_default();

                format!(
                    r#"{{
                        id: "{id}",
                        status: "{status}",
                        name: "{name}",
                        short_sha: "{short_sha}",
                        tags: {tags},
                        created_at: "{created_at}",
                        started_at: "{started_at}",
                        finished_at: "{finished_at}",
                        duration: "{duration}",
                        failure_message: "{failure_message}",
                        runner: "{runner}"
                    }}"#,
                    id = id,
                    status = status,
                    name = name,
                    short_sha = short_sha,
                    tags = tags,
                    created_at = created_at,
                    started_at = started_at,
                    finished_at = finished_at,
                    duration = duration,
                    failure_message = failure_message,
                    runner = runner
                )
            })
            .collect::<Vec<_>>()
            .join(",\n")
    }
}
#[async_trait]
impl Actor for GitlabPipelineConsumer {
    type Msg = GitlabEnvelope;
    type State = GitlabConsumerState;
    type Arguments = GitlabConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabConsumerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to broker");
        //subscribe to topic
        match subscribe_to_topic(
            args.registration_id,
            PIPELINE_CONSUMER_TOPIC.to_string(),
            args.graph_config,
        )
        .await
        {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg =
                    format!("Error subscribing to topic \"{PIPELINE_CONSUMER_TOPIC}\" {e}");
                Err(ActorProcessingErr::from(err_msg))
            }
        }
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
        //Expect transaction to start, stop if it doesn't
        match state.graph.start_txn().await {
            Ok(mut transaction) => {
                match message.data {
                    GitlabData::Pipelines((full_path, pipelines)) => {
                        let pipelines_data = pipelines
                            .iter()
                            .map(|pipeline| {
                                let artifacts = match &pipeline.job_artifacts {
                                    Some(artifacts) => GitlabPipelineConsumer::format_artifacts(
                                        &message.base_url,
                                        &artifacts,
                                    ),
                                    None => String::default(),
                                };

                                // Utility closures for optional fields

                                format!(
                                    r#"{{
                                    id: "{id}",
                                    active: "{active}",
                                    created_at: "{created_at}",
                                    sha: "{sha}",
                                    is_child: "{child}",
                                    complete: "{complete}",
                                    duration: "{duration}",
                                    total_jobs: "{total_jobs}",
                                    compute_minutes: "{compute_minutes}",
                                    failure_reason: "{failure_reason}",
                                    finished_at: "{finished_at}",
                                    source: "{source}",
                                    trigger: "{trigger}",
                                    latest: "{latest}",
                                    artifacts: [ {artifacts} ]
                                }}"#,
                                    id = pipeline.id.0,
                                    active = pipeline.active.to_string(),
                                    created_at = pipeline.created_at,
                                    sha = pipeline
                                        .sha
                                        .clone()
                                        .unwrap_or_else(|| "unknown".to_string()),
                                    child = pipeline.child.to_string(),
                                    complete = pipeline.complete.to_string(),
                                    duration = pipeline
                                        .duration
                                        .map_or("unknown".to_string(), |d| d.to_string()),
                                    total_jobs = pipeline.total_jobs.to_string(),
                                    compute_minutes = pipeline.compute_minutes.unwrap_or(0.0),
                                    failure_reason =
                                        pipeline.failure_reason.clone().unwrap_or_default(),
                                    finished_at = pipeline
                                        .finished_at
                                        .clone()
                                        .unwrap_or(DateTimeString(String::default())),
                                    source = pipeline.source.clone().unwrap_or_default(),
                                    trigger = pipeline.trigger.to_string(),
                                    latest = pipeline.latest.to_string(),
                                    artifacts = artifacts,
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(",\n");

                        let cypher_query = format!(
                            "
                            UNWIND [{pipelines_data}] AS pipeline_data
                            MATCH (proj:GitlabProject {{ full_path: \"{full_path}\" }})
                            MERGE (p:GitlabPipeline {{ id: pipeline_data.id }})
                            SET p.id = pipeline_data.id,
                                p.active = pipeline_data.active,
                                p.created_at = pipeline_data.created_at,
                                p.sha = pipeline_data.sha,
                                p.child = pipeline_data.child,
                                p.complete = pipeline_data.complete,
                                p.duration = pipeline_data.duration,
                                p.total_jobs = pipeline_data.total_jobs,
                                p.compute_minutes = pipeline_data.compute_minutes,
                                p.failure_reason = pipeline_data.failure_reason,
                                p.finished_at = pipeline_data.finished_at,
                                p.source = pipeline_data.source,
                                p.trigger = pipeline_data.trigger,
                                p.latest = pipeline_data.latest

                            MERGE (proj)-[:HAS_PIPELINE]->(p)

                            WITH p, pipeline_data.artifacts AS artifacts
                            UNWIND artifacts AS artifact
                            MERGE (a:GitlabPipelineArtifact {{ id: artifact.artifact_id }})
                            SET a.size = artifact.size,
                                a.name = artifact.name,
                                a.download_path = artifact.download_path,
                                a.expire_at = artifact.expire_at
                            MERGE (p)-[:PRODUCED]->(a)
                            "
                        );

                        debug!("Executing Cypher: {}", cypher_query);

                        transaction.run(Query::new(cypher_query)).await?;

                        if let Err(e) = transaction.commit().await {
                            error!("Failed to commit pipelines transaction: {:?}", e);
                            // Up to you if you want to stop the actor or recover
                        } else {
                            info!("Committed pipelines batch transaction to database");
                        }
                    }
                    GitlabData::Jobs((pipeline_id, jobs)) => {
                        let cypher_job_list = GitlabPipelineConsumer::format_jobs(&jobs);

                        let cypher_query = format!(
                            r#"

                        MERGE (p: GitlabPipeline {{id: "{pipeline_id}" }})
                        WITH p
                        UNWIND [{cypher_job_list}] AS job
                        MERGE (j:GitlabCiJob {{id: job.id }})
                        SET j.status = job.status,
                            j.name = job.name,
                            j.short_sha = job.short_sha,
                            j.tags = job.tags,
                            j.created_at = job.created_at,
                            j.started_at = job.started_at,
                            j.finished_at = job.finished_at,
                            j.duration = job.duration,
                            j.failure_message = job.failure_message,
                            j.runner = job.runner
                        MERGE (p)-[:HAS_JOB]->(j)

                        WITH j

                        WITH j
                        FOREACH (_ IN CASE WHEN j.runner IS NOT NULL THEN [1] ELSE [] END |
                            MERGE (r:GitlabRunner {{ runner_id: j.runner }})
                            MERGE (j)-[:EXCUTED_BY]-(r)
                        )

                        "#
                        );

                        debug!("Executing Cypher: {}", cypher_query);

                        transaction.run(Query::new(cypher_query)).await?;

                        if let Err(e) = transaction.commit().await {
                            error!("{QUERY_COMMIT_FAILED}, {e}");
                            // Up to you if you want to stop the actor or recover
                        }
                    }
                    _ => (),
                }
            }
            Err(e) => myself.stop(Some(format!("{TRANSACTION_FAILED_ERROR}. {e}"))),
        }

        Ok(())
    }
}
