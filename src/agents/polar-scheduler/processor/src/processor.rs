use cassini_client::TcpClientMessage;
use polar::graph::{
    controller::{GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property},
    nodes::schedule::ScheduleKey,
};
use polar_scheduler_common::{
    AdhocAgentAnnouncement, ConfigurationNotification, ConfigurationOutcome, GitScheduleChange,
    ScheduleKind, ScheduleNode, ScheduleNotification, configuration_topic,
};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::collections::HashMap;
use tracing::{error, info};

// Import local types
use crate::configuration::{self, Routing, Shape, StoredDocument};
use crate::types::ProcessorMsg;

pub struct ScheduleInfoProcessor;

/// The highest version of each item among documents read from the graph.
fn latest_of(stored: Vec<StoredDocument>) -> HashMap<String, StoredDocument> {
    let mut latest: HashMap<String, StoredDocument> = HashMap::new();
    for document in stored {
        if latest
            .get(&document.id)
            .is_none_or(|held| document.version > held.version)
        {
            latest.insert(document.id.clone(), document);
        }
    }
    latest
}

pub struct ProcessorState {
    tcp_client: ActorRef<TcpClientMessage>,
    graph_controller: ActorRef<GraphControllerMsg>,
    permanent_versions: HashMap<String, u64>,
    adhoc_versions: HashMap<String, u64>,
    ephemeral_patterns: HashMap<String, String>, // pattern -> agent_type
    /// The latest signed document of each configuration item, by item id: what the graph held when the
    /// processor started, and what has arrived since. It is what an agent that asks is sent, and it keeps an
    /// older document from passing for the latest. The receiver still checks versions for itself.
    documents: HashMap<String, StoredDocument>,
}

impl ScheduleInfoProcessor {
    async fn handle_git_change(
        state: &mut ProcessorState,
        change: GitScheduleChange,
    ) -> Result<(), ActorProcessingErr> {
        match change {
            GitScheduleChange::Create { path, json } | GitScheduleChange::Update { path, json } => {
                if let Shape::Document(routing) = configuration::classify(&json) {
                    return Self::handle_configuration(state, path, json, routing).await;
                }
                let schedule_node: ScheduleNode = match serde_json::from_str(&json) {
                    Ok(node) => node,
                    Err(e) => {
                        error!("Failed to parse schedule JSON from {}: {:?}", path, e);
                        return Ok(());
                    }
                };
                let node_key = match schedule_node.kind {
                    ScheduleKind::Permanent => ScheduleKey::Permanent {
                        agent_id: schedule_node.agent_id.clone().unwrap(),
                    },
                    ScheduleKind::Adhoc => ScheduleKey::Adhoc {
                        agent_type: schedule_node.agent_type.clone().unwrap(),
                    },
                    ScheduleKind::Ephemeral => ScheduleKey::Ephemeral {
                        agent_type: schedule_node.agent_type.clone().unwrap(),
                    },
                };

                // Use the original JSON string for storage
                let schedule_json = json;

                state
                    .graph_controller
                    .cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                        key: node_key.clone().into_key(),
                        props: vec![Property(
                            "schedule".to_string(),
                            GraphValue::String(schedule_json.clone()),
                        )],
                    }))?;

                match schedule_node.kind {
                    ScheduleKind::Permanent => {
                        let agent_id = schedule_node.agent_id.as_ref().unwrap();
                        let prev = state.permanent_versions.get(agent_id).copied();
                        if prev != Some(schedule_node.version) {
                            state
                                .permanent_versions
                                .insert(agent_id.clone(), schedule_node.version);
                            let notif = ScheduleNotification::PermanentUpdate {
                                agent_id: agent_id.clone(),
                                schedule_json: schedule_json.clone(),
                            };
                            publish_notification(
                                state,
                                &format!("agent.{}.schedule", agent_id),
                                &notif,
                            )
                            .await?;
                        }
                    }
                    ScheduleKind::Adhoc => {
                        let agent_type = schedule_node.agent_type.as_ref().unwrap();
                        let prev = state.adhoc_versions.get(agent_type).copied();
                        if prev != Some(schedule_node.version) {
                            state
                                .adhoc_versions
                                .insert(agent_type.clone(), schedule_node.version);
                            let notif = ScheduleNotification::AdhocUpdate {
                                agent_type: agent_type.clone(),
                                schedule_json: schedule_json.clone(),
                            };
                            publish_notification(
                                state,
                                &format!("agent.type.{}.schedule", agent_type),
                                &notif,
                            )
                            .await?;
                        }
                    }
                    ScheduleKind::Ephemeral => {
                        let agent_type = schedule_node.agent_type.as_ref().unwrap();
                        let pattern = schedule_node.config["eventPattern"].as_str().unwrap_or("");
                        if !pattern.is_empty() {
                            state
                                .ephemeral_patterns
                                .insert(pattern.to_string(), agent_type.clone());
                        }
                        let notif = ScheduleNotification::EphemeralUpdate {
                            agent_type: agent_type.clone(),
                            schedule_json: schedule_json.clone(),
                        };
                        publish_notification(
                            state,
                            &format!("agent.type.{}.schedule", agent_type),
                            &notif,
                        )
                        .await?;
                    }
                }
            }
            GitScheduleChange::Delete { path } => {
                info!("Schedule file deleted: {}", path);
                // TODO: handle deletion in graph and caches
            }
        }
        Ok(())
    }

    /// A signed configuration document. It is written to the graph first, so it is seen whatever becomes
    /// of it, and then forwarded to the agent it is for, untouched. The processor does not verify it: the
    /// receiving agent does.
    async fn handle_configuration(
        state: &mut ProcessorState,
        path: String,
        json: String,
        routing: Result<Routing, String>,
    ) -> Result<(), ActorProcessingErr> {
        let received_at = chrono::Utc::now().to_rfc3339();
        match routing {
            Ok(routing) => {
                let newer = state
                    .documents
                    .get(&routing.id)
                    .is_none_or(|held| routing.version > held.version);
                let key = ScheduleKey::Configuration {
                    id: routing.id.clone(),
                }
                .into_key();
                if newer {
                    state
                        .graph_controller
                        .cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                            key,
                            props: vec![
                                Property("document".to_string(), GraphValue::String(json.clone())),
                                Property(
                                    "target".to_string(),
                                    GraphValue::String(routing.agent.clone()),
                                ),
                                Property(
                                    "version".to_string(),
                                    GraphValue::I64(routing.version as i64),
                                ),
                                Property("source_path".to_string(), GraphValue::String(path)),
                                Property(
                                    "status".to_string(),
                                    GraphValue::String("received".to_string()),
                                ),
                                Property(
                                    "received_at".to_string(),
                                    GraphValue::String(received_at),
                                ),
                            ],
                        }))?;
                    state.documents.insert(
                        routing.id.clone(),
                        StoredDocument {
                            id: routing.id.clone(),
                            agent: routing.agent.clone(),
                            version: routing.version,
                            document: json.clone(),
                        },
                    );
                    publish_configuration(state, &routing.agent, json).await?;
                } else {
                    // Seen, and said so, but the node keeps the latest document and its version: this
                    // one must not pass for it.
                    state
                        .graph_controller
                        .cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                            key,
                            props: vec![
                                Property(
                                    "stale_version_received".to_string(),
                                    GraphValue::I64(routing.version as i64),
                                ),
                                Property(
                                    "stale_received_at".to_string(),
                                    GraphValue::String(received_at),
                                ),
                            ],
                        }))?;
                    info!(
                        id = %routing.id,
                        version = routing.version,
                        "configuration is not newer than the one held; recorded, not forwarded"
                    );
                }
            }
            Err(why) => {
                error!("Cannot route the configuration document at {path}: {why}");
                state
                    .graph_controller
                    .cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                        key: ScheduleKey::Configuration {
                            id: format!("unroutable:{path}"),
                        }
                        .into_key(),
                        props: vec![
                            Property("document".to_string(), GraphValue::String(json)),
                            Property("source_path".to_string(), GraphValue::String(path)),
                            Property(
                                "status".to_string(),
                                GraphValue::String("unroutable".to_string()),
                            ),
                            Property("reason".to_string(), GraphValue::String(why)),
                            Property("received_at".to_string(), GraphValue::String(received_at)),
                        ],
                    }))?;
            }
        }
        Ok(())
    }

    /// An agent that has just registered asks for what is held for it: its documents go out again on its
    /// own topic, oldest first, so that an agent that applies versions in order ends at the latest. It
    /// verifies each like any other, and one it has already applied is refused as no newer.
    async fn handle_replay(
        state: &mut ProcessorState,
        agent: String,
    ) -> Result<(), ActorProcessingErr> {
        let mut held: Vec<&StoredDocument> = state
            .documents
            .values()
            .filter(|d| d.agent == agent)
            .collect();
        held.sort_by_key(|d| (d.version, d.id.clone()));
        let documents: Vec<String> = held.into_iter().map(|d| d.document.clone()).collect();
        info!(agent = %agent, count = documents.len(), "replaying held configuration");
        for document in documents {
            publish_configuration(state, &agent, document).await?;
        }
        Ok(())
    }

    /// What an agent did with a signed configuration document, written to the graph as one more node in
    /// a trail, and linked to the item it is about when it says which that was. `reported_by` is who the
    /// broker says published it: an outcome is only a claim by whoever sent it, so the graph keeps both.
    fn handle_outcome(
        state: &mut ProcessorState,
        outcome: ConfigurationOutcome,
        reported_by: Option<String>,
    ) -> Result<(), ActorProcessingErr> {
        let at = chrono::DateTime::from_timestamp(outcome.at_unix_secs as i64, 0)
            .map(|t| t.to_rfc3339())
            .unwrap_or_default();
        let key = format!(
            "{}|{}|{}|{}|{}",
            outcome.agent,
            outcome.id.as_deref().unwrap_or("?"),
            outcome.version.map_or("?".to_string(), |v| v.to_string()),
            at,
            outcome.sequence
        );
        let text = |value: String| GraphValue::String(value);
        let list = |values: &[String]| {
            GraphValue::List(values.iter().cloned().map(GraphValue::String).collect())
        };
        let mut props = vec![
            Property("agent".to_string(), text(outcome.agent.clone())),
            Property("status".to_string(), text(outcome.status.clone())),
            Property("changed".to_string(), list(&outcome.changed)),
            Property("detail".to_string(), list(&outcome.detail)),
            Property("at".to_string(), text(at)),
            Property(
                "sequence".to_string(),
                GraphValue::I64(outcome.sequence as i64),
            ),
        ];
        if let Some(id) = &outcome.id {
            props.push(Property("claimed_id".to_string(), text(id.clone())));
        }
        if let Some(version) = outcome.version {
            props.push(Property(
                "claimed_version".to_string(),
                GraphValue::I64(version as i64),
            ));
        }
        if let Some(signer) = &outcome.signer {
            props.push(Property("signer".to_string(), text(signer.clone())));
        }
        if let Some(who) = reported_by {
            props.push(Property("reported_by".to_string(), text(who)));
        }
        state
            .graph_controller
            .cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: ScheduleKey::ConfigurationOutcome { key: key.clone() }.into_key(),
                props,
            }))?;
        if let Some(id) = outcome.id {
            state
                .graph_controller
                .cast(GraphControllerMsg::Op(GraphOp::EnsureEdge {
                    from: ScheduleKey::Configuration { id }.into_key(),
                    to: ScheduleKey::ConfigurationOutcome { key }.into_key(),
                    rel_type: "HAD_OUTCOME".to_string(),
                    props: vec![],
                }))?;
        }
        Ok(())
    }

    async fn handle_adhoc_announcement(
        state: &mut ProcessorState,
        ann: AdhocAgentAnnouncement,
    ) -> Result<(), ActorProcessingErr> {
        let agent_type = ann.agent_type;
        let node_key = ScheduleKey::Adhoc {
            agent_type: agent_type.clone(),
        };

        let schedule_node: ScheduleNode = serde_json::from_str(&ann.default_schedule_json)?;
        let schedule_json = ann.default_schedule_json;

        state
            .graph_controller
            .cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                key: node_key.clone().into_key(),
                props: vec![Property(
                    "schedule".to_string(),
                    GraphValue::String(schedule_json.clone()),
                )],
            }))?;

        state
            .adhoc_versions
            .insert(agent_type.clone(), schedule_node.version);
        let notif = ScheduleNotification::AdhocUpdate {
            agent_type: agent_type.clone(),
            schedule_json,
        };
        publish_notification(
            state,
            &format!("agent.type.{}.schedule", agent_type),
            &notif,
        )
        .await?;
        Ok(())
    }

    async fn handle_event(
        state: &mut ProcessorState,
        topic: String,
        _payload: Vec<u8>,
    ) -> Result<(), ActorProcessingErr> {
        if let Some(agent_type) = state.ephemeral_patterns.get(&topic) {
            info!("Ephemeral trigger for {} on {}", agent_type, topic);
        }
        Ok(())
    }
}

async fn publish_configuration(
    state: &ProcessorState,
    agent: &str,
    document_json: String,
) -> Result<(), ActorProcessingErr> {
    let notification = ConfigurationNotification {
        agent: agent.to_string(),
        document_json,
    };
    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&notification)?.to_vec();
    state.tcp_client.cast(TcpClientMessage::publish(
        configuration_topic(agent),
        payload,
        None,
    ))?;
    Ok(())
}

async fn publish_notification(
    state: &ProcessorState,
    topic: &str,
    notif: &ScheduleNotification,
) -> Result<(), ActorProcessingErr> {
    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(notif)?.to_vec();
    state
        .tcp_client
        .cast(TcpClientMessage::publish(topic.to_string(), payload, None))?;
    Ok(())
}

#[ractor::async_trait]
impl Actor for ScheduleInfoProcessor {
    type Msg = ProcessorMsg;
    type State = ProcessorState;
    /// The Cassini client, the graph controller, and the documents the graph already held.
    type Arguments = (
        ActorRef<TcpClientMessage>,
        ActorRef<GraphControllerMsg>,
        Vec<StoredDocument>,
    );

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        (tcp_client, graph_controller, stored): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        info!("ScheduleInfoProcessor starting");
        Ok(ProcessorState {
            tcp_client,
            graph_controller,
            permanent_versions: HashMap::new(),
            adhoc_versions: HashMap::new(),
            ephemeral_patterns: HashMap::new(),
            documents: latest_of(stored),
        })
    }

    async fn post_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("ScheduleInfoProcessor started, ready to process messages");
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            ProcessorMsg::GitChange(change) => {
                info!("Processor received GitChange");
                Self::handle_git_change(state, change).await?
            }
            ProcessorMsg::Announcement(ann) => {
                info!("Processor received ad-hoc agent announcement");
                Self::handle_adhoc_announcement(state, ann).await?
            }
            ProcessorMsg::Event { topic, payload } => {
                info!("Processor received Event on {}", topic);
                Self::handle_event(state, topic, payload).await?
            }
            ProcessorMsg::Outcome {
                outcome,
                reported_by,
            } => Self::handle_outcome(state, outcome, reported_by)?,
            ProcessorMsg::ReplayRequest { agent } => Self::handle_replay(state, agent).await?,
        }
        Ok(())
    }
}
