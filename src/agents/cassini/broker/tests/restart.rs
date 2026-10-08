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
            .wait_for("a registration with the new broker", Duration::from_secs(30), |_| {
                agent.registrations().len() >= 2
            })
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
