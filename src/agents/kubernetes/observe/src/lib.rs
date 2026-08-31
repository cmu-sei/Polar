use cassini_client::{OfflineBehavior, PublishRequest};
use futures::{StreamExt, TryStreamExt};
use k8s_openapi::NamespaceResourceScope;
use kube::runtime::watcher::Event;
use kube::{Api, Client, Resource};
use kube::{api::ListParams, runtime::watcher};
use kube_common::{RESOURCE_APPLIED_ACTION, RESOURCE_DELETED_ACTION, RawKubeEvent};

use polar::cassini::{CassiniClient, TcpClient};
use ractor::ActorProcessingErr;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{to_value, to_vec};
use std::fmt::Debug;
use tracing::{debug, error, instrument, trace};
use watchers::global::*;
use watchers::namespaced::*;
// pub mod pods;
pub mod supervisor;
pub mod watchers;

pub const TCP_CLIENT_NAME: &str = "kubernetes.cluster_name.supervisor_name.client";

pub enum WatcherMsg {
    Start,
}
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

pub struct WatcherActor;

#[instrument(skip(tcp_client, kube_client))]
pub async fn list_and_watch_namespaced<T>(
    tcp_client: &TcpClient,
    kube_client: Client,
    kind: &str,
    namespace: &str,
    cluster_uid: &str,
) -> Result<(), ActorProcessingErr>
where
    T: Resource + Clone + DeserializeOwned + Serialize + Debug + Send + 'static,
    <T as Resource>::DynamicType: Default,
    T: Resource<Scope = NamespaceResourceScope>,
{
    debug!("Observing {} resources in namespace {namespace}", kind);
    // get all deployed pods in our given namespace
    let api: Api<T> = Api::namespaced(kube_client, namespace);

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
            cluster_uid: cluster_uid.to_string(),
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
                    cluster_uid: cluster_uid.to_string(),
                };

                emit_event(tcp_client, ev).await?;
            }

            Event::Delete(obj) => {
                let ev = RawKubeEvent {
                    kind: kind.into(),
                    action: RESOURCE_DELETED_ACTION.into(),
                    object: serde_json::to_value(obj)?,
                    resource_version: None,
                    cluster_uid: cluster_uid.to_string(),
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
    let topic = kube_common::kube_events_topic(&ev.cluster_uid);
    let payload = to_vec(&ev)?;
    trace!("Emitting event {ev:?} on topic {topic}");
    tcp_client.publish(PublishRequest {
        topic,
        trace_ctx: None,
        payload,
        offline_behavior: OfflineBehavior::default(),
    })?;
    Ok(())
}

// TODO: Change existging calls to these macros to use constants defined in kube-common
