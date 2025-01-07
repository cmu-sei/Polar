
#[cfg(test)]
mod tests {

    use core::panic;
    use std::env;
    use cassini::broker::{Broker, BrokerArgs};
    use cassini::client::{TcpClientActor, TcpClientArgs, TcpClientMessage};
    use ractor::registry::where_is;
    use ractor::{async_trait, ActorProcessingErr, ActorRef, SupervisionEvent};
    use ractor::{concurrency::Duration, Actor};
    use cassini::{ClientMessage, BROKER_NAME, LISTENER_MANAGER_NAME};
    use tokio::sync::Notify;
    use tokio::time::timeout;
    use tracing::{debug, info};
    

    //Bind to some other port if desired
    pub const BIND_ADDR: &str = "127.0.0.1:8080";
    // pub const EXPECTED_BROKER: &str = "Expected Broker to start";
    pub const TEST_SUPERVISOR: &str = "TEST_SUPERVISOR";
    pub const TIMEOUT_ERR_MSG: &str = "Server did not start in time";
    /// Shared Notify instance to signal server readiness
    static SERVER_READY: Notify = Notify::const_new();
    
    pub struct MockSupervisorState;
    pub struct MockSupervisor;
    
    #[async_trait]
    impl Actor for MockSupervisor {
        type Msg = ();
        type State = MockSupervisorState;
        type Arguments = ();

        async fn pre_start(
            &self,
            myself: ActorRef<Self::Msg>,
            _: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            debug!("{myself:?} starting");
            Ok(MockSupervisorState)
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            _: Self::Msg,
            _: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            

            Ok(())
        }

        async fn handle_supervisor_evt(&self, myself: ActorRef<Self::Msg>, msg: SupervisionEvent, _: &mut Self::State) -> Result<(), ActorProcessingErr> {
            
            match msg {
                SupervisionEvent::ActorStarted(actor_cell) => {
                    info!("TEST_SUPERVISOR: {0:?}:{1:?} started", actor_cell.get_name(), actor_cell.get_id());
                },
                SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                    info!("TEST_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}", actor_cell.get_name(), actor_cell.get_id());
                },
                SupervisionEvent::ActorFailed(actor_cell, _) => {
                    panic!("{}" ,format!("Error: actor {0:?}:{1:?} Should not have failed", actor_cell.get_name(), actor_cell.get_id()));
                },
                SupervisionEvent::ProcessGroupChanged(..) => todo!(),
            }    
            
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_init() {
        cassini::init_logging();
        let (supervisor, supervisor_handle) = Actor::spawn(Some(TEST_SUPERVISOR.to_string()), MockSupervisor, ()).await.expect("Expected supervisor to start");

        let broker_args = BrokerArgs { bind_addr: String::from(BIND_ADDR), session_timeout: Some(5), server_cert_file: env::var("TLS_SERVER_CERT_CHAIN").unwrap(), private_key_file: env::var("TLS_SERVER_KEY").unwrap(), ca_cert_file: env::var("TLS_CA_CERT").unwrap() };
        
        //start broker
        let (_, broker_handle) = Actor::spawn(Some(BROKER_NAME.to_string()), Broker, broker_args.clone())
            .await
            .expect("Failed to start Broker");
        
        tokio::time::sleep(Duration::from_secs(1)).await;
        SERVER_READY.notify_waiters();
    
        assert_ne!(where_is(BROKER_NAME.to_string()), None);
        supervisor_handle.await.expect("Expected tests not to panic");

        broker_handle.abort();

    }

    
    #[tokio::test]
    async fn test_tcp_client_connect() {
        
        // Wait for the server to be ready
        
        timeout(Duration::from_secs(15), SERVER_READY.notified())
        .await
        .expect(TIMEOUT_ERR_MSG);

        let supervisor = where_is(TEST_SUPERVISOR.to_string()).unwrap();
        let client_name = "test_tcp_connect_client".to_string();
        
        let (client, handle) = Actor::spawn_linked(Some(client_name.clone()), TcpClientActor, TcpClientArgs {
            bind_addr: BIND_ADDR.to_string(),
            registration_id: None,
            client_cert_file: env::var("TLS_CLIENT_CERT").unwrap(),
            private_key_file: env::var("TLS_CLIENT_KEY").unwrap(),
            ca_cert_file: env::var("TLS_CA_CERT").unwrap(),
        }, supervisor.clone().into()).await.expect("Failed to start client actor");    

        tokio::time::sleep(Duration::from_secs(1)).await;

        client.send_message(TcpClientMessage::Send(ClientMessage::DisconnectRequest(None))).expect("Failed to send message to client actor {e}");
        
        client.kill_and_wait(None).await.expect("Expected to stop client");

    }
    #[tokio::test]
    async fn test_client_registers_successfully() {
        // Wait for the server to be ready
        timeout(Duration::from_secs(15), SERVER_READY.notified())
        .await
        .expect(TIMEOUT_ERR_MSG);

        let supervisor = where_is(TEST_SUPERVISOR.to_string()).unwrap();
    
    
        let (client, handle) = Actor::spawn_linked(Some("test_registration_client".to_owned()),
        TcpClientActor,
        TcpClientArgs {
            bind_addr: BIND_ADDR.to_string(),
            registration_id: None,
            client_cert_file: env::var("TLS_CLIENT_CERT").unwrap(),
            private_key_file: env::var("TLS_CLIENT_KEY").unwrap(),
            ca_cert_file: env::var("TLS_CA_CERT").unwrap(),

        }, supervisor.clone().into()).await.expect("Failed to start client actor");    

        client.send_message(TcpClientMessage::Send(
            ClientMessage::RegistrationRequest { registration_id: None }
        )).unwrap();

        tokio::time::sleep(Duration::from_secs(1)).await;
        
        let session_id = client
        .call(TcpClientMessage::GetRegistrationId, Some(Duration::from_secs(10)))
        .await.unwrap().unwrap();

        assert_ne!(session_id, String::default());
        client.kill_and_wait(None).await.expect("Expected to stop client");
        
    }

    #[tokio::test]
    async fn test_registered_client_disconnect() {
        // Wait for the server to be ready
        timeout(Duration::from_secs(15), SERVER_READY.notified())
        .await
        .expect(TIMEOUT_ERR_MSG);

        let supervisor = where_is(TEST_SUPERVISOR.to_string()).unwrap();
        let (client, _) = Actor::spawn_linked(Some("test_registered_disconnect_client".to_owned()),
        TcpClientActor,
        TcpClientArgs {
            bind_addr: BIND_ADDR.to_string(),
            registration_id: None,
            client_cert_file: env::var("TLS_CLIENT_CERT").unwrap(),
            private_key_file: env::var("TLS_CLIENT_KEY").unwrap(),
            ca_cert_file: env::var("TLS_CA_CERT").unwrap(),
        }, supervisor.clone().into()).await.expect("Failed to start client actor");    

        client.send_message(TcpClientMessage::Send(
            ClientMessage::RegistrationRequest { registration_id: None }
        )).unwrap();

        tokio::time::sleep(Duration::from_secs(1)).await;
        
        //disconnect

        let session_id = client
        .call(TcpClientMessage::GetRegistrationId, Some(Duration::from_secs(3)))
        .await.unwrap().unwrap();

        assert_ne!(session_id, String::default());

        client.send_message(TcpClientMessage::Send(
            ClientMessage::DisconnectRequest(Some(session_id))
        )).expect("Expected to forward msg");

        tokio::time::sleep(Duration::from_secs(1)).await;
        client.kill_and_wait(None).await.expect("Expected to stop client");
        

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

        let supervisor = where_is(TEST_SUPERVISOR.to_string()).unwrap();

        let (client, _) = Actor::spawn_linked(Some("test_timeout_client".to_owned()),
        TcpClientActor,
        TcpClientArgs {
            bind_addr: BIND_ADDR.to_string(),
            registration_id: None,
            client_cert_file: env::var("TLS_CLIENT_CERT").unwrap(),
            private_key_file: env::var("TLS_CLIENT_KEY").unwrap(),
            ca_cert_file: env::var("TLS_CA_CERT").unwrap(),
        }, supervisor.clone().into()).await.expect("Failed to start client actor");    

        client.send_message(TcpClientMessage::Send(
            ClientMessage::RegistrationRequest { registration_id: None }
        )).unwrap();

        tokio::time::sleep(Duration::from_secs(1)).await;

        let session_id = client
        .call(TcpClientMessage::GetRegistrationId,
            Some(Duration::from_secs(3)))
        .await.unwrap().unwrap();

        assert_ne!(session_id, String::default());
        
        tokio::time::sleep(Duration::from_secs(1)).await;
        
        //create some subscription
        client.send_message(TcpClientMessage::Send(
            ClientMessage::SubscribeRequest { 
                registration_id: Some(session_id.clone()),
                topic: String::from("Apples")
            }
        )).map_err(|e| { println!("{e}")}).unwrap();

        tokio::time::sleep(Duration::from_secs(1)).await;

        //disconnect
        client.send_message(TcpClientMessage::Send(
            ClientMessage::TimeoutMessage(Some(session_id))
        )).expect("Expected to foward msg");

        client.kill_and_wait(None).await.expect("Expected to stop client");
        
    }

    /// Confirms clients that get disconnected unexpectedly can resume their session and keep subscriptions
    #[tokio::test]
    async fn test_client_reconnect_after_timeout() {
        // Wait for the server to be ready
        timeout(Duration::from_secs(15), SERVER_READY.notified())
        .await
        .expect(TIMEOUT_ERR_MSG);

        let supervisor = where_is(TEST_SUPERVISOR.to_string()).expect("Expected supervisor to be present");

        let (client, _) = Actor::spawn_linked(Some("initial_reconnect_client".to_string()),
        TcpClientActor,
        TcpClientArgs {
            bind_addr: BIND_ADDR.to_string(),
            registration_id: None,
            client_cert_file: env::var("TLS_CLIENT_CERT").unwrap(),
            private_key_file: env::var("TLS_CLIENT_KEY").unwrap(),
            ca_cert_file: env::var("TLS_CA_CERT").unwrap(),
        }, supervisor.clone().into()).await.expect("Failed to start client actor");    

        client.send_message(TcpClientMessage::Send(
            ClientMessage::RegistrationRequest { registration_id: None }
        )).unwrap();

        tokio::time::sleep(Duration::from_secs(1)).await;

        let session_id = client
        .call(TcpClientMessage::GetRegistrationId, Some(Duration::from_secs(10)))
        .await.unwrap().unwrap();

        

        //subscribe to topic
        let topic = String::from("Apples");
        client.send_message(TcpClientMessage::Send(
            ClientMessage::SubscribeRequest { topic: topic.clone(), registration_id: Some(session_id.clone())}
        )).expect("Expected to foward msg");

        // wait a moment, then force timeout and kill first client
        tokio::time::sleep(Duration::from_secs(3)).await;
        
        client.send_message(TcpClientMessage::Send(
            ClientMessage::TimeoutMessage(Some(session_id.clone()))
        )).map_err(|e| { println!("{e}")}).expect("Expected to foward msg");

        //create new client, connect and send registration request with same session_id
        let (new_client, _) = Actor::spawn_linked(Some("test_reconnect_client".to_owned()),
        TcpClientActor,
        TcpClientArgs {
            bind_addr: BIND_ADDR.to_string(),
            registration_id: Some(session_id.clone()),
            client_cert_file: env::var("TLS_CLIENT_CERT").unwrap(),
            private_key_file: env::var("TLS_CLIENT_KEY").unwrap(),
            ca_cert_file: env::var("TLS_CA_CERT").unwrap(),
        }, supervisor.clone().into()).await.expect("Failed to start client actor");    

        let cloned_id = session_id.clone();
        // send new registration request with same registration_id to resume session
        let _ = new_client.send_after(Duration::from_secs(1), || {
            TcpClientMessage::Send(
                ClientMessage::RegistrationRequest { registration_id: Some(cloned_id) }
            )
        } ).await.expect("Expected to send re registration request");
        
        //Publish messsage
        let _ = new_client.send_message( TcpClientMessage::Send(ClientMessage::PublishRequest { topic, payload: "Hello apple".to_string(), registration_id: Some(session_id.clone())}));
        
        //wait for responses, panics
        tokio::time::sleep(Duration::from_secs(3)).await;

        client.kill_and_wait(None).await.expect("Expected to stop client");
        new_client.kill_and_wait(None).await.expect("Expected to stop client");
    }

    #[tokio::test]
    async fn test_dlq_on_reconnect() {
        let topic = String::from("Apples");
        // Wait for the server to be ready
        timeout(Duration::from_secs(15), SERVER_READY.notified())
        .await
        .expect(TIMEOUT_ERR_MSG);

        let supervisor = where_is(TEST_SUPERVISOR.to_string()).expect("Expected test supervisor to be present");
        //start a client to subscribe to some topic
        let (subscriber_client, _ ) = Actor::spawn_linked(Some(format!("dlq_subscriber_client")),
        TcpClientActor,
        TcpClientArgs {
            bind_addr: BIND_ADDR.to_string(),
            registration_id: None,
            client_cert_file: env::var("TLS_CLIENT_CERT").unwrap(),
            private_key_file: env::var("TLS_CLIENT_KEY").unwrap(),
            ca_cert_file: env::var("TLS_CA_CERT").unwrap(),
        },
        supervisor.clone().into()).await.expect("Failed to start client actor");

        //register
        subscriber_client.send_message(TcpClientMessage::Send(
            ClientMessage::RegistrationRequest { registration_id: None }
        )).unwrap();

        tokio::time::sleep(Duration::from_secs(1)).await;    

        //confirm registration
        let subscriber_session_id = subscriber_client
        .call(TcpClientMessage::GetRegistrationId, Some(Duration::from_secs(10)))
        .await.unwrap().unwrap();
        
        assert_ne!(subscriber_session_id, String::default());

        //subscribe client to topic
        subscriber_client.send_message(TcpClientMessage::Send(
            ClientMessage::SubscribeRequest { topic: topic.clone(), registration_id: Some(subscriber_session_id.clone())}
        )).expect("Expected to foward msg");
            
        // wait a moment
        tokio::time::sleep(Duration::from_secs(2)).await;

        //assert subscription exists
        //assert_ne!(where_is(format!("{subscriber_session_id}:Apples")), None);

        //send fake timeout, should stop listener
        subscriber_client.send_message(TcpClientMessage::Send(
            ClientMessage::TimeoutMessage(Some(subscriber_session_id.clone()))
        )).expect("Expected to foward msg");

        // wait a moment
        tokio::time::sleep(Duration::from_secs(2)).await;

        //create a client to send messages to that topic
        let (publisher_client, _ ) =  Actor::spawn_linked(Some(format!("dlq_publisher_client")),
        TcpClientActor,
        TcpClientArgs {
            bind_addr: BIND_ADDR.to_string(),
            registration_id: None,
            client_cert_file: env::var("TLS_CLIENT_CERT").unwrap(),
            private_key_file: env::var("TLS_CLIENT_KEY").unwrap(),
            ca_cert_file: env::var("TLS_CA_CERT").unwrap(),
        },
        supervisor.clone().into()).await.expect("Failed to start client actor");

        //register publisher
        publisher_client.send_message(TcpClientMessage::Send(
            ClientMessage::RegistrationRequest { registration_id: None }
        )).unwrap();

        
        tokio::time::sleep(Duration::from_secs(1)).await;
        //confirm registration
        let publisher_session_id = publisher_client
        .call(TcpClientMessage::GetRegistrationId, Some(Duration::from_secs(10)))
        .await.unwrap().unwrap();

        assert_ne!(publisher_session_id, String::default());

        //send a few messages
        for i in 1..10 {
            publisher_client.send_message( TcpClientMessage::Send(ClientMessage::PublishRequest { topic: topic.clone(), payload: "Hello apple".to_string(), registration_id: Some(publisher_session_id.clone())})).unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        //kill publisher
        publisher_client.kill_and_wait(None).await.expect("Expected to kill client");
        

        //create new client, connect and send registration request to resume subscriber session
        let (new_client, _) = Actor::spawn_linked(Some("read_dlq_client".to_owned()),
        TcpClientActor,
        TcpClientArgs {
            bind_addr: BIND_ADDR.to_string(),
            registration_id: Some(subscriber_session_id.clone()),
            client_cert_file: env::var("TLS_CLIENT_CERT").unwrap(),
            private_key_file: env::var("TLS_CLIENT_KEY").unwrap(),
            ca_cert_file: env::var("TLS_CA_CERT").unwrap(),
        }, supervisor.clone().into()).await.expect("Failed to start client actor");    

        let cloned_id = subscriber_session_id.clone();

        //kill first client now that we have this one
        subscriber_client.kill_and_wait(None).await.expect("Expected to kill client");

        // send new registration request with same registration_id to resume session
        let _ = new_client.send_message(
            TcpClientMessage::Send(
                ClientMessage::RegistrationRequest { registration_id: Some(cloned_id) }
            )).expect("Expected to send re registration request");        
        tokio::time::sleep(Duration::from_secs(3)).await;
        new_client.kill_and_wait(None).await.expect("Expected to kill client");
    }



    // #[tokio::test]
    // async fn test_teardown() {
    //     let supervisor = where_is(TEST_SUPERVISOR.to_string()).expect("Expected supervsior to be present");
    //     while !supervisor.get_children().is_empty() {
    //         let workers = supervisor.get_children().len();
    //         println!("Waiting for {workers} worker(s) to finish.");
    //         sleep(Duration::from_secs(5)).await;
    //     }
    //     supervisor.stop(Some("FINISHED".to_string()))
    // }
}


