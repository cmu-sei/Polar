use openapi_observer::actors::{ObserverSupervisor, ObserverSupervisorArgs};
use ractor::Actor;
use std::{env, error::Error};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    polar::init_logging("polar.openapi.supervisor".to_string());

    let args = ObserverSupervisorArgs {
        openapi_endpoint: env::var("OPENAPI_ENDPOINT").unwrap(),
    };

    let (_supervisor, handle) = Actor::spawn(
        Some("polar.openapi.supervisor".to_string()),
        ObserverSupervisor,
        args,
    )
    .await
    .expect("Expected to start observer agent");
    let _ = handle.await;

    Ok(())
}
