use crate::KubernetesObserverMessage;
use cassini_client::TcpClientMessage;
use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::core::v1::Pod;
use kube::runtime::watcher;
use kube::{
    api::{Api, ListParams, ResourceExt},
    runtime::watcher::Event,
    Client,
};
use kube_common::{get_consumer_name, KUBERNETES_CONSUMER, RESOURCE_DELETED_ACTION};
use kube_common::{RawKubeEvent, BATCH_PROCESS_ACTION, RESOURCE_APPLIED_ACTION};
use polar::{ProvenanceEvent, PROVENANCE_DISCOVERY_TOPIC, PROVENANCE_LINKER_TOPIC};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use rkyv::{rancor, to_bytes};
use serde_json::to_vec;
use tokio::task::AbortHandle;
use tracing::{debug, error, info, instrument, trace, warn};

pub struct PodObserver;

pub struct PodObserverState {
    pub tcp_client: ActorRef<TcpClientMessage>,
    pub namespace: String,
    pub kube_client: kube::Client,
    pub watcher: Option<AbortHandle>,
}

pub enum PodObserverMessage {
    Deployments,
    ConfigMaps,
    Secrets,
}

pub struct PodObserverArgs {
    pub tcp_client: ActorRef<TcpClientMessage>,
    pub kube_client: Client,
    pub namespace: String,
}

impl PodObserver {
    /// Helper function to watch for events concerning pods.
    /// Runs inside of a thread we can cancel should problems arise.
    #[instrument(
        level = "trace",
        name = "PodObserver.watch_pods",
        skip(tcp_client, kube_client, namespace)
    )]
    async fn watch_pods(
        tcp_client: ActorRef<TcpClientMessage>,
        kube_client: Client,
        namespace: String,
    ) -> Result<(), ActorProcessingErr> {
        let api: Api<Pod> = Api::namespaced(kube_client.clone(), &namespace);
        let mut watcher = watcher(api, watcher::Config::default()).boxed();
        while let Some(event) = watcher.try_next().await? {
            match event {
                Event::Apply(pod) => {
                    PodObserver::log_pod_info(&pod);

                    if let Ok(serialized) = serde_json::to_value(&pod) {
                        let message = RawKubeEvent {
                            action: String::from(RESOURCE_APPLIED_ACTION),
                            kind: "Pod".to_string(), //anywhewre we can get constants for this
                            object: serialized,
                        };

                        // look inside, see what containers the pod is using
                        if let Some(spec) = pod.spec {
                            for container in spec.containers {
                                // emit provenance events
                                let event = ProvenanceEvent::ImageRefDiscovered {
                                    uri: container.name.clone(),
                                };
                                let payload = to_bytes::<rancor::Error>(&event)?;
                                tcp_client.cast(TcpClientMessage::Publish {
                                    topic: PROVENANCE_DISCOVERY_TOPIC.to_string(),
                                    payload: payload.into(),
                                })?;
                            }
                        }

                        if let Ok(payload) = to_vec(&message) {
                            tcp_client.cast(TcpClientMessage::Publish {
                                topic: KUBERNETES_CONSUMER.to_string(),
                                payload,
                            })?;
                        }
                    }
                }
                Event::Delete(pod) => {
                    debug!("Pod deleted: {}", pod.name_any());

                    match serde_json::to_value(&pod) {
                        Ok(serialized) => {
                            let message = RawKubeEvent {
                                object: serialized,
                                action: String::from(RESOURCE_DELETED_ACTION),
                                kind: String::from("Pod"),
                            };

                            let payload = to_vec(&message)?;
                            tcp_client.cast(TcpClientMessage::Publish {
                                topic: KUBERNETES_CONSUMER.to_string(),
                                payload,
                            })?;
                        }
                        Err(e) => warn!("{e}"),
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Helper to output pod data
    /// TODO: Use this on the consumer side
    fn log_pod_info(pod: &Pod) {
        let name = pod.name_any();
        debug!("  - Pod: {}", name);

        if let Some(spec) = &pod.spec {
            // ServiceAccount
            if let Some(sa) = &spec.service_account_name {
                debug!("    -> Uses ServiceAccount: {} ", sa);
            }

            // Volumes
            if let Some(volumes) = &spec.volumes {
                for volume in volumes {
                    debug!("    -> Uses Volume: {} ", volume.name);

                    volume
                        .config_map
                        .as_ref()
                        .map(|cm| debug!("    -> Mounts ConfigMap: {} ", cm.name.clone()));

                    volume
                        .persistent_volume_claim
                        .as_ref()
                        .map(|pvc| debug!("    -> Uses PVC: {} ", pvc.claim_name));

                    volume.secret.as_ref().map(|secret| {
                        debug!(
                            "    -> Mounts Secret: {} ",
                            secret.clone().secret_name.unwrap_or_default()
                        )
                    });
                }
            }

            // Containers and InitContainers
            let containers = spec
                .containers
                .iter()
                .chain(spec.init_containers.iter().flatten());
            for container in containers {
                debug!(
                    "    -> Container Image: {} ",
                    container.image.clone().unwrap_or_default()
                );

                // Environment variable references
                for env in container.env.iter().flatten() {
                    if let Some(value_from) = &env.value_from {
                        if let Some(cm_ref) = &value_from.config_map_key_ref {
                            debug!("      -> Env from ConfigMap: {} ", cm_ref.name.clone());
                        }
                        if let Some(secret_ref) = &value_from.secret_key_ref {
                            debug!("      -> Env from Secret: {} ", secret_ref.name.clone());
                        }
                    }
                }
            }
        }
    }
}

#[async_trait]
impl Actor for PodObserver {
    type Msg = KubernetesObserverMessage;
    type State = PodObserverState;
    type Arguments = PodObserverArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: PodObserverArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        let state = PodObserverState {
            tcp_client: args.tcp_client,
            namespace: args.namespace,
            kube_client: args.kube_client,
            watcher: None,
        };

        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // start tasks to watch the cluster
        if let Err(e) = myself.send_message(KubernetesObserverMessage::Pods) {
            error!("{e}");
            myself.stop(Some(e.to_string()));
        }

        info!("trying to watch {} namespace.", state.namespace);

        //spawn a new thread to watch for pods
        let tcp_client = state.tcp_client.clone();
        let kube_client = state.kube_client.clone();
        let ns = state.namespace.clone();

        let handle =
            tokio::spawn(async move { PodObserver::watch_pods(tcp_client, kube_client, ns).await })
                .abort_handle();

        state.watcher = Some(handle);

        Ok(())
    }

    async fn post_stop(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // cleanup watcher task

        if let Some(handle) = &state.watcher {
            handle.abort();
            info!("{myself:?} watcher stopped successfully.");
        }

        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            KubernetesObserverMessage::Pods => {
                // get all deployed pods in our given namespace
                let api: Api<Pod> = Api::namespaced(state.kube_client.clone(), &state.namespace);

                match api.list(&ListParams::default()).await {
                    Ok(pod_list) => {
                        let topic = get_consumer_name("cluster", "Pod");

                        // iterate and emit provenance events
                        for pod in &pod_list.items {
                            if let Some(spec) = &pod.spec {
                                for container in &spec.containers {
                                    if let Some(image) = &container.image {
                                        let event = ProvenanceEvent::ImageRefDiscovered {
                                            uri: image.clone(),
                                        };
                                        trace!("Emitting event: {:?}", event);
                                        let payload = to_bytes::<rancor::Error>(&event)?;
                                        state.tcp_client.cast(TcpClientMessage::Publish {
                                            topic: PROVENANCE_DISCOVERY_TOPIC.to_string(),
                                            payload: payload.into(),
                                        })?;

                                        if let Some(uid) = pod.metadata.uid.clone() {
                                            trace!("Emitting event: {:?}", event);
                                            let event = ProvenanceEvent::PodContainerUsesImage {
                                                pod_uid: uid,
                                                container_name: container.name.clone(),
                                                image_ref: image.clone(),
                                            };
                                            trace!("Emitting event: {:?}", event);
                                            let payload = to_bytes::<rancor::Error>(&event)?;
                                            state.tcp_client.cast(TcpClientMessage::Publish {
                                                topic: PROVENANCE_LINKER_TOPIC.to_string(),
                                                payload: payload.into(),
                                            })?;
                                        }
                                    }
                                }
                            }
                        }

                        match serde_json::to_value(pod_list.items) {
                            Ok(serialized) => {
                                let event = RawKubeEvent {
                                    kind: String::from("Pod"),
                                    action: String::from(BATCH_PROCESS_ACTION),
                                    object: serialized,
                                };

                                let payload = serde_json::to_string(&event).unwrap().into_bytes();

                                let envelope = TcpClientMessage::Publish { topic, payload };

                                // send data for batch processing
                                state.tcp_client.cast(envelope)?;
                            }
                            Err(_e) => todo!(),
                        }
                    }
                    Err(e) => {
                        warn!("Expected to get a list of Pods. {e}");
                        //TODO: Implement a backoff depending on the reason?
                    }
                }
            }
            _ => (),
        }
        Ok(())
    }
}
