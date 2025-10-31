// control_plane/src/main.rs
// use serde::{Deserialize, Serialize};
// use std::collections::HashMap;
// use tokio::process::Command;
// use tokio::time::{sleep, Duration};
// use reqwest::Client;

// #[derive(Serialize, Deserialize, Debug)]
// struct ScenarioCommand {
//     scenario: String,
//     params: HashMap<String, String>,
// }

// control_plane/src/main.rs
use harness_controller::service::*;
use ractor::Actor;
use std::env;
use std::process::Stdio;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() {
    harness_controller::init_logging();


    // Channel for internal orchestration / logging (optional)
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<String>();

    // --- Step 1: Start Broker ---
    // println!("Launching Broker...");
    // let mut broker = Command::new("./target/debug/cassini-server")
    //     .stdout(Stdio::inherit())
    //     .stderr(Stdio::inherit())
    //     .spawn()
    //     .expect("Failed to launch broker");

    // Optionally wait a little for broker to initialize
    // TODO: healthcheck cassini instead..whenever we implement one
    // sleep(Duration::from_millis(500)).await;

    // --- Step 2: Start C and C server ---
    let server_cert_file = env::var("TLS_SERVER_CERT_CHAIN")
        .expect("Expected a value for the TLS_SERVER_CERT_CHAIN environment variable.");
    let private_key_file = env::var("TLS_SERVER_KEY")
        .expect("Expected a value for the TLS_SERVER_KEY environment variable.");
    let ca_cert_file = env::var("TLS_CA_CERT")
        .expect("Expected a value for the TLS_CA_CERT environment variable.");
    let bind_addr = env::var("CONTROLLER_BIND_ADDR").unwrap_or(String::from("0.0.0.0:3000"));

    let args = harness_controller::service::HarnessControllerArgs {
        bind_addr,
        server_cert_file,
        private_key_file,
        ca_cert_file,
    };

    let (_controller, handle) = Actor::spawn(
        Some("HARNESS_CONTROLLER".to_string()),
        HarnessController,
        args,
    )
    .await
    .unwrap();

    handle.await.ok();
    // println!("Launching Producer harness...");
    // let mut producer = Command::new("./target/debug/harness-producer")
    //     .arg("--control-port")
    //     .arg("9000") // example port
    //     .stdout(Stdio::inherit())
    //     .stderr(Stdio::inherit())
    //     .spawn()
    //     .expect("Failed to launch producer harness");

    // println!("Launching Sink harness...");
    // let mut sink = Command::new("./target/debug/harness-sink")
    //     .arg("--control-port")
    //     .arg("9001") // example port
    //     .stdout(Stdio::inherit())
    //     .stderr(Stdio::inherit())
    //     .spawn()
    //     .expect("Failed to launch sink harness");

    // --- Step 3: Wait for Harness Registration ---
    // For now, just a placeholder
    // println!("Waiting for harnesses to register...");
    // sleep(Duration::from_secs(1)).await;

    // println!("All components started. Control plane ready.");

    // // --- Step 4: Event loop (placeholder) ---
    // while let Some(event) = event_rx.recv().await {
    //     println!("Control Plane Event: {}", event);
    // }

    // --- Step 5: Shutdown ---
    // println!("Shutting down harnesses and broker...");
    // let _ = producer.kill().await;
    // let _ = sink.kill().await;
    // let _ = broker.kill().await;

    println!("Control Plane: Shutdown complete.");
}

// #[tokio::main]
// async fn main() -> anyhow::Result<()> {
//     // TODO: start a cassini client actor

//     // --- Step 1: Launch binaries ---
//     // TODO: should we *Ever* get to the point of running these as containers, we're gonna have to talk about configuring the environment variables...
//     //
//     let mut producer = Command::new("./target/debug/cassini-server")
//         .spawn()?;

//     let mut producer = Command::new("./target/debug/harness-producer")
//         .spawn()?;
//     let mut sink = Command::new("./target/debug/harness-sink")
//         .spawn()?;

//     // --- Step 2: Wait for readiness ---
//     wait_for_ready(&client, 9000).await?;
//     wait_for_ready(&client, 9001).await?;

//     // --- Step 3: Run a simple "register then exit" scenario ---
//     let cmd = ScenarioCommand {
//         scenario: "register_then_exit".into(),
//         params: [("client_id".into(), "test-client-01".into())]
//             .into_iter()
//             .collect(),
//     };

//     client
//         .post("http://127.0.0.1:9000/control")
//         .json(&cmd)
//         .send()
//         .await?;

//     // Wait a bit to let the scenario run
//     sleep(Duration::from_secs(2)).await;

//     println!("Scenario complete, shutting down harnesses");

//     // --- Step 4: Shut down everything ---
//     for port in [9000, 9001] {
//         let _ = client
//             .post(format!("http://127.0.0.1:{port}/control"))
//             .json(&serde_json::json!({ "command": "shutdown" }))
//             .send()
//             .await;
//     }

//     Ok(())
// }

// // Wait until a harness responds to ping
// async fn wait_for_ready(client: &Client, port: u16) -> anyhow::Result<()> {
//     loop {
//         if let Ok(resp) = client
//             .post(format!("http://127.0.0.1:{port}/control"))
//             .json(&serde_json::json!({ "command": "ping" }))
//             .send()
//             .await
//         {
//             if resp.status().is_success() {
//                 break;
//             }
//         }
//         sleep(Duration::from_millis(100)).await;
//     }
//     Ok(())
// }
