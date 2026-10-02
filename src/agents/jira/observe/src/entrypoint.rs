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

const JIRA_OBSERVER_SUPERVISOR: &str = "jira.supervisor.observer";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_logging(JIRA_OBSERVER_SUPERVISOR.to_string());
    let jira_url = env::var("JIRA_URL").map_err(|_| {
        "JIRA_URL is not set; provide the url of the jira server, ex 'https://example.atlassian.net'"
    })?;
    let jira_token = env::var("JIRA_TOKEN").map_err(|_| "JIRA_TOKEN is not set")?;
    let auth = JiraAuth::from_env(jira_token);
    let deployment = JiraDeployment::from_env(&jira_url);
    let proxy_ca_cert_file = env::var("PROXY_CA_CERT").ok();
    let args = supervisor::ObserverSupervisorArgs {
        jira_url,
        auth,
        deployment,
        proxy_ca_cert_file,
        // TODO: read these from configuration
        base_interval: 1800,
        max_backoff_secs: 6000,
    };
    let (_, handle) = Actor::spawn(
        Some(JIRA_OBSERVER_SUPERVISOR.to_string()),
        supervisor::ObserverSupervisor,
        args,
    )
    .await
    .map_err(|e| format!("failed to start {JIRA_OBSERVER_SUPERVISOR}: {e}"))?;

    handle.await?;

    // The supervisor stopping is never a success: this agent runs as a Job
    // and only a non-zero exit gets it restarted.
    Err(format!("{JIRA_OBSERVER_SUPERVISOR} stopped; exiting non-zero so the Job restarts it").into())
}
