#![allow(clippy::incompatible_msrv)]
use cassini_broker::{
    broker::{Broker, BrokerArgs},
    BROKER_NAME,
};
use ractor::Actor;
use tracing::error;
// ============================== Main ============================== //

#[tokio::main]
async fn main() {
    cassini_broker::init_logging();

    //introspect invironment to generate args
    match BrokerArgs::new() {
        Ok(args) => {
            // Start Supervisor
            let (_broker, handle) = Actor::spawn(Some(BROKER_NAME.to_string()), Broker, args)
                .await
                .expect("Failed to start Broker");

            handle.await.expect("Something went wrong");
        }
        Err(e) => {
            error!("Failed to load arguments. {e}");
            std::process::exit(1);
        }
    }
}
