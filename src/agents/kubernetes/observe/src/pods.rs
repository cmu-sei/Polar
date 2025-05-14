use cassini::{client::TcpClientMessage, ClientMessage};
use kube::{api::{Api, ListParams, ResourceExt}, runtime::watcher::Event, Client};
use k8s_openapi::api::core::v1::{Node, Pod, Volume};
use kube::runtime::watcher;
use ractor::{async_trait, registry::where_is, rpc::{call, CallResult}, Actor, ActorProcessingErr, ActorRef};
use futures::{StreamExt, TryStreamExt};
use tokio::{net::tcp, task::JoinHandle};
use tracing::{debug, error, info, warn};

use crate::{send_to_client, KubeMessage, KubernetesObserverMessage, KUBERNETES_CONSUMER, TCP_CLIENT_NAME};

pub struct PodObserver;

pub struct PodObserverState {
    pub registration_id: String,
    pub namespace: String,
    pub kube_client: kube::Client,
    pub watcher: Option<JoinHandle<Result<(), watcher::Error>>>,
}

pub enum PodObserverMessage {
    Deployments,
    ConfigMaps,
    Secrets
}

pub struct PodObserverArgs {
    pub registration_id: String,
    pub kube_client: Client,
}

impl PodObserver {

    /// Helper function to watch for events concerning pods.
    /// Runs inside of a thread we can cancel should problems arise.
    async fn watch_pods(registration_id: String, client: Client, namespace: String) -> Result<(), watcher::Error> {

        let api: Api<Pod> = Api::namespaced(client.clone(), &namespace);
        let mut watcher = watcher(api, watcher::Config::default()).boxed();
        while let Some(event) = watcher.try_next().await? {
            match event {
                Event::Apply(pod) => {
                    debug!("Pod updated: {}", pod.name_any());

                    match serde_json::to_string(&pod) {
                        Ok(serialized) => send_to_client(registration_id.clone(), KUBERNETES_CONSUMER.to_string(), serialized.into_bytes()),
                        Err(e) => warn!("{e}")
                    }
                
                }
                Event::Delete(pod) => {
                    debug!("Pod deleted: {}", pod.name_any());
                    match serde_json::to_string(&pod) {
                        Ok(serialized) => send_to_client(registration_id.clone(), KUBERNETES_CONSUMER.to_string(), serialized.into_bytes()),
                        Err(e) => warn!("{e}")
                    }
                },
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
            
                    volume
                    .secret
                    .as_ref()
                    .map(|secret| debug!("    -> Mounts Secret: {} ", secret.clone().secret_name.unwrap_or_default() ));
                
                }
            }
    
            // Containers and InitContainers
            let containers = spec.containers.iter().chain(spec.init_containers.iter().flatten());
            for container in containers {
                debug!("    -> Container Image: {} ", container.image.clone().unwrap_or_default());
    
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
        myself: ActorRef<Self::Msg>,
        args: PodObserverArgs,
    ) -> Result<Self::State, ActorProcessingErr> {

        let state = PodObserverState {
            registration_id: args.registration_id.clone(),
            namespace: String::from("default"),
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
            myself.stop((Some(e.to_string())));   
        }

        info!("trying to watch {} namespace.", state.namespace);
            
        //spawn a new thread to watch for pods
        let client = state.kube_client.clone();
        let id = state.registration_id.clone();
        let ns = state.namespace.clone();

        let handle = tokio::spawn(async move {
            PodObserver::watch_pods(id, client, ns ).await
        });
        
        state.watcher = Some(handle);

        Ok(())
    }

    // TODO: Implement post stop fn to cleanup watchers?
    //
    // async fn post_stop(
    //     &self,
    //     myself: ActorRef<Self::Msg>,
    //     message: Self::Msg,
    //     state: &mut Self::State,
    // ) -> Result<(), ActorProcessingErr> {
    //     Ok(())
    // }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            KubernetesObserverMessage::Pods  => {
                // get all deployed pods in our given namespace
                let api: Api<Pod> = Api::namespaced(state.kube_client.clone(), &state.namespace);

                match api.list(&ListParams::default()).await {
                    Ok(pod_list) => {
                        let serialized = serde_json::to_string_pretty(&KubeMessage::ResourceBatch {
                            resources: pod_list.clone()
                            }).unwrap();
                            debug!("{serialized}");

                            let envelope = TcpClientMessage::Send(ClientMessage::PublishRequest {
                                topic: myself.get_name().unwrap(), payload: serialized.into_bytes()  , registration_id: Some(state.registration_id.clone())
                            }
                            );
                            
                            // send data for batch processing
        
                            match where_is(TCP_CLIENT_NAME.to_owned()) {
                                Some(client) => {
                                    if let Err(e) = client.send_message(envelope) {
                                        warn!("Failed to send message to client {e}");
                                    }
                                },
                                None => todo!("If no client present, drop the message?"),
                            }
                    }
                    Err(e) => warn!("Expected to get a list of Pods. {e}")
                }

            },
            _ => (),
        }
        Ok(())
    }
}
