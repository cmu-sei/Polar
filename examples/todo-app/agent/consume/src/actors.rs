use std::time::Duration;

use common::MessageDispatcher;
use common::Todo;
use common::TodoData;
use neo4rs::Graph;
use neo4rs::Query;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;
use ractor::rpc::call;
use ractor::rpc::CallResult;
use ractor::Actor;
use ractor::async_trait;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use cassini::client::*;
use utoipa::openapi::Deprecated;
use utoipa::openapi::OpenApi;

use crate::subscribe_to_topic;
use crate::BROKER_CLIENT_NAME;


/// The supervisor for our consumer actors
pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    max_registration_attempts: u32 // amount of times the supervisor will try to get the session_id from the client
}

pub enum ConsumerSupervisorMessage {
    TopicMessage {
        topic: String,
        payload: String
    }
}

pub struct ConsumerSupervisorArgs {
    /// The address the TCP client uses to contact the cassini message broker
    pub broker_addr: String, 
    /// Absolute path to the TCP client certificate
    pub client_cert_file: String, 
     /// Absolute path to the TCP client certificate key
    pub client_private_key_file: String,
    /// Absolute path to the TCP client's CA certificate
    pub ca_cert_file: String,    
}

#[async_trait]
impl Actor for ConsumerSupervisor {
    type Msg = ConsumerSupervisorMessage;
    type State = ConsumerSupervisorState;
    type Arguments = ConsumerSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ConsumerSupervisorArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        
        let state = ConsumerSupervisorState { max_registration_attempts: 5 };
        // start dispatcher
        let _ = Actor::spawn_linked(Some("DISPATCH".to_string()), MessageDispatcher, (), myself.clone().into()).await.expect("Expected to start dispatcher");
        match Actor::spawn_linked(Some(BROKER_CLIENT_NAME.to_string()), TcpClientActor, TcpClientArgs {
            bind_addr: args.broker_addr.clone(),
            ca_cert_file: args.ca_cert_file,
            client_cert_file: args.client_cert_file,
            private_key_file: args.client_private_key_file,
            registration_id: None
            
        }, myself.clone().into()).await {
            Ok((client, _)) => {
                //when client starts, successfully, start workers
                // Set up an interval
                //TODO: make configurable
                let mut interval = tokio::time::interval(Duration::from_secs(5));
                //wait until we get a session id to start clients, try some configured amount of times every few seconds
                let mut attempts= 0; 
                loop {
                    
                    attempts += 1;
                    info!("Getting session data...");
                    if let CallResult::Success(result) = call(&client, |reply| { TcpClientMessage::GetRegistrationId(reply) }, None).await.expect("Expected to call client!") {    
                        if let Some(registration_id) = result {
                            let args = TodoConsumerArgs { registration_id };

                            let _ = Actor::spawn_linked(Some("todo:consumer:todos".to_string()), TodoConsumer, args, myself.clone().into()).await.expect("Expected to start consumer");

                            
                            break;
                        } else if attempts < state.max_registration_attempts {
                          warn!("Failed to get session data. Retrying.");
                        } else if attempts >= state.max_registration_attempts{
                            error!("Failed to retrieve session data! timed out");
                            myself.stop(Some("Failed to retrieve session data! timed out".to_string()));
                        }

                    }
                    interval.tick().await;
                }
            }
            Err(e) => {
                error!("{e}");
                myself.stop(None);
            }
        }
        
        Ok(state)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        
        match message {
            _ => todo!()
        }
        Ok(())
    }

    async fn handle_supervisor_evt(&self, myself: ActorRef<Self::Msg>, msg: SupervisionEvent, _: &mut Self::State) -> Result<(), ActorProcessingErr> {
        
        match msg {
            SupervisionEvent::ActorStarted(actor_cell) => {
                info!("CONSUMER_SUPERVISOR: {0:?}:{1:?} started", actor_cell.get_name(), actor_cell.get_id());
            },
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                info!("CONSUMER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}", actor_cell.get_name(), actor_cell.get_id());
            },
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!("Consumer_SUPERVISOR: {0:?}:{1:?} failed! {e:?}", actor_cell.get_name(), actor_cell.get_id());
            },
            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }    
        
        Ok(())
    }
}

pub struct TodoConsumer;
pub struct TodoConsumerArgs {
    pub registration_id: String
}
pub struct TodoConsumerState {
    pub registration_id: String,
    pub graph: Graph
}


#[async_trait]
impl Actor for TodoConsumer {
    type Msg = TodoData;
    type State = TodoConsumerState;
    type Arguments = TodoConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: TodoConsumerArgs
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        //subscribe to topic
        Ok(subscribe_to_topic(args.registration_id, "todo:consumer:todos".to_string()).await.unwrap())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
    
        match message {
            TodoData::Todo(vec) => {
      
                let mut transaction = state.graph.start_txn().await.expect("Expected to start transaction");

                for todo in vec as Vec<Todo> {
                    let mut query = format!("CREATE (n: Todo {{id: \"{}\", value: \"{}\", done: \"{}\" }}) return n ", todo.id, todo.value, todo.done);
                    debug!("{}", query);
    
                    transaction.run(Query::new(query)).await.expect("Could not execute query on neo4j graph");
    
                    query = format!("MATCH (a:Application) WHERE a.title = '{}' with a MATCH (t:Todo) WHERE t.id = '{}' with a,t MERGE (a)-[:hasTodo]-(t)", "todo_app_sqlite_axum", todo.id);
                    debug!("{}", query);
                    transaction.run(Query::new(query)).await.expect("Could not execute query on neo4j graph");
                }

                transaction.commit().await.expect("Expected to commit transaction.");
            },
            // TODO: Implement putting the api spec within the graph, represent each endpoint as a node?
            TodoData::OpenApiSpec(json) => {
                let mut transaction = state.graph.start_txn().await.expect("Expected to start transaction");
                
                let spec = serde_json::from_str::<OpenApi>(&json).expect("Expected to deserialize JSON string");

                //decompose the api spec, create a node for the application itself, and nodes for each endpoint, which should have relationships to their operations.
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
    
                transaction.run(Query::new(query)).await.expect("Could not execute query on neo4j graph");
                //iterate through paths
                for (endpoint, path) in spec.paths.paths.iter() {
                    debug!("found endpoint \"{endpoint}\"");
                    
                    let mut operations = Vec::new();
                    path.get.as_ref().map(|op| operations.push(("GET", op.clone())));
                    path.post.as_ref().map(|op| operations.push(("POST", op.clone()))); 
                    path.put.as_ref().map(|op| operations.push(("PUT", op.clone()))); 
                    path.delete.as_ref().map(|op| operations.push(("DELETE", op.clone()) ) ); 
                    // TODO: Add additional operation types. HEAD, Options, etc.

                    for (op_type, operation) in operations {
                        
                        let op_id = operation.operation_id.clone().unwrap_or_default();
                        let mut is_deprecated = "";
                        let mut external_docs_url = String::from("");
    
                        if let Some(deprecated) = operation.deprecated.clone() {
                            match deprecated {
                                Deprecated::True => is_deprecated="true",
                                Deprecated::False =>is_deprecated="false"
                            }
                        }
    
                        if let Some(external_docs) = operation.external_docs.clone() {
                            external_docs_url = external_docs.url.clone();
                        }
                        debug!("found {op_type} operation with id \"{}\"", operation.operation_id.clone().unwrap_or_default());
                      
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
                        
                        transaction.run(Query::new(operation_query)).await.expect("Could not execute query on neo4j graph");
    
                        //draw relationship back to app node
                        operation_query = format!("MATCH (a:Application) WHERE a.title = '{}' with a MATCH (e:Endpoint) WHERE e.operationId = '{}' WITH a,e MERGE (a)-[:hasEndpoint]->(e) ",spec.info.title ,op_id.clone());
                        debug!("{}", operation_query);
                        transaction.run(Query::new(operation_query)).await.expect("Could not execute query on neo4j graph");
                    } 
                    
                }

                transaction.commit().await.expect("Expected to commit transaction.");
            }
            _ => todo!()
    
        } //end message metch
        
        Ok(())
    }

}
