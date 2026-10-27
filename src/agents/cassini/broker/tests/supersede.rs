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

//! A client that comes back as a new session, rather than resuming its old one, takes over what
//! the broker held for its identity: but only on topics it subscribes to again, only from its own
//! identity, and only for as long as the broker keeps such things.
mod support;
use std::time::Duration;
use support::{TestClient, pause, with_broker, with_broker_retaining};

#[tokio::test(flavor = "multi_thread")]
async fn a_new_session_inherits_what_was_held_on_a_topic_it_subscribes_to() {
    with_broker(|broker| async move {
        let before = TestClient::connect(&broker, "agent.test").await;
        before.subscribe("news");
        pause().await;
        before.vanish().await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"while you were gone");
        pause().await;

        // The agent restarted and knows nothing of its old session.
        let after = TestClient::connect(&broker, "agent.test").await;
        after.subscribe("news");
        assert_eq!(
            after.wait_for_messages("news", 1).await,
            [b"while you were gone".to_vec()]
        );
        pause().await;
        assert_eq!(after.received("news").len(), 1, "once");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn topics_the_new_session_does_not_subscribe_to_are_not_delivered() {
    with_broker(|broker| async move {
        let before = TestClient::connect(&broker, "agent.test").await;
        before.subscribe("news");
        before.subscribe("weather");
        pause().await;
        before.vanish().await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"headline");
        publisher.publish("weather", b"rain");
        pause().await;

        let after = TestClient::connect(&broker, "agent.test").await;
        after.subscribe("news");
        assert_eq!(
            after.wait_for_messages("news", 1).await,
            [b"headline".to_vec()]
        );
        pause().await;
        assert!(
            after.received("weather").is_empty(),
            "it never asked for that topic"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_old_session_cannot_be_resumed_once_a_new_one_has_taken_over() {
    with_broker(|broker| async move {
        let before = TestClient::connect(&broker, "agent.test").await;
        let old = before.registration_id().unwrap();
        before.subscribe("news");
        pause().await;
        before.vanish().await;

        let _after = TestClient::connect(&broker, "agent.test").await;
        pause().await;
        let late = TestClient::connect_resuming(&broker, "agent.test", Some(old.clone())).await;
        assert_ne!(
            late.registration_id().unwrap(),
            old,
            "two sessions must not both claim what was held"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn another_identity_does_not_take_over_and_the_owner_can_still_resume() {
    with_broker(|broker| async move {
        let owner = TestClient::connect(&broker, "agent.test").await;
        let session = owner.registration_id().unwrap();
        owner.subscribe("news");
        pause().await;
        owner.vanish().await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"for the owner");
        pause().await;

        let stranger = TestClient::connect(&broker, "stranger.test").await;
        stranger.subscribe("news");
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(stranger.received("news").is_empty(), "not its to take");

        let back = TestClient::connect_resuming(&broker, "agent.test", Some(session.clone())).await;
        assert_eq!(back.registration_id().unwrap(), session, "still resumable");
        assert_eq!(
            back.wait_for_messages("news", 1).await,
            [b"for the owner".to_vec()]
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn what_was_held_expires_a_retention_window_after_it_was_taken_over() {
    with_broker_retaining(Some(2), |broker| async move {
        let before = TestClient::connect(&broker, "agent.test").await;
        before.subscribe("news");
        pause().await;
        before.vanish().await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"too late");

        let after = TestClient::connect(&broker, "agent.test").await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        after.subscribe("news");
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            after.received("news").is_empty(),
            "the window ran from the takeover, and has passed"
        );
    })
    .await;
}

// -- an older session that still looks connected -------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_new_session_also_takes_over_an_older_one_that_still_looks_connected() {
    with_broker(|broker| async move {
        // The old instance's connection is dead, but the broker has not noticed.
        let old = TestClient::connect(&broker, "agent.test").await;
        let old_session = old.registration_id().unwrap();
        old.subscribe("news");
        pause().await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"seen by the old instance");
        old.wait_for_messages("news", 1).await;

        // Its replacement registers, as a new session.
        let new = TestClient::connect(&broker, "agent.test").await;
        pause().await;
        publisher.publish("news", b"for whoever is there now");
        pause().await;
        new.subscribe("news");
        assert_eq!(
            new.wait_for_messages("news", 1).await,
            [b"for whoever is there now".to_vec()],
            "what the old session would have been sent, and not what it already had"
        );

        // Two instances must not both go on being served as the same identity.
        pause().await;
        assert_eq!(
            old.received("news").len(),
            1,
            "nothing more for the old one"
        );
        old.wait_for("its connection to be closed", Duration::from_secs(5), |c| {
            c.lost_its_connection()
        })
        .await;
        let late =
            TestClient::connect_resuming(&broker, "agent.test", Some(old_session.clone())).await;
        assert_ne!(
            late.registration_id().unwrap(),
            old_session,
            "it cannot come back"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_instance_that_lost_its_session_does_not_take_it_back_by_force() {
    with_broker(|broker| async move {
        // Both instances are alive: the old one reconnects by itself when the broker closes its
        // connection, asking for the session it had.
        let old = TestClient::connect(&broker, "agent.test").await;
        old.subscribe("news");
        pause().await;
        let new = TestClient::connect(&broker, "agent.test").await;
        // Long enough for the old one to come back several times.
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(
            !new.lost_its_connection(),
            "the old instance coming back must not close the new one's connection"
        );
        new.subscribe("news");
        pause().await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"for the new instance");
        assert_eq!(
            new.wait_for_messages("news", 1).await,
            [b"for the new instance".to_vec()],
            "the new instance is still being served"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_superseded_session_with_no_subscriptions_has_its_connection_closed_too() {
    with_broker(|broker| async move {
        // Held at once, having nothing to hold: it used to end before closing its connection, about one time in
        // five, leaving the old instance connected with no session. Done a few times over to meet the race.
        for round in 0..6 {
            let identity = format!("agent-{round}.test");
            let old = TestClient::connect(&broker, &identity).await;
            let _new = TestClient::connect(&broker, &identity).await;
            old.wait_for("its connection to be closed", Duration::from_secs(5), |c| {
                c.lost_its_connection()
            })
            .await;
        }
    })
    .await;
}
