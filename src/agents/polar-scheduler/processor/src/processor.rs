use cassini_client::TcpClientMessage;
use polar::graph::{
    controller::{
        GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property,
    },
    nodes::schedule::ScheduleKey,
};
use polar_scheduler_common::{
    AdhocAgentAnnouncement, ConfigurationNotification, GitScheduleChange, ScheduleKind,
    ScheduleNode, ScheduleNotification, configuration_topic,
};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::collections::HashMap;
use tracing::{error, info};

// Import local types
use crate::configuration::{self, Routing, Shape};
use crate::types::ProcessorMsg;

pub struct ScheduleInfoProcessor;

pub struct ProcessorState {
    tcp_client: ActorRef<TcpClientMessage>,
    graph_controller: ActorRef<GraphControllerMsg>,
    permanent_versions: HashMap<String, u64>,
    adhoc_versions: HashMap<String, u64>,
    ephemeral_patterns: HashMap<String, String>, // pattern -> agent_type
    /// The highest version of each signed configuration item forwarded so far, by item id. It only saves
    /// forwarding what the receiver would refuse as stale; the receiver does the checking.
    configuration_versions: HashMap<String, u64>,
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
                state
                    .graph_controller
                    .cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                        key: ScheduleKey::Configuration {
                            id: routing.id.clone(),
                        }
                        .into_key(),
                        props: vec![
                            Property("document".to_string(), GraphValue::String(json.clone())),
                            Property("target".to_string(), GraphValue::String(routing.agent.clone())),
                            Property("version".to_string(), GraphValue::I64(routing.version as i64)),
                            Property("source_path".to_string(), GraphValue::String(path)),
                            Property("status".to_string(), GraphValue::String("received".to_string())),
                            Property("received_at".to_string(), GraphValue::String(received_at)),
                        ],
                    }))?;

                let newer = state
                    .configuration_versions
                    .get(&routing.id)
                    .is_none_or(|seen| routing.version > *seen);
                if newer {
                    state
                        .configuration_versions
                        .insert(routing.id.clone(), routing.version);
                    publish_configuration(state, &routing.agent, json).await?;
                } else {
                    info!(
                        id = %routing.id,
                        version = routing.version,
                        "configuration is not newer than one already forwarded; recorded, not forwarded"
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
                            Property("status".to_string(), GraphValue::String("unroutable".to_string())),
                            Property("reason".to_string(), GraphValue::String(why)),
                            Property("received_at".to_string(), GraphValue::String(received_at)),
                        ],
                    }))?;
            }
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
    state.tcp_client.cast(TcpClientMessage::publish(
        topic.to_string(),
        payload,
        None,
    ))?;
    Ok(())
}

#[ractor::async_trait]
impl Actor for ScheduleInfoProcessor {
    type Msg = ProcessorMsg;
    type State = ProcessorState;
    type Arguments = (ActorRef<TcpClientMessage>, ActorRef<GraphControllerMsg>);

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        (tcp_client, graph_controller): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        info!("ScheduleInfoProcessor starting");
        Ok(ProcessorState {
            tcp_client,
            graph_controller,
            permanent_versions: HashMap::new(),
            adhoc_versions: HashMap::new(),
            ephemeral_patterns: HashMap::new(),
            configuration_versions: HashMap::new(),
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
        }
        Ok(())
    }
}
