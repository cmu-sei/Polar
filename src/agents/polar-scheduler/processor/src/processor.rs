use crate::types::*;
use polar_scheduler_common::{GitScheduleChange, AdhocAgentAnnouncement};
use cassini_client::TcpClientMessage;
use neo4rs::Graph;
use polar::graph::{GraphControllerMsg, GraphOp, GraphValue, Property};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::collections::HashMap;
use tracing::{error, info};

// Use the macro to generate the graph controller actor
polar::impl_graph_controller!(ScheduleGraphController, node_key = ScheduleKey);

pub struct ScheduleInfoProcessor;

pub struct ProcessorState {
    tcp_client: ActorRef<TcpClientMessage>,
    graph_controller: ActorRef<GraphControllerMsg<ScheduleKey>>,
    permanent_versions: HashMap<String, u64>,
    adhoc_versions: HashMap<String, u64>,
    ephemeral_patterns: HashMap<String, String>, // pattern -> agent_type
}

impl ScheduleInfoProcessor {
    async fn handle_git_change(
        state: &mut ProcessorState,
        change: GitScheduleChange,
    ) -> Result<(), ActorProcessingErr> {
        match change {
            GitScheduleChange::Create { path: _, content } | GitScheduleChange::Update { path: _, content } => {
                let schedule_node = evaluate_dhall(&content)?;
                let node_key = match schedule_node.kind {
                    ScheduleKind::Permanent => ScheduleKey::Permanent { agent_id: schedule_node.agent_id.clone().unwrap() },
                    ScheduleKind::Adhoc => ScheduleKey::Adhoc { agent_type: schedule_node.agent_type.clone().unwrap() },
                    ScheduleKind::Ephemeral => ScheduleKey::Ephemeral { agent_type: schedule_node.agent_type.clone().unwrap() },
                };

                let schedule_json = serde_json::to_string(&schedule_node)?;
                state.graph_controller.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
                    key: node_key.clone(),
                    props: vec![Property("schedule".to_string(), GraphValue::String(schedule_json.clone()))],
                }))?;

                match schedule_node.kind {
                    ScheduleKind::Permanent => {
                        let agent_id = schedule_node.agent_id.as_ref().unwrap();
                        let prev = state.permanent_versions.get(agent_id).copied();
                        if prev != Some(schedule_node.version) {
                            state.permanent_versions.insert(agent_id.clone(), schedule_node.version);
                            let notif = ScheduleNotification::PermanentUpdate {
                                agent_id: agent_id.clone(),
                                schedule_json: schedule_json.clone(),
                            };
                            publish_notification(state, &format!("agent.{}.schedule", agent_id), &notif).await?;
                        }
                    }
                    ScheduleKind::Adhoc => {
                        let agent_type = schedule_node.agent_type.as_ref().unwrap();
                        let prev = state.adhoc_versions.get(agent_type).copied();
                        if prev != Some(schedule_node.version) {
                            state.adhoc_versions.insert(agent_type.clone(), schedule_node.version);
                            let notif = ScheduleNotification::AdhocUpdate {
                                agent_type: agent_type.clone(),
                                schedule_json: schedule_json.clone(),
                            };
                            publish_notification(state, &format!("agent.type.{}.schedule", agent_type), &notif).await?;
                        }
                    }
                    ScheduleKind::Ephemeral => {
                        let agent_type = schedule_node.agent_type.as_ref().unwrap();
                        let pattern = schedule_node.config["eventPattern"].as_str().unwrap_or("");
                        if !pattern.is_empty() {
                            state.ephemeral_patterns.insert(pattern.to_string(), agent_type.clone());
                        }
                        let notif = ScheduleNotification::EphemeralUpdate {
                            agent_type: agent_type.clone(),
                            schedule_json: schedule_json.clone(),
                        };
                        publish_notification(state, &format!("agent.type.{}.schedule", agent_type), &notif).await?;
                    }
                }
            }
            GitScheduleChange::Delete { path: _ } => {
                // Handle deletion (omitted for brevity)
            }
        }
        Ok(())
    }

    async fn handle_adhoc_announcement(
        state: &mut ProcessorState,
        ann: AdhocAgentAnnouncement,
    ) -> Result<(), ActorProcessingErr> {
        let agent_type = ann.agent_type;
        let node_key = ScheduleKey::Adhoc { agent_type: agent_type.clone() };

        let schedule_node: ScheduleNode = serde_json::from_str(&ann.default_schedule_json)?;
        let schedule_json = ann.default_schedule_json;

        state.graph_controller.cast(GraphControllerMsg::Op(GraphOp::UpsertNode {
            key: node_key.clone(),
            props: vec![Property("schedule".to_string(), GraphValue::String(schedule_json.clone()))],
        }))?;

        state.adhoc_versions.insert(agent_type.clone(), schedule_node.version);
        let notif = ScheduleNotification::AdhocUpdate {
            agent_type: agent_type.clone(),
            schedule_json,
        };
        publish_notification(state, &format!("agent.type.{}.schedule", agent_type), &notif).await?;
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

async fn publish_notification(
    state: &ProcessorState,
    topic: &str,
    notif: &ScheduleNotification,
) -> Result<(), ActorProcessingErr> {
    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(notif)?.to_vec();
    state.tcp_client.cast(TcpClientMessage::Publish {
        topic: topic.to_string(),
        payload,
        trace_ctx: None,
    })?;
    Ok(())
}

fn evaluate_dhall(content: &[u8]) -> Result<ScheduleNode, ActorProcessingErr> {
    // Try Dhall first, fallback to JSON (for testing)
    if let Ok(text) = std::str::from_utf8(content) {
        if let Ok(node) = serde_dhall::from_str(text).parse() {
            return Ok(node);
        }
    }
    let json_str = std::str::from_utf8(content)?;
    let node: ScheduleNode = serde_json::from_str(json_str)?;
    Ok(node)
}

#[ractor::async_trait]
impl Actor for ScheduleInfoProcessor {
    type Msg = ProcessorMsg;
    type State = ProcessorState;
    type Arguments = (ActorRef<TcpClientMessage>, Graph);

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        (tcp_client, graph): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        info!("ScheduleInfoProcessor pre_start: spawning graph controller");
        let (graph_controller, _) = Actor::spawn(
            Some("scheduler.graph_controller".to_string()),
            ScheduleGraphController,
            graph,
        )
        .await
        .map_err(|e| {
            error!("Failed to spawn graph controller: {:?}", e);
            e
        })?;
        info!("Graph controller spawned successfully");
        Ok(ProcessorState {
            tcp_client,
            graph_controller,
            permanent_versions: HashMap::new(),
            adhoc_versions: HashMap::new(),
            ephemeral_patterns: HashMap::new(),
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
