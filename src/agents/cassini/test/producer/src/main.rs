use harness_producer::{actors::*, read_test_config};
use ractor::Actor;

#[tokio::main]
async fn main() {
    polar::init_logging();
    let config = read_test_config("config.dhall");

    // For now, just print out the config in JSON
    tracing::info!(
        "Using configuration:\n{}",
        serde_json::to_string_pretty(&config).unwrap()
    );

    let args = RootActorArguments { test_plan: config };

    let (_, handle) = Actor::spawn(None, RootActor, args)
        .await
        .expect("Expected harness supervisor to start.");

    handle
        .await
        .expect("Expected producer to successfully execute.");
}
