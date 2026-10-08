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
async fn a_message_published_while_the_subscriber_is_away_is_delivered_when_it_resumes() {
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

        let back = TestClient::connect_resuming(&broker, "away.test", Some(session)).await;
        assert_eq!(
            back.wait_for_messages("news", 1).await,
            [b"while you were out".to_vec()],
            "held for the session, and replayed to the connection that resumed it"
        );
        pause().await;
        assert_eq!(back.received("news").len(), 1, "once, not repeatedly");
    })
    .await;
}
