use cassini_client::{TCPClientConfig, TcpClientActor, TcpClientArgs, TcpClientMessage};
use cassini_types::ClientEvent;
use neo4rs::Graph;
use polar::SupervisorMessage;
use polar_scheduler_common::GitScheduleChange;
use ractor::{Actor, ActorRef, OutputPort};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, info};

// Helper to create a Cassini client actor.
async fn create_cassini_client(
    myself: ActorRef<SupervisorMessage>,
) -> Result<ActorRef<TcpClientMessage>, Box<dyn std::error::Error>> {
    let events_output = std::sync::Arc::new(OutputPort::<SupervisorMessage>::default());
    events_output.subscribe(myself.clone(), |event| {
        Some(SupervisorMessage::ClientEvent { event })
    });

    let config = TCPClientConfig::new()?;
    let (tcp_client, _handle) = Actor::spawn(
        Some("test-client".to_string()),
        TcpClientActor,
        TcpClientArgs {
            config,
            registration_id: None,
            events_output: Some(events_output),
            event_handler: None,
        },
    )
    .await?;

    Ok(tcp_client)
}

#[tokio::test]
async fn test_schedule_propagation() {
    // ===== 1. Wait for stack to be ready =====
    // Assumes dev_stack.sh up has been run.
    let broker_addr = std::env::var("BROKER_ADDR").unwrap_or("127.0.0.1:8080".to_string());
    wait_for_mtls(&broker_addr).await;

    let neo4j_endpoint = std::env::var("GRAPH_ENDPOINT").unwrap_or("bolt://localhost:7687".to_string());
    let neo4j_user = std::env::var("GRAPH_USER").unwrap_or("neo4j".to_string());
    let neo4j_pass = std::env::var("GRAPH_PASSWORD").unwrap_or("somepassword".to_string());

    let graph = Graph::connect(
        neo4rs::ConfigBuilder::new()
            .uri(neo4j_endpoint)
            .user(neo4j_user)
            .password(neo4j_pass)
            .build()
            .unwrap(),
    )
    .await
    .unwrap();

    // ===== 2. Create a test actor to receive client events (for subscriptions) =====
    let (test_actor, mut test_actor_handle) = Actor::spawn(
        Some("test-actor".to_string()),
        TestActor,
        (),
    )
    .await
    .unwrap();

    // ===== 3. Create Cassini client and subscribe to scheduler.in =====
    let tcp_client = create_cassini_client(test_actor.clone()).await.unwrap();
    tcp_client
        .cast(TcpClientMessage::Subscribe {
            topic: "scheduler.in".to_string(),
            trace_ctx: None,
        })
        .unwrap();

    // ===== 4. Publish a test schedule change =====
    let dhall_content = r#"
        { agentId = "test-permanent"
        , schedule = < Periodic = { interval = 5, unit = <minutes>.minutes } >
        , config = { dummy = "value" }
        , metadata = Some { description = "Test", owner = "test", version = Some 1 }
        }
    "#;
    let change = GitScheduleChange::Create {
        path: "test.dhall".to_string(),
        content: dhall_content.as_bytes().to_vec(),
    };
    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&change).unwrap().to_vec();
    tcp_client
        .cast(TcpClientMessage::Publish {
            topic: "scheduler.in".to_string(),
            payload,
            trace_ctx: None,
        })
        .unwrap();

    // ===== 5. Wait for the graph node to appear =====
    let mut found = false;
    for _ in 0..30 {
        let mut result = graph
            .execute(
                neo4rs::query("MATCH (s:Schedule:Permanent {agent_id: 'test-permanent'}) RETURN s"),
            )
            .await
            .unwrap();
        if result.next().await.unwrap().is_some() {
            found = true;
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }
    assert!(found, "Schedule node not created in graph");

    // ===== 6. Optionally, verify that a notification was published to the agent topic =====
    // This would require subscribing to the agent topic and checking a message arrived.
    // For simplicity, we skip it here, but you can extend.

    info!("Integration test passed!");
}

async fn wait_for_mtls(addr: &str) {
    let host = addr.split(':').next().unwrap();
    let port = addr.split(':').nth(1).unwrap().parse::<u16>().unwrap();
    let ca = std::env::var("TLS_CA_CERT").expect("TLS_CA_CERT not set");
    let client_cert = std::env::var("TLS_CLIENT_CERT").expect("TLS_CLIENT_CERT not set");
    let client_key = std::env::var("TLS_CLIENT_KEY").expect("TLS_CLIENT_KEY not set");
    let sni = std::env::var("CASSINI_SERVER_NAME").unwrap_or("localhost".to_string());

    for _ in 0..30 {
        if std::process::Command::new("openssl")
            .args(&[
                "s_client",
                "-connect", &format!("{}:{}", host, port),
                "-servername", &sni,
                "-CAfile", &ca,
                "-cert", &client_cert,
                "-key", &client_key,
                "-verify_return_error",
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return;
        }
        sleep(Duration::from_millis(500)).await;
    }
    panic!("Broker not ready after 15s");
}

// Minimal test actor that does nothing (just needed for client creation)
struct TestActor;

#[ractor::async_trait]
impl Actor for TestActor {
    type Msg = SupervisorMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _: ActorRef<Self::Msg>,
        _: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        _: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
}
