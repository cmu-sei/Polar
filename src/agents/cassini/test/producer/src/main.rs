use harness_producer::actors::*;
use ractor::Actor;
use tracing::info;

#[tokio::main]
async fn main() {
    cassini_client::init_tracing("cassini-harness-producer");

    info!("Producer Agent Starting up.");

    let (_, handle) = Actor::spawn(
        Some("cassini.harness.producer.supervisor".to_string()),
        RootActor,
        (),
    ).await.expect("Expected harness supervisor to start.");

    handle.await.expect("Expected producer to successfully execute.");

    cassini_client::shutdown_tracing();
    std::process::exit(0);
}
