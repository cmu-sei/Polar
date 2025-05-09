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

use cassini::TCPClientConfig;
use gitlab_observer::*;
use polar::init_logging;
use ractor::Actor;
use tracing::error;
use std::{env, thread};
use std::{error::Error, time::Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_logging();

    let client_config = TCPClientConfig::new();

    let gitlab_endpoint = env::var("GITLAB_ENDPOINT").expect("Expected to find a value for GITLAB_ENDPOINT. Please provide a valid endpoint to a gitlab graphql endpoint.");
    let gitlab_token = env::var("GITLAB_TOKEN").expect("Expected to find a value for GITLAB_TOKEN.");
    // Helpful for looking at services behind a proxy
    let proxy_ca_cert_file = match env::var("PROXY_CA_CERT") { Ok(path) => Some(path), Err(_) => None };


    let args = supervisor::ObserverSupervisorArgs {
        client_config,
        gitlab_endpoint,
        gitlab_token: Some(gitlab_token),
        proxy_ca_cert_file
    };

    match Actor::spawn(
        Some("gitlab.supervisor.observer".to_string()),
        supervisor::ObserverSupervisor,
        args,
    )
    .await {
        Ok((_, handle)) => {
            let _ = handle.await;
        },
        Err(e) => error!("{e}")
    }

    Ok(())
}
