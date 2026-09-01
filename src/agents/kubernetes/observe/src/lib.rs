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
///     - `RESOURCE_APPLIED_ACTION` for InitApply events (a post-recovery
///       relist after the watch's resourceVersion went stale; see the
///       Event::Apply/InitApply match arm below for why these are treated
///       identically here)
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
///
/// Shared by both `list_and_watch_global` and `list_and_watch_namespaced`,
/// which differ only in how they construct `api` (`Api::all` vs
/// `Api::namespaced`) -- everything past that point was previously
/// duplicated in full between the two.
#[instrument(skip(tcp_client, api))]
async fn list_and_watch_inner<T>(
    api: Api<T>,
    tcp_client: &TcpClient,
    kind: &str,
    cluster_uid: &str,
) -> Result<(), ActorProcessingErr>
where
    T: Resource + Clone + DeserializeOwned + Serialize + Debug + Send + 'static,
    <T as Resource>::DynamicType: Default,
{
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
            // InitApply carries the same payload shape as Apply, delivered
            // during a post-recovery relist -- per kube_runtime::watcher's
            // own docs, if the watch connection breaks and the
            // resourceVersion it was tracking is no longer valid, the
            // stream starts over with Event::Init, followed by one
            // InitApply per object in the fresh list, then InitDone.
            // kube-rs's own reference reflector buffers InitApply and
            // swaps the buffer in on InitDone for atomicity; this function
            // has no such atomicity contract (every event is emitted
            // individually, to a broker topic, regardless of which phase
            // produced it), so there is nothing to buffer -- InitApply is
            // simply treated as Apply. Init and InitDone remain silently
            // ignored: bookkeeping markers with no object payload.
            //
            // Previously only Apply was handled here; InitApply fell into
            // the catch-all below and was silently dropped, meaning any
            // object whose state changed during a watch-recovery window
            // never reached the consumer for that relist.
            Event::Apply(obj) | Event::InitApply(obj) => {
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
            // Init: relist starting, no payload, nothing to emit.
            // InitDone: relist finished, no payload, nothing to emit.
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
    cluster_uid: &str,
) -> Result<(), ActorProcessingErr>
where
    T: Resource + Clone + DeserializeOwned + Serialize + Debug + Send + 'static,
    <T as Resource>::DynamicType: Default,
    T: Resource<Scope = NamespaceResourceScope>,
{
    let api: Api<T> = Api::namespaced(kube_client, namespace);
    list_and_watch_inner(api, tcp_client, kind, cluster_uid).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn raw_kube_event_round_trips_through_json() {
        let ev = RawKubeEvent {
            kind: "Pod".to_string(),
            action: RESOURCE_APPLIED_ACTION.to_string(),
            object: json!({"metadata": {"name": "test-pod", "uid": "abc-123"}}),
            resource_version: Some("42".to_string()),
            cluster_uid: "cluster-uid-xyz".to_string(),
        };

        let bytes = to_vec(&ev).expect("RawKubeEvent must serialize");
        let decoded: RawKubeEvent =
            serde_json::from_slice(&bytes).expect("emitted bytes must deserialize back");

        assert_eq!(decoded.kind, ev.kind);
        assert_eq!(decoded.action, ev.action);
        assert_eq!(decoded.object, ev.object);
        assert_eq!(decoded.resource_version, ev.resource_version);
        assert_eq!(decoded.cluster_uid, ev.cluster_uid);
    }

    #[test]
    fn raw_kube_event_preserves_absent_resource_version() {
        // resource_version is None for every watch-stream event -- LIST is
        // the only phase that has one (see list_and_watch_inner). Confirms
        // serde round-trips that as an actual absence rather than a
        // string "null" a downstream match on resource_version could
        // mistake for a real value.
        let ev = RawKubeEvent {
            kind: "Pod".to_string(),
            action: RESOURCE_DELETED_ACTION.to_string(),
            object: json!({}),
            resource_version: None,
            cluster_uid: "cluster-uid-xyz".to_string(),
        };

        let bytes = to_vec(&ev).expect("RawKubeEvent must serialize");
        let decoded: RawKubeEvent =
            serde_json::from_slice(&bytes).expect("emitted bytes must deserialize back");

        assert_eq!(decoded.resource_version, None);
    }

    #[test]
    fn topic_is_derived_from_cluster_uid() {
        // emit_event's only actual branching logic is topic selection.
        // Worth pinning directly: a typo'd format string here silently
        // routes every event for a cluster to the wrong topic, with no
        // compile-time signal and no obvious runtime one either -- the
        // publish still succeeds, just to nobody who's listening.
        let topic = kube_common::kube_events_topic("abc-123");
        assert_eq!(topic, "polar.kubernetes.abc-123.events");
    }
}
