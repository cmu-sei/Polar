use crate::BROKER_CLIENT_NAME;
use crate::OPENAPI_PROCESSOR_NAME;
use cassini_client::TCPClientConfig;
use cassini_client::*;
use cassini_types::ClientEvent;
use cassini_types::WireTraceCtx;
use neo4rs::Graph;
use neo4rs::Query;
use polar::Supervisor;
use polar::SupervisorMessage;
use polar::health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::OutputPort;
use ractor::SupervisionEvent;
use ractor::async_trait;
use std::sync::Arc;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;
use utoipa::openapi::Deprecated;
use utoipa::openapi::OpenApi;
use openapi_common::AppData;

const HEALTHCHECK_ACTOR_NAME: &str = "polar.healthcheck";
const DRAIN_WINDOW_SECS: u64 = 5;

/// The supervisor for our consumer actors
pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    cassini_client: ActorRef<TcpClientMessage>,
    consumer_agent: Option<ActorRef<AppData>>,
    healthcheck: ActorRef<HealthCheckMessage>,
    draining: bool,
}

pub struct ConsumerSupervisorArgs;

impl Supervisor for ConsumerSupervisor {
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>) {
        match rkyv::from_bytes::<AppData, rkyv::rancor::Error>(&payload) {
            Ok(message) => {
                if let Some(consumer) = ractor::registry::where_is(topic.clone())
                    && let Err(e) = consumer.send_message(message)
                {
                    tracing::warn!("Error forwarding message. {e}");
                }
            }
            Err(err) => warn!("Failed to deserialize message: {:?}", err),
        }
    }
}

#[async_trait]
impl Actor for ConsumerSupervisor {
    type Msg = SupervisorMessage;
    type State = ConsumerSupervisorState;
    type Arguments = ConsumerSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _args: ConsumerSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        // --- NEW: healthcheck wiring ---
        let prepare_shutdown_port = Arc::new(OutputPort::<()>::default());
        prepare_shutdown_port.subscribe(myself.clone(), |()| {
            Some(SupervisorMessage::PrepareShutdown)
        });

        // ApiConsumer manages its own ad hoc Graph::connect() (like
        // jira-processor's children) rather than this supervisor owning a
        // shared GraphControllerActor -- no signal path exists yet to track
        // real graph availability. expects_graph is false for now; cert
        // rejuvenation still applies, Neo4j dep cert still checked.
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
                expects_graph: false,
                rejuvenation_threshold_secs: 300,
                dep_cert_endpoints,
                prepare_shutdown_port,
            },
            myself.get_cell(),
        )
        .await
        .map_err(|e| ActorProcessingErr::from(e))?;
        // --- end NEW ---

        let events_output = std::sync::Arc::new(OutputPort::default());

        events_output.subscribe(myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        });

        let config = TCPClientConfig::new()?;

        match Actor::spawn_linked(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                config,
                registration_id: None,
                events_output: Some(events_output),
                event_handler: None,
            },
            myself.clone().into(),
        )
        .await
        {
            Ok((client, _)) => Ok(ConsumerSupervisorState {
                cassini_client: client,
                consumer_agent: None,
                healthcheck,
                draining: false,
            }),
            Err(e) => Err(ActorProcessingErr::from(e)),
        }
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::Heartbeat => {}
            // --- NEW ---
            SupervisorMessage::PrepareShutdown => {
                info!("PrepareShutdown received");
                if let Err(e) = state.healthcheck.cast(HealthCheckMessage::ShutdownAck) {
                    error!("failed to send ShutdownAck: {e}");
                }
            }
            SupervisorMessage::GraphSignal(_) => {}
            SupervisorMessage::ForceExit => {
                warn!("drain window elapsed, exiting now");
                std::process::exit(1);
            }
            // --- end NEW ---
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    // --- NEW ---
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);
                    // --- end NEW ---
                    match Actor::spawn_linked(
                        Some(OPENAPI_PROCESSOR_NAME.to_string()),
                        ApiConsumer,
                        (),
                        myself.get_cell(),
                    )
                    .await
                    {
                        Ok((agent, _)) => {
                            state.consumer_agent = Some(agent);
                            state
                                .cassini_client
                                .send_message(TcpClientMessage::Subscribe {
                                    topic: OPENAPI_PROCESSOR_NAME.to_string(),
                                    trace_ctx: WireTraceCtx::from_current_span(),
                                })
                                .map_err(|e| {
                                    tracing::error!(
                                        "Failed to forward subscribe request to client. {e}"
                                    )
                                })
                                .ok();
                        }
                        Err(e) => return Err(ActorProcessingErr::from(e)),
                    }
                }
                // BUG FIX: was `_ => todo!()`, which caught BOTH
                // MessagePublished and TransportError. This meant every
                // incoming message on the subscribed topic panicked --
                // deserialize_and_dispatch (implemented via the Supervisor
                // trait above) was never actually called anywhere. This
                // agent likely has never successfully processed a real
                // message. Now correctly wired.
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    if state.draining {
                        warn!(
                            "draining -- logging message on topic '{topic}' \
                             ({} bytes) instead of dispatching; will be lost on exit",
                            payload.len()
                        );
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
                // --- NEW ---
                if actor_name == HEALTHCHECK_ACTOR_NAME && !state.draining {
                    state.draining = true;
                    info!("entering drain window before exiting for rejuvenation");
                    let _ = myself
                        .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
                // --- end NEW ---
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!(
                    "Consumer_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                // BUG FIX: previously logged only. ApiConsumer's Neo4j
                // queries use .expect() extensively (start_txn, run,
                // commit) -- any transaction failure panics the actor,
                // which previously left the pod running with a dead
                // consumer and no exit trigger.
                if !state.draining {
                    state.draining = true;
                    warn!("entering drain window before exit");
                    let _ = myself
                        .send_after(ractor::concurrency::Duration::from_secs(DRAIN_WINDOW_SECS), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
            }
            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }

        Ok(())
    }
}

pub struct ApiConsumer;

pub struct ApiConsumerState {
    pub graph: Graph,
}

#[async_trait]
impl Actor for ApiConsumer {
    type Msg = AppData;
    type State = ApiConsumerState;
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let config = polar::get_neo_config()?;

        match neo4rs::Graph::connect(config) {
            Ok(graph) => Ok(ApiConsumerState { graph }),
            Err(e) => Err(e.into()),
        }
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            // TODO: Implement putting the api spec within the graph, represent each endpoint as a node?
            AppData::OpenApiSpec(json) => {
                let mut transaction = state
                    .graph
                    .start_txn()
                    .await
                    .expect("Expected to start transaction");

                let spec = serde_json::from_str::<OpenApi>(&json)
                    .expect("Expected to deserialize JSON string");

                let query = format!(
                    "MERGE (o:Application {{ \
                    openapi_version: \"{}\", \
                    title: \"{}\", \
                    description: \"{}\", \
                    version: \"{}\", \
                    license: \"{}\" \
                }}) RETURN o",
                    serde_json::json!(spec.openapi).as_str().unwrap_or_default(),
                    spec.info.title,
                    spec.info.description.unwrap_or_default(),
                    spec.info.version,
                    spec.info.license.unwrap_or_default().name
                );
                debug!("{}", query);

                transaction
                    .run(Query::new(query))
                    .await
                    .expect("Could not execute query on neo4j graph");
                for (endpoint, path) in spec.paths.paths.iter() {
                    debug!("found endpoint \"{endpoint}\"");

                    let mut operations = Vec::new();
                    if let Some(op) = path.get.as_ref() {
                        operations.push(("GET", op.clone()))
                    }
                    if let Some(op) = path.post.as_ref() {
                        operations.push(("POST", op.clone()))
                    }
                    if let Some(op) = path.put.as_ref() {
                        operations.push(("PUT", op.clone()))
                    }
                    if let Some(op) = path.delete.as_ref() {
                        operations.push(("DELETE", op.clone()))
                    }
                    // TODO: Add additional operation types. HEAD, Options, etc.

                    for (op_type, operation) in operations {
                        let op_id = operation.operation_id.clone().unwrap_or_default();
                        let mut is_deprecated = "";
                        let mut external_docs_url = String::from("");

                        if let Some(deprecated) = operation.deprecated.clone() {
                            match deprecated {
                                Deprecated::True => is_deprecated = "true",
                                Deprecated::False => is_deprecated = "false",
                            }
                        }

                        if let Some(external_docs) = operation.external_docs.clone() {
                            external_docs_url = external_docs.url.clone();
                        }
                        debug!(
                            "found {op_type} operation with id \"{}\"",
                            operation.operation_id.clone().unwrap_or_default()
                        );

                        let mut operation_query = format!(
                            "MERGE (e:Endpoint {{ \
                            endpoint: '{}',\
                            operationId: '{}', \
                            description: '{}', \
                            operationType: '{}',\
                            isDeprecated: '{}', \
                            externalDocsUrl: '{}'
                        }}) RETURN e",
                            endpoint,
                            op_id.clone(),
                            operation.description.clone().unwrap_or_default(),
                            op_type,
                            is_deprecated,
                            external_docs_url
                        );
                        debug!("{}", operation_query);

                        transaction
                            .run(Query::new(operation_query))
                            .await
                            .expect("Could not execute query on neo4j graph");

                        operation_query = format!(
                            "MATCH (a:Application) WHERE a.title = '{}' with a MATCH (e:Endpoint) WHERE e.operationId = '{}' WITH a,e MERGE (a)-[:hasEndpoint]->(e) ",
                            spec.info.title,
                            op_id.clone()
                        );
                        debug!("{}", operation_query);
                        transaction
                            .run(Query::new(operation_query))
                            .await
                            .expect("Could not execute query on neo4j graph");
                    }
                }

                transaction
                    .commit()
                    .await
                    .expect("Expected to commit transaction.");
            }
        } //end message metch

        Ok(())
    }
}
