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
            if let Ok((_broker, handle)) =
                Actor::spawn(Some(BROKER_NAME.to_string()), Broker, args).await
            {
                handle.await.ok();
            } else {
                std::process::exit(1);
            }
        }
        Err(e) => {
            error!("Failed to load arguments. {e}");
            std::process::exit(1);
        }
    }
}
