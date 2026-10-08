//! The broker's orderly shutdown: what happens when it is told to stop, and when its own health
//! actor ends.
mod support;
use cassini_types::BrokerMessage;
use std::time::Duration;
use support::{TestClient, pause, with_broker, with_broker_shutting_down_within};

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

#[tokio::test(flavor = "multi_thread")]
async fn a_shutdown_that_overruns_its_budget_ends_the_process() {
    with_broker_shutting_down_within(Duration::from_millis(60), |broker| async move {
        let _client = TestClient::connect(&broker, "agent.test").await;
        pause().await;

        broker
            .actor()
            .cast(BrokerMessage::PrepareForShutdown { auth_token: None })
            .unwrap();

        // The sequence needs longer than the budget; the watchdog does not wait for it.
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(*broker.exits.lock().unwrap(), vec![1]);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shutdown_that_finishes_within_its_budget_leaves_the_watchdog_nothing_to_do() {
    with_broker_shutting_down_within(Duration::from_secs(2), |broker| async move {
        let _client = TestClient::connect(&broker, "agent.test").await;
        pause().await;
        broker
            .actor()
            .cast(BrokerMessage::PrepareForShutdown { auth_token: None })
            .unwrap();
        assert!(broker.ends_by_itself(Duration::from_secs(2)).await);

        // Well past the budget: a watchdog still armed would have fired by now.
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(*broker.exits.lock().unwrap(), Vec::<i32>::new());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_starts_the_orderly_shutdown() {
    with_broker(|broker| async move {
        let client = TestClient::connect(&broker, "agent.test").await;
        pause().await;
        cassini_broker::signals::begin_shutdown_on_signal(broker.actor(), None);
        pause().await;

        // What Kubernetes sends when the pod is deleted.
        unsafe { libc::raise(libc::SIGTERM) };

        assert!(
            broker.ends_by_itself(Duration::from_secs(10)).await,
            "SIGTERM did not start the shutdown"
        );
        client
            .wait_for("the connection to be closed", Duration::from_secs(5), |c| {
                c.lost_its_connection()
            })
            .await;
    })
    .await;
}
