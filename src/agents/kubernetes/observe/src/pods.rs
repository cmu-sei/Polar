use crate::{send_to_client, KubernetesObserverMessage, TCP_CLIENT_NAME};
use cassini_client::TcpClientMessage;
use cassini_types::ClientMessage;
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
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use serde_json::to_vec;
use tokio::task::AbortHandle;
use tracing::{debug, error, info, warn};

pub struct PodObserver;

pub struct PodObserverState {
    pub registration_id: String,
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
    pub registration_id: String,
    pub kube_client: Client,
    pub namespace: String,
}

impl PodObserver {
    /// Helper function to watch for events concerning pods.
    /// Runs inside of a thread we can cancel should problems arise.
    async fn watch_pods(
        registration_id: String,
        client: Client,
        namespace: String,
    ) -> Result<(), watcher::Error> {
        let api: Api<Pod> = Api::namespaced(client.clone(), &namespace);
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

                        if let Ok(payload) = to_vec(&message) {
                            send_to_client(
                                registration_id.clone(),
                                KUBERNETES_CONSUMER.to_string(),
                                payload,
                            )
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

                            match to_vec(&message) {
                                Ok(payload) => send_to_client(
                                    registration_id.clone(),
                                    get_consumer_name("cluster", "Pod"),
                                    payload,
                                ),
                                Err(e) => error!("{e}"),
                            }
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
            registration_id: args.registration_id,
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
        let client = state.kube_client.clone();
        let id = state.registration_id.clone();
        let ns = state.namespace.clone();

        let handle = tokio::spawn(async move { PodObserver::watch_pods(id, client, ns).await })
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
        myself: ActorRef<Self::Msg>,
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

                        match serde_json::to_value(pod_list.items) {
                            Ok(serialized) => {
                                let event = RawKubeEvent {
                                    kind: String::from("Pod"),
                                    action: String::from(BATCH_PROCESS_ACTION),
                                    object: serialized,
                                };

                                let payload = serde_json::to_string(&event).unwrap();

                                let envelope =
                                    TcpClientMessage::Send(ClientMessage::PublishRequest {
                                        topic,
                                        payload: payload.into_bytes(),
                                        registration_id: Some(state.registration_id.clone()),
                                    });

                                // send data for batch processing

                                match where_is(TCP_CLIENT_NAME.to_owned()) {
                                    Some(client) => {
                                        if let Err(e) = client.send_message(envelope) {
                                            warn!("Failed to send message to client {e}");
                                        }
                                    }
                                    None => {
                                        error!("No cassini client found, stopping");
                                        myself.stop(None);
                                    }
                                }
                            }
                            Err(e) => todo!(),
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
