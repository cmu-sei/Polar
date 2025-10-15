use clap::Parser;
use harness_producer::{actors::*, read_test_config, Arguments};
use ractor::Actor;

#[tokio::main]
async fn main() {
    let args = Arguments::parse();

    polar::init_logging();

    let config = read_test_config(&args.config);

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
