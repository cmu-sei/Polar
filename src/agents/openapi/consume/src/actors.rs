use crate::BROKER_CLIENT_NAME;
use crate::WEB_CONSUMER_NAME;
use cassini_client::TCPClientConfig;
use cassini_client::*;
use cassini_types::ClientEvent;
use neo4rs::Graph;
use neo4rs::Query;
use polar::Supervisor;
use polar::SupervisorMessage;
use ractor::async_trait;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::OutputPort;
use ractor::SupervisionEvent;
use tracing::debug;
use tracing::info;
use tracing::warn;
use utoipa::openapi::Deprecated;
use utoipa::openapi::OpenApi;
use web_agent_common::AppData;

/// The supervisor for our consumer actors
pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    cassini_client: ActorRef<TcpClientMessage>,
    consumer_agent: Option<ActorRef<AppData>>,
}

pub struct ConsumerSupervisorArgs;

impl Supervisor for ConsumerSupervisor {
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>) {
        match rkyv::from_bytes::<AppData, rkyv::rancor::Error>(&payload) {
            Ok(message) => {
                if let Some(consumer) = ractor::registry::where_is(topic.clone()) {
                    if let Err(e) = consumer.send_message(message) {
                        tracing::warn!("Error forwarding message. {e}");
                    }
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

        // define an output port for the actor to subscribe to
        let events_output = std::sync::Arc::new(OutputPort::default());

        // subscribe self to this port
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
                events_output,
            },
            myself.clone().into(),
        )
        .await
        {
            Ok((client, _)) => Ok(ConsumerSupervisorState {
                cassini_client: client,
                consumer_agent: None,
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
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    match Actor::spawn_linked(
                        Some(WEB_CONSUMER_NAME.to_string()),
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
                                .send_message(TcpClientMessage::Subscribe(
                                    WEB_CONSUMER_NAME.to_string(),
                                ))
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
                _ => todo!(),
            },
        }

        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _: &mut Self::State,
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
                info!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!(
                    "Consumer_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
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

        //load neo config and connect to graph db
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

                // decompose the api spec, create a node for the application itself, and nodes for each endpoint,
                // which should have relationships to their operations.
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
                //iterate through paths
                for (endpoint, path) in spec.paths.paths.iter() {
                    debug!("found endpoint \"{endpoint}\"");

                    let mut operations = Vec::new();
                    path.get
                        .as_ref()
                        .map(|op| operations.push(("GET", op.clone())));
                    path.post
                        .as_ref()
                        .map(|op| operations.push(("POST", op.clone())));
                    path.put
                        .as_ref()
                        .map(|op| operations.push(("PUT", op.clone())));
                    path.delete
                        .as_ref()
                        .map(|op| operations.push(("DELETE", op.clone())));
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

                        //draw relationship back to app node
                        operation_query = format!("MATCH (a:Application) WHERE a.title = '{}' with a MATCH (e:Endpoint) WHERE e.operationId = '{}' WITH a,e MERGE (a)-[:hasEndpoint]->(e) ",spec.info.title ,op_id.clone());
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
