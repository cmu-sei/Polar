use harness_producer::actors::*;
use ractor::Actor;
use tracing::info;

#[tokio::main]
async fn main() {
    polar::init_logging();

    info!("Producer Agent Starting up.");

    let (_, handle) = Actor::spawn(
        Some("cassini.harness.producer.supervisor".to_string()),
        RootActor,
        (),
    )
    .await
    .expect("Expected harness supervisor to start.");

    handle
        .await
        .expect("Expected producer to successfully execute.");
}
