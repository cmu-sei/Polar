/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

use crate::ledger::{LedgerActor, LedgerActorArgs, LedgerMessage};
use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs};
use cassini_types::ClientEvent;
use polar::graph::controller::{GraphController, GraphControllerActor, GraphControllerArgs, GraphSignal};
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use polar::{Supervisor, SupervisorMessage};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

const HEALTHCHECK_ACTOR_NAME: &str = "polar.credentials-agent.healthcheck";
const LEDGER_ACTOR_NAME: &str = "polar.credentials-agent.ledger";
const BROKER_CLIENT_NAME: &str = "CREDENTIALS_AGENT_CONSUMER_CLIENT";

/// Path to the ledger's own event log -- the actual source of truth,
/// per credentials-agent-architecture.md. Expected to live on a
/// PersistentVolume, the same way cert-issuer's own CA material is
/// persisted, so "rebuild the log at startup" rebuilds from real
/// history rather than from nothing on every pod restart.
const CREDENTIALS_EVENT_LOG_PATH_ENV: &str = "CREDENTIALS_EVENT_LOG_PATH";
const CREDENTIALS_EVENT_LOG_PATH_DEFAULT: &str = "/var/lib/credentials-agent/events.jsonl";

pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    /// The single, serialized owner of the event log and derived
    /// state -- see ledger.rs's own module doc for why this can't be
    /// split across multiple actors the way jira/process's four data
    /// streams safely are.
    ledger: ActorRef<LedgerMessage>,
    graph_controller: Option<GraphController>,
    healthcheck: ActorRef<HealthCheckMessage>,
    draining: bool,
}

impl Supervisor for ConsumerSupervisor {
    fn deserialize_and_dispatch(_topic: String, _payload: Vec<u8>) {
        // Real topic subscriptions (credentials-grants,
        // credentials-revocations, credentials-issuance-requests) are
        // the next increment -- this supervisor's startup sequence
        // (ledger, healthcheck, graph controller) is scoped and
        // proven first, deliberately, before adding message handling
        // on top of it.
    }
}

#[async_trait]
impl Actor for ConsumerSupervisor {
    type Msg = SupervisorMessage;
    type State = ConsumerSupervisorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let log_path = std::env::var(CREDENTIALS_EVENT_LOG_PATH_ENV)
            .unwrap_or_else(|_| CREDENTIALS_EVENT_LOG_PATH_DEFAULT.to_string());

        let (ledger, _) = LedgerActor::spawn_linked(
            Some(LEDGER_ACTOR_NAME.to_string()),
            LedgerActor,
            LedgerActorArgs { log_path },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());
        prepare_shutdown_port.subscribe(myself.clone(), |()| {
            Some(SupervisorMessage::PrepareShutdown)
        });

        let dep_cert_endpoints = vec![
            DepCertEndpoint::parse("cassini-ip-svc.polar.svc.cluster.local:8080:300")
                .map_err(ActorProcessingErr::from)?,
            DepCertEndpoint::parse("polar-db-svc.polar-graph.svc.cluster.local:7687:300")
                .map_err(ActorProcessingErr::from)?,
        ];

        let (healthcheck, _) = HealthCheckActor::spawn_linked(
            Some(HEALTHCHECK_ACTOR_NAME.to_string()),
            HealthCheckActor,
            HealthCheckArgs {
                expects_graph: true,
                rejuvenation_threshold_secs: 300,
                dep_cert_endpoints,
                prepare_shutdown_port,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let events_output = Arc::new(OutputPort::default());
        events_output.subscribe(myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        });

        let client_config = TCPClientConfig::new()?;

        let (_client, _) = Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config: client_config,
                registration_id: None,
                events_output: Some(events_output),
                event_handler: None,
            },
            myself.clone().into(),
        )
        .await?;

        Ok(ConsumerSupervisorState {
            ledger,
            graph_controller: None,
            healthcheck,
            draining: false,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::Heartbeat => {}
            SupervisorMessage::PrepareShutdown => {
                info!("CONSUMER_SUPERVISOR: PrepareShutdown received");
                state.draining = true;
            }
            SupervisorMessage::GraphSignal(signal) => match signal {
                GraphSignal::Available => {
                    let _ = state.healthcheck.cast(HealthCheckMessage::GraphAvailable);
                }
                GraphSignal::OpFailed(reason) => {
                    let _ = state
                        .healthcheck
                        .cast(HealthCheckMessage::GraphOpFailed(reason));
                }
            },
            SupervisorMessage::ForceExit => {
                warn!("CONSUMER_SUPERVISOR: drain window elapsed, beginning graceful shutdown");

                if let Some(controller) = state.graph_controller.take() {
                    warn!(
                        "CONSUMER_SUPERVISOR: stopping graph controller and waiting for any \
                         in-flight write to complete before exiting"
                    );
                    match controller
                        .stop_and_wait(
                            Some("supervisor shutting down".into()),
                            Some(std::time::Duration::from_secs(30)),
                        )
                        .await
                    {
                        Ok(()) => info!("CONSUMER_SUPERVISOR: graph controller stopped cleanly"),
                        Err(e) => error!(
                            "CONSUMER_SUPERVISOR: graph controller did not stop cleanly \
                             within 30s, exiting anyway: {e:?}"
                        ),
                    }
                }

                warn!("CONSUMER_SUPERVISOR: exiting now");
                std::process::exit(1);
            }
            SupervisorMessage::ClientEvent { event } => {
                if let ClientEvent::Registered { .. } = event {
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);

                    if state.graph_controller.is_none() {
                        let graph_signal_port = Arc::new(OutputPort::<GraphSignal>::default());
                        graph_signal_port.subscribe(myself.clone(), |signal| {
                            Some(SupervisorMessage::GraphSignal(signal))
                        });

                        match GraphControllerActor::spawn_linked(
                            Some("credentials-agent.consumer.graph.controller".to_string()),
                            GraphControllerActor,
                            GraphControllerArgs {
                                signal_port: graph_signal_port,
                            },
                            myself.get_cell(),
                        )
                        .await
                        {
                            Ok((graph_controller, _)) => {
                                state.graph_controller = Some(graph_controller);
                                let _ =
                                    state.healthcheck.cast(HealthCheckMessage::GraphConnected);
                            }
                            Err(e) => {
                                error!("Error initializing graph controller! {e}");
                                return Err(ActorProcessingErr::from(e));
                            }
                        }
                    } else {
                        let _ = state.healthcheck.cast(HealthCheckMessage::GraphConnected);
                    }

                    // Real topic subscriptions are the next increment.
                }
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        message: SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisionEvent::ActorTerminated(actor, _, reason) => {
                let name = actor.get_name();
                if name.as_deref() == Some(HEALTHCHECK_ACTOR_NAME) {
                    info!("Healthcheck actor terminated ({reason:?}), exiting");
                    myself.stop(None);
                } else if name.as_deref() == Some(LEDGER_ACTOR_NAME) {
                    error!("Ledger actor terminated ({reason:?}) -- exiting, since the ledger is this agent's actual source of truth and nothing else can safely continue without it");
                    myself.stop(None);
                }
            }
            SupervisionEvent::ActorFailed(actor, err) => {
                error!("Child actor {actor:?} failed: {err}");
                myself.stop(None);
            }
            _ => {}
        }
        Ok(())
    }
}
