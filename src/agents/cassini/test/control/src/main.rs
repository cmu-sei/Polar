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

use cassini_client::{init_tracing, shutdown_tracing};
use clap::Parser;
use harness_controller::service::*;
use harness_controller::{Arguments, read_test_config};
use ractor::Actor;
use std::env;
use tracing::info;

#[tokio::main]
async fn main() {
    init_tracing("cassini-controller");

    // dump_client_message_layout();
    let args = Arguments::parse();
    let test_plan = read_test_config(&args.config);

    // For now, just print out the config in JSON
    tracing::info!(
        "Using configuration:\n{}",
        serde_json::to_string_pretty(&test_plan).unwrap()
    );

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
        test_plan,
        broker_path: args.broker_path,
        producer_path: args.producer_path,
        sink_path: args.sink_path,
    };

    let (_controller, handle) = Actor::spawn(
        Some("HARNESS_CONTROLLER".to_string()),
        HarnessController,
        args,
    )
    .await
    .unwrap();

    handle.await.ok();

    info!("Control Plane: Shutdown complete.");

    shutdown_tracing();
    std::process::exit(0);
}
