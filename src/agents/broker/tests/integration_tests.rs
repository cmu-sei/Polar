#[cfg(test)]
mod tests {

    use cassini::broker::{Broker, BrokerArgs};
    use cassini::client::{TcpClientActor, TcpClientArgs, TcpClientMessage};
    use cassini::{get_subscriber_name, ClientMessage, TCPClientConfig, BROKER_NAME};
    use ractor::registry::where_is;
    use ractor::OutputPort;
    use ractor::{concurrency::Duration, Actor};
    use std::env;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::Notify;
    use tokio::time::timeout;
    use tracing::debug;

    //Bind to some other port if desired
    pub const BIND_ADDR: &str = "127.0.0.1:8080";
    pub const TIMEOUT_ERR_MSG: &str = "Server did not start in time";
    /// Shared Notify instance to signal server readiness
    static SERVER_READY: Notify = Notify::const_new();

    // tests currently running
    static ACTIVE_TESTS: AtomicUsize = AtomicUsize::new(0);
    // notified to signal all tests are complete to trigger teardown
    static TEST_NOTIFY: Notify = Notify::const_new();

    /// an example subscriber that'll await registration
    struct Subscriber;

    enum SubscriberMessage {
        Published(String),
    }

    #[ractor::async_trait]
    impl Actor for Subscriber {
        type Msg = SubscriberMessage;

        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            myself: ractor::ActorRef<Self::Msg>,
            _: (),
        ) -> Result<Self::State, ractor::errors::ActorProcessingErr> {
            debug!("{myself:?} starting");
            Ok(())
        }

        async fn handle(
            &self,
            myself: ractor::ActorRef<Self::Msg>,
            message: Self::Msg,
            _: &mut Self::State,
        ) -> Result<(), ractor::errors::ActorProcessingErr> {
            match message {
                Self::Msg::Published(msg) => {
                    tracing::info!("Subscriber ({myself:?}) received registration id '{msg}'");
                    myself.stop(None);
                }
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_init() {
        polar::init_logging();

        let broker_args = BrokerArgs {
            bind_addr: String::from(BIND_ADDR),
            session_timeout: Some(5),
            server_cert_file: env::var("TLS_SERVER_CERT_CHAIN")
                .expect("Expected to find value for TLS_SERVER_CERT_CHAIN"),
            private_key_file: env::var("TLS_SERVER_KEY")
                .expect("Expected to find value for TLS_PRIVATE_KEY"),
            ca_cert_file: env::var("TLS_CA_CERT").expect("Expected to find value for TLS_CA_CERT"),
        };

        //start broker
        let _ = Actor::spawn(Some(BROKER_NAME.to_string()), Broker, broker_args.clone())
            .await
            .expect("Failed to start Broker");

        tokio::time::sleep(Duration::from_secs(1)).await;
        SERVER_READY.notify_waiters();

        //wait to end
        TEST_NOTIFY.notified().await;
    }

    #[tokio::test]
    async fn test_client_registers_successfully() {
        // Wait for the server to be ready
        timeout(Duration::from_secs(15), SERVER_READY.notified())
            .await
            .expect(TIMEOUT_ERR_MSG);

        ACTIVE_TESTS.fetch_add(1, Ordering::SeqCst);

        let output_port = Arc::new(OutputPort::default());

        let (subscriber, subscriber_handle) = Actor::spawn(None, Subscriber, ())
            .await
            .expect("Expected to start subscriber");

        //subscribe to output
        output_port.subscribe(subscriber, |message| {
            debug!("Converting message");
            Some(SubscriberMessage::Published(message))
        });

        let (client, _) = Actor::spawn(
            Some("test_registration_client".to_owned()),
            TcpClientActor,
            TcpClientArgs {
                config: TCPClientConfig::new(),
                registration_id: None,
                output_port: output_port.clone(),
            },
        )
        .await
        .expect("Failed to start client actor");

        tokio::time::sleep(Duration::from_secs(1)).await;

        // await subscriber to die
        subscriber_handle
            .await
            .expect("Expected subscriber to get notified");

        client
            .stop_and_wait(None, None)
            .await
            .expect("Expected to stop client");

        // Signal completion
        if ACTIVE_TESTS.fetch_sub(1, Ordering::SeqCst) == 1 {
            TEST_NOTIFY.notify_one();
        }
    }

    #[tokio::test]
    async fn test_registered_client_disconnect() {
        // Wait for the server to be ready
        timeout(Duration::from_secs(15), SERVER_READY.notified())
            .await
            .expect(TIMEOUT_ERR_MSG);

        ACTIVE_TESTS.fetch_add(1, Ordering::SeqCst);

        let output_port = Arc::new(OutputPort::default());

        let (subscriber, subscriber_handle) = Actor::spawn(None, Subscriber, ())
            .await
            .expect("Expected to start subscriber");

        //subscribe to output
        output_port.subscribe(subscriber, |message| {
            debug!("Converting message");
            Some(SubscriberMessage::Published(message))
        });

        let (client, _) = Actor::spawn(
            Some("test_registered_disconnect_client".to_owned()),
            TcpClientActor,
            TcpClientArgs {
                registration_id: None,
                config: TCPClientConfig::new(),
                output_port,
            },
        )
        .await
        .expect("Failed to start client actor");

        tokio::time::sleep(Duration::from_secs(1)).await;

        //await subscriber to exit
        subscriber_handle.await.unwrap();

        let session_id = client
            .call(
                TcpClientMessage::GetRegistrationId,
                Some(Duration::from_secs(3)),
            )
            .await
            .unwrap()
            .unwrap();

        client
            .send_message(TcpClientMessage::Send(ClientMessage::DisconnectRequest(
                session_id,
            )))
            .expect("Expected to forward msg");

        tokio::time::sleep(Duration::from_secs(1)).await;
        client
            .kill_and_wait(None)
            .await
            .expect("Expected to stop client");

        // Signal completion
        if ACTIVE_TESTS.fetch_sub(1, Ordering::SeqCst) == 1 {
            TEST_NOTIFY.notify_one();
        }
    }

    /// Test scenario where a client disconnects unexpectedly,
    /// When the listener's connection fails the session manager should wait for a
    /// pre-configured amount of time before cleaning up the session and removing all subscriptions for
    /// that session.
    #[tokio::test]
    async fn test_session_timeout() {
        // Wait for the server to be ready
        timeout(Duration::from_secs(15), SERVER_READY.notified())
            .await
            .expect(TIMEOUT_ERR_MSG);

        ACTIVE_TESTS.fetch_add(1, Ordering::SeqCst);

        let output_port = Arc::new(OutputPort::default());

        let (client, _) = Actor::spawn(
            Some("test_timeout_client".to_owned()),
            TcpClientActor,
            TcpClientArgs {
                registration_id: None,
                config: TCPClientConfig::new(),
                output_port,
            },
        )
        .await
        .expect("Failed to start client actor");

        tokio::time::sleep(Duration::from_secs(1)).await;

        let session_id = client
            .call(
                TcpClientMessage::GetRegistrationId,
                Some(Duration::from_secs(3)),
            )
            .await
            .unwrap()
            .unwrap();

        tokio::time::sleep(Duration::from_secs(1)).await;

        //create some subscription
        client
            .send_message(TcpClientMessage::Send(ClientMessage::SubscribeRequest {
                registration_id: session_id.clone(),
                topic: String::from("Apples"),
            }))
            .map_err(|e| println!("{e}"))
            .unwrap();

        tokio::time::sleep(Duration::from_secs(1)).await;

        //disconnect
        client
            .send_message(TcpClientMessage::Send(ClientMessage::TimeoutMessage(
                session_id,
            )))
            .expect("Expected to foward msg");

        client
            .kill_and_wait(None)
            .await
            .expect("Expected to stop client");

        // Signal completion
        if ACTIVE_TESTS.fetch_sub(1, Ordering::SeqCst) == 1 {
            TEST_NOTIFY.notify_one();
        }
    }

    /// Confirms clients that get disconnected unexpectedly can resume their session and keep subscriptions
    #[tokio::test]
    async fn test_client_reconnect_after_timeout() {
        // Wait for the server to be ready
        timeout(Duration::from_secs(15), SERVER_READY.notified())
            .await
            .expect(TIMEOUT_ERR_MSG);

        ACTIVE_TESTS.fetch_add(1, Ordering::SeqCst);
        let output_port = Arc::new(OutputPort::default());

        let (client, _) = Actor::spawn(
            Some("initial_reconnect_client".to_string()),
            TcpClientActor,
            TcpClientArgs {
                registration_id: None,
                config: TCPClientConfig::new(),
                output_port: output_port.clone(),
            },
        )
        .await
        .expect("Failed to start client actor");

        tokio::time::sleep(Duration::from_secs(1)).await;

        let session_id = client
            .call(
                TcpClientMessage::GetRegistrationId,
                Some(Duration::from_secs(10)),
            )
            .await
            .unwrap()
            .unwrap();

        assert_ne!(session_id, None);

        //subscribe to topic
        let topic = String::from("Apples");
        client
            .send_message(TcpClientMessage::Send(ClientMessage::SubscribeRequest {
                topic: topic.clone(),
                registration_id: session_id.clone(),
            }))
            .expect("Expected to foward msg");

        // wait a moment, then force timeout and kill first client
        tokio::time::sleep(Duration::from_secs(3)).await;

        client
            .send_message(TcpClientMessage::Send(ClientMessage::TimeoutMessage(
                session_id.clone(),
            )))
            .map_err(|e| println!("{e}"))
            .expect("Expected to foward msg");

        //create new client, connect and send registration request with same session_id
        let (new_client, _) = Actor::spawn(
            Some("test_reconnect_client".to_owned()),
            TcpClientActor,
            TcpClientArgs {
                registration_id: None,
                config: TCPClientConfig::new(),
                output_port,
            },
        )
        .await
        .expect("Failed to start client actor");

        //Publish messsage
        let payload = "Hello apple";
        let _ = new_client.send_message(TcpClientMessage::Send(ClientMessage::PublishRequest {
            topic,
            payload: payload.into(),
            registration_id: session_id.clone(),
        }));

        //wait for responses, panics
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Signal completion
        if ACTIVE_TESTS.fetch_sub(1, Ordering::SeqCst) == 1 {
            TEST_NOTIFY.notify_one();
        }
    }

    #[tokio::test]
    async fn test_dlq_on_reconnect() {
        let topic = String::from("Oranges");
        // Wait for the server to be ready
        timeout(Duration::from_secs(15), SERVER_READY.notified())
            .await
            .expect(TIMEOUT_ERR_MSG);

        ACTIVE_TESTS.fetch_add(1, Ordering::SeqCst);

        let output_port = Arc::new(OutputPort::default());
        //start a client to subscribe to some topic
        let (subscriber_client, _) = Actor::spawn(
            Some(format!("dlq_subscriber_client")),
            TcpClientActor,
            TcpClientArgs {
                registration_id: None,
                config: TCPClientConfig::new(),
                output_port: output_port.clone(),
            },
        )
        .await
        .expect("Failed to start client actor");

        tokio::time::sleep(Duration::from_secs(1)).await;

        //confirm registration
        let subscriber_session_id = subscriber_client
            .call(
                TcpClientMessage::GetRegistrationId,
                Some(Duration::from_secs(10)),
            )
            .await
            .unwrap()
            .unwrap();

        assert_ne!(subscriber_session_id, None);

        //subscribe client to topic
        subscriber_client
            .send_message(TcpClientMessage::Send(ClientMessage::SubscribeRequest {
                topic: topic.clone(),
                registration_id: subscriber_session_id.clone(),
            }))
            .expect("Expected to foward msg");

        // wait a moment
        tokio::time::sleep(Duration::from_secs(2)).await;

        //send fake timeout, should stop listener
        subscriber_client
            .send_message(TcpClientMessage::Send(ClientMessage::TimeoutMessage(
                subscriber_session_id.clone(),
            )))
            .expect("Expected to foward msg");

        // wait a moment
        tokio::time::sleep(Duration::from_secs(2)).await;

        //create a client to send messages to that topic
        let (publisher_client, _) = Actor::spawn(
            Some(format!("dlq_publisher_client")),
            TcpClientActor,
            TcpClientArgs {
                registration_id: None,
                config: TCPClientConfig::new(),
                output_port: output_port.clone(),
            },
        )
        .await
        .expect("Failed to start client actor");

        tokio::time::sleep(Duration::from_secs(1)).await;
        //confirm registration
        let publisher_session_id = publisher_client
            .call(
                TcpClientMessage::GetRegistrationId,
                Some(Duration::from_secs(10)),
            )
            .await
            .unwrap()
            .unwrap();

        assert_ne!(publisher_session_id, None);

        //send a few messages
        for _ in 1..10 {
            publisher_client
                .send_message(TcpClientMessage::Send(ClientMessage::PublishRequest {
                    topic: topic.clone(),
                    payload: "Hello orange".into(),
                    registration_id: publisher_session_id.clone(),
                }))
                .unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        //kill publisher
        publisher_client
            .kill_and_wait(None)
            .await
            .expect("Expected to kill client");

        //create new client, connect and send registration request to resume subscriber session
        let (new_client, _) = Actor::spawn(
            Some("read_dlq_client".to_owned()),
            TcpClientActor,
            TcpClientArgs {
                registration_id: None,
                config: TCPClientConfig::new(),
                output_port,
            },
        )
        .await
        .expect("Failed to start client actor");

        //kill first client now that we have this one
        subscriber_client
            .kill_and_wait(None)
            .await
            .expect("Expected to kill client");

        tokio::time::sleep(Duration::from_secs(3)).await;
        new_client
            .kill_and_wait(None)
            .await
            .expect("Expected to kill client");

        // Signal completion
        if ACTIVE_TESTS.fetch_sub(1, Ordering::SeqCst) == 1 {
            TEST_NOTIFY.notify_one();
        }
    }

    #[tokio::test]
    async fn test_client_unsubscribe() {
        // Wait for the server to be ready
        timeout(Duration::from_secs(15), SERVER_READY.notified())
            .await
            .expect(TIMEOUT_ERR_MSG);

        ACTIVE_TESTS.fetch_add(1, Ordering::SeqCst);

        let output_port = Arc::new(OutputPort::default());

        let (client, _) = Actor::spawn(
            Some("test_unsubscribe_client".to_owned()),
            TcpClientActor,
            TcpClientArgs {
                registration_id: None,
                config: TCPClientConfig::new(),
                output_port: output_port.clone(),
            },
        )
        .await
        .expect("Failed to start client actor");

        tokio::time::sleep(Duration::from_secs(1)).await;

        let session_id = client
            .call(
                TcpClientMessage::GetRegistrationId,
                Some(Duration::from_secs(10)),
            )
            .await
            .unwrap()
            .unwrap();

        assert_ne!(session_id, None);

        let topic = String::from("Cherries");

        //subscribe client to topic
        client
            .send_message(TcpClientMessage::Send(ClientMessage::SubscribeRequest {
                topic: topic.clone(),
                registration_id: session_id.clone(),
            }))
            .expect("Expected to forward msg");

        tokio::time::sleep(Duration::from_secs(1)).await;

        //unsub
        client
            .send_message(TcpClientMessage::Send(ClientMessage::UnsubscribeRequest {
                registration_id: session_id.clone(),
                topic: topic.clone(),
            }))
            .expect("Expected to forward msg");

        tokio::time::sleep(Duration::from_secs(1)).await;

        assert_eq!(
            where_is(get_subscriber_name(&session_id.unwrap(), &topic.clone())),
            None
        );

        // Signal completion
        if ACTIVE_TESTS.fetch_sub(1, Ordering::SeqCst) == 1 {
            TEST_NOTIFY.notify_one();
        }
    }

    #[tokio::test]
    async fn test_client_gets_queued_messages() {
        // Wait for the server to be ready
        timeout(Duration::from_secs(15), SERVER_READY.notified())
            .await
            .expect(TIMEOUT_ERR_MSG);

        ACTIVE_TESTS.fetch_add(1, Ordering::SeqCst);

        let output_port = Arc::new(OutputPort::default());
        let (client, _) = Actor::spawn(
            Some("queued_message_test_client".to_owned()),
            TcpClientActor,
            TcpClientArgs {
                registration_id: None,
                config: TCPClientConfig::new(),
                output_port: output_port.clone(),
            },
        )
        .await
        .expect("Failed to start client actor");

        tokio::time::sleep(Duration::from_secs(1)).await;

        let session_id = client
            .call(
                TcpClientMessage::GetRegistrationId,
                Some(Duration::from_secs(10)),
            )
            .await
            .unwrap()
            .unwrap();

        assert_ne!(session_id, None);

        let topic = String::from("Comics");

        //create a client to send messages to that topic
        let (publisher_client, _) = Actor::spawn(
            Some(format!("comic_publisher_client")),
            TcpClientActor,
            TcpClientArgs {
                registration_id: None,
                config: TCPClientConfig::new(),
                output_port: output_port.clone(),
            },
        )
        .await
        .expect("Failed to start client actor");

        tokio::time::sleep(Duration::from_secs(1)).await;
        //confirm registration
        let publisher_session_id = publisher_client
            .call(
                TcpClientMessage::GetRegistrationId,
                Some(Duration::from_secs(10)),
            )
            .await
            .unwrap()
            .unwrap();

        assert_ne!(publisher_session_id, None);

        //send a few messages
        for i in 1..10 {
            publisher_client
                .send_message(TcpClientMessage::Send(ClientMessage::PublishRequest {
                    topic: topic.clone(),
                    payload: format!("comic {i}").into(),
                    registration_id: publisher_session_id.clone(),
                }))
                .unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        //subscribe client to topic
        client
            .send_message(TcpClientMessage::Send(ClientMessage::SubscribeRequest {
                topic: topic.clone(),
                registration_id: session_id.clone(),
            }))
            .expect("Expected to forward msg");

        tokio::time::sleep(Duration::from_secs(1)).await;

        // Signal completion
        if ACTIVE_TESTS.fetch_sub(1, Ordering::SeqCst) == 1 {
            TEST_NOTIFY.notify_one();
        }
    }
}
