use std::time::Duration;

use crate::BROKER_CLIENT_NAME;
use cassini::client::*;
use cassini::ClientMessage;
use cassini::TCPClientConfig;
use ractor::async_trait;
use ractor::registry::where_is;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::OutputPort;
use ractor::SupervisionEvent;
use reqwest::Client;
use tracing::debug;
use tracing::info;
use tracing::warn;
use web_agent_common::AppData;

pub struct ObserverSupervisor;

pub struct ObserverSupervisorState {
    /// The url for the openapi spec
    openapi_endpoint: String,
}

pub struct ObserverSupervisorArgs {
    /// The url for the openapi spec
    pub openapi_endpoint: String,
}

pub enum ObserverSupervisorMessage {
    ClientRegistered(String),
}

#[async_trait]
impl Actor for ObserverSupervisor {
    type Msg = ObserverSupervisorMessage;
    type State = ObserverSupervisorState;
    type Arguments = ObserverSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ObserverSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        // define an output port for the actor to subscribe to
        let output_port = std::sync::Arc::new(OutputPort::default());

        let state = ObserverSupervisorState {
            openapi_endpoint: args.openapi_endpoint,
        };

        // subscribe self to this port
        output_port.subscribe(myself.clone(), |message| {
            Some(ObserverSupervisorMessage::ClientRegistered(message))
        });

        if let Err(e) = Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config: TCPClientConfig::new(),
                registration_id: None,
                output_port,
            },
            myself.clone().into(),
        )
        .await
        {
            return Err(ActorProcessingErr::from(e));
        }

        Ok(state)
    }

    async fn post_start(
        &self,
        _: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ObserverSupervisorMessage::ClientRegistered(registration_id) => {
                let args = ApiObserverArgs {
                    registration_id,
                    openapi_endpoint: state.openapi_endpoint.clone(),
                };

                // finish init
                if let Err(e) = Actor::spawn_linked(
                    Some("polar.web.observer".to_string()),
                    ApiObserver,
                    args,
                    myself.get_cell(),
                )
                .await
                {
                    return Err(ActorProcessingErr::from(e));
                }
            }
        }
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
                    "OBSERVER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!(
                    "OBSERVER_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }

        Ok(())
    }
}

pub struct ApiObserver;

pub enum ApiObserverMessage {
    GetApiSpec,
}

pub struct ApiObserverState {
    web_client: Client,
    registration_id: String,
    openapi_endpoint: String,
}

pub struct ApiObserverArgs {
    registration_id: String,
    openapi_endpoint: String,
}

#[async_trait]
impl Actor for ApiObserver {
    type Msg = ApiObserverMessage;
    type State = ApiObserverState;
    type Arguments = ApiObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ApiObserverArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        // TODO Add configurations for client, certificates, proxy, etc.
        let client = Client::new();

        let state = ApiObserverState {
            web_client: client,
            registration_id: args.registration_id,
            openapi_endpoint: args.openapi_endpoint.clone(),
        };
        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("{myself:?} Started");

        myself.send_message(ApiObserverMessage::GetApiSpec).unwrap();

        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ApiObserverMessage::GetApiSpec => {
                info!("Retreiving api spec");

                // TODO: Replace with config data, endpoints etc.
                let resp = state
                    .web_client
                    .get(state.openapi_endpoint.as_str())
                    .send()
                    .await
                    .expect("Expected to contact app");

                // validate

                match resp.json::<utoipa::openapi::OpenApi>().await {
                    Ok(spec) => {
                        let data = AppData::OpenApiSpec(spec.to_pretty_json().unwrap());

                        let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&data).unwrap();

                        let client = where_is(BROKER_CLIENT_NAME.to_owned())
                            .expect("Expected to find TCP client");

                        client
                            .send_message(TcpClientMessage::Send(ClientMessage::PublishRequest {
                                topic: "polar.web.consumer".to_string(),
                                payload: payload.to_vec(),
                                registration_id: Some(state.registration_id.clone()),
                            }))
                            .unwrap();
                    }
                    Err(e) => todo!(),
                }
            }
        }
        Ok(())
    }
}
