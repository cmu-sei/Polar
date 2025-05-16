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

use crate::{subscribe_to_topic, GitlabConsumerArgs, GitlabConsumerState};
use polar::{QUERY_RUN_FAILED, QUERY_COMMIT_FAILED, TRANSACTION_FAILED_ERROR};
use common::types::GitlabData;
use common::REPOSITORY_CONSUMER_TOPIC;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug , info};
use gitlab_schema::{BigInt, DateTimeString};

pub struct GitlabRepositoryConsumer;

#[async_trait]
impl Actor for GitlabRepositoryConsumer {
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
        match subscribe_to_topic(args.registration_id, REPOSITORY_CONSUMER_TOPIC.to_string(), args.graph_config).await {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg = format!("Error subscribing to topic \"{REPOSITORY_CONSUMER_TOPIC}\" {e}");
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
                    GitlabData::ProjectContainerRepositories((full_path, repositories)) => {
                        
                        let repos_data = repositories
                        .iter()
                        .map(|repo| {
                            format!(
                                "{{ id: \"{id}\",
                                created_at: \"{created_at}\", 
                                updated_at: \"{updated_at}\", \
                                location: \"{location}\", 
                                name: \"{name}\", 
                                path: \"{path}\", \
                                migration_state: \"{migration_state}\", 
                                protection_rule_exists: {protection_rule_exists}, \
                                tags_count: {tags_count} }}",
                                id = repo.id,
                                created_at = repo.created_at,
                                updated_at = repo.updated_at,
                                location = repo.location,
                                name = repo.name,
                                path = repo.path,
                                migration_state = repo.migration_state,
                                protection_rule_exists = repo.protection_rule_exists,
                                tags_count = repo.tags_count,
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",\n");

                        let cypher_query = format!(r#"
                            UNWIND [{repos_data}] AS repo
                            MERGE (r:ContainerRepository {{ id: repo.id }})
                            SET
                            r.created_at = datetime(repo.created_at),
                            r.updated_at = datetime(repo.updated_at),
                            r.location = repo.location,
                            r.name = repo.name,
                            r.path = repo.path,
                            r.migration_state = repo.migration_state,
                            r.protection_rule_exists = repo.protection_rule_exists,
                            r.tags_count = repo.tags_count

                            WITH r
                            MATCH (p:GitlabProject {{ full_path: "{full_path}" }})
                            MERGE (r)-[:BELONGS_TO]-(p)
                        "#);

                        debug!(cypher_query);

                        if let Err(_) = transaction.run(neo4rs::Query::new(cypher_query)).await {
                            myself.stop(Some(QUERY_RUN_FAILED.to_string()));
                            
                        }
                        
                        if let Err(_) = transaction.commit().await {
                            myself.stop(Some(QUERY_COMMIT_FAILED.to_string()));
                        }
                        
                        info!("Committed transaction to database");
                    }
                    GitlabData::ContainerRepositoryTags((repo_path, tags)) => {

                        let tags_data = tags
                            .iter()
                            .map(|tag| {
                                format!(
                                    "{{ created_at: \"{created_at}\", \
                                    digest: \"{digest}\", \
                                    location: \"{location}\", \
                                    media_type: \"{media_type}\", \
                                    name: \"{name}\", \
                                    path: \"{path}\", \
                                    published_at: \"{published_at}\", \
                                    revision: \"{revision}\", \
                                    short_revision: \"{short_revision}\", \
                                    total_size: \"{total_size}\" }}",
                                    created_at = tag.created_at.clone().unwrap_or(DateTimeString(String::default())),
                                    digest = tag.digest.clone().unwrap_or(String::default()),
                                    location = tag.location,
                                    media_type = tag.media_type.clone().unwrap_or(String::default()),
                                    name = tag.name,
                                    path = tag.path,
                                    published_at = tag.published_at.clone().unwrap_or(DateTimeString(String::default())),
                                    revision = tag.revision.clone().unwrap_or_default(),
                                    short_revision = tag.short_revision.clone().unwrap_or_default(),
                                    total_size = tag.total_size.clone().unwrap_or(BigInt(String::default()))
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(",\n");

                        let cypher_query = format!(r#"
                            UNWIND [{tags_data}] AS tag
                            MERGE (t:ContainerImageTag {{ name: tag.name, path: tag.path }})
                            SET
                            t.created_at = tag.created_at,
                            t.digest = tag.digest,
                            t.location = tag.location,
                            t.media_type = tag.media_type,
                            t.published_at = tag.published_at,
                            t.revision = tag.revision,
                            t.short_revision = tag.short_revision,
                            t.total_size = tag.total_size
                            WITH tag, t
                            MATCH (r:ContainerRepository {{ path: "{repo_path}" }})
                            MERGE (t)-[:CONTAINS_TAG]->(r)
                        "#);

                        debug!(cypher_query);

                        if let Err(_) = transaction.run(neo4rs::Query::new(cypher_query)).await {
                            myself.stop(Some(QUERY_RUN_FAILED.to_string()));
                            
                        }
                        
                        if let Err(_) = transaction.commit().await {
                            myself.stop(Some(QUERY_COMMIT_FAILED.to_string()));
                        }
                        
                        info!("Committed transaction to database"); 

                    }
                    GitlabData::ProjectPackages((project_path, packages)) => {

                        let packages_list = packages
                        .iter()
                        .map(|pkg| {
                            format!(
                                "{{ 
                                    id: \"{id}\", \
                                    name: \"{name}\", \
                                    version: \"{version}\", \
                                    package_type: \"{package_type}\", \
                                    created_at: \"{created_at}\", \
                                    updated_at: \"{updated_at}\", \
                                    status: \"{status}\", \
                                    status_message: \"{status_message}\"
                                }}",
                                id = pkg.id,
                                name = pkg.name,
                                version = pkg.version.clone().unwrap_or_default(),
                                package_type = pkg.package_type,
                                created_at = pkg
                                    .created_at,
                                updated_at = pkg
                                    .updated_at,
                                status = pkg.status,
                                status_message = pkg.status_message.clone().unwrap_or_default(),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",\n");

                        
                        //TODO:  this works at a bare minimum to tie in projects, make a connection to a pipeline if CI/CD was used to generate it
                        let cypher_query = format!(r#"
                                UNWIND [{packages_list}] AS pkg
                                MERGE (p:GitlabPackage {{ id: pkg.id }})
                                SET
                                p.name = pkg.name,
                                p.version = pkg.version,
                                p.package_type = pkg.package_type,
                                p.created_at = datetime(pkg.created_at),
                                p.updated_at = datetime(pkg.updated_at),
                                p.status = pkg.status,
                                p.status_message = pkg.status_message
                                WITH p, pkg
                                MATCH (project: GitlabProject) WHERE project.full_path = "{project_path}"
                                MERGE (project)-[:HAS_PACKAGE]->(p)
                        
                        "#);

                        debug!(cypher_query);

                        if let Err(_) = transaction.run(neo4rs::Query::new(cypher_query)).await {
                            myself.stop(Some(QUERY_RUN_FAILED.to_string()));
                            
                        }
                       
                         // make pipeline connections if any
                    
                        for package in packages {
                            if let Some(connection) = package.pipelines {
                                match connection.nodes {
                                    Some(pipelines) => {
                                        for option in pipelines {

                                            let pipeline = option.clone().unwrap();

                                            let cypher_query = format!(r#"
                                                MATCH (package:GitlabPackage) WHERE package.id = "{}"
                                                WITH package
                                                MATCH (pipeline:GitlabPipeline) WHERE pipeline.id = "{}"
                                                WITH package, pipeline
                                                MERGE (pipeline)-[:PRODUCED]-(package)
                                            "#, package.id, pipeline.id);

                                            debug!("{cypher_query}");

                                            if let Err(_) = transaction.run(neo4rs::Query::new(cypher_query)).await {
                                                myself.stop(Some(QUERY_RUN_FAILED.to_string()));
                                                
                                            }

                                        }
                                    }
                                    None => ()
                                }
                            }
                        }
                        if let Err(_) = transaction.commit().await {
                            myself.stop(Some(QUERY_COMMIT_FAILED.to_string()));
                        }
                        
                        info!("Committed transaction to database"); 
                    }
                    _ => (),

                }        
            }
            Err(e) => myself.stop(Some(format!("{TRANSACTION_FAILED_ERROR}. {e}")))
        }

      
      
        Ok(())
    }
}
