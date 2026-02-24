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

use jira_observer::*;
use polar::init_logging;
use ractor::Actor;
use std::env;
use std::error::Error;
use tracing::error;

const JIRA_OBSERVER_SUPERVISOR: &str = "jira.supervisor.observer";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_logging(JIRA_OBSERVER_SUPERVISOR.to_string());

    let jira_url = env::var("JIRA_URL").expect("Expected to find a value for JIRA_URL. Please provide a valid url to a jira server, ex 'http://hostname/jira'.");
    let jira_token = env::var("JIRA_TOKEN").expect("Expected to find a value for JIRA_TOKEN.");
    // Helpful for looking at services behind a proxy
    let proxy_ca_cert_file = match env::var("PROXY_CA_CERT") {
        Ok(path) => Some(path),
        Err(_) => None,
    };

    let args = supervisor::ObserverSupervisorArgs {
        jira_url,
        jira_token: Some(jira_token),
        proxy_ca_cert_file,
        // TODO: read these from configuration
        base_interval: 300,
        max_backoff_secs: 6000,
    };

    match Actor::spawn(
        Some(JIRA_OBSERVER_SUPERVISOR.to_string()),
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
