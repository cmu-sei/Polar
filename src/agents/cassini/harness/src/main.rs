use cassini_harness::actors::*;
use ractor::Actor;

#[tokio::main]
async fn main() {
    polar::init_logging();

    // let args = Args::parse();

    const PRODUCER_ACTOR: &str = "cassini.harness.producer";
    const SINK_ACTOR: &str = "cassini.harness.sink";

    let p_config = AgentConfig {
        topic: "testout".to_string(),
        msg_size: 1024usize,
        role: Role::Producer,
        rate: 10u32,
        duration: 30u64,
    };

    let s_config = AgentConfig {
        topic: "testout".to_string(),
        msg_size: 0,
        role: Role::Consumer,
        rate: 0,
        duration: 0,
    };

    // // For now, just print out the config in JSON
    tracing::info!(
        "Using configuration:\n{}",
        serde_json::to_string_pretty(&s_config).unwrap()
    );

    let (root, handle) = Actor::spawn(None, RootActor, ())
        .await
        .expect("Expected harness supervisor to start.");

    let _ = Actor::spawn_linked(
        Some(PRODUCER_ACTOR.to_string()),
        HarnessAgent,
        p_config,
        root.clone().into(),
    )
    .await
    .expect("expected to start the producer and connect");

    let _ = Actor::spawn_linked(None, HarnessAgent, s_config.clone(), root.clone().into())
        .await
        .expect("expected to start the producer and connect");

    let _ = handle.await;
}
