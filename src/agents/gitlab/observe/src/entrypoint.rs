/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

use cassini_client::TCPClientConfig;
use gitlab_observer::*;
use polar::init_logging;
use ractor::Actor;
use std::env;
use std::error::Error;
use tracing::error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_logging();

    let client_config = TCPClientConfig::new();

    // Should be a regular base URL for a gitlab instance.
    // i.e. "https://example.gitlab.com" - no trailing "/"
    let gitlab_endpoint = url::Url::parse(
        env::var("GITLAB_ENDPOINT")
            .expect("Expected to find a value for GITLAB_ENDPOINT. Please provide a valid URL.")
            .as_str(),
    )
    .expect("Expected a valid URL.");

    let gitlab_token =
        env::var("GITLAB_TOKEN").expect("Expected to find a value for GITLAB_TOKEN.");
    // Helpful for looking at services behind a proxy
    let proxy_ca_cert_file = match env::var("PROXY_CA_CERT") {
        Ok(path) => Some(path),
        Err(_) => None,
    };
    let base_interval: u64 = env::var("OBSERVER_BASE_INTERVAL")
        .expect("Expected to read a value for OBSERVER_BASE_INTERVAL")
        .parse()
        .unwrap_or(300); // 5 minute default

    let args = supervisor::ObserverSupervisorArgs {
        client_config,
        gitlab_endpoint: gitlab_endpoint.to_string(),
        gitlab_token: Some(gitlab_token),
        proxy_ca_cert_file,
        // TODO: read these from configuration
        base_interval,
        max_backoff_secs: 6000,
    };

    match Actor::spawn(
        Some("gitlab.supervisor.observer".to_string()),
        supervisor::ObserverSupervisor,
        args,
    )
    .await
    {
        Ok((_, handle)) => {
            let _ = handle.await;
        }
        Err(e) => error!("{e}"),
    }

    Ok(())
}
