#![allow(clippy::incompatible_msrv)]
use cassini_broker::{
    BROKER_NAME,
    broker::{Broker, BrokerArgs},
};
use cassini_tracing::{init_tracing, shutdown_tracing};
use ractor::Actor;
use tracing::error;

// ============================== Exit Codes ============================== //
// 1 — unexpected actor/worker failure
// 2 — graceful rejuvenation shutdown (normal cert rotation cycle)
// 3 — startup failure (bad args, crypto provider, spawn failure)

#[tokio::main]
async fn main() {
    // Install aws-lc-rs as the default Rustls crypto provider.
    // Required when multiple providers are present in the dependency tree.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    init_tracing("cassini-broker");

    let exit_code = match BrokerArgs::new() {
        Ok(args) => {
            match Actor::spawn(Some(BROKER_NAME.to_string()), Broker, args).await {
                Ok((_broker, handle)) => {
                    handle.await.ok();
                    // Broker exited cleanly — graceful rejuvenation shutdown.
                    2
                }
                Err(e) => {
                    error!("Failed to spawn broker actor: {e}");
                    3
                }
            }
        }
        Err(e) => {
            error!("Failed to load arguments: {e}");
            3
        }
    };

    shutdown_tracing();
    std::process::exit(exit_code);
}
