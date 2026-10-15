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

use crate::{GitlabConsumerState, UNKNOWN_FILED};
use common::{
    METADATA_CONSUMER_TOPIC,
    types::{GitlabData, GitlabEnvelope},
};
use gitlab_queries::LicenseHistoryEntry;

use polar::{
    cassini::{CassiniClient, SubscribeRequest},
    graph::{
        controller::{GraphControllerMsg, GraphOp, GraphValue, IntoGraphKey, Property},
        nodes::gitlab::GitlabNodeKey,
    },
};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use tracing::debug;

pub struct MetaConsumer;

impl MetaConsumer {
    pub fn ops_for_licenses(instance_id: String, licenses: &[LicenseHistoryEntry]) -> Vec<GraphOp> {
        let mut ops = vec![];

        let instance_k = GitlabNodeKey::GitlabInstance {
            instance_id: instance_id.clone(),
        };

        // ensure instance exists
        ops.push(GraphOp::UpsertNode {
            key: instance_k.clone().into_key(),
            props: Vec::default(),
        });

        for entry in licenses {
            let license_key = GitlabNodeKey::License {
                instance_id: instance_id.clone(),
                license_id: entry.id.to_string(),
            };

            let created_at = entry
                .created_at
                .clone()
                .map_or(UNKNOWN_FILED.to_string(), |d| d.to_string());
            let starts_at = entry
                .starts_at
                .clone()
                .map_or(UNKNOWN_FILED.to_string(), |d| d.to_string());
            let expires_at = entry
                .expires_at
                .clone()
                .map_or(UNKNOWN_FILED.to_string(), |d| d.to_string());

            let users_in = entry
                .users_in_license_count
                .map_or(GraphValue::I64(0), |u| GraphValue::I64(u as i64));

            ops.push(GraphOp::UpsertNode {
                key: license_key.clone().into_key(),
                props: vec![
                    Property("created_at".into(), GraphValue::String(created_at)),
                    Property("starts_at".into(), GraphValue::String(starts_at)),
                    Property("expires_at".into(), GraphValue::String(expires_at)),
                    Property("plan".into(), GraphValue::String(entry.plan.clone())),
                    Property("type".into(), GraphValue::String(entry.entry_type.clone())),
                    Property("users_in_licenseCount".into(), users_in),
                ],
            });

            ops.push(GraphOp::EnsureEdge {
                from: instance_k.clone().into_key(),
                to: license_key.into_key(),
                rel_type: "OBSERVED_LICENSE".into(),
                props: Vec::default(),
            });
        }

        ops
    }
}

#[ractor::async_trait]
impl Actor for MetaConsumer {
    type Msg = GitlabEnvelope;
    type State = GitlabConsumerState;
    type Arguments = GitlabConsumerState;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to broker");
        state.tcp_client.subscribe(SubscribeRequest {
            topic: METADATA_CONSUMER_TOPIC.to_string(),
            trace_ctx: None,
        })?;
        Ok(state)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message.data {
            GitlabData::Instance(instance) => {
                let op = GraphOp::UpsertNode {
                    key: GitlabNodeKey::GitlabInstance {
                        instance_id: message.instance_id.clone(),
                    }
                    .into_key(),
                    props: vec![
                        Property(
                            "enterprise".into(),
                            GraphValue::Bool(instance.metadata.enterprise),
                        ),
                        Property(
                            "version".into(),
                            GraphValue::String(instance.metadata.version.clone()),
                        ),
                        Property(
                            "base_url".into(),
                            GraphValue::String(instance.base_url.clone()),
                        ),
                    ],
                };

                state.graph_controller.cast(GraphControllerMsg::Op(op))?;
            }
            GitlabData::Licenses(licenses) => {
                let ops = Self::ops_for_licenses(message.instance_id.clone(), &licenses);

                for op in ops {
                    state.graph_controller.cast(GraphControllerMsg::Op(op))?;
                }
            }
            _ => (),
        }
        Ok(())
    }
}
