use harness_common::client::HarnessClientConfig;
use harness_producer::actors::*;
use ractor::Actor;
use tracing::info;

#[tokio::main]
async fn main() {
    polar::init_logging();

    info!("Producer Agent Starting up.");

    let client_config = HarnessClientConfig::new();

    let (_, handle) = Actor::spawn(
        Some("cassini.harness.producer.supervisor".to_string()),
        RootActor,
        RootActorArguments {
            tcp_client_config: client_config,
        },
    )
    .await
    .expect("Expected harness supervisor to start.");

    handle
        .await
        .expect("Expected producer to successfully execute.");
}
