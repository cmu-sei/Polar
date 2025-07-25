use cassini::client::{TcpClientActor, TcpClientArgs};
use cassini::TCPClientConfig;
use k8s_openapi::api::core::v1::Namespace;
use kube::Config;
use kube::{api::ListParams, Api, Client};
use ractor::OutputPort;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use tracing::{debug, error, info, warn};

use crate::{
    pods::{PodObserver, PodObserverArgs},
    TCP_CLIENT_NAME,
};
use kube_common::KUBERNETES_OBSERVER;

pub struct ClusterObserverSupervisor;

pub struct ClusterObserverSupervisorArgs {
    pub cassini_client_config: TCPClientConfig,
}

pub struct ClusterObserverSupervisorState {
    kube_client: kube::Client,
}

pub enum SupervisorMessage {
    ClientRegistered(String),
}

impl ClusterObserverSupervisor {
    pub async fn init(
        kube_config: Config,
        args: ClusterObserverSupervisorArgs,
        myself: ActorRef<SupervisorMessage>,
    ) -> Result<ClusterObserverSupervisorState, ActorProcessingErr> {
        // try to create a client and auth with the kube api
        match Client::try_from(kube_config) {
            Ok(kube_client) => {
                debug!("Kubernetes client initialized");

                let output_port = std::sync::Arc::new(OutputPort::default());

                output_port.subscribe(myself.clone(), |message| {
                    Some(SupervisorMessage::ClientRegistered(message))
                });

                match Actor::spawn_linked(
                    Some(TCP_CLIENT_NAME.to_string()),
                    TcpClientActor,
                    TcpClientArgs {
                        config: args.cassini_client_config,
                        registration_id: None,
                        output_port,
                    },
                    myself.into(),
                )
                .await
                {
                    Ok(_) => Ok(ClusterObserverSupervisorState { kube_client }),
                    Err(e) => Err(ActorProcessingErr::from(e)),
                }
            }
            Err(e) => Err(ActorProcessingErr::from(e)),
        }
    }
    /// Fetch all namespace names from the Kubernetes cluster
    pub async fn discover_namespaces(client: Client) -> Result<Vec<String>, String> {
        // Create a Kubernetes client using in-cluster config or kubeconfig

        // Access the Namespace API
        let namespaces: Api<Namespace> = Api::all(client);

        // List all namespaces with default parameters
        match namespaces.list(&ListParams::default()).await {
            Ok(ns_list) => {
                // Extract just the namespace names
                let mut ns_names = Vec::new();
                for ns in ns_list.items {
                    if let Some(name) = ns.metadata.name {
                        ns_names.push(name);
                    }
                }

                Ok(ns_names)
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

#[async_trait]
impl Actor for ClusterObserverSupervisor {
    type Msg = SupervisorMessage;
    type State = ClusterObserverSupervisorState;
    type Arguments = ClusterObserverSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ClusterObserverSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        // Read Kubernetes credentials and other data from the environment
        info!("{myself:?} starting");

        // detect deployed environment, otherwise, try to infer configuration from the environment
        if let Ok(kube_config) = kube::Config::incluster() {
            info!("Attempting to infer kube configuration from pod environment...");
            match ClusterObserverSupervisor::init(kube_config, args, myself).await {
                Ok(state) => Ok(state),
                Err(e) => Err(ActorProcessingErr::from(e)),
            }
        } else if let Ok(kube_config) = kube::Config::infer().await {
            info!("Attempting to infer kube configuration from local environment...");
            match ClusterObserverSupervisor::init(kube_config, args, myself).await {
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
            SupervisorMessage::ClientRegistered(registration_id) => {
                match ClusterObserverSupervisor::discover_namespaces(state.kube_client.clone())
                    .await
                {
                    Ok(namespaces) => {
                        //start actors
                        for ns in namespaces {
                            let args = PodObserverArgs {
                                registration_id: registration_id.clone(),
                                kube_client: state.kube_client.clone(),
                                namespace: ns.clone(),
                            };

                            if let Err(e) = Actor::spawn_linked(
                                Some(format!("{KUBERNETES_OBSERVER}.{ns}")),
                                PodObserver,
                                args,
                                myself.get_cell().clone(),
                            )
                            .await
                            {
                                error!("{e}")
                            }
                        }
                    }
                    Err(e) => return Err(ActorProcessingErr::from(e)),
                }
            }
        }
        Ok(())
    }
}
