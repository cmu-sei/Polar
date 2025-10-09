use cyclonedx_bom::prelude::*;
use neo4rs::Graph;
use neo4rs::Query;
use polar::get_neo_config;
use ractor::async_trait;
use ractor::concurrency::Duration;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use reqwest::Client;
use tokio::task::AbortHandle;
use tracing::debug;
struct ProvenanceActor;

pub enum Command {
    Link,
    LinkPackages,
}

pub enum ProvenanceActorMessage {
    Tick(Command),
    Backoff(Option<String>),
}

struct ProvenanceActorState {
    graph: Graph,
    interval: Duration,
    abort_handle: Option<AbortHandle>,
}

impl ProvenanceActor {
    /// Queries the graph for all `Artifact` nodes with a `name` ending in `.sbom.json`
    /// and returns their `download_url` values as a vector of strings.
    /// TODO: Use this fn to fetch sboms
    pub async fn fetch_and_parse_sboms(graph: &Graph) {
        let client = Client::new();

        // TODO: remove limit after testing, do for all artifacts
        let cypher = r#"
            MATCH (a:Artifact)
            WHERE a.name ENDS WITH '.cdx.json' AND a.download_path IS NOT NULL LIMIT 10
            RETURN a.download_path AS path
        "#
        .to_string();

        debug!(cypher);

        let mut result = graph
            .execute(Query::new(cypher))
            .await
            .expect("Failed to execute Cypher query");

        while let Ok(Some(row)) = result.next().await {
            let url: String = row
                .get("url")
                .expect("Missing expected 'url' field in result row");

            // Fetch the SBOM file
            let resp = client
                .get(&url)
                .send()
                .await
                .expect("Expected to get SBOM.");

            let json = resp.text().await.expect("Expected to parse json");

            let bom = Bom::parse_from_json_v1_5(json.as_bytes()).expect("Failed to parse BOM");
            //TODO: Once we have the sboms, parse into known format and link to gitlabpackages with the same name, so if we find polar-0.1.0 we can link it to the package
            // Same goes for container images, gitlab packages for polar are versioned with git hashes, if a gitlab package version matches a polar container tag, they should be linked
            let validation_result = bom.validate();
        }
    }
}

#[async_trait]
impl Actor for ProvenanceActor {
    type Msg = ProvenanceActorMessage;
    type State = ProvenanceActorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("{myself:?} starting...");

        // get graph connection
        //TODO: Get some schedule from configuration agent
        match neo4rs::Graph::connect(get_neo_config()) {
            Ok(graph) => Ok(ProvenanceActorState {
                graph,
                interval: Duration::from_secs(30), // TODO: Make configurable
                abort_handle: None,
            }),
            Err(e) => Err(ActorProcessingErr::from(e)),
        }
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::info!("{:?} Starting loop", myself.get_name());

        state.abort_handle = Some(
            myself
                .send_interval(state.interval.clone(), || {
                    ProvenanceActorMessage::Tick(Command::LinkPackages)
                })
                .abort_handle(),
        );

        Ok(())
    }

    async fn post_stop(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if let Some(handle) = &state.abort_handle {
            handle.abort()
        }

        tracing::info!("{myself:?} stopped successfully.");

        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::info!("Counting actor handle message...");

        match message {
            ProvenanceActorMessage::Tick(command) => {
                match command {
                    Command::LinkPackages => {
                        ProvenanceActor::fetch_and_parse_sboms(&state.graph).await;
                    }
                    //TODO: Add another handler for linking package files in gtlab to container images deployed in k8s and their sboms
                    Command::Link => {
                        let query = "
                            MATCH (p:PodContainer)
                            WHERE p.image IS NOT NULL
                            WITH p, p.image AS image_ref

                            MATCH (tag:ContainerImageTag)
                            WHERE tag.location = image_ref

                            MERGE (p)-[:USES_TAG]->(tag)
                            ";

                        tracing::debug!(query);

                        if let Err(e) = state.graph.run(Query::new(query.to_string())).await {
                            tracing::warn!("{e}");
                            myself
                                .send_message(ProvenanceActorMessage::Backoff(Some(e.to_string())))
                                .expect("Expected to forward message to self");
                        }
                    }
                }
            }
            _ => todo!("Handle backoff message"),
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    polar::init_logging();
    let (_, handle) = Actor::spawn(Some("polar.provenance".to_string()), ProvenanceActor, ())
        .await
        .expect("Failed to start actor!");

    handle.await.expect("Actor failed to exit cleanly");
}
