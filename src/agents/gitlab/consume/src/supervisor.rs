use crate::BROKER_CLIENT_NAME;
use crate::GitlabConsumer;
use crate::GitlabConsumerState;
use crate::groups::GitlabGroupConsumer;
use crate::meta::MetaConsumer;
use crate::pipelines::GitlabPipelineConsumer;
use crate::projects::GitlabProjectConsumer;
use crate::repositories::GitlabRepositoryConsumer;
use crate::runners::GitlabRunnerConsumer;
use crate::users::GitlabUserConsumer;
use cassini_client::*;
use cassini_types::ClientEvent;
use common::GROUPS_CONSUMER_TOPIC;
use common::METADATA_CONSUMER_TOPIC;
use common::PIPELINE_CONSUMER_TOPIC;
use common::PROJECTS_CONSUMER_TOPIC;
use common::REPOSITORY_CONSUMER_TOPIC;
use common::RUNNERS_CONSUMER_TOPIC;
use common::USER_CONSUMER_TOPIC;
use common::types::GitlabEnvelope;
use polar::Supervisor;
use polar::SupervisorMessage;
use polar::get_neo_config;
use polar::graph::controller::GraphControllerActor;
use polar::polar_health::{HealthCheckActor, HealthCheckArgs, HealthCheckMessage, DepCertEndpoint};
use std::sync::Arc;
use ractor::Actor;
use ractor::ActorCell;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::OutputPort;
use ractor::SupervisionEvent;
use ractor::async_trait;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;
use tracing::{instrument, trace};

pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    tcp_client: ActorRef<TcpClientMessage>,
    u_consumer: Option<GitlabConsumer>,
    #[allow(dead_code)]
    healthcheck: ActorRef<HealthCheckMessage>,
}

impl Supervisor for ConsumerSupervisor {
    #[instrument(level = "trace", fields(topic=topic), skip(payload))]
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>) {
        match rkyv::from_bytes::<GitlabEnvelope, rkyv::rancor::Error>(&payload) {
            Ok(message) => {
                if let Some(consumer) = ractor::registry::where_is(topic.clone()) {
                    trace!("Forwarding message to {topic}");
                    if let Err(e) = consumer.send_message(message) {
                        tracing::warn!("Error forwarding message. {e}");
                    }
                }
            }
            Err(err) => warn!("Failed to deserialize message: {:?}", err),
        }
    }
}

impl ConsumerSupervisor {
    pub async fn spawn_children(
        myself: ActorCell,
        state: &mut ConsumerSupervisorState,
        c_state: GitlabConsumerState,
    ) -> Result<(), ActorProcessingErr> {
        if let Err(e) = Actor::spawn_linked(
            Some(METADATA_CONSUMER_TOPIC.to_string()),
            MetaConsumer,
            c_state.clone(),
            myself.clone(),
        )
        .await
        {
            return Err(format!("failed to start meta consumer. {e}").into());
        }
        state.u_consumer = Some(
            Actor::spawn_linked(
                Some(USER_CONSUMER_TOPIC.to_string()),
                GitlabUserConsumer,
                c_state.clone(),
                myself.clone(),
            )
            .await?
            .0,
        );

        if let Err(e) = Actor::spawn_linked(
            Some(GROUPS_CONSUMER_TOPIC.to_string()),
            GitlabGroupConsumer,
            c_state.clone(),
            myself.clone(),
        )
        .await
        {
            return Err(format!("failed to start groups consumer. {e}").into());
        }
        if let Err(e) = Actor::spawn_linked(
            Some(RUNNERS_CONSUMER_TOPIC.to_string()),
            GitlabRunnerConsumer,
            c_state.clone(),
            myself.clone(),
        )
        .await
        {
            return Err(format!("failed to start runners consumer. {e}").into());
        }
        if let Err(e) = Actor::spawn_linked(
            Some(PROJECTS_CONSUMER_TOPIC.to_string()),
            GitlabProjectConsumer,
            c_state.clone(),
            myself.clone(),
        )
        .await
        {
            return Err(format!("failed to start projects consumer. {e}").into());
        }
        if let Err(e) = Actor::spawn_linked(
            Some(PIPELINE_CONSUMER_TOPIC.to_string()),
            GitlabPipelineConsumer,
            c_state.clone(),
            myself.clone(),
        )
        .await
        {
            return Err(format!("failed to start pipeline consumer. {e}").into());
        }
        if let Err(e) = Actor::spawn_linked(
            Some(REPOSITORY_CONSUMER_TOPIC.to_string()),
            GitlabRepositoryConsumer,
            c_state.clone(),
            myself.clone(),
        )
        .await
        {
            return Err(format!("failed to start repository consumer. {e}").into());
        }

        Ok(())
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

        let events_output = std::sync::Arc::new(OutputPort::default());
        //subscribe
        events_output.subscribe(myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        });

        let client_config = TCPClientConfig::new()?;

        let (tcp_client, _) = Actor::spawn_linked(
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

        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());
        prepare_shutdown_port.subscribe(myself.clone(), |()| {
            Some(SupervisorMessage::PrepareShutdown)
        });

        let (healthcheck, _) = Actor::spawn_linked(
            Some("polar.healthcheck".to_string()),
            HealthCheckActor,
            HealthCheckArgs {
                expects_graph: true,
                rejuvenation_threshold_secs: 300,
                dep_cert_endpoints: vec![
                    DepCertEndpoint {
                        host: "cassini-ip-svc.polar.svc.cluster.local".to_string(),
                        port: 8080,
                        min_ttl_secs: 300,
                    },
                    DepCertEndpoint {
                        host: "polar-db-svc.polar-graph.svc.cluster.local".to_string(),
                        port: 7687,
                        min_ttl_secs: 300,
                    },
                ],
                prepare_shutdown_port,
            },
            myself.clone().into(),
        )
        .await?;

        let state = ConsumerSupervisorState {
            tcp_client,
            u_consumer: None,
            healthcheck,
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
                let _ = state.healthcheck.cast(HealthCheckMessage::ShutdownAck);
            }
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    info!("Initializing agent.");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);
                    match Actor::spawn_linked(
                        Some("polar.gitlab.consumer.graph".to_string()),
                        GraphControllerActor,
                        (),
                        myself.get_cell(),
                    )
                    .await
                    {
                        Ok((graph_controller, _)) => {
                            let _ = state.healthcheck.cast(HealthCheckMessage::GraphConnected);
                            let c_state = GitlabConsumerState {
                                graph_controller,
                                tcp_client: state.tcp_client.clone(),
                            };
                            Self::spawn_children(myself.get_cell(), state, c_state).await?;
                        }
                        Err(e) => {
                            error!("{e}");
                            myself.stop(Some(e.to_string()));
                        }
                    }
                    info!("Finished initialization. Waiting for messages...");
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    ConsumerSupervisor::deserialize_and_dispatch(topic, payload);
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniDisconnected);
                }
                ClientEvent::ControlResponse { .. } => {}
                _ => (),
            },
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(actor_cell) => {
                debug!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} started",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                // we no actors start w/o names
                let actor_name = actor_cell.get_name();

                warn!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_name,
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                // we no actors start w/o names
                let actor_name = actor_cell.get_name();

                error!(
                    "Consumer_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_name,
                    actor_cell.get_id()
                );
            }

            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }

        Ok(())
    }
}
