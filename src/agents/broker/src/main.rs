#![allow(clippy::incompatible_msrv)]
use cassini::{
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

    let server_cert_file = env::var("TLS_SERVER_CERT_CHAIN").unwrap();
    let private_key_file = env::var("TLS_SERVER_KEY").unwrap();
    let ca_cert_file = env::var("TLS_CA_CERT").unwrap();
    let bind_addr = env::var("CASSINI_BIND_ADDR").unwrap(); // add default?

    //TODO: Make bind address configurable so that we can provide an IP via config.

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
