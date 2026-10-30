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
//! (docs/architecture/policy-enforcement.md, stage 0). Refusals are tested where the broker refuses (stage 5b): today it
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

/// A refused publish is named by its number, so the client's outbox lets go of that message and of no other.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_publish_is_named_by_its_number() {
    let _serial = support::serial().await;
    // No policy, and a certificate that declares nothing: every publish is refused.
    let broker = support::TestBroker::start_enforcing().await;
    let mut raw = support::RawConnection::open(&broker, "stranger.test", None).await;
    let session = raw.register().await;
    raw.send(&cassini_types::ClientMessage::PublishRequest {
        topic: "news".to_string(),
        payload: std::sync::Arc::new(b"x".to_vec()),
        registration_id: session,
        trace_ctx: None,
        share_identity: true,
        epoch: 41,
        seq: 7,
    })
    .await;
    let origin = loop {
        match raw.read().await.expect("the broker answers") {
            cassini_types::ClientMessage::PublishResponse {
                result: Err(_),
                origin,
                ..
            } => break origin,
            _ => continue,
        }
    };
    broker.stop().await;
    assert_eq!(origin, Some(cassini_types::MessageId { epoch: 41, seq: 7 }));
}

/// Each publish is acknowledged by its own number (docs/architecture/cassini-client-reliability.md, build step 2): the
/// transport numbers what it is given 1, 2, 3 in its epoch, and every number comes back once.
#[tokio::test(flavor = "multi_thread")]
async fn each_publish_is_acknowledged_by_its_own_number() {
    with_broker(|broker| async move {
        let client = TestClient::connect(&broker, "publisher.test").await;
        for n in 0..5u8 {
            client.publish(if n % 2 == 0 { "news" } else { "weather" }, &[n]);
        }
        client
            .wait_for("five acknowledgements", Duration::from_secs(5), |c| {
                c.acknowledged_ids().len() == 5
            })
            .await;
        let ids = client.acknowledged_ids();
        let mut seqs: Vec<u64> = ids.iter().map(|id| id.seq).collect();
        seqs.sort();
        assert_eq!(seqs, [1, 2, 3, 4, 5], "each once");
        assert!(ids.iter().all(|id| id.epoch == ids[0].epoch), "{ids:?}");
    })
    .await;
}
