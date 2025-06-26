use neo4rs::Graph;
use neo4rs::Query;
use polar::get_neo_config;
use ractor::async_trait;
use ractor::concurrency::Duration;
use ractor::Actor;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use tokio::task::AbortHandle;

struct ProvenanceActor;

struct ProvenanceActorState {
    graph: Graph,
    interval: Duration,
    abort_handle: Option<AbortHandle>,
}

enum ProvenanceActorMessage {
    Link,
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
