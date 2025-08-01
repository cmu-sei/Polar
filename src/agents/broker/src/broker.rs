use crate::{
    get_subscriber_name,
    listener::{ListenerManager, ListenerManagerArgs},
    session::{SessionManager, SessionManagerArgs},
    subscriber::SubscriberManager,
    topic::{TopicManager, TopicManagerArgs},
    LISTENER_MGR_NOT_FOUND_TXT, SUBSCRIBE_REQUEST_FAILED_TXT, UNEXPECTED_MESSAGE_STR,
};
use crate::{
    BrokerMessage, LISTENER_MANAGER_NAME, PUBLISH_REQ_FAILED_TXT, REGISTRATION_REQ_FAILED_TXT,
    SESSION_MANAGER_NAME, SESSION_NOT_FOUND_TXT, SUBSCRIBER_MANAGER_NAME,
    SUBSCRIBER_MGR_NOT_FOUND_TXT, TOPIC_MANAGER_NAME,
};
use ractor::{
    async_trait,
    registry::where_is,
    rpc::{call, CallResult::Success},
    Actor, ActorProcessingErr, ActorRef, SupervisionEvent,
};
use tracing::{debug, info, warn};

// ============================== Broker Supervisor Actor Definition ============================== //

pub struct Broker;

// State object containing references to worker actors
pub struct BrokerState;

/// Would-be configurations for the broker actor itself, as well its workers
#[derive(Clone)]
pub struct BrokerArgs {
    /// Socket address to listen for connections on
    pub bind_addr: String,

    /// The amount of time (in seconds) before a session times out
    /// Should be between 10 and 300 seconds
    pub session_timeout: Option<u64>,

    pub server_cert_file: String,
    pub private_key_file: String,
    pub ca_cert_file: String,
}

#[async_trait]
impl Actor for Broker {
    type Msg = BrokerMessage;
    type State = BrokerState;
    type Arguments = BrokerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: BrokerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("Broker: Starting {myself:?}");

        let listener_manager_args = ListenerManagerArgs {
            bind_addr: args.bind_addr,
            server_cert_file: args.server_cert_file,
            private_key_file: args.private_key_file,
            ca_cert_file: args.ca_cert_file,
        };

        // The Broker is the Supervisor for all of the other Supervisors, including:

        // Listeners Supervisor
        Actor::spawn_linked(
            Some(LISTENER_MANAGER_NAME.to_owned()),
            ListenerManager,
            listener_manager_args,
            myself.clone().into(),
        )
        .await
        .expect("The Broker cannot initialize without the ListenerManager. Panicking.");

        // Set default timeout for sessions, or use args
        let mut session_timeout: u64 = 90;

        if let Some(timeout) = args.session_timeout {
            session_timeout = timeout;
        }

        // Sessions Supervisor
        Actor::spawn_linked(
            Some(SESSION_MANAGER_NAME.to_string()),
            SessionManager,
            SessionManagerArgs {
                session_timeout: session_timeout,
            },
            myself.clone().into(),
        )
        .await
        .expect("The Broker cannot initialize without the SessionManager. Panicking.");

        let topic_mgr_args = TopicManagerArgs { topics: None };

        // Topics Supervisor
        Actor::spawn_linked(
            Some(TOPIC_MANAGER_NAME.to_owned()),
            TopicManager,
            topic_mgr_args,
            myself.clone().into(),
        )
        .await
        .expect("The Broker cannot initialize without the TopicManager. Panicking.");

        // Subscribers Supervisor
        Actor::spawn_linked(
            Some(SUBSCRIBER_MANAGER_NAME.to_string()),
            SubscriberManager,
            (),
            myself.clone().into(),
        )
        .await
        .expect("The Broker cannot initialize without the SubscriberManager. Panicking.");

        let state = BrokerState;
        Ok(state)
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(actor_cell) => {
                debug!(
                    "Worker agent: {0:?}:{1:?} started",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                )
            }

            SupervisionEvent::ActorTerminated(actor_cell, ..) => {
                debug!(
                    "Worker {0:?}:{1:?} terminated, restarting..",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                )
            }

            SupervisionEvent::ActorFailed(actor_cell, e) => {
                warn!(
                    "Worker agent: {0:?}:{1:?} failed! {e}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );

                // Determine type of actor that failed and restart

                // NOTE: "Remote" actors can't have their types checked? But
                // they do send serializable messages. If we can deserialize
                // them to a datatype here, that may be another acceptable means
                // of determining type.

                // TODO: Figure out what panics/failures we can/can't recover
                // from Missing certificate files and the inability to forward
                // some messages count as bad states
                _myself.stop(Some("ACTOR_FAILED".to_string()));

                // Vaughn, this sounds like a whiteboarding session. These are
                // top-level supervisors, so if they fail we definitely should
                // _try_ to recover them, but each one may have a fairly hefty
                // amount of stuff to recover. This may be akin to a full
                // restart / re-init of the broker, if one of these fails.
            }
            SupervisionEvent::ProcessGroupChanged(_) => (),
        }

        Ok(())
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Vaughn, for these lifecycle events where we don't intend to do much
        // (yet), maybe we could make these debug!(...), so we can cut down on
        // some of the verbosity, unless someone has the logging turned up to
        // eleven.
        debug!("Broker: Started {myself:?}");
        Ok(())
    }

    async fn post_stop(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Broker: Stopped {myself:?}");
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BrokerMessage::RegistrationRequest {
                registration_id,
                client_id,
            } => {
                debug!("Received Registration Request from client: {client_id:?}");
                match registration_id {
                    Some(id) => {
                        // We were provided some session id, find it and forward the request.
                        let session = where_is(id.clone()).expect("Expected to find session. {id}");

                        if let Err(e) = session.send_message(BrokerMessage::RegistrationRequest {
                            registration_id: Some(id.clone()),
                            client_id: client_id.clone(),
                        }) {
                            let err_msg = format!(
                                "{REGISTRATION_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}: {e}"
                            );
                            if let Some(listener) = where_is(id.clone()) {
                                listener
                                    .send_message(BrokerMessage::RegistrationResponse {
                                        registration_id: Some(id.clone()),
                                        client_id: client_id.clone(),
                                        success: false,
                                        error: Some(err_msg),
                                    })
                                    .unwrap();
                            }
                        }

                        // Alert subscriber manager so that any waiting messages get sent
                        let sub_mgr = where_is(SUBSCRIBER_MANAGER_NAME.to_string())
                            .expect("Expected to find subscriber manager.");

                        sub_mgr
                            .send_message(BrokerMessage::RegistrationRequest {
                                registration_id: Some(id.clone()),
                                client_id: client_id.clone(),
                            })
                            .expect("Expected to send message");
                    }
                    None => {
                        // start new session
                        let manager = where_is(SESSION_MANAGER_NAME.to_string())
                            .expect("Expected to find SESSION_MANAGER");
                        manager
                            .send_message(BrokerMessage::RegistrationRequest {
                                registration_id: registration_id.clone(),
                                client_id: client_id.clone(),
                            })
                            .expect("Expected to send message to session manager.");
                    }
                }
            }
            BrokerMessage::SubscribeRequest {
                registration_id,
                topic,
            } => {
                //Look for existing subscriber actor.

                if let Some(registration_id) = registration_id {
                    match where_is(get_subscriber_name(&registration_id, &topic)) {
                        Some(_) => {
                            //Send success message, session already subscribed
                            match &where_is(registration_id.clone()) {
                                //session's probably dead for one reason or another if we fail here,
                                //log and move on
                                Some(session) => session
                                    .send_message(BrokerMessage::SubscribeAcknowledgment {
                                        registration_id: registration_id.clone(),
                                        topic: topic.clone(),
                                        result: Ok(()),
                                    })
                                    .unwrap_or_else(|e| {
                                        warn!("Session {registration_id} not found! {e}!");
                                    }),
                                None => warn!("Session {registration_id} not found!"),
                            }
                        }
                        None => {
                            match where_is(topic.clone()) {
                                Some(actor) => {
                                    let subscriber_manager =
                                        where_is(SUBSCRIBER_MANAGER_NAME.to_string())
                                            .expect("Expected to find subscriber manager.");
                                    let t = topic.clone();
                                    let id = registration_id.clone();

                                    // If no subscriber exists,
                                    match call(&subscriber_manager, |reply| { BrokerMessage::Subscribe { reply: reply, registration_id: id.clone(), topic: t.clone() } }, None)
                                    .await.expect("Expected to call Subscriber Manager") {
                                        Success(add_subscribeer) => {
                                            match add_subscribeer {
                                                Ok(_) => {
                                                    //  create one, then notify topic actor, subscribe to it on behalf of the session
                                                    match call(&actor,|reply| {
                                                        BrokerMessage::Subscribe { reply, registration_id: registration_id.clone(), topic: topic.clone() } }, None)
                                                        .await.expect("Expected to call topic actor") {
                                                        Success(result) => {
                                                            match result {
                                                                Ok(_) => {
                                                                    //send ack to session
                                                                    if let Some(session) = where_is(registration_id.clone()) {
                                                                        if let Err(e) = session.send_message(BrokerMessage::SubscribeAcknowledgment { registration_id: registration_id.clone(), topic: topic.clone(), result: Ok(()) }) {
                                                                            warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {e}");
                                                                        }
                                                                    } else {
                                                                        warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {SESSION_NOT_FOUND_TXT}");
                                                                    }
                                                                }
                                                                Err(e) => {
                                                                    warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {e}");
                                                                }
                                                            }
                                                        }
                                                        _ => ()
                                                    }
                                                }
                                                Err(e) => {
                                                    warn!("{e}");
                                                    todo!("Send error message")
                                                }
                                            }
                                        }
                                        _ => panic!("Subscriber Manager failed to fulfill subscription. Likely because of failure")
                                    }
                                }
                                None => {
                                    //no topic actor exists, tell manager to make one
                                    let topic_mgr = where_is(TOPIC_MANAGER_NAME.to_owned())
                                        .expect("Expected to find topic manager.");
                                    let sub_mgr = where_is(SUBSCRIBER_MANAGER_NAME.to_owned())
                                        .expect("Expected to find subscriber manager.");

                                    let t = topic.clone();

                                    // create a topic on the session's behalf
                                    match call(
                                        &topic_mgr,
                                        |reply| BrokerMessage::AddTopic {
                                            reply: reply,
                                            registration_id: Some(registration_id.clone()),
                                            topic: t.clone(),
                                        },
                                        None,
                                    )
                                    .await
                                    .expect("Expected to call Topic Manager")
                                    {
                                        Success(result) => {
                                            match result {
                                                Ok(_) => {
                                                    // create subscriber
                                                    match call(&sub_mgr, |reply| {
                                                        BrokerMessage::Subscribe { reply, registration_id: registration_id.clone(), topic: topic.clone() }
                                                    }, None).await.expect("Expected to communicate with subscriber manager")
                                                    {
                                                        Success(result) => {
                                                            match result{
                                                                Ok(_) => {
                                                                    //send ack to session
                                                                    if let Some(session) = where_is(registration_id.clone()) {
                                                                        if let Err(_e) = session.send_message(BrokerMessage::SubscribeAcknowledgment { registration_id: registration_id, topic: topic.clone(), result: Ok(()) }) {
                                                                            warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {SESSION_NOT_FOUND_TXT}");
                                                                        }
                                                                    } else { warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {SESSION_NOT_FOUND_TXT}"); }
                                                                }
                                                                Err(e) => {
                                                                    //send err to session
                                                                    if let Some(session) = where_is(registration_id.clone()) {
                                                                        if let Err(e) = session.send_message(BrokerMessage::SubscribeAcknowledgment { registration_id: registration_id, topic: topic.clone(), result: Err(e) }) {
                                                                            warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {SESSION_NOT_FOUND_TXT} {e}");
                                                                        }
                                                                    } else { warn!("{SUBSCRIBE_REQUEST_FAILED_TXT} {SESSION_NOT_FOUND_TXT} {e}"); }
                                                                }
                                                            }
                                                        },
                                                        _ => todo!("Failed to communicate with critical worker")
                                                    }
                                                }
                                                Err(e) => {
                                                    warn!("{e}");
                                                    todo!("Send error message")
                                                }
                                            }
                                        }
                                        _ => panic!("Topic Manager failed to add a topic."),
                                    }
                                }
                            }
                        }
                    }
                }
            }
            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
            } => {
                if let Some(registration_id) = registration_id {
                    //find topic actor, tell it to forget session
                    if let Some(topic_actor) = where_is(topic.clone()) {
                        if let Err(_e) =
                            topic_actor.send_message(BrokerMessage::UnsubscribeRequest {
                                registration_id: Some(registration_id.clone()),
                                topic: topic.clone(),
                            })
                        {
                            warn!("Failed to find topic to unsubscribe to")
                        }
                        // tell subscriber manager to cleanup subscribers
                        let subscriber_mgr = where_is(SUBSCRIBER_MANAGER_NAME.to_string())
                            .expect(SUBSCRIBER_MGR_NOT_FOUND_TXT);
                        subscriber_mgr
                            .send_message(BrokerMessage::UnsubscribeRequest {
                                registration_id: Some(registration_id.clone()),
                                topic: topic.clone(),
                            })
                            .expect("Error sending message to subscriber manager.");
                    }
                }
            }
            BrokerMessage::SubscribeAcknowledgment {
                registration_id,
                topic,
                ..
            } => {
                where_is(registration_id.clone()).map(|session_agent_ref| {
                    session_agent_ref
                        .send_message(BrokerMessage::SubscribeAcknowledgment {
                            registration_id: registration_id.clone(),
                            topic: topic.clone(),
                            result: Ok(()),
                        })
                        .unwrap();
                });
            }
            BrokerMessage::PublishRequest {
                registration_id,
                topic,
                payload,
            } => {
                //publish to topic
                if let Some(registration_id) = registration_id {
                    match where_is(topic.clone()) {
                        Some(actor) => {
                            if let Err(e) = actor.send_message(BrokerMessage::PublishRequest {
                                registration_id: Some(registration_id.clone()),
                                topic: topic.clone(),
                                payload: payload.clone(),
                            }) {
                                warn!("{PUBLISH_REQ_FAILED_TXT}: {e}");
                                //send err to session
                                if let Some(session) = where_is(registration_id.clone()) {
                                    if let Err(e) =
                                        session.send_message(BrokerMessage::PublishResponse {
                                            topic,
                                            payload: payload.clone(),
                                            result: Err(e.to_string()),
                                        })
                                    {
                                        warn!("{PUBLISH_REQ_FAILED_TXT} {e}");
                                    }
                                }
                            }
                        }
                        None => {
                            // topic doesn't exist

                            if let Some(manager) = where_is(TOPIC_MANAGER_NAME.to_string()) {
                                //tell topicmgr to add one, await it to complete,
                                match call(
                                    &manager,
                                    |reply| BrokerMessage::AddTopic {
                                        reply,
                                        registration_id: None,
                                        topic: topic.clone(),
                                    },
                                    None,
                                )
                                .await
                                .expect("{PUBLISH_REQ_FAILED_TXT}: {TOPIC_MGR_NOT_FOUND_TXT}")
                                {
                                    Success(add_topic_result) => {
                                        match add_topic_result {
                                            Ok(topic_actor) => {
                                                //push message to that topic
                                                if let Err(e) = topic_actor.send_message(
                                                    BrokerMessage::PublishRequest {
                                                        registration_id: Some(
                                                            registration_id.clone(),
                                                        ),
                                                        topic: topic.clone(),
                                                        payload: payload.clone(),
                                                    },
                                                ) {
                                                    warn!("{PUBLISH_REQ_FAILED_TXT}: {e}");
                                                    //send err to session
                                                    if let Some(session) =
                                                        where_is(registration_id.clone())
                                                    {
                                                        if let Err(e) = session.send_message(
                                                            BrokerMessage::PublishResponse {
                                                                topic,
                                                                payload: payload.clone(),
                                                                result: Err(e.to_string()),
                                                            },
                                                        ) {
                                                            warn!("{PUBLISH_REQ_FAILED_TXT} {e}");
                                                        }
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                //send err to session
                                                if let Some(session) =
                                                    where_is(registration_id.clone())
                                                {
                                                    if let Err(e) = session.send_message(
                                                        BrokerMessage::PublishResponse {
                                                            topic,
                                                            payload: payload.clone(),
                                                            result: Err(e.to_string()),
                                                        },
                                                    ) {
                                                        warn!("{PUBLISH_REQ_FAILED_TXT} {e}");
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    _ => {
                                        todo!("Failed to communicate with critical actor for some reason, respond")
                                    }
                                }
                            }
                        }
                    }
                }
            } // end publish req
            BrokerMessage::PublishResponse {
                topic,
                payload,
                result,
            } => match where_is(SUBSCRIBER_MANAGER_NAME.to_owned()) {
                Some(actor) => actor
                    .send_message(BrokerMessage::PublishResponse {
                        topic,
                        payload,
                        result,
                    })
                    .expect("Failed to forward notification to subscriber manager"),
                None => tracing::error!("Failed to lookup subscriber manager!"),
            },
            BrokerMessage::ErrorMessage { error, .. } => {
                warn!("Error Received: {error}");
            }
            BrokerMessage::DisconnectRequest {
                client_id,
                registration_id,
            } => {
                //start cleanup
                info!("Cleaning up session {registration_id:?}");
                let subscriber_mgr = where_is(SUBSCRIBER_MANAGER_NAME.to_string())
                    .expect(SUBSCRIBER_MGR_NOT_FOUND_TXT);
                subscriber_mgr
                    .send_message(BrokerMessage::DisconnectRequest {
                        client_id: client_id.clone(),
                        registration_id: registration_id.clone(),
                    })
                    .expect("Expected to forward message");

                // Tell listener manager to kill listener, it's not coming back
                let listener_mgr =
                    where_is(LISTENER_MANAGER_NAME.to_string()).expect(LISTENER_MGR_NOT_FOUND_TXT);

                listener_mgr
                    .send_message(BrokerMessage::DisconnectRequest {
                        client_id: client_id.clone(),
                        registration_id,
                    })
                    .expect("Expected to forward message");
            }
            BrokerMessage::TimeoutMessage {
                client_id,
                registration_id,
                error,
            } => {
                //cleanup subscribers

                let manager = where_is(SUBSCRIBER_MANAGER_NAME.to_string())
                    .expect(SUBSCRIBER_MGR_NOT_FOUND_TXT);
                manager
                    .send_message(BrokerMessage::TimeoutMessage {
                        client_id: client_id.clone(),
                        registration_id: registration_id.clone(),
                        error: error.clone(),
                    })
                    .expect("Expected to forward message");
            }
            _ => warn!(UNEXPECTED_MESSAGE_STR),
        }
        Ok(())
    }
}
