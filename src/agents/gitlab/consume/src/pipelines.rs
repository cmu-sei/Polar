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


use crate::{subscribe_to_topic, GitlabConsumerArgs, GitlabConsumerState, TRANSACTION_FAILED_ERROR};
use common::types::GitlabData;
use common::PIPELINE_CONSUMER_TOPIC;
use gitlab_queries::projects::GitlabCiJob;
use gitlab_queries::projects::CiJobArtifact;
use neo4rs::Query;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, error, info,};

pub struct GitlabPipelineConsumer;

impl GitlabPipelineConsumer {
    fn format_artifacts(artifacts: &[CiJobArtifact]) -> String {
        artifacts
            .iter()
            .map(|artifact| {
                format!(
                    r#"{{ artifact_id: "{}", name: "{}", size: "{}", expire_at: "{}", download_path: "{}" }}"#,
                    artifact.id,
                    artifact.name.clone().unwrap_or_default(),
                    // TODO: implement display for the enum artifact.file_type,
                    artifact.size,
                    artifact.expire_at.clone().unwrap_or_default(),
                    artifact.download_path.clone().unwrap_or_default()
                    
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn format_jobs(jobs: &Vec<GitlabCiJob>) -> String {
        jobs.iter()
            .map(|job| {
                
                let id = job.id.as_ref().map_or(String::new(), |v| v.0.clone());
                let status = job.status.as_ref().map_or(String::new(), |v| format!("{v}"));
                let runner = match &job.runner {
                    Some(runner) => &runner.id.0,
                    None => &String::default()
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
    type Msg = GitlabData;
    type State = GitlabConsumerState;
    type Arguments = GitlabConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: GitlabConsumerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to broker");
        //subscribe to topic
        match subscribe_to_topic(args.registration_id, PIPELINE_CONSUMER_TOPIC.to_string(), args.graph_config).await {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg = format!("Error subscribing to topic \"{PIPELINE_CONSUMER_TOPIC}\" {e}");
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
                match message {
                    GitlabData::Pipelines((full_path, pipelines)) => {
                        let pipelines_data = pipelines
                        .iter()
                        .map(|pipeline| {
                            // if this pipeline produced artifacts, add them
                            let artifacts = match &pipeline.job_artifacts {
                                Some(artifacts) => GitlabPipelineConsumer::format_artifacts(&artifacts),
                                None => String::default()
                            };
                            
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
                                    artifacts: [ {artifacts} ]
                                    
                                }}"#,
                                id = pipeline.id.0,
                                active = pipeline.active,
                                created_at = pipeline.created_at,
                                sha = pipeline.sha.clone().unwrap_or_default(),
                                child = pipeline.child,
                                complete = pipeline.complete,
                                duration = pipeline.duration.unwrap_or_default(),
                                total_jobs = pipeline.total_jobs,
                                artifacts = artifacts,
                                // jobs = jobs
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
                                p.status = pipeline_data.status,
                                p.created_at = pipeline_data.created_at,
                                p.sha = pipeline_data.sha,
                                p.duration = pipeline_data.duration,
                                p.complete = pipeline_data.complete,
                                p.total_jobs = pipeline_data.total_jobs
                                
                            MERGE (proj)-[:HAS_PIPELINE]->(p)
                        
                            WITH p, pipeline_data.artifacts AS artifacts
                            UNWIND artifacts AS artifact
                            MERGE (a:Artifact {{ id: artifact.artifact_id }})
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
                    
                        let cypher_query = format!(r#"
                        
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

                        "#);

                        debug!("Executing Cypher: {}", cypher_query);
                    
                        transaction.run(Query::new(cypher_query)).await?;

                    
                        if let Err(e) = transaction.commit().await {
                            error!("Failed to commit pipelines transaction: {:?}", e);
                            // Up to you if you want to stop the actor or recover
                        } else {
                            info!("Committed pipelines batch transaction to database");
                        }

                   }
                    _ => (),
                }        
            }
            Err(e) => myself.stop(Some(format!("{TRANSACTION_FAILED_ERROR}. {e}")))
        }

        Ok(())
    }
}
