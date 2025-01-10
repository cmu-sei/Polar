#![allow(clippy::incompatible_msrv)]
use std::env;
use cassini::{broker::{Broker, BrokerArgs}, init_logging, BROKER_NAME};
use ractor::Actor;

// ============================== Main ============================== //

#[tokio::main]
async fn main() {
    init_logging();
    //start supervisor
    
    let server_cert_file = env::var("TLS_SERVER_CERT_CHAIN").unwrap();
    let private_key_file = env::var("TLS_SERVER_KEY").unwrap();
    let ca_cert_file = env::var("TLS_CA_CERT").unwrap();



    let args = BrokerArgs { bind_addr: String::from("127.0.0.1:8080"), session_timeout: None, server_cert_file , private_key_file, ca_cert_file  };
    let (_broker, handle) = Actor::spawn(Some(BROKER_NAME.to_string()), Broker, args)
        .await
        .expect("Failed to start Broker");
    
    handle.await.expect("Something went wrong");

}
