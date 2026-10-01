use credentials_agent_consumer::supervisor::ConsumerSupervisor;
use ractor::Actor;

#[tokio::main]
async fn main() {
    polar::init_logging("credentials-agent-consumer".to_string());

    let (_agent, handle) = Actor::spawn(
        Some("credentials-agent-consumer.supervisor".to_string()),
        ConsumerSupervisor,
        (),
    )
    .await
    .expect("Expected agent to start");
    handle.await.expect("supervisor task panicked");

    // The supervisor is meant to live as long as the pod. This agent runs as
    // a Job, so a clean exit would count as completion and nothing would
    // restart it. Exiting non-zero makes the Job start a fresh pod, which
    // reruns polar-init and rebuilds every client from fresh certificates.
    tracing::error!("supervisor stopped; exiting non-zero so the Job restarts the pod");
    std::process::exit(1);
}
