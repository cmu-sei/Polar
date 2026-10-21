/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

use cassini_client::TcpClientMessage;
use polar::graph::{
    controller::{GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property},
    nodes::schedule::ScheduleKey,
};
use polar_scheduler_common::{
    ConfigurationNotification, ConfigurationOutcome, GitScheduleChange, configuration_topic,
};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::collections::HashMap;
use tracing::{error, info, warn};

// Import local types
use crate::configuration::{self, Routing, Shape};
use crate::types::ProcessorMsg;

pub struct ScheduleInfoProcessor;

pub struct ProcessorState {
    tcp_client: ActorRef<TcpClientMessage>,
    graph_controller: ActorRef<GraphControllerMsg>,
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
                // Only a signed document is handled; the unsigned schedule path was removed
                // (docs/architecture/policy-enforcement.md, "Decisions").
                match configuration::classify(&json) {
                    Shape::Document(routing) => {
                        return Self::handle_configuration(state, path, json, routing).await;
                    }
                    Shape::NotADocument => {
                        warn!(%path, "not a signed document, so it is ignored");
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
            ProcessorMsg::Outcome {
                outcome,
                reported_by,
            } => Self::handle_outcome(state, outcome, reported_by)?,
        }
        Ok(())
    }
}
