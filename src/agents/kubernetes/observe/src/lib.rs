use cassini_client::{TcpClient, TcpClientMessage};
use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::core::v1::Node;
use k8s_openapi::api::rbac::v1::{ClusterRole, ClusterRoleBinding};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use k8s_openapi::NamespaceResourceScope;
use kube::runtime::watcher::Event;
use kube::{api::ListParams, runtime::watcher};
use kube::{Api, Client, Resource};
use kube_common::{
    RawKubeEvent, BATCH_PROCESS_ACTION, KUBERNETES_CONSUMER, RESOURCE_APPLIED_ACTION,
    RESOURCE_DELETED_ACTION,
};
use ractor::ActorProcessingErr;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{to_value, to_vec};
use std::fmt::Debug;
use tracing::{debug, error, instrument, trace};
// pub mod pods;
pub mod supervisor;

pub const TCP_CLIENT_NAME: &str = "kubernetes.cluster_name.supervisor_name.client";

pub struct KubernetesObserver;

pub struct KubernetesObserverState {
    pub namespace: String,
    pub client: kube::Client,
}

/// Standard messages exchanged by the observer actors internal to the agent
pub enum KubernetesObserverMessage {
    Pods,
    Deployments,
    ConfigMaps,
    Secrets,
}

pub struct KubernetesObserverArgs {
    pub namespace: String,
}

// pub enum WatcherMessage;

pub struct WatcherActor;

// pub struct WatcherState {
//     watcher: Box<dyn ResourceWatcher>,
//     tcp_client: ActorRef<TcpClientMessage>,
// }

/// Performs a full LIST + WATCH lifecycle for a Kubernetes resource.
///
/// ## Semantics
///
/// 1. Performs an initial LIST using `watcher::Config` semantics.
/// 2. Emits `RESOURCE_APPLIED_ACTION` for all existing objects.
/// 3. Enters a continuous WATCH stream.
/// 4. Emits:
///     - `RESOURCE_APPLIED_ACTION` for Apply events
///     - `RESOURCE_DELETED_ACTION` for Delete events
///     - `RESOURCE_APPLIED_ACTION` for Restarted batches
/// 5. If the watcher stream terminates unexpectedly, this function returns an error,
///    allowing the caller (actor) to fail and be restarted by supervision.
///
/// ## Failure Model
///
/// - Any error during LIST or WATCH propagation bubbles up.
/// - Unexpected stream termination is treated as fatal.
/// - Supervisor is expected to restart the actor.
///
/// ## Type Constraints
///
/// `T` must implement Kubernetes `Resource` traits and be serializable.
///
/// This function is intentionally actor-agnostic. Lifecycle control
/// is delegated to the caller.
#[instrument(skip(tcp_client, kube_client))]
pub async fn list_and_watch_global<T>(
    tcp_client: &TcpClient,
    kube_client: Client,
    kind: &str,
) -> Result<(), ActorProcessingErr>
where
    T: Resource + Clone + DeserializeOwned + Serialize + Debug + Send + 'static,
    <T as Resource>::DynamicType: Default,
{
    // get all deployed pods in our given namespace
    let api: Api<T> = Api::all(kube_client);
    debug!("Observing {} resources.", kind);
    // ---- LIST ----
    let list = api.list(&ListParams::default()).await?;

    let resource_version = list.metadata.resource_version.clone();

    debug!(
        "Discovered k8s {} objects of kind: {kind}.",
        list.items.len()
    );
    for obj in list.items {
        let ev = RawKubeEvent {
            kind: kind.to_string(),
            action: BATCH_PROCESS_ACTION.into(),
            object: to_value(&obj)?,
            resource_version: resource_version.clone(),
        };

        emit_event(tcp_client, ev).await?;
    }
    // ---------------------------------------------------------------------
    // WATCHER CONFIGURATION
    // ---------------------------------------------------------------------
    //
    // We rely on kube_runtime's watcher to:
    //  - Perform initial LIST
    //  - Handle relisting on 410 Gone
    //  - Maintain resourceVersion continuity
    //
    // We enable bookmarks for better stream continuity and observability.
    //
    let watcher_config = watcher::Config {
        label_selector: None,
        field_selector: None,
        timeout: None,
        list_semantic: watcher::ListSemantic::MostRecent,
        initial_list_strategy: watcher::InitialListStrategy::ListWatch,
        page_size: None,
        bookmarks: true,
    };

    // ---------------------------------------------------------------------
    // STREAM INITIALIZATION
    // ---------------------------------------------------------------------

    let mut stream = watcher(api, watcher_config).boxed();

    // ---------------------------------------------------------------------
    // EVENT LOOP
    // ---------------------------------------------------------------------
    //
    // This loop is intentionally infinite. If it exits, something abnormal
    // occurred and we fail hard so supervision can restart us.
    //
    while let Some(event) = stream.try_next().await? {
        trace!("Observed kube event for kind {kind}: {event:?}");

        match event {
            Event::Apply(obj) => {
                let ev = RawKubeEvent {
                    kind: kind.into(),
                    action: RESOURCE_APPLIED_ACTION.into(),
                    object: serde_json::to_value(obj)?,
                    resource_version: None, // runtime watcher manages this internally
                };

                emit_event(tcp_client, ev).await?;
            }

            Event::Delete(obj) => {
                let ev = RawKubeEvent {
                    kind: kind.into(),
                    action: RESOURCE_DELETED_ACTION.into(),
                    object: serde_json::to_value(obj)?,
                    resource_version: None,
                };

                emit_event(tcp_client, ev).await?;
            }
            _ => (),
        }
    }

    // ---------------------------------------------------------------------
    // STREAM TERMINATION
    // ---------------------------------------------------------------------
    //
    // Reaching here means the stream ended without error.
    // That should not normally happen.
    //
    error!("Watcher stream for {kind} terminated unexpectedly");

    Err(ActorProcessingErr::from(format!(
        "watch stream for {kind} ended unexpectedly"
    )))
}
#[instrument(skip(tcp_client, kube_client))]
pub async fn list_and_watch_namespaced<T>(
    tcp_client: &TcpClient,
    kube_client: Client,
    kind: &str,
    namespace: &str,
) -> Result<(), ActorProcessingErr>
where
    T: Resource + Clone + DeserializeOwned + Serialize + Debug + Send + 'static,
    <T as Resource>::DynamicType: Default,
    T: Resource<Scope = NamespaceResourceScope>,
{
    debug!("Observing {} resources in namespace {namespace}", kind);
    // get all deployed pods in our given namespace
    let api: Api<T> = Api::namespaced(kube_client, &namespace);

    // ---- LIST ----
    let list = api.list(&ListParams::default()).await?;

    let resource_version = list.metadata.resource_version.clone();

    for obj in list.items {
        debug!("Discovered k8s object of kind: {kind}.");
        let ev = RawKubeEvent {
            kind: kind.to_string(),
            action: RESOURCE_APPLIED_ACTION.into(),
            object: to_value(&obj)?,
            resource_version: resource_version.clone(),
        };

        emit_event(tcp_client, ev).await?;
    }
    // ---------------------------------------------------------------------
    // WATCHER CONFIGURATION
    // ---------------------------------------------------------------------
    //
    // We rely on kube_runtime's watcher to:
    //  - Perform initial LIST
    //  - Handle relisting on 410 Gone
    //  - Maintain resourceVersion continuity
    //
    // We enable bookmarks for better stream continuity and observability.
    //
    let watcher_config = watcher::Config {
        label_selector: None,
        field_selector: None,
        timeout: None,
        list_semantic: watcher::ListSemantic::MostRecent,
        initial_list_strategy: watcher::InitialListStrategy::ListWatch,
        page_size: None,
        bookmarks: true,
    };

    // ---------------------------------------------------------------------
    // STREAM INITIALIZATION
    // ---------------------------------------------------------------------

    let mut stream = watcher(api, watcher_config).boxed();

    // ---------------------------------------------------------------------
    // EVENT LOOP
    // ---------------------------------------------------------------------
    //
    // This loop is intentionally infinite. If it exits, something abnormal
    // occurred and we fail hard so supervision can restart us.
    //
    while let Some(event) = stream.try_next().await? {
        trace!("Observed kube event for kind {kind}: {event:?}");

        match event {
            Event::Apply(obj) => {
                let ev = RawKubeEvent {
                    kind: kind.into(),
                    action: RESOURCE_APPLIED_ACTION.into(),
                    object: serde_json::to_value(obj)?,
                    resource_version: None, // runtime watcher manages this internally
                };

                emit_event(tcp_client, ev).await?;
            }

            Event::Delete(obj) => {
                let ev = RawKubeEvent {
                    kind: kind.into(),
                    action: RESOURCE_DELETED_ACTION.into(),
                    object: serde_json::to_value(obj)?,
                    resource_version: None,
                };

                emit_event(tcp_client, ev).await?;
            }
            _ => (),
        }
    }

    // ---------------------------------------------------------------------
    // STREAM TERMINATION
    // ---------------------------------------------------------------------
    //
    // Reaching here means the stream ended without error.
    // That should not normally happen.
    //
    error!("Watcher stream for {kind} terminated unexpectedly");

    Err(ActorProcessingErr::from(format!(
        "watch stream for {kind} ended unexpectedly"
    )))
}
/// Serializes and publishes a RawKubeEvent.
///
/// This function is intentionally small and synchronous from a
/// lifecycle standpoint — it does not retry or buffer.
/// Backpressure must be handled at the TCP client layer.
///
/// Failures propagate upward.
async fn emit_event(tcp_client: &TcpClient, ev: RawKubeEvent) -> Result<(), ActorProcessingErr> {
    let payload = to_vec(&ev)?;
    trace!("Emitting event {ev:?}");
    tcp_client.cast(TcpClientMessage::Publish {
        topic: KUBERNETES_CONSUMER.to_string(),
        payload,
    })?;

    Ok(())
}

pub enum WatcherMsg {
    Start,
}

pub struct GlobalWatcherState {
    pub kube_client: Client,
    pub tcp_client: TcpClient,
    pub kind: &'static str,
}
pub struct NamespacedWatcherState {
    pub kube_client: Client,
    pub tcp_client: TcpClient,
    pub namespace: String,
    pub kind: &'static str,
}

#[macro_export]
macro_rules! impl_namespaced_watcher {
    (
        $actor_name:ident,
        resource = $resource_ty:ty,
        kind = $kind:literal
    ) => {
        pub struct $actor_name;

        #[ractor::async_trait]
        impl ractor::Actor for $actor_name {
            type Msg = WatcherMsg;
            type State = crate::NamespacedWatcherState;
            type Arguments = crate::NamespacedWatcherState;

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

#[macro_export]
macro_rules! impl_global_watcher {
    (
        $actor_name:ident,
        resource = $resource_ty:ty,
        kind = $kind:literal
    ) => {
        pub struct $actor_name;

        #[ractor::async_trait]
        impl ractor::Actor for $actor_name {
            type Msg = WatcherMsg;
            type State = GlobalWatcherState;
            type Arguments = GlobalWatcherState;

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
                        if let Err(e) = list_and_watch_global::<$resource_ty>(
                            &state.tcp_client,
                            state.kube_client.clone(),
                            state.kind,
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

impl_global_watcher!(NodeWatcher, resource = Node, kind = "Node");
impl_global_watcher!(
    CRDWatcher,
    resource = CustomResourceDefinition,
    kind = "CRD"
);
impl_global_watcher!(
    ClusterRoleWatcher,
    resource = ClusterRole,
    kind = "ClusterRole"
);
impl_global_watcher!(
    ClusterRoleBindingWatcher,
    resource = ClusterRoleBinding,
    kind = "ClusterRoleBinding"
);

//
