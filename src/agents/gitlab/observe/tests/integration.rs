mod tests {

    use broker::{Broker, BrokerArgs};
    use cassini::*;
    use client::{TcpClientActor, TcpClientArgs, TcpClientMessage};
    use gitlab_observer::*;
    use ractor::registry::where_is;
    use ractor::rpc::call;
    use ractor::Actor;
    use reqwest::Client;
    use supervisor::ObserverSupervisor;
    use std::env;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::{error::Error, time::Duration};
    use tokio::sync::Notify;
    use tokio::time::timeout;

    //Bind to some other port if desired
    pub const BIND_ADDR: &str = "127.0.0.1:8080";
    // pub const EXPECTED_BROKER: &str = "Expected Broker to start";
    pub const TEST_SUPERVISOR: &str = "TEST_SUPERVISOR";
    pub const TIMEOUT_ERR_MSG: &str = "Server did not start in time";
    /// Shared Notify instance to signal server readiness
    static BROKER_READY: Notify = Notify::const_new();

    // tests currently running
    static ACTIVE_TESTS: AtomicUsize = AtomicUsize::new(0);
    // notified to signal all tests are complete to trigger teardown
    static TEST_NOTIFY: Notify = Notify::const_new();

    #[tokio::test]
    async fn broker_init() {
        polar::init_logging();

        let broker_args = BrokerArgs {
            bind_addr: String::from(BIND_ADDR),
            session_timeout: Some(5),
            server_cert_file: env::var("TLS_SERVER_CERT_CHAIN").unwrap(),
            private_key_file: env::var("TLS_SERVER_KEY").unwrap(),
            ca_cert_file: env::var("TLS_CA_CERT").unwrap(),
        };

        //start broker
        let _ = Actor::spawn(Some(BROKER_NAME.to_string()), Broker, broker_args.clone())
            .await
            .expect("Failed to start Broker");

        tokio::time::sleep(Duration::from_secs(1)).await;

        //start client
        let _ = Actor::spawn(
            Some(BROKER_CLIENT_NAME.to_string()),
            TcpClientActor,
            TcpClientArgs {
                bind_addr: BIND_ADDR.to_string(),
                registration_id: None,
                client_cert_file: env::var("TLS_CLIENT_CERT").unwrap(),
                private_key_file: env::var("TLS_CLIENT_KEY").unwrap(),
                ca_cert_file: env::var("TLS_CA_CERT").unwrap(),
            },
        )
        .await
        .expect("Expected client to start");

        BROKER_READY.notify_waiters();

        assert_ne!(where_is(BROKER_NAME.to_string()), None);

        //wait to end
        TEST_NOTIFY.notified().await;
    }

    #[tokio::test]
    pub async fn test_user_observer() {
        timeout(Duration::from_secs(15), BROKER_READY.notified())
            .await
            .expect(TIMEOUT_ERR_MSG);

        ACTIVE_TESTS.fetch_add(1, Ordering::SeqCst);

        let gitlab_endpoint = env::var("GITLAB_ENDPOINT").unwrap();
        let broker_addr = env::var("BROKER_ADDR").unwrap();
        let gitlab_token = env::var("GITLAB_TOKEN").unwrap();
        
        let client =
            where_is(BROKER_CLIENT_NAME.to_string()).expect("Expected client to be present");

        tokio::time::sleep(Duration::from_secs(3)).await;

        //confirm registration
        let session_id = call(
            &client,
            |reply| TcpClientMessage::GetRegistrationId(reply),
            Some(Duration::from_secs(10)),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();

        let args = GitlabObserverArgs {
            registration_id: session_id,
            gitlab_endpoint,
            token: Some(gitlab_token),
            web_client: Client::new()
        };

        //start users observer
        let (observer, handle) = Actor::spawn(
            Some(GITLAB_USERS_OBSERVER.to_string()),
            users::GitlabUserObserver,
            args,
        )
        .await
        .expect("Expected to start observer agent");

        //wait for interval to pass

        tokio::time::sleep(Duration::from_secs(5)).await;
        observer.stop(None);

        // Signal completion
        if ACTIVE_TESTS.fetch_sub(1, Ordering::SeqCst) == 1 {
            TEST_NOTIFY.notify_one();
        }
    }
}
