use clap::Parser;
use harness_controller::service::*;
use harness_controller::{read_test_config, Arguments};
use ractor::Actor;
use std::{env, string};
use tracing::info;

#[tokio::main]
async fn main() {
    harness_controller::init_logging();

    // dump_client_message_layout();
    let args = Arguments::parse();
    let test_plan = read_test_config(&args.config);

    // For now, just print out the config in JSON
    tracing::info!(
        "Using configuration:\n{}",
        serde_json::to_string_pretty(&test_plan).unwrap()
    );

    // --- Step 2: Start C and C server ---
    let server_cert_file = env::var("TLS_SERVER_CERT_CHAIN")
        .expect("Expected a value for the TLS_SERVER_CERT_CHAIN environment variable.");
    let private_key_file = env::var("TLS_SERVER_KEY")
        .expect("Expected a value for the TLS_SERVER_KEY environment variable.");
    let ca_cert_file = env::var("TLS_CA_CERT")
        .expect("Expected a value for the TLS_CA_CERT environment variable.");
    let bind_addr = env::var("CONTROLLER_BIND_ADDR").unwrap_or(String::from("0.0.0.0:3000"));

    let args = harness_controller::service::HarnessControllerArgs {
        bind_addr,
        server_cert_file,
        private_key_file,
        ca_cert_file,
        test_plan,
        broker_path: args.broker_path,
        producer_path: args.producer_path,
        sink_path: args.sink_path,
    };

    let (_controller, handle) = Actor::spawn(
        Some("HARNESS_CONTROLLER".to_string()),
        HarnessController,
        args,
    )
    .await
    .unwrap();

    handle.await.ok();

    info!("Control Plane: Shutdown complete.");
}
