/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

use crate::UNEXPECTED_MESSAGE_STR;
use crate::budget::Bounds;
use crate::deliveries::{DeliveryOutbox, next_delivery_id};
use crate::{
    CLIENT_NOT_FOUND_TXT, DISCONNECTED_REASON, REGISTRATION_REQ_FAILED_TXT, SESSION_NOT_FOUND_TXT,
    SUBSCRIBE_REQUEST_FAILED_TXT, TIMEOUT_REASON, get_subscriber_name,
};
use cassini_tracing::try_set_parent_otel;
use cassini_types::{
    Backlog, BrokerMessage, HeldDelivery, HeldMessages, MessageId, Payload, ShutdownPhase,
};
use ractor::{
    Actor, ActorProcessingErr, ActorRef, RpcReplyPort, SupervisionEvent, registry::where_is,
    rpc::CallResult,
};
use std::collections::HashMap;
use std::{sync::Arc, time::Duration};
use tracing::{Instrument, debug, error, info, trace, trace_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;

pub const SUBSCRIBER_NOT_FOUND_TXT: &str = "Subscriber not found!";

/// Our supervisor for the subscribers
/// When a user subscribes to a new topic, this actor is notified inb conjunction with the topic manager.
/// A new process is started to wait and listen for new messages on that topic and forward messages.
/// Clients are only considered subscribed if an actor process exists and is managed by this actor
pub struct SubscriberManager;

pub struct SubscriberManagerArgs {
    pub session_mgr: ActorRef<BrokerMessage>,
    /// The memory budget and each subscriber's high water.
    pub bounds: Bounds,
}

pub struct SubscriberManagerState {
    subscribers: HashMap<String, ActorRef<BrokerMessage>>,
    session_mgr: ActorRef<BrokerMessage>,
    /// Reference to the topic manager; initially None, will be set via
    /// SetTopicManagerRef once TopicManager exists (SubscriberManager is
    /// spawned before TopicManager, so it can't be passed at construction
    /// time). Used to forward UnsubscribeRequest when a subscription ends,
    /// whether via explicit unsubscribe or the subscriber dying.
    // Nothing reads it, though the comment above says it is used to forward UnsubscribeRequest: whether that
    // forwarding happens elsewhere, or is missing, is for remaining-work.md, not a lint fix.
    #[allow(dead_code)]
    topic_mgr: Option<ActorRef<BrokerMessage>>,
    is_shutting_down: bool,
    bounds: Bounds,
}

// ============================== SubscriberManager Actor ============================== //

#[ractor::async_trait]
impl Actor for SubscriberManager {
    type Msg = BrokerMessage;
    type State = SubscriberManagerState;
    type Arguments = SubscriberManagerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: SubscriberManagerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::debug!("{myself:?} starting");
        Ok(SubscriberManagerState {
            subscribers: HashMap::new(),
            session_mgr: args.session_mgr,
            topic_mgr: None,
            is_shutting_down: false,
            bounds: args.bounds,
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        tracing::debug!("{myself:?} Started");
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<BrokerMessage>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BrokerMessage::SubscriptionsOf {
                registration_id,
                reply_to,
            } => {
                let topics = myself
                    .get_children()
                    .into_iter()
                    .filter_map(|cell| {
                        let name = cell.get_name()?;
                        let (id, topic) = name.split_once(':')?;
                        (id == registration_id).then(|| topic.to_string())
                    })
                    .collect();
                let _ = reply_to.send(topics);
            }
            BrokerMessage::InitiateShutdownPhase {
                phase: ShutdownPhase::TerminateSubscribers,
            } => {
                self.handle_initiate_shutdown_phase(myself.clone(), state)
                    .await?;
            }
            BrokerMessage::RegistrationRequest {
                registration_id,
                client_id,
                trace_ctx,
                peer_identity,
                ..
            } => {
                self.handle_registration_request(
                    myself.clone(),
                    state,
                    registration_id,
                    client_id,
                    trace_ctx,
                    peer_identity,
                )
                .await?;
            }
            BrokerMessage::CreateSubscriber {
                registration_id,
                topic,
                trace_ctx,
                reply,
            } => {
                self.handle_create_subscriber(
                    myself.clone(),
                    state,
                    registration_id,
                    topic,
                    trace_ctx,
                    reply,
                )
                .await?;
            }
            BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                self.handle_unsubscribe_request(
                    myself.clone(),
                    state,
                    registration_id,
                    topic,
                    trace_ctx,
                )
                .await?;
            }
            BrokerMessage::DisconnectRequest {
                client_id,
                registration_id,
                trace_ctx,
                ..
            } => {
                self.handle_disconnect_request(
                    myself.clone(),
                    state,
                    client_id,
                    registration_id,
                    trace_ctx,
                )
                .await?;
            }
            BrokerMessage::TimeoutMessage {
                registration_id, ..
            } => {
                self.handle_timeout_message(myself.clone(), state, registration_id)
                    .await?;
            }
            BrokerMessage::SessionSuperseded { registration_id } => {
                Self::hold_subscriptions(&myself, state, registration_id).await;
            }
            BrokerMessage::SessionDetached { registration_id } => {
                for subscriber in Self::subscribers_of(&myself, &registration_id) {
                    let _ = subscriber.send_message(BrokerMessage::SessionDetached {
                        registration_id: registration_id.clone(),
                    });
                }
            }
            // From the pressure manager, at each stall check: to every subscriber.
            BrokerMessage::BudgetExhausted => {
                for subscriber in myself.get_children() {
                    let _ = subscriber.send_message(BrokerMessage::BudgetExhausted);
                }
            }
            BrokerMessage::StallCheck => {
                for subscriber in myself.get_children() {
                    let _ = subscriber.send_message(BrokerMessage::StallCheck);
                }
            }
            other => warn!(?other, "{UNEXPECTED_MESSAGE_STR}"),
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<BrokerMessage>,
        msg: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                debug!(
                    "Subscription ended for session {0:?}, {reason:?}",
                    actor_cell.get_name()
                );
                self.handle_actor_terminated(myself.clone(), state).await?;
            }
            SupervisionEvent::ActorFailed(actor_cell, error) => {
                self.handle_actor_failed(actor_cell, error).await?;
            }
            SupervisionEvent::ProcessGroupChanged(..) => (),
        }
        Ok(())
    }
}

impl SubscriberManager {
    /// Has every subscriber of a superseded session keep what it receives, then tells the session
    /// manager they all do. Until then the session stays up, so nothing reaches a stopped session.
    async fn hold_subscriptions(
        myself: &ActorRef<BrokerMessage>,
        state: &SubscriberManagerState,
        registration_id: String,
    ) {
        for subscriber in myself.get_children().into_iter().filter(|cell| {
            cell.get_name()
                .and_then(|name| name.split_once(':').map(|(id, _)| id == registration_id))
                .unwrap_or(false)
        }) {
            let held = ractor::rpc::call(
                &subscriber,
                |reply| BrokerMessage::HoldSubscription { reply },
                Some(Duration::from_millis(500)),
            )
            .await;
            if !matches!(held, Ok(CallResult::Success(()))) {
                warn!(%registration_id, "a subscriber did not confirm holding: {held:?}");
            }
        }
        let _ = state
            .session_mgr
            .send_message(BrokerMessage::SessionHeld { registration_id });
    }

    /// Takes what the sessions held for this session's identity have kept on `topic`, oldest
    /// session first, ending those subscribers.
    async fn claim_held(
        state: &SubscriberManagerState,
        registration_id: &str,
        topic: &str,
    ) -> HeldMessages {
        let held_sessions = match state
            .session_mgr
            .call(
                |reply_to| BrokerMessage::GetHeldSessions {
                    registration_id: registration_id.to_string(),
                    reply_to,
                },
                Some(Duration::from_secs(2)),
            )
            .await
        {
            Ok(CallResult::Success(ids)) => ids,
            other => {
                warn!(%registration_id, "could not learn what is held for this identity: {other:?}");
                return Vec::new();
            }
        };
        let mut claimed = Vec::new();
        for old in held_sessions {
            let Some(holder) = where_is(get_subscriber_name(&old, topic)) else {
                continue;
            };
            match ractor::rpc::call(
                &holder,
                |reply| BrokerMessage::ClaimHeld { reply },
                Some(Duration::from_millis(500)),
            )
            .await
            {
                Ok(CallResult::Success(mut messages)) => {
                    info!(%registration_id, from = %old, %topic, count = messages.len(), "inheriting held messages");
                    claimed.append(&mut messages);
                }
                other => warn!(from = %old, %topic, "could not claim held messages: {other:?}"),
            }
        }
        claimed
    }

    /// The subscribers of the session `registration_id`, each registered as `<registration id>:<topic>`.
    fn subscribers_of(
        myself: &ActorRef<BrokerMessage>,
        registration_id: &str,
    ) -> Vec<ractor::ActorCell> {
        myself
            .get_children()
            .into_iter()
            .filter(|cell| {
                cell.get_name()
                    .and_then(|name| name.split_once(':').map(|(id, _)| id == registration_id))
                    .unwrap_or(false)
            })
            .collect()
    }

    /// Removes all subscriptions for a given session
    fn forget_subscriptions(
        registration_id: String,
        myself: ActorRef<BrokerMessage>,
        reason: Option<String>,
    ) {
        let mut stopped = 0usize;
        for subscriber in myself.get_children().into_iter().filter(|cell| {
            let Some(sub_name) = cell.get_name() else {
                return false;
            };
            let Some((sub_id, _topic)) = sub_name.split_once(':') else {
                return false;
            };
            registration_id == sub_id
        }) {
            subscriber.stop(reason.clone());
            stopped += 1;
        }
        debug!(%registration_id, stopped, "Stopped subscriptions for registration");
    }

    // ===== Handler methods =====
    async fn handle_initiate_shutdown_phase(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
    ) -> Result<(), ActorProcessingErr> {
        info!("Starting termination of subscriber agents");
        state.is_shutting_down = true;

        let children: Vec<_> = myself.get_children();

        for subscriber in &children {
            let _ = subscriber.send_message(BrokerMessage::PrepareForShutdown { auth_token: None });
            subscriber.stop(Some("SHUTDOWN_TERMINATE".to_string()));
        }

        if children.is_empty() {
            self.signal_terminate_complete(myself.clone(), state)
                .await?;
        }
        Ok(())
    }

    async fn handle_registration_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
        registration_id: Option<String>,
        client_id: String,
        trace_ctx: Option<opentelemetry::Context>,
        _peer_identity: Option<String>,
    ) -> Result<(), ActorProcessingErr> {
        // Reject new registrations during shutdown
        if state.is_shutting_down {
            warn!("Rejecting registration request during shutdown termination phase");
            return Ok(());
        }

        let span = trace_span!("cassini.subscriber_manager.handle_registration_request", %client_id, has_registration_id = registration_id.is_some());
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("Subscriber manager received registration request for client {client_id}");

        if let Some(registration_id) = registration_id {
            info!("Gathering Subscriptions for session {registration_id}");
            if where_is(registration_id.clone()).is_some() {
                let cloned_session_id = registration_id.clone();

                for subscriber in myself.get_children().into_iter().filter(|cell| {
                    let Some(sub_name) = cell.get_name() else {
                        return false;
                    };
                    let Some((sub_id, _topic)) = sub_name.split_once(':') else {
                        return false;
                    };
                    cloned_session_id == sub_id
                }) {
                    if let Err(e) = subscriber.send_message(BrokerMessage::RegistrationRequest {
                        registration_id: Some(registration_id.clone()),
                        client_id: client_id.clone(),
                        trace_ctx: Some(span.context()),
                        peer_identity: None,
                    }) {
                        warn!(error = %e, "{REGISTRATION_REQ_FAILED_TXT}: {SUBSCRIBER_NOT_FOUND_TXT}");
                    }
                }
            } else {
                let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT}: {SESSION_NOT_FOUND_TXT}");
                warn!("{err_msg}");

                if let Some(listener) = where_is(client_id.clone()) {
                    if let Err(e) = listener.send_message(BrokerMessage::RegistrationResponse {
                        client_id: client_id.clone(),
                        result: Err(err_msg.clone()),
                        trace_ctx: Some(span.context()),
                    }) {
                        warn!(error = %e, "{err_msg}");
                    }
                } else {
                    error!("{err_msg}: {CLIENT_NOT_FOUND_TXT}");
                }
            }
        } else {
            warn!("RegistrationRequest missing registration_id; nothing to rehydrate");
        }
        Ok(())
    }

    async fn handle_create_subscriber(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
        reply: RpcReplyPort<cassini_types::CreatedSubscriber>,
    ) -> Result<(), ActorProcessingErr> {
        if state.is_shutting_down {
            warn!("Rejecting subscriber creation during shutdown");
            let _ = reply.send(Err(ActorProcessingErr::from("Broker is shutting down")));
            return Ok(());
        }

        let span =
            trace_span!("cassini.subscriber_manager.create_subscriber", %registration_id, %topic);
        try_set_parent_otel(&span, trace_ctx);
        trace!("Subscriber manager received subscribe command");

        let subscriber_id = get_subscriber_name(&registration_id, &topic);
        // What its client has not acknowledged: the subscriber's own, named in the budget's report.
        let backlog = Arc::new(Backlog::subscriber(
            &registration_id,
            &topic,
            state.bounds.high_water(),
        ));
        state.bounds.budget.register(&backlog);

        // Get the session ref from the session manager
        let session_ref = match state
            .session_mgr
            .call(
                |reply_to| BrokerMessage::GetSessionRef {
                    registration_id: registration_id.clone(),
                    reply_to,
                },
                Some(Duration::from_millis(100)),
            )
            .await
        {
            Ok(CallResult::Success(Some(ref_))) => ref_,
            // A subscription that cannot be made is refused to whoever asked; it is not this manager failing. Failing
            // here shut the broker down (a failed worker is fatal to it), and under load a subscribe that raced a
            // session's end, or a session manager slower than the wait, took every client's broker away.
            Ok(CallResult::Success(None)) => {
                warn!(%registration_id, %topic, "no session to subscribe; refused");
                let _ = reply.send(Err(ActorProcessingErr::from("Session not found")));
                return Ok(());
            }
            other => {
                warn!(%registration_id, %topic, ?other, "the session could not be looked up; refused");
                let _ = reply.send(Err(ActorProcessingErr::from("Failed to get session ref")));
                return Ok(());
            }
        };

        match Actor::spawn_linked(
            Some(subscriber_id.clone()),
            SubscriberAgent,
            SubscriberAgentArgs {
                session: session_ref.clone(),
                backlog: backlog.clone(),
                bounds: state.bounds.clone(),
            },
            myself.clone().into(),
        )
        .instrument(span.clone())
        .await
        {
            Ok((subscriber, _)) => {
                // What was held for this identity on this topic goes to the new subscriber first, ahead of anything
                // published once it is attached to the topic, and is held by it until the new client acknowledges it.
                for held in Self::claim_held(state, &registration_id, &topic).await {
                    let _ = subscriber.send_message(BrokerMessage::PublishResponse {
                        topic: topic.clone(),
                        payload: held.payload,
                        result: Ok(()),
                        trace_ctx: None,
                        published_by: held.published_by,
                        origin: held.origin,
                        delivery_id: None,
                        refusal: None,
                    });
                }
                info!(%registration_id, %topic, "subscription created");
                state.subscribers.insert(subscriber_id, subscriber.clone());
                let _ = reply.send(Ok(subscriber));
            }
            Err(e) => {
                let err_msg = format!("Failed to spawn subscriber actor. {e}");
                let _ = reply.send(Err(ActorProcessingErr::from(err_msg)));
            }
        }
        Ok(())
    }

    async fn handle_unsubscribe_request(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span =
            trace_span!("cassini.subscriber_manager.unsubscribe_request", %registration_id, %topic);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("subscriber manager received unsubscribe request");

        let subscriber_name = format!("{registration_id}:{topic}");
        if let Some(subscriber) = state.subscribers.get(&subscriber_name) {
            info!(%registration_id, %topic, "subscription ended by the client");
            subscriber.stop(Some("UNSUBSCRIBED".to_string()));
            // Forward acknowledgment via session manager
            if let Err(e) =
                state
                    .session_mgr
                    .send_message(BrokerMessage::UnsubscribeAcknowledgment {
                        registration_id,
                        topic,
                        result: Ok(()),
                    })
            {
                warn!(error = %e, "Failed to send unsubscribe ack to session manager");
            }
        } else {
            warn!("Session agent {registration_id} not subscribed to topic {topic}");
        }
        Ok(())
    }

    async fn handle_disconnect_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
        client_id: String,
        registration_id: Option<String>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.subscriber_manager.disconnect_request", %client_id, has_registration_id = registration_id.is_some());
        try_set_parent_otel(&span, trace_ctx);
        async {
            if let Some(registration_id) = registration_id {
                SubscriberManager::forget_subscriptions(
                    registration_id,
                    myself.clone(),
                    Some(DISCONNECTED_REASON.to_string()),
                );

                if state.is_shutting_down {
                    self.check_terminate_complete(myself.clone(), state).await?;
                }
            } else {
                warn!("Failed to process disconnect request! registration_id missing.")
            }
            Ok::<(), ActorProcessingErr>(())
        }
        .instrument(span)
        .await
    }

    async fn handle_timeout_message(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
        registration_id: String,
    ) -> Result<(), ActorProcessingErr> {
        SubscriberManager::forget_subscriptions(
            registration_id,
            myself.clone(),
            Some(TIMEOUT_REASON.to_string()),
        );
        if state.is_shutting_down {
            self.check_terminate_complete(myself.clone(), state).await?;
        }
        Ok(())
    }

    async fn handle_actor_terminated(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
    ) -> Result<(), ActorProcessingErr> {
        if state.is_shutting_down {
            self.check_terminate_complete(myself.clone(), state).await?;
        }
        Ok(())
    }

    async fn handle_actor_failed(
        &self,
        actor_cell: ractor::ActorCell,
        error: Box<dyn std::error::Error + Send + Sync>,
    ) -> Result<(), ActorProcessingErr> {
        error!(
            "Subscriber actor {} ({:?}) failed unexpectedly: {}",
            actor_cell.get_name().unwrap_or("<unnamed>".to_string()),
            actor_cell.get_id(),
            error
        );
        Ok(())
    }

    async fn check_terminate_complete(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberManagerState,
    ) -> Result<(), ActorProcessingErr> {
        if state.is_shutting_down && myself.get_children().is_empty() {
            self.signal_terminate_complete(myself.clone(), state)
                .await?;
        }
        Ok(())
    }

    async fn signal_terminate_complete(
        &self,
        myself: ActorRef<BrokerMessage>,
        _state: &mut SubscriberManagerState,
    ) -> Result<(), ActorProcessingErr> {
        info!("All subscriber agents terminated");

        if let Some(supervisor) = myself.try_get_supervisor() {
            supervisor.send_message(BrokerMessage::ShutdownPhaseComplete {
                phase: ShutdownPhase::TerminateSubscribers,
            })?;
        }

        myself.stop(Some("SHUTDOWN_TERMINATED".to_string()));
        Ok(())
    }
}

/// Our subscriber actor.
/// The existence of a running "Subscriber" signifies a clients subscription
/// it is responsible for forwarding new messages received on its given topic
pub struct SubscriberAgent;

/// Define the state for the actor
pub struct SubscriberAgentState {
    registration_id: String,
    // session_ref is now stored directly
    session_ref: ActorRef<BrokerMessage>,
    topic: String,
    /// Every message given to this subscriber, from when it is given until its client acknowledges it.
    outbox: DeliveryOutbox,
    /// The session this subscription belonged to was superseded: nothing is forwarded, everything
    /// is kept until a new session of the same identity claims it, or the broker lets it go.
    held: bool,
    /// The bytes its outbox holds, against its high water: its own (the budget's report names it, and reads nothing
    /// else of it).
    backlog: Arc<Backlog>,
    bounds: Bounds,
    /// Its topic, once the topic has taken it (`TopicAttached`), and whether it last told the topic it holds it back.
    topic_ref: Option<ActorRef<BrokerMessage>>,
    told_holding: bool,
}

pub struct SubscriberAgentArgs {
    pub session: ActorRef<BrokerMessage>,
    pub backlog: Arc<Backlog>,
    pub bounds: Bounds,
}

#[ractor::async_trait]
impl Actor for SubscriberAgent {
    type Msg = BrokerMessage;
    type State = SubscriberAgentState;
    type Arguments = SubscriberAgentArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: SubscriberAgentArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let name = myself.get_name().ok_or_else(|| {
            ActorProcessingErr::from(format!(
                "{SUBSCRIBE_REQUEST_FAILED_TXT}: Expected subscriber to have name"
            ))
        })?;

        if let Some((registration_id, topic)) = name.split_once(':') {
            Ok(SubscriberAgentState {
                registration_id: registration_id.to_string(),
                session_ref: args.session,
                topic: topic.to_string(),
                outbox: DeliveryOutbox::default(),
                held: false,
                backlog: args.backlog,
                bounds: args.bounds,
                topic_ref: None,
                told_holding: false,
            })
        } else {
            Err(ActorProcessingErr::from(format!(
                "{SUBSCRIBE_REQUEST_FAILED_TXT}: Bad name given: {name}"
            )))
        }
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} Started");
        Ok(())
    }

    /// An ended subscriber holds its topic back no more.
    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        state.backlog.set_connected(false);
        Self::tell_topic(state);
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BrokerMessage::SessionDetached { .. } => {
                state.backlog.set_connected(false);
                Self::tell_topic(state);
            }
            BrokerMessage::TopicAttached { topic } => {
                state.topic_ref = Some(topic);
                state.told_holding = false;
                Self::tell_topic(state);
            }
            // The stall checks, from the pressure manager: away while the budget is exhausted, it lets go of what it
            // holds; connected, it looks whether it has stalled.
            BrokerMessage::BudgetExhausted => {
                Self::follow_the_budget(state);
                Self::shed_if_away(state);
                Self::look_for_stall(&myself, state, true);
            }
            BrokerMessage::StallCheck => {
                Self::follow_the_budget(state);
                Self::look_for_stall(&myself, state, false);
            }
            // Its session ended its connection for its stall: counted.
            BrokerMessage::StallEnded => state.bounds.budget.count_slow_disconnection(),
            BrokerMessage::RegistrationRequest {
                registration_id,
                client_id,
                trace_ctx,
                peer_identity,
                ..
            } => {
                self.handle_registration_request(
                    myself.clone(),
                    state,
                    registration_id,
                    client_id,
                    trace_ctx,
                    peer_identity,
                )
                .await?;
            }
            BrokerMessage::PublishResponse {
                topic,
                payload,
                trace_ctx,
                published_by,
                origin,
                ..
            } => {
                self.handle_publish_response(
                    myself.clone(),
                    state,
                    topic,
                    payload,
                    trace_ctx,
                    published_by,
                    origin,
                )
                .await?;
            }
            BrokerMessage::PublishResponseAck { delivery_id } => {
                if let Some(delivery) = state.outbox.acknowledge(delivery_id) {
                    state.backlog.remove(delivery.payload.weight());
                    Self::tell_topic(state);
                }
            }
            BrokerMessage::PushMessageFailed { delivery_id } => {
                // Still held: it is sent again when the session is resumed.
                debug!(topic = %state.topic, delivery_id, held = state.outbox.len(), "a delivery did not reach the client");
            }
            BrokerMessage::HoldSubscription { reply } => {
                state.held = true;
                state.backlog.set_connected(false);
                Self::tell_topic(state);
                let _ = reply.send(());
            }
            BrokerMessage::ClaimHeld { reply } => {
                let held = state.outbox.take();
                state.backlog.remove(state.backlog.bytes());
                debug!(topic = %state.topic, count = held.len(), "handing over what was held");
                let _ = reply.send(held);
                myself.stop(Some("CLAIMED".to_string()));
            }
            other => warn!(?other, "{UNEXPECTED_MESSAGE_STR}"),
        }
        Ok(())
    }
}

impl SubscriberAgent {
    /// Its high water follows the budget as it is now (the runtime budget may have moved it).
    fn follow_the_budget(state: &mut SubscriberAgentState) {
        let high_water = state.bounds.high_water();
        if state.backlog.high_water() != high_water {
            state.backlog.set_high_water(high_water);
        }
    }

    /// Tells its topic whether it holds the topic back (connected and above its high water), when that changed.
    fn tell_topic(state: &mut SubscriberAgentState) {
        let holds = state.backlog.holds_back();
        if holds == state.told_holding {
            return;
        }
        if let Some(topic) = &state.topic_ref {
            state.told_holding = holds;
            let _ = topic.send_message(BrokerMessage::TopicHold {
                registration_id: state.registration_id.clone(),
                holds,
            });
        }
    }

    /// Looks whether it has stalled, from its own backlog, and says so to its session, which decides by the one stall
    /// policy (`crate::pressure::verdict`): above its high water for the stall bound, or, while the budget is
    /// exhausted, holding anything with nothing acknowledged for the stall bound.
    fn look_for_stall(
        myself: &ActorRef<BrokerMessage>,
        state: &SubscriberAgentState,
        exhausted: bool,
    ) {
        let stall = state.bounds.stall;
        let above = state
            .backlog
            .above_for()
            .filter(|above_for| state.backlog.holds_back() && *above_for >= stall);
        let (stalled_for, by_exhaustion) = match above {
            Some(above_for) => (above_for, false),
            None if exhausted => match state.backlog.stalled_for().filter(|s| *s >= stall) {
                Some(stalled_for) => (stalled_for, true),
                None => return,
            },
            None => return,
        };
        let _ = state
            .session_ref
            .send_message(BrokerMessage::SubscriberStalled {
                topic: state.topic.clone(),
                held: state.backlog.bytes(),
                stalled_for,
                exhausted: by_exhaustion,
                stall,
                subscriber: myself.clone(),
            });
    }

    /// The budget is exhausted: a subscriber whose client is away lets go of what it holds, so the budget can recover
    /// (what a subscriber is owed is kept only while the budget allows). Counted, and logged.
    fn shed_if_away(state: &mut SubscriberAgentState) {
        if state.backlog.is_connected() || state.outbox.is_empty() {
            return;
        }
        let bytes = state.outbox.bytes();
        let shed = state.outbox.take();
        // All it was owed, as it was counted (its frames' bytes; the outbox counts payloads).
        state.backlog.remove(state.backlog.bytes());
        state.bounds.budget.count_refused(shed.len() as u64);
        // What it would resume is gone: its session ends now, and its client is told when it comes back.
        let _ = state.session_ref.send_message(BrokerMessage::SessionShed {
            topic: state.topic.clone(),
            count: shed.len(),
        });
        warn!(
            subscriber = state.backlog.name(),
            count = shed.len(),
            bytes,
            "the broker's memory budget is exhausted: a subscriber whose client is away lets go of what it held"
        );
    }

    // ===== Handler methods for SubscriberAgent =====
    async fn handle_registration_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberAgentState,
        registration_id: Option<String>,
        client_id: String,
        trace_ctx: Option<opentelemetry::Context>,
        _peer_identity: Option<String>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.subscriber.handle_registration_request", %client_id, has_registration_id = registration_id.is_some());
        try_set_parent_otel(&span, trace_ctx);
        let _enter = span.enter();

        trace!("Subscriber actor received registration request for client {client_id}");

        if let Some(id) = registration_id {
            // Reconnection: everything the client did not acknowledge is sent again, oldest first, with the number
            // it had (the client may have had it before its connection went: delivery is at-least-once).
            state.backlog.set_connected(true);
            Self::tell_topic(state);
            let session = state.session_ref.clone();
            info!(
                held = state.outbox.len(),
                "Forwarding unacknowledged messages to session: {id}"
            );
            for (delivery_id, delivery) in state.outbox.iter() {
                if let Err(e) = session.send_message(Self::push(
                    &state.topic,
                    *delivery_id,
                    delivery,
                    Some(span.context()),
                )) {
                    warn!("Failed to forward message to subscriber! {e} Ending subscription");
                    myself.stop(None);
                    break;
                }
            }
        }
        Ok(())
    }

    /// The message that hands `delivery` to the session, to write to its client.
    fn push(
        topic: &str,
        delivery_id: u64,
        delivery: &HeldDelivery,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> BrokerMessage {
        BrokerMessage::PushMessage {
            payload: delivery.payload.clone(),
            topic: topic.to_string(),
            trace_ctx,
            published_by: delivery.published_by.clone(),
            origin: delivery.origin,
            delivery_id,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_publish_response(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SubscriberAgentState,
        topic: String,
        payload: Payload,
        trace_ctx: Option<opentelemetry::Context>,
        published_by: Option<String>,
        origin: Option<MessageId>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("cassini.subscriber.publish_response", %topic);
        try_set_parent_otel(&span, trace_ctx);
        let _enter = span.enter();
        trace!("Subscriber actor received publish response for topic \"{topic}\"");
        debug!(
            "New message on topic: \"{topic}\", forwarding to session: {}",
            state.registration_id
        );
        // A subscriber whose client is away is a holder like any other: while the budget is exhausted it refuses the
        // newest rather than grow. A connected one is bounded by its high water, which its topic keeps.
        if !state.backlog.is_connected() && state.bounds.budget.is_exhausted() {
            state.bounds.budget.count_refused(1);
            warn!(
                subscriber = state.backlog.name(),
                bytes = payload.len(),
                held = state.outbox.len(),
                "the broker's memory budget is exhausted: a message for a subscriber whose client is away is refused"
            );
            return Ok(());
        }
        // Held from now until the client acknowledges it, whether it is sent now or later.
        let delivery_id = next_delivery_id();
        state.backlog.add(payload.weight());
        Self::tell_topic(state);
        let delivery = HeldDelivery {
            payload,
            published_by,
            origin,
        };
        let push = Self::push(&topic, delivery_id, &delivery, Some(span.context()));
        state.outbox.hold(delivery_id, delivery);
        if state.held {
            return Ok(());
        }
        if let Err(e) = state.session_ref.send_message(push) {
            warn!("Failed to forward message to subscriber! {e} Ending subscription");
            myself.stop(None);
            return Ok(());
        }
        Ok(())
    }
}
