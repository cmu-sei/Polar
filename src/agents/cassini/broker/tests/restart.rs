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

//! A broker that goes away and comes back with nothing: the clients that stayed up have to find it
//! again, as new sessions, and be able to do their work.
mod support;
use std::time::Duration;
use support::{FacadeClient, TestClient, pause, with_broker};

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_stays_up_finds_a_restarted_broker_and_can_subscribe_again() {
    with_broker(|broker| async move {
        let agent = FacadeClient::connect(&broker, "agent.test").await;
        let first = agent.client.registration_id().unwrap();
        agent.client.subscribe("news");
        pause().await;

        // The broker is gone for a while, long enough for several attempts to fail.
        let _away = broker.restart(Duration::from_secs(3)).await;

        agent
            .client
            .wait_for(
                "a registration with the new broker",
                Duration::from_secs(30),
                |_| agent.registrations().len() >= 2,
            )
            .await;
        assert_eq!(
            agent.registrations().len(),
            2,
            "told once of each session, so an agent initialises once for each"
        );
        let second = agent.registrations()[1].clone();
        assert_ne!(
            second, first,
            "the new broker knows nothing of the old session"
        );

        // What the agent does on every registration: subscribe again.
        agent.client.subscribe("news");
        pause().await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"after the restart");
        assert_eq!(
            agent.client.wait_for_messages("news", 1).await,
            [b"after the restart".to_vec()]
        );
    })
    .await;
}

// -- what the front knows while the connection is lost -------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_connection_ends_the_transport_and_the_front_brings_a_new_one() {
    with_broker(|broker| async move {
        let agent = FacadeClient::connect(&broker, "agent.test").await;
        let before = agent.transport_id().expect("a transport");
        pause().await;

        broker.restart(Duration::from_secs(3)).await;
        agent
            .client
            .wait_for("a second registration", Duration::from_secs(40), |_| {
                agent.registrations().len() >= 2
            })
            .await;

        let after = agent.transport_id().expect("a transport");
        assert_ne!(
            before, after,
            "the front replaces the transport; the transport does not mend itself"
        );
        assert_eq!(
            agent.registrations().len(),
            2,
            "told once for each registration"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_front_knows_it_is_offline_while_the_broker_is_away() {
    with_broker(|broker| async move {
        let agent = FacadeClient::connect(&broker, "agent.test").await;
        assert!(agent.is_online());
        pause().await;

        let broker_for_restart = broker.clone();
        let away =
            tokio::spawn(async move { broker_for_restart.restart(Duration::from_secs(3)).await });

        agent
            .client
            .wait_for("the front to go offline", Duration::from_secs(5), |_| {
                !agent.is_online()
            })
            .await;
        away.await.unwrap();
        agent
            .client
            .wait_for(
                "the front to come back online",
                Duration::from_secs(40),
                |_| agent.is_online(),
            )
            .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_publish_made_while_the_broker_is_away_arrives_when_it_returns() {
    with_broker(|broker| async move {
        let publisher = FacadeClient::connect(&broker, "publisher.test").await;
        pause().await;

        let broker_for_restart = broker.clone();
        let away =
            tokio::spawn(async move { broker_for_restart.restart(Duration::from_secs(3)).await });
        publisher
            .client
            .wait_for("the front to go offline", Duration::from_secs(5), |_| {
                !publisher.is_online()
            })
            .await;
        publisher
            .client
            .publish("news", b"sent while the broker was away");
        away.await.unwrap();

        // A reader that arrives after the broker is back and asks for the topic.
        publisher
            .client
            .wait_for(
                "the front to come back online",
                Duration::from_secs(40),
                |_| publisher.is_online(),
            )
            .await;
        let reader = TestClient::connect(&broker, "reader.test").await;
        reader.subscribe("news");
        assert_eq!(
            reader.wait_for_messages("news", 1).await,
            [b"sent while the broker was away".to_vec()],
            "held by the front, then sent in order once it registered"
        );
    })
    .await;
}

// -- a publish sent the moment its client is registered ------------------------------------------
//
// A client publishes as soon as it is told it is registered, and a front sends everything it held at that moment.
// The broker binds the connection to its session at some point near the registration; these tests make that binding
// slow (`cassini_broker::testing::SessionBindingDelay`), so a publish sent at once always arrives in the window if
// there is one. Before the fix the Listener told the client first and bound after: the publish was dropped with only
// a log line, and the front's outbox kept it unanswered until a next registration that never came.

/// Long enough that a publish sent when its client hears it is registered arrives inside it, however loaded the machine.
const SLOW_BINDING: Duration = Duration::from_secs(2);

#[tokio::test(flavor = "multi_thread")]
async fn a_publish_sent_the_moment_its_client_is_registered_is_taken() {
    with_broker(|broker| async move {
        let reader = TestClient::connect(&broker, "reader.test").await;
        reader.subscribe("news");
        reader
            .wait_for("its subscription", Duration::from_secs(10), |c| {
                c.subscribed("news")
            })
            .await;

        let _slow = cassini_broker::testing::SessionBindingDelay::hold(SLOW_BINDING);
        let publisher = FacadeClient::connect(&broker, "publisher.test").await;
        publisher.client.publish("news", b"sent at once");
        assert_eq!(
            reader.wait_for_messages("news", 1).await,
            [b"sent at once".to_vec()],
            "taken by the broker, not dropped while the connection was being bound"
        );
        publisher
            .client
            .wait_for("its acknowledgement", Duration::from_secs(10), |c| {
                c.acknowledged("news") == 1
            })
            .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_publish_held_while_the_broker_was_away_is_taken_however_slowly_the_new_connection_is_bound()
 {
    with_broker(|broker| async move {
        let publisher = FacadeClient::connect(&broker, "publisher.test").await;
        pause().await;

        let broker_for_restart = broker.clone();
        let away =
            tokio::spawn(async move { broker_for_restart.restart(Duration::from_secs(3)).await });
        publisher
            .client
            .wait_for("the front to go offline", Duration::from_secs(5), |_| {
                !publisher.is_online()
            })
            .await;
        // Held by the front, and sent the moment it is registered with the broker that returns.
        let slow = cassini_broker::testing::SessionBindingDelay::hold(SLOW_BINDING);
        publisher
            .client
            .publish("news", b"sent while the broker was away");
        away.await.unwrap();
        publisher
            .client
            .wait_for(
                "the front to come back online",
                Duration::from_secs(40),
                |_| publisher.is_online(),
            )
            .await;
        publisher
            .client
            .wait_for("its acknowledgement", Duration::from_secs(10), |c| {
                c.acknowledged("news") == 1
            })
            .await;
        drop(slow);

        let reader = TestClient::connect(&broker, "reader.test").await;
        reader.subscribe("news");
        assert_eq!(
            reader.wait_for_messages("news", 1).await,
            [b"sent while the broker was away".to_vec()],
            "held by the front, sent once it registered, and taken by the broker"
        );
    })
    .await;
}

// -- what a broker restart loses ------------------------------------------------------------------
//
// The broker is best-effort (decided 2026-10-09; see remaining-work.md): what it holds is in
// memory. These tests state what that means, so the decision is visible and a change to it
// shows up as these tests changing. Revisit with a durable broker if a workload needs it.

#[tokio::test(flavor = "multi_thread")]
async fn a_message_queued_for_a_topic_with_no_subscriber_is_delivered_when_one_arrives() {
    // The control for the next test: without a restart the broker does keep it.
    with_broker(|broker| async move {
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("orphan", b"waiting for a reader");
        pause().await;

        let reader = TestClient::connect(&broker, "reader.test").await;
        reader.subscribe("orphan");
        assert_eq!(
            reader.wait_for_messages("orphan", 1).await,
            [b"waiting for a reader".to_vec()]
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_queued_for_a_topic_with_no_subscriber_is_lost_when_the_broker_restarts() {
    with_broker(|broker| async move {
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("orphan", b"waiting for a reader");
        pause().await;

        broker.restart(Duration::from_secs(1)).await;

        let reader = TestClient::connect(&broker, "reader.test").await;
        reader.subscribe("orphan");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(
            reader.received("orphan").is_empty(),
            "the queue was in the broker's memory"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn what_was_held_for_a_vanished_subscriber_is_lost_when_the_broker_restarts() {
    with_broker(|broker| async move {
        let away = TestClient::connect(&broker, "agent.test").await;
        away.subscribe("news");
        pause().await;
        away.vanish().await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"held for the agent");
        pause().await;

        broker.restart(Duration::from_secs(1)).await;

        // The agent comes back as a new session after the restart and subscribes again. Without
        // the restart it would inherit what was held (tests/supersede.rs).
        let back = TestClient::connect(&broker, "agent.test").await;
        back.subscribe("news");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(
            back.received("news").is_empty(),
            "the session, and what was held for it, went with the broker"
        );
    })
    .await;
}
