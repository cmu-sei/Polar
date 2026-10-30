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

//! The frame limit (docs/architecture/cassini-backpressure.md, "Decided" 3): a frame header over `MAX_FRAME_BYTES`
//! ends the connection before anything is allocated for it, and a client never sends one.
mod support;
use cassini_types::{MAX_FRAME_BYTES, MAX_PAYLOAD_BYTES};
use std::time::Duration;
use support::{RawConnection, TestClient, with_broker};

#[tokio::test(flavor = "multi_thread")]
async fn a_frame_header_over_the_limit_closes_the_connection_before_its_body_is_read() {
    with_broker(|broker| async move {
        let mut raw = RawConnection::open(&broker, "oversized.test", None).await;
        // No body follows: a broker that took the length at its word would wait for it, with the frame allocated.
        raw.send_header(MAX_FRAME_BYTES as u32 + 1).await;
        assert!(
            raw.closed_within(Duration::from_secs(5)).await,
            "the broker closes a connection that announces a frame over the limit"
        );
        // The broker is unharmed.
        let client = TestClient::connect(&broker, "after.test").await;
        client.subscribe("news");
        client
            .wait_for("the subscription", Duration::from_secs(5), |c| {
                c.subscribed("news")
            })
            .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_refuses_to_send_a_publish_over_the_limit_and_keeps_its_connection() {
    with_broker(|broker| async move {
        let client = TestClient::connect(&broker, "publisher.test").await;
        let subscriber = TestClient::connect(&broker, "subscriber.test").await;
        subscriber.subscribe("news");
        subscriber
            .wait_for("the subscription", Duration::from_secs(5), |c| {
                c.subscribed("news")
            })
            .await;
        client.publish("news", &vec![0; MAX_FRAME_BYTES]);
        client
            .wait_for("the refusal", Duration::from_secs(10), |c| {
                !c.refusals("news").is_empty()
            })
            .await;
        assert!(!client.lost_its_connection(), "the connection is kept");
        // The largest payload a frame carries is delivered.
        client.publish("news", &vec![1; MAX_PAYLOAD_BYTES]);
        subscriber
            .wait_for("the largest payload", Duration::from_secs(20), |c| {
                c.received_count("news") == 1
            })
            .await;
        assert_eq!(subscriber.received("news")[0].len(), MAX_PAYLOAD_BYTES);
    })
    .await;
}
