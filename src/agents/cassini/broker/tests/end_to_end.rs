//! The broker end to end: a real broker and real clients over mutual TLS.
mod support;
use support::{TestClient, pause, with_broker};

#[tokio::test(flavor = "multi_thread")]
async fn a_published_message_reaches_a_subscriber() {
    with_broker(|broker| async move {
        let subscriber = TestClient::connect(&broker, "subscriber.test").await;
        subscriber.subscribe("greetings");
        pause().await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("greetings", b"hello");

        assert_eq!(
            subscriber.wait_for_messages("greetings", 1).await,
            [b"hello".to_vec()]
        );
    })
    .await;
}

// What the broker does today when a client goes away. Written down before changing it, so the
// change shows up as these tests changing.

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_vanishes_keeps_its_session_and_can_resume_it() {
    with_broker(|broker| async move {
        let away = TestClient::connect(&broker, "away.test").await;
        let session = away.registration_id().unwrap();
        away.subscribe("news");
        pause().await;
        away.vanish().await;
        pause().await;

        let back = TestClient::connect_resuming(&broker, "away.test", Some(session.clone())).await;
        assert_eq!(
            back.registration_id().unwrap(),
            session,
            "the broker still had the session"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_published_while_the_subscriber_is_away_is_lost() {
    // Today: the broker tries to deliver to the session whose connection is gone, fails
    // ("Listener not found"), and the message is gone. It is not held for the session that
    // resumes, and it is not left on the topic for a new subscriber. Retention changes this.
    with_broker(|broker| async move {
        let away = TestClient::connect(&broker, "away.test").await;
        let session = away.registration_id().unwrap();
        away.subscribe("news");
        pause().await;
        away.vanish().await;
        pause().await;

        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"while you were out");
        pause().await;
        pause().await;

        let back = TestClient::connect_resuming(&broker, "away.test", Some(session)).await;
        let stranger = TestClient::connect(&broker, "stranger.test").await;
        stranger.subscribe("news");
        pause().await;
        pause().await;

        assert!(back.received("news").is_empty(), "not held for the session");
        assert!(
            stranger.received("news").is_empty(),
            "not left on the topic"
        );
    })
    .await;
}
