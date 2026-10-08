//! The broker end to end: a real broker and real clients over mutual TLS.
mod support;
use std::time::Duration;
use support::{FacadeClient, TestClient, pause, with_broker, with_broker_retaining};

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

// -- how long a disconnected session is kept ---------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_goodbye_ends_the_session_at_once() {
    with_broker(|broker| async move {
        let leaving = TestClient::connect(&broker, "leaving.test").await;
        let session = leaving.registration_id().unwrap();
        leaving.subscribe("news");
        pause().await;
        leaving.say_goodbye();
        pause().await;

        let back = TestClient::connect_resuming(&broker, "leaving.test", Some(session.clone())).await;
        assert_ne!(
            back.registration_id().unwrap(),
            session,
            "a client that said goodbye is not coming back for its session"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_vanished_session_is_kept_for_the_retention_window_and_then_removed() {
    with_broker_retaining(Some(2), |broker| async move {
        let away = TestClient::connect(&broker, "away.test").await;
        let session = away.registration_id().unwrap();
        away.subscribe("news");
        pause().await;
        away.vanish().await;

        // Back inside the window: the same session.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let back = TestClient::connect_resuming(&broker, "away.test", Some(session.clone())).await;
        assert_eq!(back.registration_id().unwrap(), session, "kept inside the window");

        // Gone again, and this time gone for longer than the window.
        back.vanish().await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        let late = TestClient::connect_resuming(&broker, "away.test", Some(session.clone())).await;
        assert_ne!(
            late.registration_id().unwrap(),
            session,
            "removed once the window passed"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resuming_a_session_stops_its_clock() {
    with_broker_retaining(Some(2), |broker| async move {
        let away = TestClient::connect(&broker, "away.test").await;
        let session = away.registration_id().unwrap();
        away.subscribe("news");
        pause().await;
        away.vanish().await;
        tokio::time::sleep(Duration::from_millis(500)).await;

        let back = TestClient::connect_resuming(&broker, "away.test", Some(session)).await;
        // Connected for longer than the window: it must not be swept while someone is using it.
        tokio::time::sleep(Duration::from_secs(4)).await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"still here");
        assert_eq!(
            back.wait_for_messages("news", 1).await,
            [b"still here".to_vec()]
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn what_was_held_for_a_session_goes_when_the_session_does() {
    with_broker_retaining(Some(2), |broker| async move {
        let away = TestClient::connect(&broker, "away.test").await;
        away.subscribe("news");
        pause().await;
        away.vanish().await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"too late for you");
        tokio::time::sleep(Duration::from_secs(4)).await;

        let newcomer = TestClient::connect(&broker, "newcomer.test").await;
        newcomer.subscribe("news");
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            newcomer.received("news").is_empty(),
            "a different identity does not inherit it, and the holder has expired"
        );
    })
    .await;
}

// -- the client that the agents use ------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_loses_its_connection_comes_back_to_its_session_and_what_it_missed() {
    with_broker(|broker| async move {
        let agent = FacadeClient::connect(&broker, "agent.test").await;
        let session = agent.client.registration_id().unwrap();
        agent.client.subscribe("news");
        pause().await;

        agent.lose_the_connection().await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"while the connection was down");

        // The front brings its own connection back, asking for the session it had.
        agent
            .client
            .wait_for("a second registration", std::time::Duration::from_secs(10), |_| {
                agent.registrations().len() >= 2
            })
            .await;
        assert_eq!(
            agent.registrations(),
            [session.clone(), session],
            "the same session, resumed"
        );
        assert_eq!(
            agent.client.wait_for_messages("news", 1).await,
            [b"while the connection was down".to_vec()]
        );
    })
    .await;
}

// -- a client is told once that it registered --------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_new_session_is_announced_to_the_client_once() {
    with_broker(|broker| async move {
        let client = TestClient::connect(&broker, "agent.test").await;
        pause().await;
        assert_eq!(
            client.registration_events(),
            1,
            "every announcement makes an agent initialise again"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_session_is_announced_to_the_client_once() {
    with_broker(|broker| async move {
        let away = TestClient::connect(&broker, "agent.test").await;
        let session = away.registration_id().unwrap();
        away.vanish().await;
        pause().await;

        let back = TestClient::connect_resuming(&broker, "agent.test", Some(session)).await;
        pause().await;
        assert_eq!(back.registration_events(), 1);
    })
    .await;
}

// -- a client that publishes to a topic it subscribes to ----------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_client_hears_what_it_publishes_to_a_topic_it_subscribes_to() {
    with_broker(|broker| async move {
        let client = TestClient::connect(&broker, "agent.test").await;
        client.subscribe("selftest");
        pause().await;
        client.publish("selftest", b"probe");
        assert_eq!(
            client.wait_for_messages("selftest", 1).await,
            [b"probe".to_vec()],
            "the probe of a self-test depends on this"
        );
    })
    .await;
}
