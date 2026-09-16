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
    handle.await.unwrap();
}
