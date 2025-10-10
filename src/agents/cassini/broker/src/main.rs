#![allow(clippy::incompatible_msrv)]
use cassini_broker::{
    broker::{Broker, BrokerArgs},
    BROKER_NAME,
};
use polar::init_logging;
use ractor::Actor;
use std::env;

// ============================== Main ============================== //

#[tokio::main]
async fn main() {
    init_logging();

    let server_cert_file = env::var("TLS_SERVER_CERT_CHAIN").expect("Expected a value for the TLS_SERVER_CERT_CHAIN environment variable.");
    let private_key_file = env::var("TLS_SERVER_KEY").expect("Expected a value for the TLS_SERVER_KEY environment variable.");
    let ca_cert_file = env::var("TLS_CA_CERT").expect("Expected a value for the TLS_CA_CERT environment variable.");
    let bind_addr = env::var("CASSINI_BIND_ADDR").unwrap_or(String::from("0.0.0.0:8080"));

    let args = BrokerArgs {
        bind_addr,
        session_timeout: None,
        server_cert_file,
        private_key_file,
        ca_cert_file,
    };

    // Start Supervisor
    let (_broker, handle) = Actor::spawn(Some(BROKER_NAME.to_string()), Broker, args)
        .await
        .expect("Failed to start Broker");

    handle.await.expect("Something went wrong");
}
