use credentials_agent_observer::supervisor::{ObserverSupervisor, SERVICE_NAME};
use ractor::Actor;

#[tokio::main]
async fn main() {
    polar::init_logging(SERVICE_NAME.to_string());

    let (_supervisor, handle) = Actor::spawn(
        Some(format!("{SERVICE_NAME}.supervisor")),
        ObserverSupervisor,
        (),
    )
    .await
    .expect("Expected observer supervisor to start");
    handle.await.expect("supervisor task panicked");

    // The supervisor is meant to live as long as the pod. This agent runs as
    // a Job, so a clean exit would count as completion and nothing would
    // restart it. Exiting non-zero makes the Job start a fresh pod, which
    // reruns polar-init and rebuilds every client from fresh certificates.
    tracing::error!("supervisor stopped; exiting non-zero so the Job restarts the pod");
    std::process::exit(1);
}
