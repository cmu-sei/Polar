use crate::{GitlabConsumerState, UNKNOWN_FILED};
use cassini_client::TcpClientMessage;
use common::{
    types::{GitlabData, GitlabEnvelope},
    METADATA_CONSUMER_TOPIC,
};
use gitlab_queries::LicenseHistoryEntry;

use crate::GitlabNodeKey;
use polar::graph::{GraphControllerMsg, GraphOp, GraphValue, Property};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use tracing::debug;

pub struct MetaConsumer;

impl MetaConsumer {
    pub fn ops_for_licenses(
        instance_id: String,
        licenses: &[LicenseHistoryEntry],
    ) -> Vec<GraphOp<GitlabNodeKey>> {
        let mut ops = vec![];

        let instance_k = GitlabNodeKey::GitlabInstance {
            instance_id: instance_id.clone(),
        };

        // ensure instance exists
        ops.push(GraphOp::UpsertNode {
            key: instance_k.clone(),
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
                key: license_key.clone(),
                props: vec![
                    Property("createdAt".into(), GraphValue::String(created_at)),
                    Property("startsAt".into(), GraphValue::String(starts_at)),
                    Property("expiresAt".into(), GraphValue::String(expires_at)),
                    Property("plan".into(), GraphValue::String(entry.plan.clone())),
                    Property("type".into(), GraphValue::String(entry.entry_type.clone())),
                    Property("usersInLicenseCount".into(), users_in),
                ],
            });

            ops.push(GraphOp::EnsureEdge {
                from: instance_k.clone(),
                to: license_key,
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
        state.tcp_client.cast(TcpClientMessage::Subscribe(
            METADATA_CONSUMER_TOPIC.to_string(),
        ))?;
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
                    },
                    props: vec![
                        Property(
                            "enterprise".into(),
                            GraphValue::Bool(instance.metadata.enterprise.clone()),
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
