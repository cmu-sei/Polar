use neo4rs::Graph;
use neo4rs::Query;
use polar::get_neo_config;
use ractor::async_trait;
use ractor::concurrency::Duration;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use reqwest::Client;
use serde_json::Value;
use tokio::task::AbortHandle;
use tracing::debug;

struct ProvenanceActor;

struct ProvenanceActorState {
    graph: Graph,
    interval: Duration,
    abort_handle: Option<AbortHandle>,
}

enum ProvenanceActorMessage {
    Link,
}

impl ProvenanceActor {
    /// Queries the graph for all `Artifact` nodes with a `name` ending in `.sbom.json`
    /// and returns their `download_url` values as a vector of strings.
    /// TODO: Use this fn to fetch sboms
    pub async fn fetch_and_parse_sboms(graph: &Graph, client: &Client) {
        let cypher = r#"
            MATCH (a:Artifact)
            WHERE a.name ENDS WITH '.sbom.json' AND exists(a.download_url)
            RETURN a.download_url AS url
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

            let json: Value = resp.json().await.expect("Expected to parse json");

            debug!("{json}");
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
        match neo4rs::Graph::connect(get_neo_config()).await {
            Ok(graph) => Ok(ProvenanceActorState {
                graph,
                interval: Duration::from_secs(30),
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
                .send_interval(state.interval.clone(), || ProvenanceActorMessage::Link)
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
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::info!("Counting actor handle message...");

        match message {
            //TODO: Add another handler for linking package files in gtlab to container images deployed in k8s and their sboms
            ProvenanceActorMessage::Link => {
                let query = "
                    MATCH (p:PodContainer)
                    WHERE p.image IS NOT NULL
                    WITH p, p.image AS image_ref

                    MATCH (tag:ContainerImageTag)
                    WHERE tag.location = image_ref

                    MERGE (p)-[:USES_TAG]->(tag)
                    RETURN p.name AS pod_name, tag.location AS matched_tag
                    ";

                tracing::debug!(query);

                if let Err(e) = state.graph.run(Query::new(query.to_string())).await {
                    tracing::warn!("{e}");
                }
            }
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
