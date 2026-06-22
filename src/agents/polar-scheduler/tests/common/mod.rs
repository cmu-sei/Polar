use std::process::{Child, Command};
use std::time::Duration;
use tokio::time::sleep;
use tracing::info;

/// Spawn a binary and return the child process.
pub fn spawn_binary(bin_path: &str, args: &[&str]) -> Child {
    info!("Spawning {} with args {:?}", bin_path, args);
    Command::new(bin_path)
        .args(args)
        .spawn()
        .expect("failed to spawn binary")
}

/// Wait for a TCP port to be reachable.
pub async fn wait_for_port(host: &str, port: u16, timeout: Duration) {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if tokio::net::TcpStream::connect(format!("{}:{}", host, port)).await.is_ok() {
            return;
        }
        sleep(Duration::from_millis(100)).await;
    }
    panic!("Timeout waiting for {}:{}", host, port);
}

/// Wait for an HTTP endpoint to return 200.
pub async fn wait_for_http(url: &str, timeout: Duration) {
    let client = reqwest::Client::new();
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if client.get(url).send().await.map(|r| r.status().is_success()).unwrap_or(false) {
            return;
        }
        sleep(Duration::from_millis(100)).await;
    }
    panic!("Timeout waiting for HTTP {}", url);
}
