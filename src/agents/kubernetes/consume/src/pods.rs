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
use k8s_openapi::api::core::v1::Pod;
use kube_common::KubeMessage;
use neo4rs::Query;
use polar::{ProvenanceEvent, PROVENANCE_DISCOVERY_TOPIC, QUERY_COMMIT_FAILED, QUERY_RUN_FAILED};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use rkyv::rancor;
use serde_json::from_value;
use tracing::{debug, error, info, instrument, trace};
use cassini_types::WireTraceCtx;

use crate::{KubeConsumerArgs, KubeConsumerState};

use std::collections::HashSet;

pub struct PodConsumer;

impl PodConsumer {
    #[instrument(
        level = "debug",
        name = "PodConsumer::pods_to_cypher",
        skip(state, pods)
    )]
    pub fn pods_to_cypher(
        state: &mut KubeConsumerState,
        pods: &[Pod],
    ) -> Result<Vec<String>, ActorProcessingErr> {
        let mut statements = Vec::new();
        let mut seen_volumes = HashSet::new();
        let mut seen_configmaps = HashSet::new();
        let mut seen_secrets = HashSet::new();
        let mut seen_pvcs = HashSet::new();
        let mut seen_images = HashSet::new();

        for pod in pods {
            //add unique id to podsmut
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

            // Pod node
            statements.push(format!(
                "MERGE (p:Pod {{uid: '{uid}', name: '{pod_name}', namespace: '{namespace}' }}) \
                 SET p.serviceAccountName = '{sa_name}'"
            ));

            if let Some(spec) = &pod.spec {
                // Volumes
                if let Some(volumes) = &spec.volumes {
                    for volume in volumes {
                        let vol_key = format!("{}::{}", namespace, volume.name);
                        if seen_volumes.insert(vol_key.clone()) {
                            statements.push(format!(
                                "MERGE (v:Volume {{ name: '{}', namespace: '{}' }})",
                                volume.name, namespace
                            ));
                        }

                        statements.push(format!(
                            "MATCH (p:Pod {{ name: '{}', namespace: '{}' }}), \
                                   (v:Volume {{ name: '{}', namespace: '{}' }}) \
                             MERGE (p)-[:USES_VOLUME]->(v)",
                            pod_name, namespace, volume.name, namespace
                        ));

                        // Volume -> ConfigMap
                        if let Some(cm) = &volume.config_map {
                            if seen_configmaps.insert(format!("{}::{}", namespace, cm.name)) {
                                statements.push(format!(
                                    "MERGE (cm:ConfigMap {{ name: '{}', namespace: '{}' }})",
                                    cm.name, namespace
                                ));
                            }
                            statements.push(format!(
                                "MATCH (v:Volume {{ name: '{}', namespace: '{}' }}), \
                                       (cm:ConfigMap {{ name: '{}', namespace: '{}' }}) \
                                 MERGE (v)-[:BACKED_BY]->(cm)",
                                volume.name, namespace, cm.name, namespace
                            ));
                        }

                        // Volume -> Secret
                        if let Some(secret) = &volume.secret {
                            if let Some(secret_name) = &secret.secret_name {
                                if seen_secrets.insert(format!("{}::{}", namespace, secret_name)) {
                                    statements.push(format!(
                                        "MERGE (s:Secret {{ name: '{}', namespace: '{}' }})",
                                        secret_name, namespace
                                    ));
                                }
                                statements.push(format!(
                                    "MATCH (v:Volume {{ name: '{}', namespace: '{}' }}), \
                                           (s:Secret {{ name: '{}', namespace: '{}' }}) \
                                     MERGE (v)-[:BACKED_BY]->(s)",
                                    volume.name, namespace, secret_name, namespace
                                ));
                            }
                        }

                        // Volume -> PVC
                        if let Some(pvc) = &volume.persistent_volume_claim {
                            if seen_pvcs.insert(format!("{}::{}", namespace, pvc.claim_name)) {
                                statements.push(format!(
                                    "MERGE (pvc:PersistentVolumeClaim {{ name: '{}', namespace: '{}' }})",
                                    pvc.claim_name, namespace
                                ));
                            }
                            statements.push(format!(
                                "MATCH (v:Volume {{ name: '{}', namespace: '{}' }}), \
                                       (pvc:PersistentVolumeClaim {{ name: '{}', namespace: '{}' }}) \
                                 MERGE (v)-[:BACKED_BY]->(pvc)",
                                volume.name, namespace, pvc.claim_name, namespace
                            ));
                        }
                    }
                }

                // Containers and InitContainers
                let containers = spec
                    .containers
                    .iter()
                    .chain(spec.init_containers.iter().flatten());

                for container in containers {
                    if let Some(image) = &container.image {
                        let pod_uid = pod.metadata.uid.clone().unwrap(); // TODO: I know this is unwise, but we need a hard identifier for pods.
                        if seen_images.insert(image.clone()) {
                            statements.push(format!(
                                "
                                // Ensure a PodContainer node for this pod+container
                                MERGE (p:Pod {{ pod_uid: '{pod_uid}'}})
                                SET
                                p.name = '{pod_name}',
                                p.namespace = '{namespace}'

                                MERGE (c:PodContainer {{
                                    pod_uid: '{pod_uid}',
                                    container_name: '{container_name}'
                                }})
                                SET
                                    c.namespace = '{namespace}',
                                    c.pod_name  = '{pod_name}',
                                    c.image     = '{image}'

                                MERGE (p)-[:HAS_CONTAINER]->(c)
                                    ",
                                container_name = container.name.as_str(),
                                image = image.as_str(),
                            ));
                        }
                    }

                    if let Some(envs) = &container.env {
                        for env in envs {
                            if let Some(value_from) = &env.value_from {
                                if let Some(cm_ref) = &value_from.config_map_key_ref {
                                    if seen_configmaps
                                        .insert(format!("{}::{}", namespace, cm_ref.name))
                                    {
                                        statements.push(format!(
                                            "MERGE (cm:ConfigMap {{ name: '{}', namespace: '{}' }})",
                                            cm_ref.name, namespace
                                        ));
                                    }
                                    //link configmap to container
                                    statements.push(format!(
                                        "MATCH (p:Pod {{ name: '{}', namespace: '{}' }}), \
                                               (cm:ConfigMap {{ name: '{}', namespace: '{}' }}) \
                                         MERGE (p)-[:USES_CONFIGMAP]->(cm)",
                                        pod_name, namespace, cm_ref.name, namespace
                                    ));
                                }

                                if let Some(secret_ref) = &value_from.secret_key_ref {
                                    if seen_secrets
                                        .insert(format!("{}::{}", namespace, secret_ref.name))
                                    {
                                        statements.push(format!(
                                            "MERGE (s:Secret {{ name: '{}', namespace: '{}' }})",
                                            secret_ref.name, namespace
                                        ));
                                    }
                                    statements.push(format!(
                                        "MATCH (p:Pod {{ name: '{}', namespace: '{}' }}), \
                                               (s:Secret {{ name: '{}', namespace: '{}' }}) \
                                         MERGE (p)-[:USES_SECRET]->(s)",
                                        pod_name, namespace, secret_ref.name, namespace
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(statements)
    }
}

// pub enum Message {
//     MessageK
// }
#[async_trait]
impl Actor for PodConsumer {
    type Msg = KubeMessage; // TODO: Looks like ractor can't use generic enums for message types, very inconvenient
    type State = KubeConsumerState;
    type Arguments = KubeConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: KubeConsumerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to broker");

        args.broker_client
            .send_message(TcpClientMessage::Subscribe {
                topic: myself.get_name().unwrap().to_string(),
                trace_ctx: WireTraceCtx::from_current_span(),
            })?;

        //load neo config and connect to graph db
        match neo4rs::Graph::connect(args.graph_config) {
            Ok(graph) => Ok(KubeConsumerState {
                broker_client: args.broker_client,
                registration_id: args.registration_id,
                graph,
            }),
            Err(e) => Err(ActorProcessingErr::from(format!(
                "Failed to connect to Neo4j: {e}"
            ))),
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
                    KubeMessage::ResourceBatch { resources, .. } => {
                        match serde_json::from_value::<Vec<Pod>>(resources) {
                            Ok(pods) => {
                                debug!("Received {} pod(s)", pods.len());

                                let queries = Self::pods_to_cypher(state, &pods)?;

                                for query in queries {
                                    debug!("{query:?}");
                                    if let Err(_e) = transaction.run(Query::new(query)).await {
                                        myself.stop(Some(QUERY_RUN_FAILED.to_string()));
                                    }
                                }

                                if let Err(_e) = transaction.commit().await {
                                    myself.stop(Some(QUERY_COMMIT_FAILED.to_string()));
                                }

                                info!("Transaction committed.");
                            }
                            Err(e) => todo!("{e}"),
                        }
                    }
                    KubeMessage::ResourceApplied { resource, .. } => {
                        match from_value::<Pod>(resource) {
                            Ok(pod) => {
                                let queries = Self::pods_to_cypher(state, &vec![pod])?;

                                for query in queries {
                                    debug!("{query:?}");
                                    if let Err(e) = transaction.run(Query::new(query)).await {
                                        let err = format!("{QUERY_RUN_FAILED} {e}");
                                        error!("{err}");
                                        myself.stop(Some(err));
                                    }
                                }

                                if let Err(_e) = transaction.commit().await {
                                    myself.stop(Some(QUERY_COMMIT_FAILED.to_string()));
                                }

                                info!("Transaction committed.");
                            }
                            Err(e) => todo!("{e}"),
                        }
                    }
                    KubeMessage::ResourceDeleted { resource, .. } => {
                        match from_value::<Pod>(resource) {
                            Ok(pod) => {
                                //add unique id to podsmut
                                let uid = pod.metadata.uid.clone().unwrap_or_default();

                                let timestamp = chrono::Utc::now().to_rfc3339();

                                let cypher_query = format!(
                                    r#"
                                    MATCH (pod:Pod) WHERE pod.uid = "{uid}"
                                    WITH pod
                                    SET pod.deleteAt = "{timestamp}"

                                    WITH pod

                                    MATCH (c:PodContainer)<-[:HAS_CONTAINER]-(pod)
                                    UNWIND c as container
                                    SET container.deleteAt = "{timestamp}"
                                    "#
                                );
                                debug!("{cypher_query}");
                                if let Err(e) = transaction.run(Query::new(cypher_query)).await {
                                    let err = format!("{QUERY_RUN_FAILED} {e}");
                                    error!("{err}");
                                    myself.stop(Some(err));
                                }

                                if let Err(_e) = transaction.commit().await {
                                    myself.stop(Some(QUERY_COMMIT_FAILED.to_string()));
                                }

                                // TODO: Detach and delete pod node
                                // let delete_query =
                                //     format!("MATCH (p:Pod {{ name: '{pod_name}', namespace: '{namespace}' }}) DETACH DELETE p");
                            }
                            Err(e) => error!("{e}"),
                        }
                    }
                    _ => todo!(),
                }
            }
            Err(_e) => todo!(), //myself.stop(Some(format!("{TRANSACTION_FAILED_ERROR}. {e}")))
        }

        Ok(())
    }
}
