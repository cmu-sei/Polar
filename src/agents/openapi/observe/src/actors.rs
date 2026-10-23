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

use crate::BROKER_CLIENT_NAME;
use cassini_client::TCPClientConfig;
use cassini_client::*;
use cassini_types::ClientEvent;
use cassini_types::WireTraceCtx;
use openapi_common::AppData;
use polar::SupervisorMessage;
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::OutputPort;
use ractor::SupervisionEvent;
use ractor::async_trait;
use ractor::registry::where_is;
use reqwest::Client;
use tracing::debug;
use tracing::info;
use tracing::warn;

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

pub struct ObserverSupervisor;

pub struct ObserverSupervisorState {
    /// The url for the openapi spec
    openapi_endpoint: String,
    healthcheck: ActorRef<HealthCheckMessage>,
    draining: bool,
}

pub struct ObserverSupervisorArgs {
    /// The url for the openapi spec
    pub openapi_endpoint: String,
}

#[async_trait]
impl Actor for ObserverSupervisor {
    type Msg = SupervisorMessage;
    type State = ObserverSupervisorState;
    type Arguments = ObserverSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ObserverSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let dep_cert_endpoints = vec![
            DepCertEndpoint::parse("cassini-ip-svc.polar.svc.cluster.local:8080:300")
                .map_err(ActorProcessingErr::from)?,
        ];

        let (healthcheck, _) = HealthCheckActor::spawn_linked(
            Some(HEALTHCHECK_ACTOR_NAME.to_string()),
            HealthCheckActor,
            HealthCheckArgs::new(false, 300, dep_cert_endpoints),
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;
        // --- end NEW ---

        let state = ObserverSupervisorState {
            openapi_endpoint: args.openapi_endpoint,
            healthcheck,
            draining: false,
        };

        let events_output = std::sync::Arc::new(OutputPort::default());

        events_output.subscribe(myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        });

        let config = TCPClientConfig::new()?;

        if let Err(e) = Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config,
                registration_id: None,
                delivery: cassini_client::DeliveryMode::Broadcast(events_output),
                answers: None,
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
            SupervisorMessage::Heartbeat => {}
            SupervisorMessage::GraphSignal(_) => {}
            SupervisorMessage::ForceExit => {
                warn!("drain window elapsed, exiting now");
                std::process::exit(1);
            }
            // --- end NEW ---
            SupervisorMessage::ClientEvent { event } => {
                match event {
                    ClientEvent::Registered { .. } => {
                        // --- NEW ---
                        let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);
                        // --- end NEW ---
                        let args = ApiObserverArgs {
                            openapi_endpoint: state.openapi_endpoint.clone(),
                        };
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
                    ClientEvent::TransportError { reason } => {
                        warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                        // --- NEW ---
                        let _ = state
                            .healthcheck
                            .cast(HealthCheckMessage::CassiniDisconnected);
                        // --- end NEW ---
                    }
                    _ => (),
                }
            }
        }

        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                let actor_name = actor_cell.get_name().unwrap_or_default();
                info!(
                    "OBSERVER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_name,
                    actor_cell.get_id()
                );
                // --- NEW ---
                if actor_name == HEALTHCHECK_ACTOR_NAME && !state.draining {
                    state.draining = true;
                    info!("entering drain window before exiting for rejuvenation");
                    let _ = myself
                        .send_after(
                            ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS),
                            || SupervisorMessage::ForceExit,
                        )
                        .await;
                }
                // --- end NEW ---
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!(
                    "OBSERVER_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                // BUG FIX: previously logged only -- ApiObserver panicking
                // (e.g. on a malformed spec or network failure) would leave
                // the pod running indefinitely with a dead child and no exit
                // trigger.
                if !state.draining {
                    state.draining = true;
                    warn!("entering drain window before exit");
                    let _ = myself
                        .send_after(
                            ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS),
                            || SupervisorMessage::ForceExit,
                        )
                        .await;
                }
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
    openapi_endpoint: String,
}

pub struct ApiObserverArgs {
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
        _myself: ActorRef<Self::Msg>,
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

                        client.send_message(TcpClientMessage::publish(
                            "polar.web.consumer".to_string(),
                            payload.to_vec(),
                            WireTraceCtx::from_current_span(),
                        ))?;
                    }
                    // BUG FIX: was `Err(_) => todo!()` -- a live panic on
                    // any malformed OpenAPI response. Now returns an error
                    // from handle() instead, which ractor surfaces as
                    // ActorFailed to the parent supervisor -- caught by the
                    // supervisor's now-wired drain-and-exit logic above,
                    // rather than panicking the actor uncontrolled.
                    Err(e) => {
                        return Err(ActorProcessingErr::from(format!(
                            "failed to parse OpenAPI spec response: {e}"
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}
