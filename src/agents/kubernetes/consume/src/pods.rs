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

use cassini_client::TcpClientMessage;
use chrono::Utc;
use k8s_openapi::api::core::v1::Pod;
use kube_common::KubeMessage;
use polar::{
    graph::{GraphOp, GraphValue, Property},
    ProvenanceEvent, PROVENANCE_DISCOVERY_TOPIC,
};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use rkyv::{rancor, to_bytes};
use serde_json::from_value;
use tracing::{debug, error, info, trace};

use crate::{KubeConsumerArgs, KubeConsumerState, KubeNodeKey};

pub struct PodConsumer;

impl PodConsumer {
    /// This is where a majority of the processing logic takes place.
    /// We receive some Pod representation, and construct graph operations that will become queries based on the keys contained within them.
    fn handle_pod(
        pod: &Pod,
        graph_controller: &ActorRef<GraphOp<KubeNodeKey>>,
        tcp_client: &ActorRef<TcpClientMessage>,
    ) -> Result<(), ActorProcessingErr> {
        let uid = pod.metadata.uid.clone().unwrap_or_default();
        let pod_name = pod.metadata.name.clone().unwrap_or_default();
        let namespace = pod
            .metadata
            .namespace
            .clone()
            .unwrap_or_else(|| "default".to_string());

        let sa_name = pod
            .spec
            .as_ref()
            .and_then(|s| s.service_account_name.clone())
            .unwrap_or_default();

        let pod_key = KubeNodeKey::Pod { uid: uid.clone() };

        let props = vec![
            Property("name".to_string(), GraphValue::String(pod_name.clone())),
            Property(
                "namespace".to_string(),
                GraphValue::String(namespace.clone()),
            ),
            Property("sa_name".to_string(), GraphValue::String(sa_name)),
            Property(
                "observed_at".to_string(),
                GraphValue::String(Utc::now().to_rfc3339()),
            ),
        ];

        graph_controller.cast(GraphOp::UpsertNode {
            key: pod_key.clone(),
            props,
        })?;

        let Some(spec) = &pod.spec else { return Ok(()) };

        if let Some(volumes) = &spec.volumes {
            for volume in volumes {
                let vol_key = KubeNodeKey::Volume {
                    name: volume.name.clone(),
                    namespace: namespace.clone(),
                };

                graph_controller.cast(GraphOp::UpsertNode {
                    key: vol_key.clone(),
                    props: Vec::default(),
                })?;

                graph_controller.cast(GraphOp::EnsureEdge {
                    from: pod_key.clone(),
                    rel_type: "USES_VOLUME".to_string(),
                    to: vol_key.clone(),
                    props: Vec::default(),
                })?;

                if let Some(cm) = &volume.config_map {
                    let cm_key = KubeNodeKey::ConfigMap {
                        name: cm.name.clone(),
                        namespace: namespace.clone(),
                    };

                    graph_controller.cast(GraphOp::UpsertNode {
                        key: cm_key.clone(),
                        props: Vec::default(),
                    })?;
                    graph_controller.cast(GraphOp::EnsureEdge {
                        from: vol_key.clone(),
                        rel_type: "BACKED_BY".to_string(),
                        to: cm_key,
                        props: Vec::default(),
                    })?;
                }

                if let Some(secret) = &volume.secret {
                    if let Some(secret_name) = &secret.secret_name {
                        let s_key = KubeNodeKey::Secret {
                            name: secret_name.clone(),
                            namespace: namespace.clone(),
                        };

                        graph_controller.cast(GraphOp::UpsertNode {
                            key: s_key.clone(),
                            props: Vec::default(),
                        })?;

                        graph_controller.cast(GraphOp::EnsureEdge {
                            from: vol_key.clone(),
                            rel_type: "BACKED_BY".to_string(),
                            to: s_key,
                            props: Vec::default(),
                        })?;
                    }
                }

                if let Some(pvc) = &volume.persistent_volume_claim {
                    let pvc_key = KubeNodeKey::PersistentVolumeClaim {
                        name: pvc.claim_name.clone(),
                        namespace: namespace.clone(),
                    };

                    graph_controller.cast(GraphOp::UpsertNode {
                        key: pvc_key.clone(),
                        props: Vec::default(),
                    })?;

                    graph_controller.cast(GraphOp::EnsureEdge {
                        from: vol_key,
                        rel_type: "BACKED_BY".to_string(),
                        to: pvc_key.clone(),
                        props: Vec::default(),
                    })?;
                }
            }
        }

        // We can consider init containers as part of the whole here
        let containers = spec
            .containers
            .iter()
            .chain(spec.init_containers.iter().flatten());

        for container in containers {
            let Some(image) = &container.image else {
                continue;
            };
            let container_key = KubeNodeKey::PodContainer {
                pod_uid: uid.clone(),
                name: container.name.clone(),
            };
            // TODO: add additional properties
            graph_controller.cast(GraphOp::UpsertNode {
                key: container_key.clone(),
                props: Vec::default(),
            })?;

            graph_controller.cast(GraphOp::EnsureEdge {
                from: pod_key.clone(),
                rel_type: "HAS_CONTAINER".to_string(),
                to: container_key.clone(),
                props: Vec::default(),
            })?;

            // emit provenance messages to trigger resolution

            let event = ProvenanceEvent::ImageRefDiscovered {
                uri: image.to_owned(),
            };
            trace!("Emitting event: {event:?} to topic: {PROVENANCE_DISCOVERY_TOPIC}");
            let payload = to_bytes::<rancor::Error>(&event)?;
            tcp_client.cast(TcpClientMessage::Publish {
                topic: PROVENANCE_DISCOVERY_TOPIC.to_string(),
                payload: payload.into(),
            })?;

            // env -> configmap / secret
            if let Some(envs) = &container.env {
                for env in envs {
                    let Some(value_from) = &env.value_from else {
                        continue;
                    };

                    if let Some(cm_ref) = &value_from.config_map_key_ref {
                        let cm_key = KubeNodeKey::ConfigMap {
                            name: cm_ref.name.clone(),
                            namespace: namespace.clone(),
                        };

                        graph_controller.cast(GraphOp::EnsureEdge {
                            from: pod_key.clone(),
                            rel_type: "USES_CONFIGMAP".to_string(),
                            to: cm_key,
                            props: Vec::default(),
                        })?;
                    }

                    if let Some(secret_ref) = &value_from.secret_key_ref {
                        let s_key = KubeNodeKey::Secret {
                            name: secret_ref.name.clone(),
                            namespace: namespace.clone(),
                        };

                        graph_controller.cast(GraphOp::EnsureEdge {
                            from: pod_key.clone(),
                            rel_type: "USES_SECRET".to_string(),
                            to: s_key,
                            props: Vec::default(),
                        })?;
                    }
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Actor for PodConsumer {
    type Msg = KubeMessage;
    type State = KubeConsumerState;
    type Arguments = KubeConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: KubeConsumerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to broker");

        args.broker_client
            .send_message(TcpClientMessage::Subscribe(myself.get_name().unwrap()))?;

        let state = KubeConsumerState {
            graph_controller: args.graph_controller,
            broker_client: args.broker_client,
        };
        Ok(state)
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
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        //Expect transaction to start, stop if it doesn't
        match message {
            KubeMessage::ResourceBatch { resources, .. } => {
                let pods = serde_json::from_value::<Vec<Pod>>(resources)?;
                debug!("Received {} pod(s)", pods.len());

                for pod in pods {
                    debug!("processing pod {name:?}", name = pod.metadata.name);
                    Self::handle_pod(&pod, &state.graph_controller, &state.broker_client)?;
                }
            }
            KubeMessage::ResourceApplied { resource, .. } => match from_value::<Pod>(resource) {
                Ok(pod) => {
                    Self::handle_pod(&pod, &state.graph_controller, &state.broker_client)?;
                }
                Err(e) => todo!("{e}"),
            },
            KubeMessage::ResourceDeleted { resource, .. } => match from_value::<Pod>(resource) {
                Ok(pod) => {
                    trace!("Received ResourceDeleted directive.");
                    let uid = pod.metadata.uid.clone().unwrap_or_default();

                    let timestamp = chrono::Utc::now().to_rfc3339();

                    let p_key = KubeNodeKey::Pod { uid };

                    let props = vec![Property(
                        "deleteAt".to_string(),
                        GraphValue::String(timestamp.into()),
                    )];

                    let op = GraphOp::UpsertNode { key: p_key, props };

                    state.graph_controller.cast(op)?;
                }
                Err(e) => error!("{e}"),
            },
            _ => todo!(),
        }

        Ok(())
    }
}
