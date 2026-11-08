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

//! Acknowledged deliveries (docs/architecture/cassini-client-reliability.md, build step 5): the broker keeps each
//! delivery for its subscriber until the client acknowledges it, and sends what was not acknowledged again when the
//! session is resumed.
mod support;
use std::time::Duration;
use support::{RawConnection, TestClient, with_broker};

const WAIT: Duration = Duration::from_secs(5);

#[tokio::test(flavor = "multi_thread")]
async fn a_delivery_names_its_publisher_and_their_number_and_carries_its_own() {
    with_broker(|broker| async move {
        let mut sink = RawConnection::open(&broker, "sink.test", None).await;
        let session = sink.register().await;
        sink.subscribe(&session, "news").await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"headline");
        let delivery = sink.next_delivery("news", WAIT).await.expect("delivered");
        assert_eq!(delivery.payload, b"headline");
        assert_eq!(delivery.published_by.as_deref(), Some("publisher.test"));
        publisher
            .wait_for("the acknowledgement", WAIT, |c| {
                !c.acknowledged_ids().is_empty()
            })
            .await;
        assert_eq!(delivery.origin, Some(publisher.acknowledged_ids()[0]));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn what_the_client_did_not_acknowledge_is_sent_again_on_resume_and_what_it_did_is_not() {
    with_broker(|broker| async move {
        let mut sink = RawConnection::open(&broker, "sink.test", None).await;
        let session = sink.register().await;
        sink.subscribe(&session, "news").await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"first");
        publisher.publish("news", b"second");
        let first = sink.next_delivery("news", WAIT).await.expect("the first");
        let second = sink.next_delivery("news", WAIT).await.expect("the second");
        sink.acknowledge(second.delivery_id).await;
        // Let the acknowledgement reach the subscriber before the connection goes.
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(sink);

        let mut back = RawConnection::open(&broker, "sink.test", None).await;
        assert_eq!(back.register_resuming(Some(session.clone())).await, session);
        let again = back.next_delivery("news", WAIT).await.expect("sent again");
        assert_eq!(again, first, "the same delivery, with its number");
        back.acknowledge(again.delivery_id).await;
        assert_eq!(
            back.next_delivery("news", Duration::from_millis(500)).await,
            None,
            "the acknowledged one is not sent again"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(back);

        let mut third = RawConnection::open(&broker, "sink.test", None).await;
        third.register_resuming(Some(session)).await;
        assert_eq!(
            third
                .next_delivery("news", Duration::from_millis(500))
                .await,
            None,
            "nothing is owed once everything is acknowledged"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_client_acknowledges_each_delivery_it_hands_to_the_agent() {
    with_broker(|broker| async move {
        let sink = TestClient::connect(&broker, "sink.test").await;
        let session = sink.registration_id().unwrap();
        sink.subscribe("news");
        sink.wait_for("the subscription", WAIT, |c| c.subscribed("news"))
            .await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"once");
        sink.wait_for_messages("news", 1).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        sink.vanish().await;
        // Acknowledged on hand-off, so a resumed session is owed nothing.
        let mut back = RawConnection::open(&broker, "sink.test", None).await;
        back.register_resuming(Some(session)).await;
        assert_eq!(
            back.next_delivery("news", Duration::from_millis(500)).await,
            None
        );
    })
    .await;
}
