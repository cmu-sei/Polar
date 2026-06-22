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

use gitlab_observer::*;
use polar::init_logging;
use ractor::Actor;
use std::error::Error;
use tracing::error;

const GITLAB_OBSERVER_SUPERVISOR: &str = "gitlab.supervisor.observer";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    init_logging(GITLAB_OBSERVER_SUPERVISOR.to_string());

    match Actor::spawn(
        Some(GITLAB_OBSERVER_SUPERVISOR.to_string()),
        supervisor::ObserverSupervisor,
        (),
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
