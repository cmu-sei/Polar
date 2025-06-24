use crate::{subscribe_to_topic, GitlabConsumerArgs, GitlabConsumerState};
use common::types::GitlabData;
use common::METADATA_CONSUMER_TOPIC;
use polar::TRANSACTION_FAILED_ERROR;

use neo4rs::Query;
use polar::{QUERY_COMMIT_FAILED, QUERY_RUN_FAILED};
use ractor::{concurrency::Duration, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, error, info};

pub struct MetaConsumer;

#[ractor::async_trait]
impl Actor for MetaConsumer {
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
        match subscribe_to_topic(
            args.registration_id,
            METADATA_CONSUMER_TOPIC.to_string(),
            args.graph_config,
        )
        .await
        {
            Ok(state) => Ok(state),
            Err(e) => {
                let err_msg =
                    format!("Error subscribing to topic \"{METADATA_CONSUMER_TOPIC}\" {e}");
                Err(ActorProcessingErr::from(err_msg))
            }
        }
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match state.graph.start_txn().await {
            Ok(mut transaction) => match message {
                GitlabData::Instance(instance) => {
                    let cypher_query = format!(
                        r#"
                            MERGE (instance:GitlabInstance
                            {{
                                enterprise: "{}",
                                version: "{}",
                                base_url: "{}"
                            }}
                            )
                        "#,
                        instance.metadata.enterprise, instance.metadata.version, instance.base_url
                    );

                    debug!("{cypher_query}");

                    if let Err(e) = transaction.run(Query::new(cypher_query)).await {
                        error!("{e}");
                        myself.stop(Some(e.to_string()));
                    }

                    if let Err(_e) = transaction.commit().await {
                        myself.stop(Some(QUERY_COMMIT_FAILED.to_string()))
                    }

                    info!("Committed transaction to database");
                }
                GitlabData::Licenses(licenses) => {
                    let entries_data = licenses
                        .iter()
                        .map(|entry| {
                            format!(
                                r#"{{
                                     id: "{id}",
                                     createdAt: "{created_at}",
                                     startsAt: "{starts_at}",
                                     expiresAt: "{expires_at}",
                                     blockChangesAt: "{block_changes_at}",
                                     activatedAt: "{activated_at}",
                                     name: "{name}",
                                     email: "{email}",
                                     company: "{company}",
                                     plan: "{plan}",
                                     type: "{entry_type}",
                                     usersInLicenseCount: {users_in_license_count}
                                 }}"#,
                                id = entry.id,
                                created_at = entry
                                    .created_at
                                    .as_ref()
                                    .map_or("".into(), |d| d.to_string()),
                                starts_at = entry
                                    .starts_at
                                    .as_ref()
                                    .map_or("".into(), |d| d.to_string()),
                                expires_at = entry
                                    .expires_at
                                    .as_ref()
                                    .map_or("".into(), |d| d.to_string()),
                                block_changes_at = entry
                                    .block_changes_at
                                    .as_ref()
                                    .map_or("".into(), |d| d.to_string()),
                                activated_at = entry
                                    .activated_at
                                    .as_ref()
                                    .map_or("".into(), |d| d.to_string()),
                                name = entry.name.clone().unwrap_or_default(),
                                email = entry.email.clone().unwrap_or_default(),
                                company = entry.company.clone().unwrap_or_default(),
                                plan = entry.plan.clone(),
                                entry_type = entry.entry_type,
                                users_in_license_count = entry
                                    .users_in_license_count
                                    .map(|n| n.to_string())
                                    .unwrap_or_else(|| "null".into()),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",\n");

                    //TODO: Add connection to instance node
                    let cypher_query = format!(
                        r#"
                         UNWIND [{entries_data}] AS entry
                         MERGE (lic:LicenseEntry {{ id: entry.id }})
                         SET lic.createdAt = entry.createdAt,
                             lic.startsAt = entry.startsAt,
                             lic.expiresAt = entry.expiresAt,
                             lic.blockChangesAt = entry.blockChangesAt,
                             lic.activatedAt = entry.activatedAt,
                             lic.name = entry.name,
                             lic.email = entry.email,
                             lic.company = entry.company,
                             lic.plan = entry.plan,
                             lic.type = entry.type,
                             lic.usersInLicenseCount = entry.usersInLicenseCount
                         "#,
                        entries_data = entries_data
                    );

                    debug!("{cypher_query}");

                    if let Err(e) = transaction.run(Query::new(cypher_query)).await {
                        error!("{e}");
                        myself.stop(Some(e.to_string()));
                    }

                    if let Err(_e) = transaction.commit().await {
                        myself.stop(Some(QUERY_COMMIT_FAILED.to_string()))
                    }

                    info!("Committed transaction to database");
                }
                _ => (),
            },
            Err(e) => myself.stop(Some(format!("{TRANSACTION_FAILED_ERROR}. {e}"))),
        }

        Ok(())
    }
}
