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

//! The client hears what the broker did with its subscriptions: a subscription taken is acknowledged
//! (docs/architecture/policy-enforcement.md, stage 0). Refusals are tested where the broker refuses (stage 5): today it
//! refuses only while shutting down, when it is already closing every connection.
mod support;
use std::time::Duration;
use support::{TestClient, with_broker};

#[tokio::test(flavor = "multi_thread")]
async fn a_subscription_the_broker_takes_is_acknowledged() {
    with_broker(|broker| async move {
        let client = TestClient::connect(&broker, "subscriber.test").await;
        client.subscribe("news");
        client
            .wait_for("the acknowledgment", Duration::from_secs(5), |c| {
                c.subscribed("news")
            })
            .await;
        assert!(client.refusals("news").is_empty());
    })
    .await;
}
