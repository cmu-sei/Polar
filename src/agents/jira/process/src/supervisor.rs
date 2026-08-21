use std::sync::Arc;
use std::time::Duration;

use crate::BROKER_CLIENT_NAME;
use crate::JiraConsumerArgs;
use crate::groups::JiraGroupConsumer;
use crate::issues::JiraIssueConsumer;
use crate::projects::JiraProjectConsumer;
use crate::users::JiraUserConsumer;
use cassini_backoff::{Backoff, ExponentialBackoff};
use cassini_client::TCPClientConfig;
use cassini_client::{TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientMessage;
use cassini_types::ClientEvent;
use jira_common::JIRA_GROUPS_CONSUMER_TOPIC;
use jira_common::JIRA_ISSUES_CONSUMER_TOPIC;
use jira_common::JIRA_PROJECTS_CONSUMER_TOPIC;
use jira_common::JIRA_USERS_CONSUMER_TOPIC;
use jira_common::types::JiraData;
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use polar::{Supervisor, SupervisorMessage};
use polar::graph::controller::{GraphController, GraphControllerActor, GraphControllerArgs, GraphSignal};
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::OutputPort;
use ractor::SupervisionEvent;
use ractor::async_trait;
use ractor::registry::where_is;
use ractor::rpc::CallResult;
use ractor::rpc::call;
use tokio::task::JoinHandle;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    graph_controller: Option<GraphController>,
    healthcheck: ActorRef<HealthCheckMessage>,
    draining: bool,
}
impl Supervisor for ConsumerSupervisor {
    fn deserialize_and_dispatch(topic: String, new_payload: Vec<u8>) {
        warn!("Payload:{:?}", topic);
        match rkyv::from_bytes::<ClientMessage, rkyv::rancor::Error>(&new_payload) {
            Ok(msg) => {
                match msg {
                    ClientMessage::PublishRequest {topic, payload, .. } => {
                        match rkyv::from_bytes::<JiraData, rkyv::rancor::Error>(&payload) {
                            Ok(message) => {
                                if let Some(consumer) = where_is(topic.clone())
                                    && let Err(e) = consumer.send_message(message)
                                {
                                    tracing::warn!("Error forwarding message. {e}");
                                }
                            },
                            Err(err) => warn!("Failed to deserialize message: {:?}", err),
                        }
                    },
                    other => {
                        warn!("Received unexpected ClientMessage variant: {:?}", other);
                    }
                }
            }
            Err(err) => warn!("Failed to deserialize message on topic '{topic}': {:?}", err),
        }
    }
}
impl ConsumerSupervisor {
    /// Helper to try restarting an actor at least 10 times using an exponential backoff strategy.
        async fn restart_actor(
        actor_name: String,
        supervisor: ActorRef<SupervisorMessage>,
        graph_controller: GraphController,
    ) -> Result<ActorRef<JiraData>, String> {
        let mut backoff = ExponentialBackoff::default();
        backoff.initial_interval = Duration::from_secs(3);
        backoff.max_interval = Duration::from_secs(300);
        backoff.max_elapsed_time = Some(Duration::from_secs(3000));

        let mut count = 0;

        while let Some(duration) = backoff.next_backoff() {
            count += 1;

            let registration_id = ConsumerSupervisor::get_registration_id()
                .await
                .expect("Expected agent to be registered.");

            let args = JiraConsumerArgs {
                registration_id,
                graph_controller: graph_controller.clone(),
            };

            debug!("Restarting actor: {actor_name}, attempt: {count}");

            let spawn_result: Result<(ActorRef<JiraData>, JoinHandle<()>), ActorProcessingErr> =
                match actor_name.as_str() {
                    JIRA_ISSUES_CONSUMER_TOPIC => Actor::spawn_linked(
                        Some(JIRA_ISSUES_CONSUMER_TOPIC.to_string()),
                        JiraIssueConsumer,
                        args.clone(),
                        supervisor.clone().into(),
                    )
                    .await
                    .map_err(|e| ActorProcessingErr::from(format!("{e}"))),
                    JIRA_GROUPS_CONSUMER_TOPIC => Actor::spawn_linked(
                        Some(JIRA_GROUPS_CONSUMER_TOPIC.to_string()),
                        JiraGroupConsumer,
                        args.clone(),
                        supervisor.clone().into(),
                    )
                    .await
                    .map_err(|e| ActorProcessingErr::from(format!("{e}"))),
                    JIRA_PROJECTS_CONSUMER_TOPIC => Actor::spawn_linked(
                        Some(JIRA_PROJECTS_CONSUMER_TOPIC.to_string()),
                        JiraProjectConsumer,
                        args.clone(),
                        supervisor.clone().into(),
                    )
                    .await
                    .map_err(|e| ActorProcessingErr::from(format!("{e}"))),
                    JIRA_USERS_CONSUMER_TOPIC => Actor::spawn_linked(
                        Some(JIRA_USERS_CONSUMER_TOPIC.to_string()),
                        JiraUserConsumer,
                        args.clone(),
                        supervisor.clone().into(),
                    )
                    .await
                    .map_err(|e| ActorProcessingErr::from(format!("{e}"))),
                    _ => Err(ActorProcessingErr::from("Unsupported actor name")),
                };

            match spawn_result {
                Ok(_) => break,
                Err(_) => {
                    warn!(
                        "Failed to start actor {actor_name}, retrying in {:?}",
                        duration
                    );
                    tokio::time::sleep(duration).await;
                }
            }
        }

        match where_is(actor_name.clone()) {
            Some(actor) => Ok(actor.into()),
            None => Err(format!(
                "Failed to start actor {actor_name} after {count} attempt(s)!"
            )),
        }
    }

    pub async fn get_registration_id() -> Option<String> {
        let client =
            where_is(BROKER_CLIENT_NAME.to_string()).expect("Expected to find TCP Client.");

        match call(&client, TcpClientMessage::GetRegistrationId, None)
            .await
            .expect("Expected to call TCP Client")
        {
            CallResult::Success(registration_id) => registration_id,
            _ => panic!("Couldn't contact TCP Client"),
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

        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());
        prepare_shutdown_port.subscribe(myself.clone(), |()| {
            Some(SupervisorMessage::PrepareShutdown)
        });

        // Graph availability is now tracked properly: the supervisor owns
        // a single shared GraphControllerActor (spawned on first Registered
        // event), and all four child consumers route writes through it
        // instead of each independently holding a raw neo4rs::Graph
        // connection. GraphAvailable/GraphOpFailed now flow to this
        // healthcheck the same way every other consumer-shaped agent does.
        let dep_cert_endpoints = vec![
            DepCertEndpoint::parse("cassini-ip-svc.polar.svc.cluster.local:8080:300")
                .map_err(|e| ActorProcessingErr::from(e))?,
            DepCertEndpoint::parse("polar-db-svc.polar-graph.svc.cluster.local:7687:300")
                .map_err(|e| ActorProcessingErr::from(e))?,
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
        .map_err(|e| ActorProcessingErr::from(e))?;

        let events_output = std::sync::Arc::new(OutputPort::default());

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

        let state = ConsumerSupervisorState {
            graph_controller: None,
            healthcheck,
            draining: false,
        };
        Ok(state)
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
                if let Err(e) = state.healthcheck.cast(HealthCheckMessage::ShutdownAck) {
                    error!("CONSUMER_SUPERVISOR: failed to send ShutdownAck: {e}");
                }
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
                            Some(Duration::from_secs(30)),
                        )
                        .await
                    {
                        Ok(()) => {
                            info!("CONSUMER_SUPERVISOR: graph controller stopped cleanly");
                        }
                        Err(e) => {
                            error!(
                                "CONSUMER_SUPERVISOR: graph controller did not stop cleanly \
                                 within 30s, exiting anyway: {e:?}"
                            );
                        }
                    }
                }

                warn!("CONSUMER_SUPERVISOR: exiting now");
                std::process::exit(1);
            }
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { registration_id } => {
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);

                    if state.graph_controller.is_none() {
                        let graph_signal_port = Arc::new(OutputPort::<GraphSignal>::default());
                        graph_signal_port.subscribe(myself.clone(), |signal| {
                            Some(SupervisorMessage::GraphSignal(signal))
                        });

                        match GraphControllerActor::spawn_linked(
                            Some("jira.process.graph.controller".to_string()),
                            GraphControllerActor,
                            GraphControllerArgs { signal_port: graph_signal_port },
                            myself.get_cell(),
                        )
                        .await
                        {
                            Ok((graph_controller, _)) => {
                                state.graph_controller = Some(graph_controller);
                                let _ = state.healthcheck.cast(HealthCheckMessage::GraphConnected);
                            }
                            Err(e) => {
                                error!("Error initializing graph controller! {e}");
                                return Err(ActorProcessingErr::from(e));
                            }
                        }
                    } else {
                        let _ = state.healthcheck.cast(HealthCheckMessage::GraphConnected);
                    }

                    let graph_controller = state
                        .graph_controller
                        .clone()
                        .expect("graph_controller was just set above");

                    let args = JiraConsumerArgs {
                        registration_id,
                        graph_controller,
                    };

                    if let Err(e) = Actor::spawn_linked(
                        Some(JIRA_PROJECTS_CONSUMER_TOPIC.to_string()),
                        JiraProjectConsumer,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start projects consumer. {e}");
                        myself.stop(None);
                    }
                    if let Err(e) = Actor::spawn_linked(
                        Some(JIRA_GROUPS_CONSUMER_TOPIC.to_string()),
                        JiraGroupConsumer,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start groups consumer. {e}");
                        myself.stop(None);
                    }
                    if let Err(e) = Actor::spawn_linked(
                        Some(JIRA_USERS_CONSUMER_TOPIC.to_string()),
                        JiraUserConsumer,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start users consumer. {e}");
                        myself.stop(None);
                    }

                    if let Err(e) = Actor::spawn_linked(
                        Some(JIRA_ISSUES_CONSUMER_TOPIC.to_string()),
                        JiraIssueConsumer,
                        args.clone(),
                        myself.clone().into(),
                    )
                    .await
                    {
                        error!("failed to start issues consumer. {e}");
                        myself.stop(None);
                    }
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    if state.draining {
                        warn!(
                            "CONSUMER_SUPERVISOR: draining -- logging message on topic '{topic}' \
                             ({} bytes) instead of dispatching; will be lost on exit",
                            payload.len()
                        );
                        // TODO: publish to dead-letter queue once Cassini supports one.
                    } else {
                        ConsumerSupervisor::deserialize_and_dispatch(topic, payload);
                    }
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniDisconnected);
                }
                _ => (),
            },
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
            SupervisionEvent::ActorStarted(actor_cell) => {
                info!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} started",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                let actor_name = actor_cell.get_name().unwrap_or_default();

                info!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_name,
                    actor_cell.get_id()
                );

                // Clean termination of the healthcheck actor means the
                // rejuvenation sequence completed -- exit via the bounded
                // drain window, same as every other agent today. Any other
                // actor terminating (a data consumer child) keeps the
                // existing backoff-restart recovery path, unchanged.
                if actor_name == HEALTHCHECK_ACTOR_NAME {
                    if !state.draining {
                        state.draining = true;
                        info!(
                            "CONSUMER_SUPERVISOR: healthcheck actor terminated cleanly, \
                             entering drain window before exiting for rejuvenation"
                        );
                        let _ = myself
                            .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                                SupervisorMessage::ForceExit
                            })
                            .await;
                    }
                    return Ok(());
                }

                match ConsumerSupervisor::restart_actor(
                    actor_name.clone(),
                    myself.clone(),
                    state.graph_controller.clone().expect("graph_controller already spawned by the time a child could fail"),
                )
                .await
                {
                    Ok(actor) => {
                        info!("Restarted actor {0:?}:{1:?}", actor_name, actor.get_id())
                    }
                    Err(e) => {
                        error!("Failed to recover actor: {e}");
                        myself.stop(Some(e))
                    }
                }
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                let actor_name = actor_cell.get_name().unwrap_or_default();

                warn!(
                    "Consumer_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_name,
                    actor_cell.get_id()
                );

                // A healthcheck actor failure (e.g. hard cert-expiry
                // deadline, or -- once wired -- a graph-availability
                // deadline) is fatal: drain and exit, same as every other
                // agent. A data consumer child failing keeps the existing
                // backoff-restart recovery path.
                if actor_name == HEALTHCHECK_ACTOR_NAME {
                    if !state.draining {
                        state.draining = true;
                        warn!(
                            "CONSUMER_SUPERVISOR: entering drain window before exit; \
                             any messages that arrive will be logged, not dispatched"
                        );
                        let _ = myself
                            .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                                SupervisorMessage::ForceExit
                            })
                            .await;
                    }
                    return Ok(());
                }

                match ConsumerSupervisor::restart_actor(
                    actor_name.clone(),
                    myself.clone(),
                    state.graph_controller.clone().expect("graph_controller already spawned by the time a child could fail"),
                )
                .await
                {
                    Ok(actor) => {
                        info!("Restarted actor {0:?}:{1:?}", actor_name, actor.get_id())
                    }
                    Err(e) => {
                        error!("Failed to recover actor: {e}");
                        myself.stop(Some(e))
                    }
                }
            }

            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }

        Ok(())
    }
}
