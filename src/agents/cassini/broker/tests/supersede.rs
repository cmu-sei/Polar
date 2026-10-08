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
