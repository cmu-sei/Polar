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
use cassini_client::{TcpClientMessage, TCPClientConfig, TcpClientActor, TcpClientArgs};
use cassini_types::{ClientEvent,WireTraceCtx};
use credentials_agent_common::{CREDENTIALS_GRANTS_TOPIC, CREDENTIALS_REVOCATIONS_TOPIC};
use credentials_agent_common::events::CredentialsEvent;
use polar::graph::controller::{GraphController, GraphControllerActor, GraphControllerArgs, GraphSignal};
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use polar::{Supervisor, SupervisorMessage};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, SupervisionEvent, async_trait};
use ractor::registry::where_is;
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
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>) {
        match rkyv::from_bytes::<CredentialsEvent, rkyv::rancor::Error>(&payload) {
            Ok(event) => {
                // Both credentials-grants and credentials-revocations
                // carry the same CredentialsEvent type -- unlike
                // jira/process's convention, topic name isn't used to
                // select a different actor, since there's only ever
                // one writer (LedgerActor) here at all.
                if let Some(ledger) = where_is(LEDGER_ACTOR_NAME.to_string()) {
                    if let Err(e) = ledger.send_message(LedgerMessage::AppendNoReply(event)) {
                        warn!("Failed to forward event from topic {topic} to ledger: {e}");
                    }
                } else {
                    warn!("Ledger actor not yet registered, dropping event from topic {topic}");
                }
            }
            Err(e) => {
                warn!("Failed to deserialize CredentialsEvent from topic {topic}: {e}");
            }
        }
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

                    if let Some(client) = where_is(BROKER_CLIENT_NAME.to_string()) {
                        for topic in [CREDENTIALS_GRANTS_TOPIC, CREDENTIALS_REVOCATIONS_TOPIC] {
                            if let Err(e) = client.send_message(TcpClientMessage::Subscribe {
                                topic: topic.to_string(),
                                trace_ctx: WireTraceCtx::from_current_span(),
                            }) {
                                error!("Failed to subscribe to {topic}: {e}");
                            }
                        }
                    } else {
                        error!("Broker client not found while attempting to subscribe");
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::GrantRecord;

    async fn spawn_ledger_registered(log_path: &str) -> ActorRef<LedgerMessage> {
        let (ledger, _) = Actor::spawn(
            Some(LEDGER_ACTOR_NAME.to_string()),
            LedgerActor,
            LedgerActorArgs {
                log_path: log_path.to_string(),
            },
        )
        .await
        .expect("ledger should spawn cleanly");
        ledger
    }

    async fn check_grant(ledger: &ActorRef<LedgerMessage>, identity: &str) -> Option<GrantRecord> {
        match ractor::rpc::call(
            ledger,
            |reply| LedgerMessage::CheckGrant {
                identity: identity.to_string(),
                reply,
            },
            None,
        )
        .await
        .expect("call should succeed")
        {
            ractor::rpc::CallResult::Success(result) => result,
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn deserialize_and_dispatch_forwards_a_real_event_to_the_real_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let ledger = spawn_ledger_registered(path.to_str().unwrap()).await;

        let event = CredentialsEvent::Grant {
            target_identity: "alice".to_string(),
            permissions: vec!["read".to_string()],
            granted_by: "admin".to_string(),
            timestamp: 1000,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&event)
            .unwrap()
            .to_vec();

        // The real function every incoming Cassini message actually
        // goes through -- not a reimplementation of it.
        ConsumerSupervisor::deserialize_and_dispatch(
            CREDENTIALS_GRANTS_TOPIC.to_string(),
            bytes,
        );

        // A subsequent call() to the same actor only returns after
        // every prior message (including our dispatch above) has
        // already been processed, by ractor's own sequential-mailbox
        // guarantee -- no arbitrary sleep needed to synchronize.
        let grant = check_grant(&ledger, "alice").await;
        assert!(grant.is_some());
        assert_eq!(grant.unwrap().granted_by, "admin");

        ledger.stop(None);
    }

    #[tokio::test]
    async fn deserialize_and_dispatch_handles_missing_ledger_gracefully() {
        // No ledger registered at all -- must not panic, just log and
        // drop, matching the documented behavior for this case.
        let event = CredentialsEvent::Revoke {
            serial: "abc123".to_string(),
            reason: "compromised".to_string(),
            revoked_by: "admin".to_string(),
            timestamp: 1000,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&event)
            .unwrap()
            .to_vec();

        ConsumerSupervisor::deserialize_and_dispatch(
            CREDENTIALS_REVOCATIONS_TOPIC.to_string(),
            bytes,
        );
        // Reaching this line at all is the assertion -- no panic.
    }
}
