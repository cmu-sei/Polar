/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

use cassini_types::ClientEvent;
use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Pod, Secret};
use kube::Config;
use kube::ResourceExt;
use kube::runtime::{watcher, watcher::Event};
use kube::{Api, Client, api::ListParams};
use kube_common::{
    KIND_KUSTOMIZATION, KIND_OCI_REPOSITORY, KUBERNETES_DISCOVERY_ANNOUNCE,
    KUBERNETES_DISCOVERY_QUERY,
};
use polar::cassini::{CassiniClient, OfflineBehavior, PublishRequest, SubscribeRequest, TcpClient};
use polar_agent_builder::{
    AgentBuilder, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage,
};
use ractor::concurrency::Duration;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use serde_json::to_value;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use tracing::{instrument, trace};

use crate::{
    GlobalWatcherState, KustomizationWatcher, NamespacedWatcherState, NodeWatcher,
    OciRepositoryWatcher, PolarAgentWatcher, TCP_CLIENT_NAME, WatcherMsg, emit_event,
    impl_namespaced_watcher,
};
use futures::{StreamExt, TryStreamExt};
use kube_common::{RESOURCE_APPLIED_ACTION, RESOURCE_DELETED_ACTION, RawKubeEvent};

// TODO: establish what kind of messages these receive
pub type NamespaceWatcherMap = HashMap<String, ActorRef<()>>;
pub type Watcher = ActorRef<WatcherMsg>;

/// How long the infrastructure supervisor gives this agent to drain: its three
/// watchers are stopped one after another, each bounded at 10 s, plus a margin. If
/// it is not done by then the process exits 1 regardless.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(3 * 10 + 10);

/// This supervisor's own, real, local message type -- the shared,
/// core polar::SupervisorMessage can't structurally carry either of
/// the two, new, real signals this migration needs
/// (InfrastructureRespawned, PrepareShutdown) without
/// polar-agent-builder depending back on lib, a genuine circular
/// dependency. AgentBuilder<M> only requires M: ractor::Message
/// (automatically satisfied by any Send + 'static type), so a
/// smaller, local type matching exactly what this supervisor
/// genuinely needs is the correct, clean fix, not a workaround.
pub enum ObserverSupervisorMessage {
    ClientEvent {
        event: ClientEvent,
    },
    /// InfrastructureSupervisor's own InfrastructureEvent::PrepareShutdown,
    /// forwarded here -- HealthCheckActor (now InfrastructureSupervisor's
    /// own, direct child, not this supervisor's) has genuinely,
    /// cleanly terminated for rejuvenation. Coordinate this
    /// supervisor's own, direct children (the watcher trees) shutting
    /// down first, then send
    /// InfrastructureSupervisorMessage::ProceedWithShutdown -- see
    /// this crate's own, real handle() arm for the complete sequence.
    PrepareShutdown,
}

/// Publishes this cluster's UID on the discovery announce topic: raw UTF-8 bytes with no envelope, a single scalar.
/// Called when the agent registers and whenever a consumer asks with discovery.query.
fn announce(tcp_client: &TcpClient, cluster_uid: &str) -> Result<(), ActorProcessingErr> {
    debug!("Announcing cluster {cluster_uid} on {KUBERNETES_DISCOVERY_ANNOUNCE}");
    tcp_client.publish(PublishRequest {
        topic: KUBERNETES_DISCOVERY_ANNOUNCE.to_string(),
        trace_ctx: None,
        payload: cluster_uid.as_bytes().to_vec(),
        offline_behavior: OfflineBehavior::default(),
        share_identity: true,
    })?;
    Ok(())
}

pub struct ClusterObserverSupervisor;

pub struct ClusterObserverSupervisorState {
    kube_client: kube::Client,
    tcp_client: TcpClient,
    healthcheck: HealthHandle,
    /// This cluster's identity per cmu-sei/Polar issue 236: the UID of the kube-system namespace. Resolved before
    /// anything is watched, stamped on every event and announced for consumers.
    cluster_uid: Arc<str>,
    /// Sent InfrastructureSupervisorMessage::ProceedWithShutdown once
    /// this supervisor's own, direct children have genuinely, fully
    /// shut down -- see PrepareShutdown's own doc comment above.
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    /// Watches Nodes cluster-wide.
    node_watcher: Option<Watcher>,
    /// Watches the operator's PolarAgent resources cluster-wide.
    polar_agent_watcher: Option<Watcher>,
    namespace_watcher: Option<Watcher>,
    /// Watches Flux OCIRepository resources cluster-wide.
    oci_repository_watcher: Option<Watcher>,
    /// Watches Flux Kustomization resources cluster-wide.
    kustomization_watcher: Option<Watcher>,
}

impl ClusterObserverSupervisor {
    /// The cluster's identity: the UID of the kube-system namespace, a permanent fixture of every cluster whose UID
    /// no other cluster shares. Kubernetes has no first-class cluster ID. Fatal when it cannot be read, because
    /// nothing downstream can safely proceed without it.
    async fn resolve_cluster_uid(kube_client: &Client) -> Result<Arc<str>, ActorProcessingErr> {
        let namespaces: Api<Namespace> = Api::all(kube_client.clone());
        let kube_system = namespaces.get("kube-system").await?;
        match kube_system.metadata.uid {
            Some(uid) => Ok(Arc::from(uid)),
            None => Err(ActorProcessingErr::from(
                "kube-system namespace has no uid set -- cannot establish cluster identity",
            )),
        }
    }

    pub async fn init(
        kube_config: Config,
        tcp_client: TcpClient,
        healthcheck: HealthHandle,
        infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    ) -> Result<ClusterObserverSupervisorState, ActorProcessingErr> {
        // try to create a client and auth with the kube api
        match Client::try_from(kube_config) {
            Ok(kube_client) => {
                debug!("Kubernetes client initialized");

                // Before anything else is spawned: the events every watcher publishes carry it.
                let cluster_uid = Self::resolve_cluster_uid(&kube_client).await?;
                info!("Resolved cluster identity: {cluster_uid}");

                Ok(ClusterObserverSupervisorState {
                    kube_client,
                    tcp_client,
                    healthcheck,
                    cluster_uid,
                    infrastructure,
                    namespace_watcher: None,
                    node_watcher: None,
                    polar_agent_watcher: None,
                    oci_repository_watcher: None,
                    kustomization_watcher: None,
                })
            }
            Err(e) => Err(ActorProcessingErr::from(e)),
        }
    }
}

#[async_trait]
impl Actor for ClusterObserverSupervisor {
    type Msg = ObserverSupervisorMessage;
    type State = ClusterObserverSupervisorState;
    /// The configuration `run` judged; the builder is made from it.
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        // Read Kubernetes credentials and other data from the environment
        info!("{myself:?} starting");

        // The real, entire, formerly hand-rolled sequence --
        // HealthCheckActor, RouterActor, TcpClient -- collapsed into
        // one, real, composed call. No with_encryption(): this agent
        // only publishes observations, it never encrypts a payload.
        // It does need the Kubernetes API, which the deployment
        // provisions (ClusterRole, binding, token); the builder checks
        // that against POLAR_CAPABILITIES before anything starts.
        let resources = AgentBuilder::new(myself.clone(), TCP_CLIENT_NAME)
            .with_lifecycle_handler(|event| Some(ObserverSupervisorMessage::ClientEvent { event }))
            .into_configured(configured)
            .build()
            .await?;

        // The real "infra → domain" half of InfrastructureSupervisor's
        // own, real respawn/shutdown protocol -- subscribed directly,
        // the same, established pattern every other real port in this
        // codebase already uses.
        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => {
                    Some(ObserverSupervisorMessage::PrepareShutdown)
                }
            });

        // detect deployed environment, otherwise, try to infer configuration from the environment
        if let Ok(kube_config) = kube::Config::incluster() {
            info!("Attempting to infer kube configuration from pod environment...");
            ClusterObserverSupervisor::init(
                kube_config,
                resources.tcp_client,
                resources.healthcheck,
                resources.infrastructure,
            )
            .await
        } else if let Ok(kube_config) = kube::Config::infer().await {
            info!("Attempting to infer kube configuration from local environment...");
            ClusterObserverSupervisor::init(
                kube_config,
                resources.tcp_client,
                resources.healthcheck,
                resources.infrastructure,
            )
            .await
        } else {
            Err(ActorProcessingErr::from(
                "Failed to configure kubernetes client!",
            ))
        }
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                let actor_name = actor_cell.get_name().unwrap_or_default();
                info!(
                    "CLUSTER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_name,
                    actor_cell.get_id()
                );

                // InfrastructureSupervisor itself -- this supervisor's
                // one, real, direct, linked child from AgentBuilder's
                // own build() call. Its own termination is the real,
                // genuine signal this supervisor's own graceful
                // shutdown sequence (see PrepareShutdown's own handler
                // in handle(), below) is now fully, completely done --
                // exit so the Job controller creates a new pod with
                // fresh certs. Matched by real, genuine actor id, not
                // name (InfrastructureSupervisor is spawned unnamed).
                if actor_cell.get_id() == state.infrastructure.get_id() {
                    info!(
                        "CLUSTER_SUPERVISOR: infrastructure shutdown complete, exiting for rejuvenation"
                    );
                    std::process::exit(1);
                }
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!(
                    "CLUSTER_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                // Any child actor failure is fatal -- exit so the Job
                // controller creates a new pod. This is a deliberately blunt
                // fail-fast policy for now: we want a visible Job restart
                // event as the signal something needs investigating, rather
                // than silently degrading. Finer-grained resilience (e.g.
                // per-watcher restart without a full pod cycle) can be added
                // later if warranted, and should route through the logging
                // agent / OTel trace once that's wired up.
                std::process::exit(1);
            }
            SupervisionEvent::ProcessGroupChanged(..) => {}
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
            ObserverSupervisorMessage::PrepareShutdown => {
                info!(
                    "CLUSTER_SUPERVISOR: PrepareShutdown received -- stopping this \
                     supervisor's own, direct children before infrastructure"
                );

                if let Some(w) = state.namespace_watcher.take() {
                    w.stop_and_wait(None, Some(Duration::from_secs(10))).await?;
                }
                if let Some(w) = state.oci_repository_watcher.take() {
                    w.stop_and_wait(None, Some(Duration::from_secs(10))).await?;
                }
                if let Some(w) = state.kustomization_watcher.take() {
                    w.stop_and_wait(None, Some(Duration::from_secs(10))).await?;
                }
                if let Some(w) = state.polar_agent_watcher.take() {
                    w.stop_and_wait(None, Some(Duration::from_secs(10))).await?;
                }
                if let Some(w) = state.node_watcher.take() {
                    w.stop_and_wait(None, Some(Duration::from_secs(10))).await?;
                }

                let _ = state
                    .infrastructure
                    .cast(InfrastructureSupervisorMessage::ProceedWithShutdown);
            }
            ObserverSupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cassini_connected();

                    // Subscriptions belong to a connection, unlike the watchers below, which live as long as the
                    // process: they are made again on every Registered, first connect and reconnect alike.
                    if let Err(e) = state.tcp_client.subscribe(SubscribeRequest {
                        topic: KUBERNETES_DISCOVERY_QUERY.to_string(),
                        trace_ctx: None,
                    }) {
                        error!("{e}");
                        return Err(ActorProcessingErr::from(e.to_string()));
                    }

                    // Announced unconditionally too: a consumer already running before this observer connected
                    // has no other way to learn that this cluster exists.
                    announce(&state.tcp_client, &state.cluster_uid)?;

                    if state.namespace_watcher.is_none() {
                        let ns_watcher_state = NamespaceSupervisorState {
                            tcp_client: state.tcp_client.clone(),
                            kube_client: state.kube_client.clone(),
                            cluster_uid: state.cluster_uid.clone(),
                            supervisors: HashMap::new(),
                        };

                        let (ns_watcher, _) = Actor::spawn_linked(
                            Some("cluster.nodes".into()),
                            NamespaceSupervisor,
                            ns_watcher_state,
                            myself.clone().into(),
                        )
                        .await?;

                        state.namespace_watcher = Some(ns_watcher);
                    } else {
                        debug!("Namespace watcher already running; skipping respawn on reconnect");
                    }

                    if state.oci_repository_watcher.is_none() {
                        let oci_watcher_state = GlobalWatcherState {
                            tcp_client: state.tcp_client.clone(),
                            kube_client: state.kube_client.clone(),
                            kind: KIND_OCI_REPOSITORY,
                            cluster_uid: state.cluster_uid.clone(),
                        };

                        let (oci_watcher, _) = Actor::spawn_linked(
                            Some("cluster.flux.ocirepositories".into()),
                            OciRepositoryWatcher,
                            oci_watcher_state,
                            myself.clone().into(),
                        )
                        .await?;

                        state.oci_repository_watcher = Some(oci_watcher);
                    } else {
                        debug!(
                            "OCIRepository watcher already running; skipping respawn on reconnect"
                        );
                    }

                    if state.kustomization_watcher.is_none() {
                        let ks_watcher_state = GlobalWatcherState {
                            tcp_client: state.tcp_client.clone(),
                            kube_client: state.kube_client.clone(),
                            kind: KIND_KUSTOMIZATION,
                            cluster_uid: state.cluster_uid.clone(),
                        };

                        let (ks_watcher, _) = Actor::spawn_linked(
                            Some("cluster.flux.kustomizations".into()),
                            KustomizationWatcher,
                            ks_watcher_state,
                            myself.clone().into(),
                        )
                        .await?;

                        state.kustomization_watcher = Some(ks_watcher);
                    } else {
                        debug!(
                            "Kustomization watcher already running; skipping respawn on reconnect"
                        );
                    }

                    if state.polar_agent_watcher.is_none() {
                        let agent_watcher_state = GlobalWatcherState {
                            tcp_client: state.tcp_client.clone(),
                            kube_client: state.kube_client.clone(),
                            kind: polar_operator_common::KIND,
                            cluster_uid: state.cluster_uid.clone(),
                        };

                        let (agent_watcher, _) = Actor::spawn_linked(
                            Some("cluster.polar.agents".into()),
                            PolarAgentWatcher,
                            agent_watcher_state,
                            myself.clone().into(),
                        )
                        .await?;

                        state.polar_agent_watcher = Some(agent_watcher);
                    } else {
                        debug!("PolarAgent watcher already running; skipping respawn on reconnect");
                    }

                    if state.node_watcher.is_none() {
                        let node_watcher_state = GlobalWatcherState {
                            tcp_client: state.tcp_client.clone(),
                            kube_client: state.kube_client.clone(),
                            kind: "Node",
                            cluster_uid: state.cluster_uid.clone(),
                        };

                        let (node_watcher, _) = Actor::spawn_linked(
                            Some("cluster.nodes.watcher".into()),
                            NodeWatcher,
                            node_watcher_state,
                            myself.clone().into(),
                        )
                        .await?;

                        state.node_watcher = Some(node_watcher);
                    } else {
                        debug!("Node watcher already running; skipping respawn on reconnect");
                    }
                }
                ClientEvent::MessagePublished { topic, .. } => {
                    // The one topic this agent subscribes to: a consumer asking every observer to announce itself.
                    if topic == KUBERNETES_DISCOVERY_QUERY {
                        debug!("Received discovery.query, re-announcing cluster identity");
                        announce(&state.tcp_client, &state.cluster_uid)?;
                    } else {
                        warn!("Received a message on unexpected topic '{topic}'; ignoring");
                    }
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => (),
            },
        }
        Ok(())
    }
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
// impl_namespaced_watcher!(ContainerWatcher, resource = Container, kind = "Container");

pub struct NamespaceWatcherSupervisor;
pub struct NamespaceWatcherSupervisorArgs {
    pub kube_client: Client,
    pub tcp_client: TcpClient,
    pub namespace: String,
    pub cluster_uid: Arc<str>,
}
pub struct NamespaceWatcherSupervisorState {
    pub kube_client: Client,
    pub tcp_client: TcpClient,
    pub namespace: String,
    pub cluster_uid: Arc<str>,
    pub deployment_watcher: Watcher,
    pub replicaset_watcher: Watcher,
    pub pod_watcher: Watcher,
}

#[async_trait]
impl Actor for NamespaceWatcherSupervisor {
    type Msg = ();
    type State = NamespaceWatcherSupervisorState;
    type Arguments = NamespaceWatcherSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        // TODO: This is getting ugly fast, various calls to the impl_watcher! macro, not very DRY init flow, it works, but I do hate it.
        // We could benefit from a function that instantiates the namesapce watcher, and that doesn't eat so much memory doing all the cloning.
        let d_watcher = NamespacedWatcherState {
            tcp_client: args.tcp_client.clone(),
            kube_client: args.kube_client.clone(),
            kind: "Deployment",
            namespace: args.namespace.clone(),
            cluster_uid: args.cluster_uid.clone(),
        };

        let deployment_watcher = Actor::spawn_linked(
            Some(format!("cluster.{ns}.deployments", ns = args.namespace)),
            DeploymentWatcher,
            d_watcher,
            myself.clone().into(),
        )
        .await?
        .0;

        let rs_watcher = NamespacedWatcherState {
            tcp_client: args.tcp_client.clone(),
            kube_client: args.kube_client.clone(),
            kind: "ReplicaSet",
            namespace: args.namespace.clone(),
            cluster_uid: args.cluster_uid.clone(),
        };

        let replicaset_watcher = Actor::spawn_linked(
            Some(format!("cluster.{ns}.replicasets", ns = args.namespace)),
            ReplicasetWatcher,
            rs_watcher,
            myself.clone().into(),
        )
        .await?
        .0;

        let p_watcher = NamespacedWatcherState {
            tcp_client: args.tcp_client.clone(),
            kube_client: args.kube_client.clone(),
            kind: "Pod",
            namespace: args.namespace.clone(),
            cluster_uid: args.cluster_uid.clone(),
        };

        let pod_watcher = Actor::spawn_linked(
            Some(format!("cluster.{ns}.pods", ns = args.namespace)),
            PodWatcher,
            p_watcher,
            myself.clone().into(),
        )
        .await?
        .0;

        let j_watcher = NamespacedWatcherState {
            tcp_client: args.tcp_client.clone(),
            kube_client: args.kube_client.clone(),
            kind: "Job",
            namespace: args.namespace.clone(),
            cluster_uid: args.cluster_uid.clone(),
        };

        let _job_watcher = Actor::spawn_linked(
            Some(format!("cluster.{ns}.jobs", ns = args.namespace)),
            JobWatcher,
            j_watcher,
            myself.clone().into(),
        )
        .await?
        .0;

        let state = NamespaceWatcherSupervisorState {
            namespace: args.namespace,
            cluster_uid: args.cluster_uid,
            tcp_client: args.tcp_client,
            kube_client: args.kube_client,
            deployment_watcher,
            replicaset_watcher,
            pod_watcher,
        };

        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} started");
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        //TODO: handle messages that might come in
        debug!("Received message {message:?}");
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // TODO: We do nothing for lifecycle events at the moment,
        // but we should think about what to do should any of these children fail
        debug!("Saw supervision event {msg:?}");
        Ok(())
    }
}

struct NamespaceSupervisor;

pub struct NamespaceSupervisorState {
    pub kube_client: Client,
    pub tcp_client: TcpClient,
    pub cluster_uid: Arc<str>,
    pub supervisors: NamespaceWatcherMap,
}
impl NamespaceSupervisor {
    /// A typed specific override of list_and_watch implementations.
    /// When we see namespaces, we want to spawn supervisors that will handle watchers for us.
    #[instrument(skip(tcp_client, kube_client, cluster_uid))]
    pub async fn list_and_watch_namespaces(
        myself: &Watcher,
        tcp_client: &TcpClient,
        kube_client: Client,
        supervisors: &mut NamespaceWatcherMap,
        cluster_uid: &Arc<str>,
    ) -> Result<(), ActorProcessingErr> {
        // get all deployed pods in our given namespace
        let api: Api<Namespace> = Api::all(kube_client.clone());
        let kind = "Namespace";

        debug!("Observing {} resources.", kind);
        // ---- LIST ----
        let list = api.list(&ListParams::default()).await?;

        let resource_version = list.metadata.resource_version.clone();

        for ns in list.items {
            debug!("Discovered k8s object of kind: {kind}. {ns:?} ");
            let ev = RawKubeEvent {
                cluster_uid: cluster_uid.to_string(),
                kind: kind.to_string(),
                action: RESOURCE_APPLIED_ACTION.into(),
                object: to_value(&ns)?,
                resource_version: resource_version.clone(),
            };

            emit_event(tcp_client, ev).await?;
            let ns_name = ns.name_any();

            let supervisor_state = NamespaceWatcherSupervisorArgs {
                tcp_client: tcp_client.to_owned(),
                kube_client: kube_client.clone(),
                namespace: ns_name.clone(),
                cluster_uid: cluster_uid.clone(),
            };
            //spawn watcher supervisor
            match Actor::spawn_linked(
                Some(format!("cluster.{ns_name}.supervisor")),
                NamespaceWatcherSupervisor,
                supervisor_state,
                myself.clone().into(),
            )
            .await
            {
                Ok((supervisior, _)) => {
                    let _ = supervisors.insert(ns_name, supervisior);
                }
                Err(e) => return Err(e.into()),
            }
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
                    let ns_name = obj.name_any();

                    debug!("Discovered new namespace {ns_name}");
                    let ev = RawKubeEvent {
                        cluster_uid: cluster_uid.to_string(),
                        kind: kind.into(),
                        action: RESOURCE_APPLIED_ACTION.into(),
                        object: serde_json::to_value(obj)?,
                        resource_version: None, // runtime watcher manages this internally
                    };

                    emit_event(tcp_client, ev).await?;

                    match supervisors.get(&ns_name) {
                        Some(_s) => (),
                        None => {
                            //spawn watcher supervisor
                            let supervisor_state = NamespaceWatcherSupervisorArgs {
                                kube_client: kube_client.clone(),
                                tcp_client: tcp_client.to_owned(),
                                namespace: ns_name.clone(),
                                cluster_uid: cluster_uid.clone(),
                            };

                            match Actor::spawn_linked(
                                Some(format!("cluster.{ns_name}.supervisor")),
                                NamespaceWatcherSupervisor,
                                supervisor_state,
                                myself.clone().into(),
                            )
                            .await
                            {
                                Ok((supervisior, _)) => {
                                    let _ = supervisors.insert(ns_name, supervisior);
                                }
                                Err(e) => return Err(e.into()),
                            }
                        }
                    }
                }

                Event::Delete(obj) => {
                    let ns_name = obj.name_any();

                    debug!("Namespace {ns_name} was deleted.");
                    let ev = RawKubeEvent {
                        cluster_uid: cluster_uid.to_string(),
                        kind: kind.into(),
                        action: RESOURCE_DELETED_ACTION.into(),
                        object: serde_json::to_value(obj)?,
                        resource_version: None,
                    };

                    emit_event(tcp_client, ev).await?;

                    if let Some(_supervisor) = supervisors.get(&ns_name) {
                        debug!("removing namespace supervisor for {ns_name} ");
                        if let Some((_ns, supervisor)) = supervisors.remove_entry(&ns_name) {
                            supervisor
                                .stop_children_and_wait(None, Some(Duration::from_millis(500)))
                                .await;
                            supervisor.stop(None);
                        }
                    }
                }
                _ => (),
            }
        }

        error!("Watcher stream for {kind} terminated unexpectedly");

        Err(ActorProcessingErr::from(format!(
            "watch stream for {kind} ended unexpectedly"
        )))
    }
}
#[async_trait]
impl Actor for NamespaceSupervisor {
    type Msg = WatcherMsg;
    type State = NamespaceSupervisorState;
    type Arguments = NamespaceSupervisorState;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} stgarted");
        myself.cast(WatcherMsg::Start)?;
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            WatcherMsg::Start => {
                if let Err(e) = Self::list_and_watch_namespaces(
                    &myself,
                    &state.tcp_client,
                    state.kube_client.clone(),
                    &mut state.supervisors,
                    &state.cluster_uid,
                )
                .await
                {
                    error!("Failed to watch namespaces {e}");
                    return Err(e);
                }
            }
        }
        Ok(())
    }
}
