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

//! Many messages, fast: every one is delivered, and every one is acknowledged to its publisher. The load harness
//! (docs/architecture/policy-enforcement.md, stage 5a) saw both fail in the cluster: the last messages of a stream never
//! arrived, and most acknowledgements never came back.

mod support;

use std::time::Duration;
use support::*;

const MESSAGES: usize = 5000;

async fn every_message_is_delivered_and_acknowledged(publishers: usize) {
    with_broker(|broker| async move {
        let mut pairs = Vec::new();
        for i in 0..publishers {
            let topic = format!("load-{i}");
            let subscriber = TestClient::connect(&broker, &format!("subscriber-{i}.test")).await;
            subscriber.subscribe(&topic);
            subscriber
                .wait_for("the subscription", Duration::from_secs(10), |c| c.subscribed(&topic))
                .await;
            let publisher = TestClient::connect(&broker, &format!("publisher-{i}.test")).await;
            pairs.push((topic, subscriber, publisher));
        }
        for n in 0..MESSAGES {
            for (topic, _, publisher) in &pairs {
                publisher.publish(topic, &(n as u64).to_be_bytes());
            }
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while tokio::time::Instant::now() < deadline
            && !pairs.iter().all(|(topic, subscriber, publisher)| {
                subscriber.received(topic).len() >= MESSAGES && publisher.acknowledged(topic) >= MESSAGES
            })
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        for (topic, subscriber, publisher) in &pairs {
            let got: Vec<u64> = subscriber
                .received(topic)
                .iter()
                .map(|p| u64::from_be_bytes(p.as_slice().try_into().unwrap()))
                .collect();
            let acknowledged = publisher.acknowledged(topic);
            let in_order = got.windows(2).all(|w| w[0] < w[1]);
            assert!(
                got.len() == MESSAGES && in_order && acknowledged == MESSAGES,
                "{topic}: {} of {MESSAGES} delivered (in order: {in_order}; first missing: {:?}), {acknowledged} acknowledged",
                got.len(),
                (0..MESSAGES as u64).find(|n| !got.contains(n)),
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn one_publisher_s_every_message_is_delivered_and_acknowledged() {
    every_message_is_delivered_and_acknowledged(1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn four_publishers_every_message_is_delivered_and_acknowledged() {
    every_message_is_delivered_and_acknowledged(4).await;
}
