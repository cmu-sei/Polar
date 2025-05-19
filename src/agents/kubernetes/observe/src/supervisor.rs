use std::time::Duration;
use cassini::client::{TcpClientActor, TcpClientArgs, TcpClientMessage};
use kube::Client;
use ractor::{async_trait, registry::where_is, rpc::{call, CallResult}, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use tracing::{debug, error, info, warn};
use cassini::TCPClientConfig;

use crate::{pods::{PodObserver, PodObserverArgs}, KubernetesObserverMessage, KUBERNETES_OBSERVER, TCP_CLIENT_NAME};


pub struct ClusterObserverSupervisor;

pub struct ClusterObserverSupervisorArgs {
    pub cassini_client_config: TCPClientConfig,
}

pub struct ClusterObserverSupervisorState;

#[async_trait]
impl Actor for ClusterObserverSupervisor {
    type Msg = KubernetesObserverMessage;
    type State = ClusterObserverSupervisorState;
    type Arguments = ClusterObserverSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ClusterObserverSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {

        match Client::try_default().await {
            Ok(kube_client) => {
                debug!("Kubernetes client initialized");

                let client_started_result = Actor::spawn_linked(
                    Some(TCP_CLIENT_NAME.to_string()),
                    TcpClientActor,
                    TcpClientArgs { config: args.cassini_client_config, registration_id: None },
                    myself.clone().into(),
                )
                .await;
                
                match client_started_result {
                    Ok((client, _)) => {
                        // Set up an interval
                        let mut interval = tokio::time::interval(Duration::from_millis(1000));
                        //wait until we get a session id to start clients, try some configured amount of times every few seconds
                        let mut attempts = 0;

                        loop {
                            attempts += 1;
                            info!("Getting session data...");
                            if let CallResult::Success(result) = call(
                                &client,
                                |reply| TcpClientMessage::GetRegistrationId(reply),
                                None,
                            )
                            .await
                            .expect("Expected to call client!")
                            {
                                //if we successfully register with the broker
                                if let Some(registration_id) = result {
                                    //start actors
                                    let args = PodObserverArgs {
                                        registration_id: registration_id.clone(),
                                        kube_client: kube_client.clone(),
                                    };
                                     
                                    if let Err(e) = Actor::spawn_linked(Some(KUBERNETES_OBSERVER.to_string()), PodObserver, args, myself.get_cell().clone()).await {
                                        error!("{e}")
                                    }

                                    break;
                                } else if attempts < 5 {
                                    warn!("Failed to get session data. Retrying.");
                                } else if attempts >= 5 {
                                    error!("Failed to retrieve session data! timed out");
                                    myself.stop(Some(
                                        "Failed to retrieve session data! timed out".to_string(),
                                    ));
                                }
                            }
                            interval.tick().await;
                        }
                    }
                    Err(e) => {
                        error!("Failed to start tcp client {e}");
                        myself.stop(None);
                    }
                }


                Ok(ClusterObserverSupervisorState)

            }
            Err(e) => Err(ActorProcessingErr::from(e))
        }
        
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {


        Ok(())
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

        Ok(())
    }
}
