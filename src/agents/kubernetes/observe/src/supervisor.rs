use cassini_client::{TcpClient, TcpClientMessage};
use cassini_types::ClientEvent;
use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Pod, Secret};
use kube::Config;
use kube::ResourceExt;
use kube::runtime::{watcher, watcher::Event};
use kube::{Api, Client, api::ListParams};
use polar::{SupervisorMessage, spawn_tcp_client};
use ractor::concurrency::Duration;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use serde_json::to_value;
use std::collections::HashMap;
use tracing::{debug, error, info, warn};
use tracing::{instrument, trace};

use crate::{
    NamespacedWatcherState, TCP_CLIENT_NAME, WatcherMsg, emit_event, impl_namespaced_watcher,
};
use futures::{StreamExt, TryStreamExt};
use kube_common::{RESOURCE_APPLIED_ACTION, RESOURCE_DELETED_ACTION, RawKubeEvent};

// TODO: establish what kind of messages these receive
pub type NamespaceWatcherMap = HashMap<String, ActorRef<()>>;
pub type Watcher = ActorRef<WatcherMsg>;

pub struct ClusterObserverSupervisor;

pub struct ClusterObserverSupervisorState {
    kube_client: kube::Client,
    tcp_client: ActorRef<TcpClientMessage>,
    #[allow(dead_code)]
    node_watcher: Option<Watcher>,
    namespace_watcher: Option<Watcher>,
}

impl ClusterObserverSupervisor {
    pub async fn init(
        kube_config: Config,
        myself: ActorRef<SupervisorMessage>,
    ) -> Result<ClusterObserverSupervisorState, ActorProcessingErr> {
        // try to create a client and auth with the kube api
        match Client::try_from(kube_config) {
            Ok(kube_client) => {
                debug!("Kubernetes client initialized");

                let tcp_client = spawn_tcp_client(TCP_CLIENT_NAME, myself.into(), |event| {
                    Some(SupervisorMessage::ClientEvent { event })
                })
                .await?;

                Ok(ClusterObserverSupervisorState {
                    kube_client,
                    tcp_client,
                    namespace_watcher: None,
                    node_watcher: None,
                })
            }
            Err(e) => Err(ActorProcessingErr::from(e)),
        }
    }
}

#[async_trait]
impl Actor for ClusterObserverSupervisor {
    type Msg = SupervisorMessage;
    type State = ClusterObserverSupervisorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        // Read Kubernetes credentials and other data from the environment
        info!("{myself:?} starting");

        // detect deployed environment, otherwise, try to infer configuration from the environment
        if let Ok(kube_config) = kube::Config::incluster() {
            info!("Attempting to infer kube configuration from pod environment...");
            match ClusterObserverSupervisor::init(kube_config, myself).await {
                Ok(state) => Ok(state),
                Err(e) => Err(ActorProcessingErr::from(e)),
            }
        } else if let Ok(kube_config) = kube::Config::infer().await {
            info!("Attempting to infer kube configuration from local environment...");
            match ClusterObserverSupervisor::init(kube_config, myself).await {
                Ok(state) => Ok(state),
                Err(e) => Err(ActorProcessingErr::from(e)),
            }
        } else {
            Err(ActorProcessingErr::from(
                "Failed to configure kubernetes client!",
            ))
        }
    }

    async fn handle_supervisor_evt(
        &self,
        _: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                info!(
                    "CLUSTER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!(
                    "CLUSTER_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
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
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let ns_watcher_state = NamespaceSupervisorState {
                        tcp_client: state.tcp_client.clone(),
                        kube_client: state.kube_client.clone(),
                        supervisors: HashMap::new(),
                    };

                    let (ns_watcher, _) = Actor::spawn_linked(
                        Some("cluster.nodes".into()),
                        NamespaceSupervisor,
                        ns_watcher_state,
                        myself.clone().into(),
                    )
                    .await?;

                    // TODO: We need to start a node watcher too, but it tends to fail for some reason.
                    // We'll have to investigate.

                    state.namespace_watcher = Some(ns_watcher);
                }
                ClientEvent::MessagePublished { .. } => {
                    todo!("Handle incoming messages")
                }
                ClientEvent::TransportError { reason } => {
                    error!("Transport error occurred! {reason}");
                    myself.stop(Some(reason))
                }
                ClientEvent::ControlResponse { .. } => {
                    // ignore
                }
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
// impl_namespaced_watcher!(ContainerWatcher, resource = Container, kind = "Container");

pub struct NamespaceWatcherSupervisor;
pub struct NamespaceWatcherSupervisorArgs {
    pub kube_client: Client,
    pub tcp_client: TcpClient,
    pub namespace: String,
}
pub struct NamespaceWatcherSupervisorState {
    pub kube_client: Client,
    pub tcp_client: TcpClient,
    pub namespace: String,
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

        let d_watcher = NamespacedWatcherState {
            tcp_client: args.tcp_client.clone(),
            kube_client: args.kube_client.clone(),
            kind: "Deployment",
            namespace: args.namespace.clone(),
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
        };

        let pod_watcher = Actor::spawn_linked(
            Some(format!("cluster.{ns}.pods", ns = args.namespace)),
            PodWatcher,
            p_watcher,
            myself.clone().into(),
        )
        .await?
        .0;

        let state = NamespaceWatcherSupervisorState {
            namespace: args.namespace,
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
    pub supervisors: NamespaceWatcherMap,
}
impl NamespaceSupervisor {
    /// A typed specific override of list_and_watch implementations.
    /// When we see namespaces, we want to spawn supervisors that will handle watchers for us.
    #[instrument(skip(tcp_client, kube_client))]
    pub async fn list_and_watch_namespaces(
        myself: &Watcher,
        tcp_client: &TcpClient,
        kube_client: Client,
        supervisors: &mut NamespaceWatcherMap,
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
                        kind: kind.into(),
                        action: RESOURCE_DELETED_ACTION.into(),
                        object: serde_json::to_value(obj)?,
                        resource_version: None,
                    };

                    emit_event(tcp_client, ev).await?;

                    match supervisors.get(&ns_name) {
                        Some(_supervisor) => {
                            debug!("removing namespace supervisor for {ns_name} ");
                            if let Some((_ns, supervisor)) = supervisors.remove_entry(&ns_name) {
                                supervisor
                                    .stop_children_and_wait(None, Some(Duration::from_millis(500)))
                                    .await;
                                supervisor.stop(None);
                            }
                        }
                        None => (),
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
                )
                .await
                {
                    error!("Failed to watch namespaces {e}");
                    return Err(e.into());
                }
            }
        }
        Ok(())
    }
}
