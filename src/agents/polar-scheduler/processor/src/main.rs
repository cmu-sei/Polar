use ractor::Actor;
use polar_scheduler::supervisor::{RootSupervisor, SERVICE_NAME};

#[tokio::main]
async fn main() {
    polar::init_logging(SERVICE_NAME.to_string());

    let (_supervisor, handle) = Actor::spawn(
        Some(format!("{}.supervisor", SERVICE_NAME)),
        RootSupervisor,
        (),
    )
    .await
    .expect("Failed to spawn RootSupervisor");

    handle.await.unwrap();
}
