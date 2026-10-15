/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

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
