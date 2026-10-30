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

use crate::BROKER_NOT_FOUND_TXT;
use crate::{
    CLIENT_NOT_FOUND_TXT, PUBLISH_REQ_FAILED_TXT, REGISTRATION_REQ_FAILED_TXT,
    SUBSCRIBE_REQUEST_FAILED_TXT,
};
use crate::{UNEXPECTED_MESSAGE_STR, get_subscriber_name};
use cassini_tracing::try_set_parent_otel;
use cassini_types::{
    BrokerMessage, ControlError, ControlOp, ControlResult, DisconnectReason, MessageId,
    ShutdownPhase,
};
use ractor::{
    Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait, registry::where_is,
};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{Instrument, debug, error, info, trace, trace_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use uuid::Uuid;

/// The manager process for our concept of client sessions.
/// When the broker receives word of a new connection from the ListenerManager, it requests
/// that the client be "registered", signifying the client as legitimate.
pub struct SessionManager;

/// Private representation of a connected session.
struct Session {
    agent_ref: ActorRef<BrokerMessage>,
    pending_publishes: usize, // Number of unacknowledged publish requests
    /// When the client's connection went away without a goodbye. `None` while it is connected.
    /// A session detached for longer than the retention window is ended.
    detached_at: Option<tokio::time::Instant>,
    /// The verified identity of the client that registered this session.
    identity: Option<String>,
    /// A new session of the same identity has taken over: this one cannot be resumed, and is
    /// only waiting for its subscribers to hold, then stops.
    superseded: bool,
}

/// What a superseded session left behind: its subscribers, holding what they receive for a new
/// session of the same identity to claim, until the retention window after the takeover.
struct HeldSession {
    registration_id: String,
    identity: String,
    since: tokio::time::Instant,
    /// Every subscriber of the session confirmed it is holding.
    ready: bool,
}

pub struct SessionManagerState {
    sessions: HashMap<String, Session>,
    broker: ActorRef<BrokerMessage>,
    listener_mgr: Option<ActorRef<BrokerMessage>>,
    /// Reference to the subscriber manager; initially None, set via
    /// SetSubscriberManagerRef once SubscriberManager exists (SessionManager
    /// is spawned before it). Used to continue the layered disconnect flow
    /// (Listener -> Session -> Subscriber -> Topic) -- without this, a
    /// session ending from a real socket disconnect never reaches
    /// SubscriberManager at all, and every subscription it owned leaks
    /// forever with no reachable cleanup path.
    subscriber_mgr: Option<ActorRef<BrokerMessage>>,
    retention: std::time::Duration,
    /// Superseded sessions, oldest first.
    held: Vec<HeldSession>,
    /// Questions about an identity with a takeover still in progress, answered when it is done.
    parked: Vec<(String, ractor::RpcReplyPort<Vec<String>>)>,
    is_shutting_down: bool,
    drain_phase_active: bool,
    drain_completion_notified: bool,
    total_pending_publishes: usize,
    pub pending_drain_count: usize,
}

pub struct SessionManagerArgs {
    pub broker: ActorRef<BrokerMessage>,
    /// Reference to the listener manager; initially None, will be set via SetListenerManager.
    pub listener_mgr: Option<ActorRef<BrokerMessage>>,
    /// How long a session whose client went away without a goodbye is kept.
    pub retention: std::time::Duration,
}

// ============================== SessionManager Actor ============================== //

#[async_trait]
impl Actor for SessionManager {
    type Msg = BrokerMessage;
    type State = SessionManagerState;
    type Arguments = SessionManagerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: SessionManagerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        // Often enough that a session is ended soon after its window passes, never faster than
        // a tenth of a second or slower than every thirty.
        let tick = (args.retention / 4).clamp(
            std::time::Duration::from_millis(100),
            std::time::Duration::from_secs(30),
        );
        drop(myself.send_interval(tick, || BrokerMessage::SessionExpiryTick));
        Ok(SessionManagerState {
            broker: args.broker,
            listener_mgr: args.listener_mgr,
            subscriber_mgr: None,
            sessions: HashMap::new(),
            retention: args.retention,
            held: Vec::new(),
            parked: Vec::new(),
            is_shutting_down: false,
            drain_phase_active: false,
            drain_completion_notified: false,
            total_pending_publishes: 0,
            pending_drain_count: 0,
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{myself:?} started");
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<BrokerMessage>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BrokerMessage::PrepareForShutdown { .. } => {
                self.handle_prepare_for_shutdown(myself.clone(), state)
                    .await?;
            }
            BrokerMessage::InitiateShutdownPhase {
                phase: ShutdownPhase::DrainExistingSessions,
            } => {
                self.handle_initiate_drain_phase(myself.clone(), state)
                    .await?;
            }
            BrokerMessage::SessionPendingUpdate {
                registration_id,
                delta,
            } => {
                self.handle_session_pending_update(myself.clone(), state, registration_id, delta)
                    .await?;
            }
            BrokerMessage::RegistrationRequest {
                registration_id,
                client_id,
                trace_ctx,
                peer_identity,
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
            // The control plane's ListSessions: every session the broker holds, connected or kept for its client's
            // return, but not one taken over by a newer session of its identity. A session's subscriptions are its
            // subscriber actors, each registered as `<registration id>:<topic>`.
            BrokerMessage::GetSessions { reply_to, .. } => {
                let names = ractor::registry::registered();
                let sessions: cassini_types::SessionMap = state
                    .sessions
                    .iter()
                    .filter(|(_, session)| !session.superseded)
                    .map(|(id, _)| {
                        let prefix = format!("{id}:");
                        let subscriptions = names
                            .iter()
                            .filter_map(|name| name.strip_prefix(&prefix).map(str::to_string))
                            .collect();
                        (
                            id.clone(),
                            cassini_types::SessionDetails {
                                registration_id: id.clone(),
                                subscriptions,
                            },
                        )
                    })
                    .collect();
                let _ = reply_to.send(sessions);
            }
            BrokerMessage::GetSessionRef {
                registration_id,
                reply_to,
            } => {
                let session_ref = state
                    .sessions
                    .get(&registration_id)
                    .map(|s| s.agent_ref.clone());
                reply_to.send(session_ref).ok();
            }
            BrokerMessage::SetListenerManager { listener_mgr } => {
                state.listener_mgr = Some(listener_mgr);
                debug!("SessionManager received listener manager ref");
            }
            BrokerMessage::SetSubscriberManagerRef { subscriber_mgr } => {
                state.subscriber_mgr = Some(subscriber_mgr);
                debug!("SessionManager received subscriber manager ref");
            }
            BrokerMessage::DisconnectRequest {
                client_id,
                registration_id,
                reason,
                trace_ctx,
            } => {
                match (registration_id, &reason) {
                    // The client is not coming back for this session.
                    (Some(registration_id), DisconnectReason::Goodbye) => {
                        Self::end_session(state, &registration_id, client_id, reason, trace_ctx)
                    }
                    // The connection went away with no word: keep the session for the
                    // retention window, in case the client returns.
                    (
                        Some(registration_id),
                        DisconnectReason::RemoteClosed | DisconnectReason::TransportError(_),
                    ) => {
                        Self::detach_session(state, &registration_id);
                    }
                    // Never registered: there is no session to end or keep.
                    (None, _) => {}
                }
            }
            // A write to the client failed: its connection is gone.
            BrokerMessage::TimeoutMessage {
                registration_id, ..
            } => {
                Self::detach_session(state, &registration_id);
            }
            BrokerMessage::SessionExpiryTick => {
                Self::end_expired_sessions(state);
            }
            BrokerMessage::SessionHeld { registration_id } => {
                Self::complete_takeover(state, &registration_id);
            }
            BrokerMessage::GetHeldSessions {
                registration_id,
                reply_to,
            } => {
                let identity = state
                    .sessions
                    .get(&registration_id)
                    .and_then(|session| session.identity.clone());
                match identity {
                    None => {
                        let _ = reply_to.send(Vec::new());
                    }
                    Some(identity) => {
                        state.parked.push((identity, reply_to));
                        Self::answer_parked(state);
                    }
                }
            }

            _ => (),
        }

        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                debug!(
                    "Session: {0:?}:{1:?} terminated. {reason:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );

                if let Some(name) = actor_cell.get_name()
                    && let Some(session) = state.sessions.remove(&name)
                {
                    self.handle_actor_terminated(myself.clone(), state, session.pending_publishes)
                        .await?;
                }
            }

            SupervisionEvent::ActorFailed(actor_cell, error) => warn!(
                "Worker agent: {0:?}:{1:?} failed! {error}",
                actor_cell.get_name(),
                actor_cell.get_id()
            ),
            SupervisionEvent::ProcessGroupChanged(_) => (),
        }
        Ok(())
    }
}

impl SessionManager {
    /// The client's connection is gone and it did not say goodbye: keep the session, with its
    /// subscribers and what they hold, until the retention window passes.
    fn detach_session(state: &mut SessionManagerState, registration_id: &str) {
        if let Some(session) = state.sessions.get_mut(registration_id)
            && session.detached_at.is_none()
        {
            info!(%registration_id, retention = ?state.retention, "client went away, keeping its session");
            session.detached_at = Some(tokio::time::Instant::now());
        }
    }

    /// Ends the session now: the session actor stops (the map entry and its accounting go when the
    /// supervisor sees it end), and the subscriber manager forgets its subscriptions, which stops
    /// the subscriber actors and drops what they held.
    fn end_session(
        state: &mut SessionManagerState,
        registration_id: &str,
        client_id: String,
        reason: DisconnectReason,
        trace_ctx: Option<opentelemetry::Context>,
    ) {
        if let Some(session) = state.sessions.get(registration_id) {
            session
                .agent_ref
                .stop(Some(format!("session ended: {reason:?}")));
        }
        // Continue the layered flow (Listener -> Session -> Subscriber -> Topic), so every
        // subscription the session owned is cleaned up.
        match &state.subscriber_mgr {
            Some(subscriber_mgr) => {
                let _ = subscriber_mgr.send_message(BrokerMessage::DisconnectRequest {
                    client_id,
                    registration_id: Some(registration_id.to_string()),
                    reason,
                    trace_ctx,
                });
            }
            None => warn!("SessionManager: subscriber_mgr not set, cannot forward disconnect"),
        }
    }

    /// A new session of `identity` has registered: every older session of that identity is
    /// superseded, whether its client has gone or only looks connected (a pod replaced before
    /// the broker has noticed the old connection is dead). It can no longer be resumed; a
    /// connection it still has is closed; its subscribers are told to hold what they receive,
    /// and the session stops once they all do.
    fn supersede_older(
        state: &mut SessionManagerState,
        identity: &str,
        new_id: &str,
        new_client_id: &str,
    ) {
        let superseded: Vec<String> = state
            .sessions
            .iter()
            .filter(|(id, session)| {
                id.as_str() != new_id
                    && !session.superseded
                    && session.identity.as_deref() == Some(identity)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for registration_id in superseded {
            info!(%registration_id, %identity, new_session = %new_id, "superseded by a new session of the same identity");
            if let Some(session) = state.sessions.get_mut(&registration_id) {
                session.superseded = true;
                if session.detached_at.is_none() {
                    let _ = session
                        .agent_ref
                        .send_message(BrokerMessage::CloseConnection {
                            except: new_client_id.to_string(),
                        });
                }
            }
            state.held.push(HeldSession {
                registration_id: registration_id.clone(),
                identity: identity.to_string(),
                since: tokio::time::Instant::now(),
                ready: false,
            });
            match &state.subscriber_mgr {
                Some(subscriber_mgr) => {
                    let _ = subscriber_mgr
                        .send_message(BrokerMessage::SessionSuperseded { registration_id });
                }
                None => warn!("SessionManager: subscriber_mgr not set, cannot hold subscriptions"),
            }
        }
    }

    /// The subscribers of a superseded session all hold: the session itself is no longer needed.
    fn complete_takeover(state: &mut SessionManagerState, registration_id: &str) {
        if let Some(held) = state
            .held
            .iter_mut()
            .find(|held| held.registration_id == registration_id)
        {
            held.ready = true;
        }
        // Drained, not stopped: a stop is a signal, ahead of the queue, and a session with no subscribers is held at once,
        // so it would end before closing its connection (`CloseConnection`, sent as it was superseded), and the old
        // instance would stay connected with no session.
        if let Some(session) = state.sessions.get(registration_id) {
            let _ = session.agent_ref.drain();
        }
        Self::answer_parked(state);
    }

    /// Answers each parked question whose identity has no takeover still in progress.
    fn answer_parked(state: &mut SessionManagerState) {
        let parked = std::mem::take(&mut state.parked);
        for (identity, reply_to) in parked {
            let in_progress = state
                .held
                .iter()
                .any(|held| held.identity == identity && !held.ready);
            if in_progress {
                state.parked.push((identity, reply_to));
            } else {
                let ids = state
                    .held
                    .iter()
                    .filter(|held| held.identity == identity)
                    .map(|held| held.registration_id.clone())
                    .collect();
                let _ = reply_to.send(ids);
            }
        }
    }

    /// Ends every session that has been without its client for longer than the retention window,
    /// and lets go of what superseded sessions held once their window has passed.
    fn end_expired_sessions(state: &mut SessionManagerState) {
        let lapsed: Vec<String> = state
            .held
            .iter()
            .filter(|held| held.ready && held.since.elapsed() >= state.retention)
            .map(|held| held.registration_id.clone())
            .collect();
        for registration_id in lapsed {
            info!(%registration_id, "retention window passed, letting go of what was held");
            state
                .held
                .retain(|held| held.registration_id != registration_id);
            if let Some(subscriber_mgr) = &state.subscriber_mgr {
                let _ = subscriber_mgr.send_message(BrokerMessage::DisconnectRequest {
                    client_id: "retention".to_string(),
                    registration_id: Some(registration_id),
                    reason: DisconnectReason::RemoteClosed,
                    trace_ctx: None,
                });
            }
        }
        let expired: Vec<String> = state
            .sessions
            .iter()
            .filter(|(_, session)| {
                !session.superseded
                    && session
                        .detached_at
                        .is_some_and(|since| since.elapsed() >= state.retention)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for registration_id in expired {
            info!(%registration_id, "retention window passed, ending the session");
            Self::end_session(
                state,
                &registration_id,
                "retention".to_string(),
                DisconnectReason::RemoteClosed,
                None,
            );
        }
    }

    // ===== Handler methods for SessionManager =====
    async fn handle_prepare_for_shutdown(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut SessionManagerState,
    ) -> Result<(), ActorProcessingErr> {
        info!("SessionManager preparing for shutdown");
        state.is_shutting_down = true;

        // Notify all sessions to prepare for shutdown and stop accepting new work
        for (registration_id, session) in &mut state.sessions {
            if let Err(e) = session
                .agent_ref
                .send_message(BrokerMessage::PrepareForShutdown { auth_token: None })
            {
                warn!(
                    "Failed to notify session {} of shutdown: {}",
                    registration_id, e
                );
            }
        }
        Ok(())
    }

    async fn handle_initiate_drain_phase(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionManagerState,
    ) -> Result<(), ActorProcessingErr> {
        info!("Starting drain of existing sessions");
        state.drain_phase_active = true;
        state.drain_completion_notified = false;
        state.total_pending_publishes = 0;

        let pending: Vec<_> = state.sessions.keys().cloned().collect();
        state.pending_drain_count = pending.len();

        for reg_id in &pending {
            if let Some(session) = state.sessions.get(reg_id) {
                let _ = session
                    .agent_ref
                    .send_message(BrokerMessage::PrepareForShutdown { auth_token: None });
                state.total_pending_publishes += session.pending_publishes;
            }
        }

        if state.total_pending_publishes == 0 && !state.sessions.is_empty() {
            // All work is done; stop remaining sessions immediately
            for (_id, session) in state.sessions.drain() {
                session.agent_ref.stop(Some("DRAIN_COMPLETE".to_string()));
            }
            self.signal_drain_complete(myself.clone(), state).await?;
        }

        if pending.is_empty() {
            self.signal_drain_complete(myself.clone(), state).await?;
            return Ok(());
        }
        Ok(())
    }

    async fn handle_session_pending_update(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionManagerState,
        registration_id: String,
        delta: i32,
    ) -> Result<(), ActorProcessingErr> {
        if let Some(session) = state.sessions.get_mut(&registration_id) {
            // Update session's pending count
            let new_pending = (session.pending_publishes as i32 + delta) as usize;
            session.pending_publishes = new_pending;
            // Update global total
            state.total_pending_publishes = (state.total_pending_publishes as i32 + delta) as usize;

            debug!(%registration_id, pending = session.pending_publishes, "Pending publish count updated");

            // If we're in drain phase, check if we can complete
            if state.drain_phase_active && !state.drain_completion_notified {
                self.check_drain_complete(myself.clone(), state).await?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_registration_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionManagerState,
        registration_id: Option<String>,
        client_id: String,
        trace_ctx: Option<opentelemetry::Context>,
        peer_identity: Option<String>,
    ) -> Result<(), ActorProcessingErr> {
        let span =
            trace_span!("session_manager.registration_request", %client_id, ?registration_id);
        try_set_parent_otel(&span, trace_ctx);
        async {
            trace!("received registration request");

            if state.is_shutting_down {
                warn!("Rejecting registration request during shutdown");
                // Use stored listener_mgr to send response (listener manager will forward to correct listener)
                if let Some(listener_mgr) = &state.listener_mgr {
                    listener_mgr.send_message(BrokerMessage::RegistrationResponse {
                        client_id,
                        result: Err("Broker is shutting down".to_string()),
                        trace_ctx: Some(span.context()),
                    })?;
                    return Ok(());
                } else {
                    error!("SessionManager: listener_mgr not set, cannot send registration response");
                    return Ok(());
                }
            }

            let Some(listener_ref) = where_is(client_id.clone()) else {
                warn!("{REGISTRATION_REQ_FAILED_TXT} {CLIENT_NOT_FOUND_TXT}");
                return Ok(());
            };

            // Try to reuse an existing session. Only the identity that owns it may: a registration id is not a bearer token.
            if let Some(existing_id) = &registration_id {
                if let Some(session) = state.sessions.get(existing_id)
                    && !session.superseded
                    && !may_resume(session.identity.as_deref(), peer_identity.as_deref())
                {
                    warn!(
                        registration_id = %existing_id,
                        session_identity = ?session.identity,
                        connection_identity = ?peer_identity,
                        "a connection asked to resume a session that is not its identity's: giving it a new one"
                    );
                }
                if let Some(session) = state.sessions.get_mut(existing_id).filter(|session| {
                    !session.superseded
                        && may_resume(session.identity.as_deref(), peer_identity.as_deref())
                }) {
                    info!(registration_id = %existing_id, "reusing existing session");
                    session.detached_at = None;
                    if let Err(e) = session
                        .agent_ref
                        .send_message(BrokerMessage::ReassignClient {
                            client_ref: listener_ref.clone().into(),
                            trace_ctx: Some(span.context()),
                        })
                    {
                        error!("Failed to reassign client to existing session: {}", e);
                    } else {
                        let Some(_listener_mgr) = &state.listener_mgr else {
                            error!(
                                "SessionManager: listener_mgr not set, cannot send registration response"
                            );
                            return Ok(());
                        };
                        listener_ref.send_message(BrokerMessage::RegistrationResponse {
                            client_id: client_id.clone(),
                            result: Ok(existing_id.clone()),
                            trace_ctx: Some(span.context()),
                        })?;

                        // Only now that the session agent has been told of the new connection are
                        // the subscribers told to replay what they held for it. Told at the same
                        // time (as the broker used to), they replayed to the old, dead connection
                        // before it was replaced, and the messages failed again.
                        if let Some(subscriber_mgr) = &state.subscriber_mgr {
                            let _ = subscriber_mgr.send_message(BrokerMessage::RegistrationRequest {
                                registration_id: Some(existing_id.clone()),
                                client_id,
                                trace_ctx: Some(span.context()),
                                peer_identity: None,
                            });
                        } else {
                            warn!(
                                "SessionManager: subscriber_mgr not set, cannot rehydrate subscribers"
                            );
                        }
                        return Ok(());
                    }
                } else {
                    warn!(registration_id = %existing_id, "requested session not found, creating new one");
                }
            }

            // Create new session
            let new_id = Uuid::new_v4().to_string();
            info!(registration_id = %new_id, "starting session for client");

            match Actor::spawn_linked(
                Some(new_id.clone()),
                SessionAgent,
                SessionAgentArgs {
                    client_ref: listener_ref.clone().into(),
                },
                myself.clone().into(),
            )
            .await
            {
                Ok((session_agent, _)) => {
                    state.sessions.insert(
                        new_id.clone(),
                        Session {
                            agent_ref: session_agent.clone(),
                            pending_publishes: 0,
                            detached_at: None,
                            identity: peer_identity.clone(),
                            superseded: false,
                        },
                    );
                    // A client asking to resume a session that was superseded is the instance that
                    // lost it, still running. It is given a session of its own, but it does not
                    // displace the one that took over: two live instances would otherwise take
                    // turns closing each other's connections.
                    let is_the_loser = registration_id
                        .as_ref()
                        .is_some_and(|id| state.held.iter().any(|held| &held.registration_id == id));
                    if let Some(identity) = &peer_identity
                        && !is_the_loser
                    {
                        Self::supersede_older(state, identity, &new_id, &client_id);
                    }
                    // Pass broker ref to the session agent
                    let _ = session_agent.send_message(BrokerMessage::SetBrokerRef {
                        broker: state.broker.clone(),
                    });
                    let Some(_listener_mgr) = &state.listener_mgr else {
                        error!(
                            "SessionManager: listener_mgr not set, cannot send registration response for new session"
                        );
                        return Ok(());
                    };
                    listener_ref.send_message(BrokerMessage::RegistrationResponse {
                        client_id,
                        result: Ok(new_id.clone()),
                        trace_ctx: Some(span.context()),
                    })?;
                }
                Err(e) => {
                    error!(registration_id = %new_id, "failed to spawn session agent: {e}");
                }
            }
            Ok::<(), ActorProcessingErr>(())
        }
        .instrument(span.clone())
        .await
    }

    async fn handle_actor_terminated(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionManagerState,
        pending_publishes: usize,
    ) -> Result<(), ActorProcessingErr> {
        // Subtract pending publishes from the global total
        state.total_pending_publishes -= pending_publishes;

        // If we are in drain phase, check if we can complete
        if state.drain_phase_active && !state.drain_completion_notified {
            self.check_drain_complete(myself.clone(), state).await?;
        }
        Ok(())
    }

    async fn check_drain_complete(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionManagerState,
    ) -> Result<(), ActorProcessingErr> {
        if !state.drain_phase_active || state.drain_completion_notified {
            return Ok(());
        }

        // Calculate total pending publishes
        let total_pending = state.total_pending_publishes;
        let sessions_empty = state.sessions.is_empty();

        if total_pending == 0 && sessions_empty {
            info!("All sessions drained and terminated");
            self.signal_drain_complete(myself.clone(), state).await?;
        } else {
            debug!(
                total_pending,
                session_count = state.sessions.len(),
                "Still draining"
            );
        }

        Ok(())
    }

    async fn signal_drain_complete(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionManagerState,
    ) -> Result<(), ActorProcessingErr> {
        if state.drain_completion_notified {
            return Ok(());
        }
        state.drain_completion_notified = true;
        state.drain_phase_active = false;

        info!("All sessions drained and terminated");

        if let Some(supervisor) = myself.try_get_supervisor() {
            supervisor.send_message(BrokerMessage::ShutdownPhaseComplete {
                phase: ShutdownPhase::DrainExistingSessions,
            })?;
        }

        myself.stop(Some("SHUTDOWN_DRAINED".to_string()));
        Ok(())
    }
}

/// Worker process for handling client sessions.
pub struct SessionAgent;

pub struct SessionAgentArgs {
    pub client_ref: ActorRef<BrokerMessage>,
}

pub struct SessionAgentState {
    pub client_ref: ActorRef<BrokerMessage>,
    registration_id: String, // store for convenience
    shutting_down: bool,
    broker: Option<ActorRef<BrokerMessage>>, // will be set after creation
}

#[async_trait]
impl Actor for SessionAgent {
    type Msg = BrokerMessage;
    type State = SessionAgentState;
    type Arguments = SessionAgentArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let registration_id = myself.get_name().unwrap_or_default();
        Ok(SessionAgentState {
            client_ref: args.client_ref,
            registration_id,
            shutting_down: false,
            broker: None,
        })
    }

    async fn post_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<BrokerMessage>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            BrokerMessage::SetBrokerRef { broker } => {
                state.broker = Some(broker);
            }
            BrokerMessage::PrepareForShutdown { .. } => {
                self.handle_prepare_for_shutdown(state).await?;
            }
            BrokerMessage::RegistrationRequest {
                client_id,
                trace_ctx,
                ..
            } => {
                self.handle_re_registration_request(myself.clone(), state, client_id, trace_ctx)
                    .await?;
            }
            BrokerMessage::ReassignClient {
                client_ref,
                trace_ctx,
            } => {
                self.handle_reassign_client(state, client_ref, trace_ctx)
                    .await?;
            }
            BrokerMessage::CloseConnection { except } => {
                if state.client_ref.get_name().as_deref() != Some(except.as_str()) {
                    info!(registration_id = %state.registration_id, "closing the connection of a superseded session");
                    state.client_ref.stop(Some("SUPERSEDED".to_string()));
                }
            }
            BrokerMessage::PublishRequest {
                registration_id,
                topic,
                payload,
                reply_to,
                trace_ctx,
                published_by,
                id,
            } => {
                self.handle_publish_request(
                    myself.clone(),
                    state,
                    registration_id,
                    topic,
                    payload,
                    reply_to,
                    trace_ctx,
                    published_by,
                    id,
                )
                .await?;
            }
            BrokerMessage::PushMessage {
                payload,
                topic,
                trace_ctx,
                published_by,
                origin,
                delivery_id,
            } => {
                self.handle_push_message(
                    myself.clone(),
                    state,
                    payload,
                    topic,
                    trace_ctx,
                    published_by,
                    origin,
                    delivery_id,
                )
                .await?;
            }
            BrokerMessage::PublishRequestAck {
                topic,
                id,
                trace_ctx,
            } => {
                self.handle_publish_ack(myself.clone(), state, topic, id, trace_ctx)
                    .await?;
            }
            BrokerMessage::SubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => {
                self.handle_subscribe_request(
                    myself.clone(),
                    state,
                    registration_id,
                    topic,
                    trace_ctx,
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
            BrokerMessage::UnsubscribeAcknowledgment {
                registration_id,
                topic,
                result,
            } => {
                self.handle_unsubscribe_ack(myself.clone(), state, registration_id, topic, result)
                    .await?;
            }
            BrokerMessage::SubscribeAcknowledgment {
                registration_id,
                topic,
                result,
                trace_ctx,
            } => {
                self.handle_subscribe_ack(
                    myself.clone(),
                    state,
                    registration_id,
                    topic,
                    result,
                    trace_ctx,
                )
                .await?;
            }
            BrokerMessage::TimeoutMessage {
                client_id,
                registration_id,
                error,
            } => {
                self.handle_timeout_message(
                    myself.clone(),
                    state,
                    client_id,
                    registration_id,
                    error,
                )
                .await?;
            }
            BrokerMessage::ControlRequest {
                registration_id,
                op,
                trace_ctx,
                ..
            } => {
                self.handle_control_request(myself.clone(), state, registration_id, op, trace_ctx)
                    .await?;
            }
            BrokerMessage::ControlResponse {
                registration_id,
                result,
                trace_ctx,
            } => {
                self.handle_control_response(state, registration_id, result, trace_ctx)
                    .await?;
            }
            _ => {
                warn!("{}", format!("{UNEXPECTED_MESSAGE_STR}: {message:?}"));
            }
        }

        Ok(())
    }
}

impl SessionAgent {
    // ===== Handler methods for SessionAgent =====
    async fn handle_prepare_for_shutdown(
        &self,
        state: &mut SessionAgentState,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Session agent entering shutdown mode – will reject new requests");
        state.shutting_down = true;
        // Do NOT disconnect – client will disconnect when finished.
        Ok(())
    }

    async fn handle_re_registration_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionAgentState,
        client_id: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let registration_id = state.registration_id.clone();
        let span = trace_span!("session.re_registration_request", %client_id, %registration_id);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("received re-registration request");

        match where_is(client_id.clone()) {
            Some(listener) => {
                state.client_ref = ActorRef::from(listener);

                info!("re-established comms with client");
                if let Err(e) = state
                    .client_ref
                    .send_message(BrokerMessage::RegistrationResponse {
                        client_id: client_id.clone(),
                        result: Ok(registration_id.clone()),
                        trace_ctx: Some(span.context()),
                    })
                {
                    error!("failed to send registration ack to listener: {e}");
                }

                match myself.try_get_supervisor() {
                    Some(manager) => {
                        if let Err(e) = manager.send_message(BrokerMessage::RegistrationResponse {
                            client_id,
                            result: Ok(registration_id),
                            trace_ctx: Some(span.context()),
                        }) {
                            let err_msg = format!("{REGISTRATION_REQ_FAILED_TXT} {e}!");
                            error!("{err_msg}");
                            let _ = state.client_ref.send_message(
                                BrokerMessage::RegistrationResponse {
                                    client_id: state.client_ref.get_name().unwrap_or_default(),
                                    result: Err(err_msg),
                                    trace_ctx: Some(span.context()),
                                },
                            );
                            myself.stop(Some(e.to_string()));
                        }
                    }
                    None => warn!("couldn't find supervisor for session agent"),
                }
            }
            None => {
                warn!("could not find listener for client: {client_id}");
            }
        }
        Ok(())
    }

    async fn handle_reassign_client(
        &self,
        state: &mut SessionAgentState,
        client_ref: ActorRef<BrokerMessage>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("session.reassign_client", registration_id = %state.registration_id);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        debug!("reassigning client listener");
        state.client_ref = client_ref;
        // If the session had pending publishes, they will continue using the new client.
        // No need to notify subscribers; they are still attached to this session.
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_publish_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionAgentState,
        registration_id: String,
        topic: String,
        payload: Arc<Vec<u8>>,
        _reply_to: Option<ActorRef<BrokerMessage>>, // added but ignored; we'll use our own
        trace_ctx: Option<opentelemetry::Context>,
        published_by: Option<String>,
        id: MessageId,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("session.publish_request", %registration_id, %topic);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();

        if state.shutting_down {
            warn!("Rejecting publish during shutdown");
            return Ok(());
        }

        trace!("received publish request");

        // Clone the registration_id for use in multiple places
        let reg_id_clone = registration_id.clone();

        // Notify session manager that we're about to send a publish request
        if let Some(manager) = myself.try_get_supervisor() {
            manager
                .send_message(BrokerMessage::SessionPendingUpdate {
                    registration_id: reg_id_clone.clone(),
                    delta: 1,
                })
                .ok();
        }

        let broker = match &state.broker {
            Some(b) => b.clone(),
            None => {
                let err_msg = format!("{PUBLISH_REQ_FAILED_TXT} {BROKER_NOT_FOUND_TXT}");
                error!("{err_msg}");
                let _ = state
                    .client_ref
                    .send_message(BrokerMessage::PublishResponse {
                        topic,
                        payload: Arc::new(Vec::new()),
                        result: Err(err_msg),
                        trace_ctx: Some(span.context()),
                        published_by: None,
                        origin: Some(id),
                        delivery_id: None,
                    });
                // no broker -> timeout path (client may be fine; broker is not)
                let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                    client_id: state.client_ref.get_name().unwrap_or_default(),
                    registration_id,
                    error: Some("broker not found".to_owned()),
                });

                // Notify manager that this publish failed immediately (delta -1)
                if let Some(manager) = myself.try_get_supervisor() {
                    manager
                        .send_message(BrokerMessage::SessionPendingUpdate {
                            registration_id: reg_id_clone.clone(),
                            delta: -1,
                        })
                        .ok();
                }
                return Ok(());
            }
        };

        if let Err(e) = broker.send_message(BrokerMessage::PublishRequest {
            registration_id: reg_id_clone.clone(),
            topic,
            payload,
            // Set reply_to to ourselves so the topic agent can ack us directly
            reply_to: Some(myself.clone()),
            trace_ctx: Some(span.context()),
            published_by,
            id,
        }) {
            error!("failed to forward publish to broker: {e}");
            // Notify manager that publish failed (delta -1)
            if let Some(manager) = myself.try_get_supervisor() {
                manager
                    .send_message(BrokerMessage::SessionPendingUpdate {
                        registration_id: reg_id_clone.clone(),
                        delta: -1,
                    })
                    .ok();
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_push_message(
        &self,
        _myself: ActorRef<BrokerMessage>,
        state: &mut SessionAgentState,
        payload: Arc<Vec<u8>>,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
        published_by: Option<String>,
        origin: Option<MessageId>,
        delivery_id: u64,
    ) -> Result<(), ActorProcessingErr> {
        let registration_id = state.registration_id.clone();
        let span = trace_span!("session.push_message", %registration_id, %topic);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("received push directive; forwarding to client listener");

        if let Err(e) = state.client_ref.cast(BrokerMessage::PublishResponse {
            topic: topic.clone(),
            payload,
            result: Ok(()),
            trace_ctx: Some(span.context()),
            published_by,
            origin,
            delivery_id: Some(delivery_id),
        }) {
            warn!("{PUBLISH_REQ_FAILED_TXT}: {CLIENT_NOT_FOUND_TXT}: {e}");

            if let Some(subscriber) = where_is(get_subscriber_name(&registration_id, &topic)) {
                subscriber
                    .send_message(BrokerMessage::PushMessageFailed { delivery_id })
                    .inspect_err(|_| {
                        warn!("failed to tell the subscriber on {topic} a delivery failed")
                    })
                    .ok();
            }
        }
        Ok(())
    }

    async fn handle_publish_ack(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionAgentState,
        topic: String,
        id: MessageId,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let registration_id = state.registration_id.clone();

        let span = trace_span!("session.publish_request_ack", %registration_id, %topic);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("received publish request ack");

        if let Err(e) = state
            .client_ref
            .send_message(BrokerMessage::PublishRequestAck {
                topic,
                id,
                trace_ctx: Some(span.context()),
            })
        {
            error!("failed to forward publish ack to client: {e}");
            let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                client_id: state.client_ref.get_name().unwrap_or_default(),
                registration_id: registration_id.clone(),
                error: Some(format!("{e}")),
            });
        }

        // Notify session manager that a pending publish has completed
        if let Some(manager) = myself.try_get_supervisor() {
            let _ = manager.send_message(BrokerMessage::SessionPendingUpdate {
                registration_id: registration_id.clone(),
                delta: -1,
            });
        }
        Ok(())
    }

    async fn handle_subscribe_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionAgentState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("session.subscribe_request", %registration_id, %topic);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();

        if state.shutting_down {
            warn!("Rejecting subscribe during shutdown");
            // Refused, and the client told so: a silent drop looks like a subscription that was taken.
            crate::topic::acknowledge_subscribe(
                &registration_id,
                &topic,
                Err("Broker is shutting down".to_string()),
                Some(span.context()),
            );
            return Ok(());
        }

        trace!("received subscribe request");

        let broker = match &state.broker {
            Some(b) => b.clone(),
            None => {
                error!("{SUBSCRIBE_REQUEST_FAILED_TXT} {BROKER_NOT_FOUND_TXT}");
                let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                    client_id: state.client_ref.get_name().unwrap_or_default(),
                    registration_id,
                    error: Some("broker not found".to_owned()),
                });
                return Ok(());
            }
        };

        if let Err(e) = broker.send_message(BrokerMessage::SubscribeRequest {
            registration_id,
            topic,
            trace_ctx: Some(span.context()),
        }) {
            error!("failed to forward subscribe to broker: {e}");
        }
        Ok(())
    }

    async fn handle_unsubscribe_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionAgentState,
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("session.unsubscribe_request", %registration_id, %topic);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();

        trace!("received unsubscribe request");

        let broker = match &state.broker {
            Some(b) => b.clone(),
            None => {
                error!("{BROKER_NOT_FOUND_TXT}");
                let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                    client_id: state.client_ref.get_name().unwrap_or_default(),
                    registration_id,
                    error: Some("broker not found".to_owned()),
                });
                return Ok(());
            }
        };

        if let Err(e) = broker.send_message(BrokerMessage::UnsubscribeRequest {
            registration_id,
            topic,
            trace_ctx: Some(span.context()),
        }) {
            error!("failed to forward unsubscribe to broker: {e}");
        }
        Ok(())
    }

    async fn handle_unsubscribe_ack(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionAgentState,
        registration_id: String,
        topic: String,
        result: Result<(), String>,
    ) -> Result<(), ActorProcessingErr> {
        if let Err(e) = state
            .client_ref
            .send_message(BrokerMessage::UnsubscribeAcknowledgment {
                registration_id,
                topic,
                result,
            })
        {
            error!("failed to forward unsubscribe ack to client: {e}");
            let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                client_id: state.client_ref.get_name().unwrap_or_default(),
                registration_id: myself.get_name().unwrap_or_default(),
                error: Some(format!("{e}")),
            });
        }
        Ok(())
    }

    async fn handle_subscribe_ack(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionAgentState,
        registration_id: String,
        topic: String,
        result: Result<(), String>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("session.subscribe_ack", %registration_id, %topic);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("received subscribe acknowledgment");

        match myself.try_get_supervisor() {
            Some(manager) => {
                let _ = manager.send_message(BrokerMessage::SubscribeAcknowledgment {
                    registration_id: registration_id.clone(),
                    topic: topic.clone(),
                    result: result.clone(),
                    trace_ctx: Some(span.context()),
                });

                if let Err(e) =
                    state
                        .client_ref
                        .send_message(BrokerMessage::SubscribeAcknowledgment {
                            registration_id,
                            topic,
                            result,
                            trace_ctx: Some(span.context()),
                        })
                {
                    error!("failed to forward subscribe ack to client: {e}");
                    let _ = myself.send_message(BrokerMessage::TimeoutMessage {
                        client_id: state.client_ref.get_name().unwrap_or_default(),
                        registration_id: myself.get_name().unwrap_or_default(),
                        error: Some(format!("{e}")),
                    });
                }
            }
            None => return Err(ActorProcessingErr::from("Failed to find supervisor")),
        }
        Ok(())
    }

    async fn handle_timeout_message(
        &self,
        myself: ActorRef<BrokerMessage>,
        _state: &mut SessionAgentState,
        client_id: String,
        registration_id: String,
        error: Option<String>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("session.forward_timeout", %client_id, %registration_id);
        let _g = span.enter();

        trace!("forwarding timeout to manager");

        if let Some(manager) = myself.try_get_supervisor() {
            let _ = manager.send_message(BrokerMessage::TimeoutMessage {
                client_id,
                registration_id,
                error,
            });
        } else {
            error!("couldn't find supervisor");
        }
        Ok(())
    }

    async fn handle_control_request(
        &self,
        myself: ActorRef<BrokerMessage>,
        state: &mut SessionAgentState,
        registration_id: String,
        op: ControlOp,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("session.control_request", %registration_id);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("received control request");

        // Look up the broker actor directly (it's not our supervisor)
        let broker = match &state.broker {
            Some(b) => b.clone(),
            None => {
                error!("{BROKER_NOT_FOUND_TXT}");
                let _ = state
                    .client_ref
                    .send_message(BrokerMessage::ControlResponse {
                        registration_id,
                        result: Err(ControlError::InternalError("broker not found".to_string())),
                        trace_ctx: Some(span.context()),
                    });
                return Ok(());
            }
        };

        if let Err(e) = broker.send_message(BrokerMessage::ControlRequest {
            registration_id: registration_id.clone(),
            op,
            reply_to: Some(myself.clone()),
            trace_ctx: Some(span.context()),
        }) {
            error!("failed to forward control request to broker: {e}");
            let _ = state
                .client_ref
                .send_message(BrokerMessage::ControlResponse {
                    registration_id,
                    result: Err(ControlError::InternalError(format!(
                        "Failed to process message. {e}"
                    ))),
                    trace_ctx: Some(span.context()),
                });
        }
        Ok(())
    }

    async fn handle_control_response(
        &self,
        _state: &mut SessionAgentState,
        registration_id: String,
        result: Result<ControlResult, ControlError>,
        trace_ctx: Option<opentelemetry::Context>,
    ) -> Result<(), ActorProcessingErr> {
        let span = trace_span!("session.control_response", %registration_id);
        try_set_parent_otel(&span, trace_ctx);
        let _g = span.enter();
        trace!("received control response");

        let _ = _state
            .client_ref
            .send_message(BrokerMessage::ControlResponse {
                registration_id,
                result,
                trace_ctx: Some(span.context()),
            });
        Ok(())
    }
}

/// Whether a connection presenting `connection` may resume a session that belongs to `session`: only the same
/// identity, and never when either is unknown (a certificate with no DNS name has no identity to match). Pure.
pub fn may_resume(session: Option<&str>, connection: Option<&str>) -> bool {
    matches!((session, connection), (Some(s), Some(c)) if s == c)
}
