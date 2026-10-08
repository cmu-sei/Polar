//! The broker's orderly shutdown: what happens when it is told to stop, and when its own health
//! actor ends.
mod support;
use cassini_types::BrokerMessage;
use std::time::Duration;
use support::{TestClient, pause, with_broker};

#[tokio::test(flavor = "multi_thread")]
async fn a_broker_told_to_shut_down_finishes_the_sequence_and_ends() {
    with_broker(|broker| async move {
        let client = TestClient::connect(&broker, "agent.test").await;
        client.subscribe("news");
        pause().await;

        broker
            .actor()
            .cast(BrokerMessage::PrepareForShutdown { auth_token: None })
            .unwrap();

        assert!(
            broker.ends_by_itself(Duration::from_secs(20)).await,
            "the shutdown sequence never finished"
        );
        client
            .wait_for("the connection to be closed", Duration::from_secs(5), |c| {
                c.lost_its_connection()
            })
            .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_health_actor_ending_starts_the_orderly_shutdown_rather_than_cutting_it_off() {
    with_broker(|broker| async move {
        let client = TestClient::connect(&broker, "agent.test").await;
        pause().await;

        // What the health actor does when it has decided the broker should go.
        ractor::registry::where_is("cassini.healthcheck".to_string())
            .expect("the broker has a health actor")
            .stop(Some("rejuvenation".to_string()));

        // The process is still here to see it: the broker did not exit from inside its handler.
        assert!(
            broker.ends_by_itself(Duration::from_secs(20)).await,
            "the shutdown sequence never finished"
        );
        client
            .wait_for("the connection to be closed", Duration::from_secs(5), |c| {
                c.lost_its_connection()
            })
            .await;
    })
    .await;
}
