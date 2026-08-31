use crate::WatcherMsg;
use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{ConfigMap, Pod, Secret};
use kube::Client;
use polar::cassini::TcpClient;
use tracing::{debug, error};
pub struct NamespacedWatcherState {
    pub kube_client: Client,
    pub tcp_client: TcpClient,
    pub namespace: String,
    pub kind: &'static str,
    pub cluster_uid: std::sync::Arc<str>,
}

#[macro_export]
macro_rules! impl_namespaced_watcher {
    (
        $actor_name:ident,
        resource = $resource_ty:ty,
        kind = $kind:expr
    ) => {
        pub struct $actor_name;

        #[ractor::async_trait]
        impl ractor::Actor for $actor_name {
            type Msg = WatcherMsg;
            type State = $crate::NamespacedWatcherState;
            type Arguments = $crate::NamespacedWatcherState;

            async fn pre_start(
                &self,
                myself: ractor::ActorRef<Self::Msg>,
                state: Self::Arguments,
            ) -> Result<Self::State, ractor::ActorProcessingErr> {
                debug!("{myself:?} starting.");

                Ok(state)
            }

            async fn post_start(
                &self,
                myself: ractor::ActorRef<Self::Msg>,
                _state: &mut Self::State,
            ) -> Result<(), ractor::ActorProcessingErr> {
                debug!("{myself:?} started");
                myself.cast(WatcherMsg::Start)?;
                Ok(())
            }

            async fn handle(
                &self,
                myself: ractor::ActorRef<Self::Msg>,
                message: Self::Msg,
                state: &mut Self::State,
            ) -> Result<(), ractor::ActorProcessingErr> {
                match message {
                    WatcherMsg::Start => {
                        if let Err(e) = crate::list_and_watch_namespaced::<$resource_ty>(
                            &state.tcp_client,
                            state.kube_client.clone(),
                            state.kind,
                            &state.namespace,
                            &state.cluster_uid,
                        )
                        .await
                        {
                            error!("{e}");
                            myself.stop(None);
                        }
                    }
                }

                Ok(())
            }
        }
    };
}

impl_namespaced_watcher!(
    DeploymentWatcher,
    resource = Deployment,
    kind = "Deployment"
);
impl_namespaced_watcher!(
    ReplicasetWatcher,
    resource = ReplicaSet,
    kind = "ReplicaSet"
);
impl_namespaced_watcher!(PodWatcher, resource = Pod, kind = "Pod");
impl_namespaced_watcher!(SecretWatcher, resource = Secret, kind = "Secret");
impl_namespaced_watcher!(ConfigMapWatcher, resource = ConfigMap, kind = "ConfigMap");
impl_namespaced_watcher!(JobWatcher, resource = Job, kind = "Job");
