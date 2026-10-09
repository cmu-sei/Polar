//! The broker's orderly shutdown: what happens when it is told to stop, and when its own health
//! actor ends. (SIGTERM is the builder's listener and is tested there: the process's signals are
//! not the test's to raise.)
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
        broker.health_actor().stop(Some("rejuvenation".to_string()));

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
async fn a_shutdown_that_finishes_within_its_budget_lets_the_infrastructure_come_down() {
    // The watchdog is the builder's now and stays armed until the process ends, which covers the
    // time between the broker finishing and `main` returning; in the pod `main` returns at once.
    // What a finished shutdown does is tell the infrastructure it may stop.
    with_broker_shutting_down_within(Duration::from_secs(20), |broker| async move {
        let _client = TestClient::connect(&broker, "agent.test").await;
        pause().await;
        let health = broker.health_actor();
        broker
            .actor()
            .cast(BrokerMessage::PrepareForShutdown { auth_token: None })
            .unwrap();
        assert!(broker.ends_by_itself(Duration::from_secs(10)).await);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while health.get_status() != ractor::ActorStatus::Stopped {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the health actor was never stopped: the infrastructure was not told to proceed"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(*broker.exits.lock().unwrap(), Vec::<i32>::new(), "well inside the budget");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shutdown_requested_over_the_wire_is_under_the_same_budget() {
    // A request the control manager accepted did not come through the infrastructure, so the
    // broker asks it to begin; its watchdog then bounds the whole sequence.
    with_broker_shutting_down_within(Duration::from_millis(60), |broker| async move {
        let _client = TestClient::connect(&broker, "agent.test").await;
        pause().await;
        // To the control manager directly, as an accepted wire request reaches it.
        ractor::registry::where_is(cassini_broker::CONTROL_MANAGER_NAME.to_string())
            .expect("the broker has a control manager")
            .send_message(BrokerMessage::PrepareForShutdown { auth_token: None })
            .unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(*broker.exits.lock().unwrap(), vec![1]);
    })
    .await;
}
