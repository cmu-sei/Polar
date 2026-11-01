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

use ractor::{ActorProcessingErr, ActorRef, RpcReplyPort};
use rkyv::{Archive, Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub mod held;
pub mod socket;
pub mod trace;
pub use held::{Backlog, Hold, Payload};
pub use trace::WireTraceCtx;

pub type SessionMap = HashMap<String, SessionDetails>;

/// What the subscriber manager answers a `CreateSubscriber` with: the subscriber, which owns its backlog and tells its
/// topic whether it holds the topic back (`TopicHold`).
pub type CreatedSubscriber = Result<ActorRef<BrokerMessage>, ActorProcessingErr>;

/// A message a subscriber keeps for its client: delivered and not acknowledged, or not yet delivered.
#[derive(Debug, Clone)]
pub struct HeldDelivery {
    pub payload: Payload,
    /// Who published it, if they shared it.
    pub published_by: Option<String>,
    /// Its publisher's number for it.
    pub origin: Option<MessageId>,
}

/// What a held subscriber hands over: each message it kept, oldest first.
pub type HeldMessages = Vec<HeldDelivery>;

/// The largest frame either end reads or writes: a hard cap on a frame's declared length, checked before it sizes an
/// allocation. Without it, `read_u32` handed a value like `0xFFFFFFFF` turns directly into a ~4 GiB `vec![0u8; len]`.
/// 64 MiB, decided 2026-10-30 (docs/architecture/cassini-backpressure.md, "Decided" 3): a connection's window is never
/// below one frame, and a broker whose memory budget is below it refuses to start.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// The room a frame keeps beside its payload for the rest of the message (the topic, the ids, the trace context).
pub const FRAME_ENVELOPE_ALLOWANCE: usize = 64 * 1024;

/// The largest payload a client takes to publish: what fits in one frame beside its envelope. A larger one is refused
/// with an error to the caller, before it is sent.
pub const MAX_PAYLOAD_BYTES: usize = MAX_FRAME_BYTES - FRAME_ENVELOPE_ALLOWANCE;

/// Our representation of a connected session, and how close it is to timing out
/// TODO: Eventually, we might want to add metadata to the session struct to track additional information.
/// Consider stuff like last_activity_time, last_message_received_time, etc.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Serialize, Deserialize, Archive)]
pub struct SessionDetails {
    pub registration_id: String,
    pub subscriptions: HashSet<String>,
    // last_activity: u64,
}

/// Helper to distinguish why a listener's connection may have died.
///
#[derive(Debug, Clone)]
pub enum DisconnectReason {
    /// The client said goodbye: it sent an explicit disconnect and is not coming back for this
    /// session, so the broker can end it at once.
    Goodbye,
    /// The connection closed with no goodbye (the client died, was killed, or lost its network).
    /// The session is kept for a while in case the client returns.
    RemoteClosed,
    //TODO: It's possible we may eventually want to detect rougue clients, this would let us be explicit.
    // PolicyViolation(PolicyViolation),
    TransportError(String),
}

/// A published message's number: the `epoch` its publisher's client chose when it started, and its `seq` within it.
/// The pair names one message of one publisher; nothing else about it is promised (an epoch is random, so two epochs
/// are not ordered).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    Serialize,
    Deserialize,
    Archive,
)]
pub struct MessageId {
    pub epoch: u64,
    pub seq: u64,
}

/// The publisher's own identity, attached to a published message so a
/// subscriber can know who sent it -- extracted by the broker from
/// the publisher's verified mTLS client certificate at connection
/// time, never client-supplied. `share_identity` is the one thing a
/// publisher genuinely controls: whether this known identity is
/// actually forwarded to subscribers on delivery, or suppressed for
/// this specific publish. A publisher can choose to stay anonymous,
/// but can never spoof a different identity -- `identity` itself is
/// always broker-derived.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    Serialize,
    Deserialize,
    Archive,
)]
pub struct PublisherContext {
    pub identity: String,
    pub share_identity: bool,
}

/// Internal messagetypes for the Broker.
/// Activities that flow from an actor will also be traced leveraging Contexts,
/// These are optional because they aren't initialzied until the listener begins to handle the message
/// Because of this, they will be often left out of the listener's handler at first
#[derive(Debug)]
pub enum BrokerMessage {
    /// Initiate graceful shutdown sequence
    PrepareForShutdown { auth_token: Option<String> },

    /// Control message to initiate a specific shutdown phase
    InitiateShutdownPhase { phase: ShutdownPhase },

    /// Signal that a shutdown phase has been completed by a manager
    ShutdownPhaseComplete { phase: ShutdownPhase },

    /// Sent by SessionAgent to SessionManager to report a change in pending publish count
    SessionPendingUpdate {
        registration_id: String,
        delta: i32, // +1 for new publish, -1 for completed publish
    },

    /// Instruct a topic actor to flush its queue and report completion
    FlushQueue { reply_to: ActorRef<BrokerMessage> },

    /// Sent by a topic actor to TopicManager when its queue is flushed
    FlushQueueComplete { topic: String },

    /// Periodic check for shutdown completion (sent by broker to itself)
    CheckShutdownComplete,

    /// Registration request from the client.
    /// When a client connects over TCP, it cannot send messages until it receives a registrationID and a session has been created for it
    /// In the event of a disconnect, a client should be able to either resume their session by providing that registration ID, or
    /// have a new one assigned to it by sending an empty registration request
    RegistrationRequest {
        //Id for a new, potentially unauthenticated/unauthorized client client
        registration_id: Option<String>,
        client_id: String,
        trace_ctx: Option<opentelemetry::Context>,
        /// The connecting client's own DNS-SAN identity, extracted by
        /// the broker from their verified mTLS client certificate at
        /// TLS-accept time. Never client-supplied -- the wire-level
        /// ClientMessage::RegistrationRequest never carries this at
        /// all, since the client has no way to send it.
        peer_identity: Option<String>,
    },
    /// Reassign a session to a new client listener (used during reconnection)
    ReassignClient {
        client_ref: ActorRef<BrokerMessage>,
        trace_ctx: Option<opentelemetry::Context>,
    },
    GetSessions {
        reply_to: RpcReplyPort<SessionMap>,
        trace_ctx: Option<opentelemetry::Context>,
    },
    /// A heartbeat tick messgae sent by sessions to track uptime
    HeartbeatTick,
    /// Registration response to the client after attempting registration
    /// Ok result contains new registration id,
    /// Err shoudl contain an error message
    RegistrationResponse {
        client_id: String,
        result: Result<String, String>,
        trace_ctx: Option<opentelemetry::Context>,
        /// With an `Ok`, the session registered: the Listener binds its connection to it before it tells the client,
        /// so whatever the client sends once told reaches a bound connection. `None` with an `Err`.
        session: Option<ActorRef<BrokerMessage>>,
    },
    /// Publish request from the client.
    PublishRequest {
        registration_id: String,
        topic: String,
        /// Charged against the broker's memory budget from when its reader admitted it.
        payload: Payload,
        reply_to: Option<ActorRef<BrokerMessage>>,
        trace_ctx: Option<opentelemetry::Context>,
        /// Already resolved by from_client_message at decode time --
        /// the publisher's own peer_identity if they chose to share
        /// it, None otherwise. Everything downstream (topic/subscriber
        /// routing, delivery) just propagates this as-is; nothing past
        /// decode time needs to know about share_identity/peer_identity
        /// separately again.
        published_by: Option<String>,
        /// The message's `(epoch, seq)` as its publisher numbered it, returned in its acknowledgement.
        id: MessageId,
        /// The publisher's connection's ingress credit for this frame, let go when the topic accepts the publish (or
        /// wherever the publish is dropped): until then the connection's reader counts it against its window.
        window: Hold,
    },
    /// Publish response to the client -- also reused, unmodified in
    /// shape, as the delivery vehicle when forwarding a published
    /// message to a subscriber (see session.rs's handle_push_message).
    /// published_by is only meaningful in that second case; for the
    /// original publisher's own ack it's left None.
    PublishResponse {
        topic: String,
        payload: Payload,
        result: Result<(), String>,
        trace_ctx: Option<opentelemetry::Context>,
        published_by: Option<String>,
        /// The `(epoch, seq)` of the publish this is about: the refused one, for a refusal; the delivered one's, as its
        /// publisher numbered it, for a delivery.
        origin: Option<MessageId>,
        /// For a delivery to a client, its number, which the client returns in `PublishResponseAck`; none for a
        /// refusal, and none on its way from a topic to a subscriber, which numbers it.
        delivery_id: Option<u64>,
        /// For a refusal, what kind it is; none for a delivery.
        refusal: Option<PublishRefusal>,
    },
    /// Message sent to the client to let them know they successfully published a message: the topic accepted the
    /// publish numbered `id`.
    PublishRequestAck {
        topic: String,
        id: MessageId,
        trace_ctx: Option<opentelemetry::Context>,
    },
    /// The client has the delivery numbered `delivery_id`: its subscriber lets go of it. From the client to its
    /// Listener, which passes it to the subscriber of the topic the delivery was on.
    PublishResponseAck { delivery_id: u64 },
    /// The client could not decode the `frame`th frame its Listener wrote (`ClientMessage::FrameUndecodable`).
    FrameUndecodable { frame: u64 },
    /// Internal, from a Listener's reader to its Listener: the client's registration is of another protocol version,
    /// or does not decode; it is refused by name, and the connection closed.
    RefuseRegistration { refusal: Refusal },
    /// Internal, from a subscriber whose client is away to its session: the budget was exhausted, and it let go of
    /// what it held (`count` messages on `topic`). The session asks to be ended (cassini-backpressure.md, the
    /// decisions, 14).
    SessionShed { topic: String, count: usize },
    /// Internal, from a session agent to the session manager: end this session now, before the retention window, for
    /// `reason`; a client that comes back asking for it is told.
    EndSession {
        registration_id: String,
        reason: String,
    },
    /// Internal, from the session manager to a Listener: the session its client asked to resume was ended early, for
    /// `reason` (written to the client as `ClientMessage::SessionEnded`, before its new session).
    SessionEnded {
        registration_id: String,
        reason: String,
    },
    /// Subscribe request from the client.
    SubscribeRequest {
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    },
    /// Sent to the subscriber manager to create a new subscriber actor to handle pushing messages to the client.
    /// If successful, the associated topic actor is notified, adding the id of the new actor to it's subscriber list
    CreateSubscriber {
        topic: String,
        registration_id: String,
        trace_ctx: Option<opentelemetry::Context>,
        reply: RpcReplyPort<CreatedSubscriber>,
    },
    AddSubscriber {
        registration_id: String,
        subscriber_ref: ActorRef<BrokerMessage>,
        trace_ctx: Option<opentelemetry::Context>,
    },
    /// instructs the topic manager to create a new topic actor,
    /// optionally at the behest of a session client during the processing of a SubscribeRequest
    /// which would also prompt the creation of a subscriber agent for that topic.
    AddTopic {
        registration_id: Option<String>,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    },
    GetTopics {
        registration_id: String,
        reply_to: RpcReplyPort<HashSet<String>>,
        trace_ctx: Option<opentelemetry::Context>,
    },
    /// Sent to session actors to forward messages to their clients.
    /// Messages that fail to be delivered for some reason are kept in their queues.
    PushMessage {
        // reply: RpcReplyPort<Result<(), String>>,
        payload: Payload,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
        published_by: Option<String>,
        origin: Option<MessageId>,
        /// The subscriber's number for this delivery, which the client acknowledges.
        delivery_id: u64,
    },
    /// Sent back to subscription actors if sessions fail to forward messages to the client. The subscriber still
    /// holds the delivery (it holds every delivery until it is acknowledged), and sends it again on a resume.
    PushMessageFailed { delivery_id: u64 },
    /// Subscribe acknowledgment to the client.
    SubscribeAcknowledgment {
        registration_id: String,
        topic: String,
        result: Result<(), String>, // Ok for success, Err with error message
        trace_ctx: Option<opentelemetry::Context>,
    },
    /// Unsubscribe request from the client.
    UnsubscribeRequest {
        registration_id: String,
        topic: String,
        trace_ctx: Option<opentelemetry::Context>,
    },
    /// Unsubscribe acknowledgment to the client.
    UnsubscribeAcknowledgment {
        registration_id: String,
        topic: String,
        result: Result<(), String>, // Ok for success, Err with error message
    },
    /// Disconnect request from the client.
    /// Internal: time for the session manager to end the disconnected sessions that have been
    /// kept for longer than the retention window.
    SessionExpiryTick,
    /// Internal: a client came back as a new session, so this detached session of the same
    /// identity can no longer be resumed. The subscriber manager has its subscribers hold what
    /// they receive, and answers with `SessionHeld`.
    SessionSuperseded { registration_id: String },
    /// Internal: a session was superseded while its client still looked connected. The session
    /// agent closes that connection, unless it is the one named (a client that registered a
    /// second session on the connection it already had).
    CloseConnection { except: String },
    /// Internal: every subscriber of the superseded session is holding; the session may stop.
    SessionHeld { registration_id: String },
    /// Internal: which sessions of the same identity as `registration_id` are held for it, oldest
    /// first. Answered once every takeover in progress for that identity has completed.
    GetHeldSessions {
        registration_id: String,
        reply_to: ractor::RpcReplyPort<Vec<String>>,
    },
    /// Internal: a Listener decides its client's subscriptions again, and drops those now refused, telling the
    /// client (docs/architecture/policy-enforcement.md, "Decisions": held subscriptions are judged again). `None` is
    /// every subscription it holds, after a change of authority; a list is a resumed session's.
    JudgeSubscriptions { topics: Option<Vec<String>> },
    /// Internal: the topics a session is subscribed to, which a Listener judges when the session is resumed.
    SubscriptionsOf {
        registration_id: String,
        reply_to: ractor::RpcReplyPort<Vec<String>>,
    },
    /// Internal: a subscriber of a superseded session keeps what it receives instead of
    /// forwarding it, and stays subscribed.
    HoldSubscription { reply: ractor::RpcReplyPort<()> },
    /// Internal: a held subscriber hands over what it holds, and ends.
    ClaimHeld {
        reply: ractor::RpcReplyPort<HeldMessages>,
    },
    DisconnectRequest {
        reason: DisconnectReason,
        client_id: String,
        registration_id: Option<String>,
        trace_ctx: Option<opentelemetry::Context>,
    },
    /// Error message to the client.
    ErrorMessage { client_id: String, error: String },
    TimeoutMessage {
        client_id: String,
        ///name of the session agent that died
        registration_id: String,
        error: Option<String>,
        // trace_ctx: Option<Context>,
    },
    /// control-plane message, reply_to is session ActorRef
    ControlRequest {
        registration_id: String,
        op: ControlOp,
        reply_to: Option<ActorRef<BrokerMessage>>, // session actor ref
        trace_ctx: Option<opentelemetry::Context>,
    },
    // Control response forwarded back across broker actor layers (or directly by ControlManager)
    ControlResponse {
        registration_id: String,
        result: Result<ControlResult, ControlError>,
        trace_ctx: Option<opentelemetry::Context>,
    },

    /// Internal message for ListenerManager to register a newly spawned listener.
    RegisterListener {
        client_id: String,
        listener_ref: ActorRef<BrokerMessage>,
    },
    /// Internal message for SessionAgent to receive the broker reference.
    SetBrokerRef { broker: ActorRef<BrokerMessage> },
    /// Internal message for SubscriberManager to get a session actor ref.
    GetSessionRef {
        registration_id: String,
        reply_to: RpcReplyPort<Option<ActorRef<BrokerMessage>>>,
    },
    /// Internal message for SessionManager to receive its listener manager ref.
    SetListenerManager {
        listener_mgr: ActorRef<BrokerMessage>,
    },
    /// Internal message for SubscriberManager to receive its topic manager ref.
    SetTopicManagerRef { topic_mgr: ActorRef<BrokerMessage> },
    /// Internal message for SessionManager to receive its subscriber manager ref.
    SetSubscriberManagerRef {
        subscriber_mgr: ActorRef<BrokerMessage>,
    },
    /// Periodic self-tick for a TopicAgent to check its own queue for
    /// messages that have been waiting too long with no subscriber to
    /// receive them.
    QueueExpiryTick,
    /// Internal, from a subscriber to its topic: it holds the topic back (connected and above its high water), or no
    /// longer does (below its low water, away, or ended). The topic accepts nothing while any does
    /// (docs/architecture/cassini-backpressure.md, phase 1).
    TopicHold {
        registration_id: String,
        holds: bool,
    },
    /// Internal, from a topic to a subscriber it takes: where to say whether it holds the topic back.
    TopicAttached { topic: ActorRef<BrokerMessage> },
    /// Internal, from the pressure manager through the subscriber manager to each subscriber: look whether you may
    /// have stalled (above your high water for the stall bound), and say so to your session.
    StallCheck,
    /// Internal, from a subscriber to its session: it may have stalled, for `stalled_for`, holding `held` bytes, above
    /// its high water or with nothing acknowledged while the budget is exhausted. The session decides from its own
    /// facts, by the one stall policy (`pressure::verdict`): it ends the connection only if that made no progress for
    /// the stall bound, and tells `subscriber` if it ended it (`StallEnded`).
    SubscriberStalled {
        topic: String,
        held: usize,
        stalled_for: std::time::Duration,
        stall: std::time::Duration,
        subscriber: ActorRef<BrokerMessage>,
    },
    /// Internal, from a session to a stalled subscriber: its connection was ended for the stall (counted).
    StallEnded,
    /// Internal, from a session agent to the session manager: it ended its connection for a stall (logged with its
    /// identity).
    SessionStallEnded {
        registration_id: String,
        topic: String,
        held: usize,
        stalled_for: std::time::Duration,
    },
    /// Internal, from a Listener to its session: its connection, and how to end it or make its reader refuse for now
    /// from outside the Listener (which may be waiting on a write to its client).
    ConnectionOpened {
        client_id: String,
        control: Arc<dyn ConnectionControl>,
    },
    /// Internal, from a Listener's reader to its session: it read an acknowledgement (at most a few a second).
    ConnectionProgress { client_id: String },
    /// Internal, from a Listener's reader to its session: it waits for a frame's admission, or no longer does; while
    /// it waits, what follows the frame (its client's acknowledgements among it) is not read.
    ConnectionWaiting { client_id: String, waiting: bool },
    /// Internal, from the session manager to the subscriber manager: the session's client went away; its
    /// subscribers hold their topics back no longer, and keep what they hold while the budget allows.
    SessionDetached { registration_id: String },
    /// Internal, from the pressure manager through the subscriber manager to each subscriber, at each stall check
    /// while the broker's memory budget is exhausted: one whose client is away lets go of what it holds, so the budget
    /// can recover (what a subscriber is owed is kept only while the budget allows); one connected looks whether it has
    /// stalled, as on `StallCheck`, counting time without an acknowledgement too.
    BudgetExhausted,
}

/// How a session reaches its connection from outside the connection's Listener, which may be waiting on a write to a
/// client that does not read: given by the Listener (`BrokerMessage::ConnectionOpened`), so nothing is looked up.
pub trait ConnectionControl: Send + Sync + std::fmt::Debug {
    /// Ends the connection, saying `why`: its writes and its reader give up, and its Listener ends.
    fn cut(&self, why: &str);
    /// Its reader refuses for now, for `how_long`, each publish it cannot admit at once, and stops waiting for the
    /// one it waits for, so it reads on to what follows.
    fn refuse_for(&self, how_long: std::time::Duration);
}

impl BrokerMessage {
    /// `peer_identity` is passed separately, exactly like `client_id`
    /// already is -- both are connection-derived, broker-side facts
    /// the wire message itself never carries. Only meaningful for the
    /// RegistrationRequest arm; every other arm ignores it.
    pub fn from_client_message(
        msg: ClientMessage,
        client_id: String,
        peer_identity: Option<String>,
    ) -> Self {
        match msg {
            // Its protocol is judged by the Listener's reader before it is converted.
            ClientMessage::RegistrationRequest {
                registration_id,
                trace_ctx,
                ..
            } => BrokerMessage::RegistrationRequest {
                registration_id,
                client_id,
                trace_ctx: trace_ctx.map(|w| w.to_opentelemetry_context()),
                peer_identity,
            },
            ClientMessage::PublishRequest {
                topic,
                payload,
                registration_id,
                trace_ctx,
                share_identity,
                epoch,
                seq,
            } => BrokerMessage::PublishRequest {
                registration_id,
                topic,
                // Not charged: a reader that admits a frame charges it itself (the Listener's read task).
                payload: Payload::uncharged(payload),
                // Client doesn't know the session actor, so reply_to is None
                reply_to: None,
                trace_ctx: trace_ctx.map(|w| w.to_opentelemetry_context()),
                // Resolved once, here, at decode time -- the earliest
                // point both share_identity (from the wire message)
                // and peer_identity (this connection's own,
                // broker-derived identity) are simultaneously
                // available.
                published_by: if share_identity {
                    peer_identity.clone()
                } else {
                    None
                },
                id: MessageId { epoch, seq },
                window: Hold::none(),
            },
            ClientMessage::SubscribeRequest {
                topic,
                registration_id,
                trace_ctx,
            } => BrokerMessage::SubscribeRequest {
                registration_id,
                topic,
                trace_ctx: trace_ctx.map(|w| w.to_opentelemetry_context()),
            },
            ClientMessage::UnsubscribeRequest {
                registration_id,
                topic,
                trace_ctx,
            } => BrokerMessage::UnsubscribeRequest {
                registration_id,
                topic,
                trace_ctx: trace_ctx.map(|w| w.to_opentelemetry_context()),
            },
            ClientMessage::DisconnectRequest {
                registration_id,
                trace_ctx,
            } => BrokerMessage::DisconnectRequest {
                client_id,
                registration_id,
                trace_ctx: trace_ctx.map(|w| w.to_opentelemetry_context()),
                reason: DisconnectReason::Goodbye,
            },
            ClientMessage::ControlRequest {
                registration_id,
                op,
                trace_ctx,
            } => BrokerMessage::ControlRequest {
                registration_id,
                op,
                reply_to: None,
                trace_ctx: trace_ctx.map(|w| w.to_opentelemetry_context()),
            },
            ClientMessage::PublishResponseAck { delivery_id } => {
                BrokerMessage::PublishResponseAck { delivery_id }
            }
            ClientMessage::FrameUndecodable { frame } => BrokerMessage::FrameUndecodable { frame },

            _ => {
                // For other message types, we don't have conversion yet
                BrokerMessage::ErrorMessage {
                    client_id,
                    error: format!("Unsupported message conversion: {:?}", msg),
                }
            }
        }
    }
}

/// Phases of graceful shutdown
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutdownPhase {
    /// Stop accepting new connections and reject new messages
    StopAcceptingNewConnections,
    /// Allow existing sessions to complete/drain
    DrainExistingSessions,
    /// Flush all queued messages in topics
    FlushTopicQueues,
    /// Terminate subscriber agents
    TerminateSubscribers,
    /// Terminate all listener actors and stop the listener manager
    TerminateListeners,
}

// ========================
// Control Operation Types
// ========================
#[derive(Debug, Clone, Serialize, Archive, Deserialize)]
pub enum ControlOp {
    // Session Management
    GetSessionInfo { registration_id: String },
    DisconnectSession { registration_id: String },
    ListSessions,

    // Topic / Subscription Management
    ListTopics,
    ListSubscribers { topic: String },

    // Shutdown
    PrepareForShutdown { auth_token: String },

    // Broker / System
    GetBrokerStats,
    ShutdownBroker { graceful: bool },

    // Diagnostics
    Ping,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Serialize, Archive, Deserialize)]
pub enum ControlResult {
    SessionInfo(SessionDetails),
    SessionList(SessionMap),
    SubscriberList(Vec<String>),
    TopicList(HashSet<String>),
    // BrokerStats(BrokerStats),
    Pong,
    Disconnected,
    ShutdownInitiated,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Serialize, Archive, Deserialize)]
pub enum ControlError {
    NotFound(String),
    PermissionDenied(String),
    InternalError(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Archive)]
pub struct SessionSummary {
    pub registration_id: String,
}

/// The words the broker gives a revoked certificate that asks to register, for people; its code, `RefusalCode::Revoked`,
/// is what the custody daemon's custodian reads its own revocation by (docs/architecture/key-custody.md, "Part A's
/// bootstrap: the custodian").
pub const REVOKED_REFUSAL: &str = "the broker refused: this certificate is revoked";

/// What a refused registration means. A reader acts on this, never on the refusal's text, which is for people and may
/// change: an older daemon that matched the text did not hear its revocation once the text changed (2026-10-28,
/// `remaining-work.md`).
#[derive(Serialize, Deserialize, Archive, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalCode {
    /// The certificate the connection presented is revoked; the broker closes the connection.
    Revoked,
    /// Any other refusal: the broker shutting down, or a session it could not find or make.
    Other,
    /// The client speaks another protocol version than the broker (`PROTOCOL_VERSION`): a client and broker must be
    /// built together; the broker closes the connection, and the client waits before it tries again. Last, so no
    /// earlier code's tag moves.
    ProtocolMismatch,
}

/// The version of the protocol between Cassini's clients and its broker: sent at registration, refused by the broker
/// when it is not its own. Raised with every change to the wire.
pub const PROTOCOL_VERSION: u32 = 1;

/// What a refused publish means, for the client to act on (a reader never acts on the refusal's words).
#[derive(
    Serialize,
    Deserialize,
    Archive,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum PublishRefusal {
    /// The broker will not take it: its policy, or the connection's certificate, refuses it. The client lets go of it
    /// and tells the agent, which may be refused this topic.
    Denied,
    /// The broker cannot take it now (it is shutting down, a part of it is not there, its memory budget is
    /// exhausted): the client keeps it and sends it again later, on its next registration or after a wait.
    Unavailable,
}

/// A refused registration: its code, and why in words.
#[derive(Serialize, Deserialize, Archive, Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: RefusalCode,
    pub reason: String,
}

impl Refusal {
    /// The refusal of a revoked certificate.
    pub fn revoked() -> Self {
        Refusal {
            code: RefusalCode::Revoked,
            reason: REVOKED_REFUSAL.to_string(),
        }
    }

    /// Any other refusal, `reason` saying why.
    pub fn other(reason: impl Into<String>) -> Self {
        Refusal {
            code: RefusalCode::Other,
            reason: reason.into(),
        }
    }

    /// The refusal of a client of protocol version `theirs`, by its name and with both versions.
    pub fn protocol(theirs: u32) -> Self {
        Refusal {
            code: RefusalCode::ProtocolMismatch,
            reason: format!(
                "protocol mismatch: this broker speaks Cassini protocol version {PROTOCOL_VERSION} and the client \
                 version {theirs}; build the client with this broker's cassini crates"
            ),
        }
    }

    /// The refusal of a connection whose first frame does not decode: most likely a client older than protocol
    /// versions. Its code is `Other`, which such a client can decode, and its words name the protocol.
    pub fn undecodable_registration() -> Self {
        Refusal::other(format!(
            "protocol mismatch: this broker speaks Cassini protocol version {PROTOCOL_VERSION} and could not decode \
             the client's registration (a client built before protocol versions?); build the client with this \
             broker's cassini crates"
        ))
    }
}

/// Events emitted by the client.
#[derive(Clone, Debug)]
pub enum ClientEvent {
    Registered {
        registration_id: String,
    },
    MessagePublished {
        topic: String,
        payload: Vec<u8>,
        trace_ctx: Option<WireTraceCtx>,
        published_by: Option<String>,
    },
    TransportError {
        reason: String,
    },
    /// The broker refused to register the session (`code` says what that means, `reason` why in words) and closes the
    /// connection. The custody daemon's custodian reads it: `RefusalCode::Revoked` means its own certificate is revoked
    /// (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian").
    RegistrationRefused {
        code: RefusalCode,
        reason: String,
    },
    ControlResponse {
        registration_id: String,
        result: Result<ControlResult, ControlError>,
        trace_ctx: Option<WireTraceCtx>,
    },
    /// The broker accepted the publish numbered `id` on `topic` (its topic took it; that is not yet a delivery).
    PublishAcknowledged {
        topic: String,
        id: MessageId,
    },
    /// The broker took the subscription: messages published on `topic` from now on are delivered.
    ClientSubscribed {
        topic: String,
    },
    /// The subscription was refused (`reason` says why), by the broker or, before it was sent, by the agent's own
    /// topic check: nothing published on `topic` is delivered to this client. An agent logs it. The client's facade
    /// keeps it as wanted: it is not sent again when the agent subscribes again on registering, only when the agent's
    /// authority changes and its topic check admits it (docs/architecture/policy-enforcement.md, stage 5b).
    SubscribeRefused {
        topic: String,
        reason: String,
    },
    /// A publish on `topic` was refused (`reason` says why), by the broker or, before it was sent, by the agent's
    /// own topic check: it was not delivered to anyone, and no `PublishAcknowledged` follows.
    PublishRefused {
        topic: String,
        reason: String,
    },
    /// The client itself refused a publish on `topic` before sending it (`reason` says why): its outbox and its
    /// buffer were full, and it refuses the newest rather than drop an older one (docs/architecture/
    /// cassini-backpressure.md, phase 1, client side). Nothing was sent; a caller that queued is told with an error too.
    PublishDropped {
        topic: String,
        reason: String,
    },
    /// The broker could not take a publish on `topic` now: it is not refused. `reason` says why (it is shutting down,
    /// or its memory budget is exhausted), and who sends it again: through the client's facade it is kept and sent
    /// again, on its next registration or after a wait; a transport with no facade keeps nothing, and `reason` says
    /// its caller must send it again (`cassini_client::DEFERRED_AND_KEPT`, `DEFERRED_NOT_KEPT`).
    PublishDeferred {
        topic: String,
        reason: String,
    },
    /// The session this client asked to resume, `registration_id`, was ended by the broker before its retention window,
    /// for `reason`: what it held for this client was let go while it was away (cassini-backpressure.md, the
    /// decisions, 14). The client has a new session (`Registered` follows); what it was owed is lost, and the agent
    /// resynchronises as it can.
    SessionEnded {
        registration_id: String,
        reason: String,
    },
}
/// External Messages for client comms
/// These messages are serialized/deserialized to/from JSON
#[derive(Serialize, Deserialize, Archive, Debug, Clone)]
pub enum ClientMessage {
    RegistrationRequest {
        registration_id: Option<String>,
        trace_ctx: Option<WireTraceCtx>,
        /// The client's `PROTOCOL_VERSION`.
        protocol: u32,
    },
    /// Response to an attempt to register.
    /// Ok contains the value of the registration id,
    /// Err the refusal: its code, and why in words
    RegistrationResponse {
        result: Result<String, Refusal>,
        trace_ctx: Option<WireTraceCtx>,
    },
    /// Publish request from the client. The client numbers what it publishes: `epoch` is chosen at random when its
    /// client starts, and `seq` counts 1, 2, 3 within it (docs/architecture/cassini-client-reliability.md, section 4),
    /// so the acknowledgement names this message and no other.
    PublishRequest {
        topic: String,
        payload: Arc<Vec<u8>>,
        registration_id: String,
        trace_ctx: Option<WireTraceCtx>,
        share_identity: bool,
        epoch: u64,
        seq: u64,
    },
    /// Publish response to the client -- also delivers a subscribed
    /// message; see BrokerMessage::PublishResponse's own doc. A refusal (`result` an `Err`) names the publish it
    /// refuses in `origin`. A delivery carries its origin, who published it (`published_by`, if they shared it) and
    /// their number for it (`origin`), and the broker's number for the delivery (`delivery_id`), which the client
    /// returns in `PublishResponseAck` once it has it; until then the broker keeps it for this client
    /// (docs/architecture/cassini-client-reliability.md, section 5).
    PublishResponse {
        topic: String,
        payload: Arc<Vec<u8>>,
        result: Result<(), String>,
        trace_ctx: Option<WireTraceCtx>,
        published_by: Option<String>,
        origin: Option<MessageId>,
        delivery_id: Option<u64>,
        /// For a refusal (`result` an `Err`), what kind it is, which the client acts on: the words are for people.
        refusal: Option<PublishRefusal>,
    },
    /// The client has the delivery numbered `delivery_id`, and the broker may let go of it. Sent when the client has
    /// handed the delivery to the agent (its router, or whoever listens to its events).
    PublishResponseAck { delivery_id: u64 },
    /// The topic accepted the publish numbered `(epoch, seq)`. Each publish is acknowledged by itself and in no
    /// particular order: topics accept independently, so a later message can be acknowledged before an earlier one,
    /// and an acknowledgement says nothing about any other message (there is no cumulative acknowledgement).
    PublishRequestAck {
        topic: String,
        epoch: u64,
        seq: u64,
        trace_ctx: Option<WireTraceCtx>,
    },
    SubscribeRequest {
        registration_id: String,
        topic: String,
        trace_ctx: Option<WireTraceCtx>,
    },
    /// Subscribe acknowledgment to the client.
    SubscribeAcknowledgment {
        topic: String,
        result: Result<(), String>,
        trace_ctx: Option<WireTraceCtx>,
    },
    /// Unsubscribe request from the client.
    UnsubscribeRequest {
        registration_id: String,
        topic: String,
        trace_ctx: Option<WireTraceCtx>,
    },
    UnsubscribeAcknowledgment {
        topic: String,
        result: Result<(), String>,
        trace_ctx: Option<WireTraceCtx>,
    },
    ///Disconnect, sending a session id to end, if any
    DisconnectRequest {
        registration_id: Option<String>,
        trace_ctx: Option<WireTraceCtx>,
    },
    ErrorMessage {
        error: String,
        trace_ctx: Option<WireTraceCtx>,
    },
    ControlRequest {
        registration_id: String,
        op: ControlOp,
        trace_ctx: Option<WireTraceCtx>,
    },
    ControlResponse {
        registration_id: String,
        result: Result<ControlResult, ControlError>,
        trace_ctx: Option<WireTraceCtx>,
    },
    /// From the client: the `frame`th frame the broker wrote on this connection (counting from one) did not decode,
    /// so the client cannot name what it was. A delivery is let go as acknowledged: the client will never decode it,
    /// and holding it would hold the budget and stall the session for good (cassini-backpressure.md, second review,
    /// finding B). Last but one, so no earlier variant's tag moves.
    FrameUndecodable { frame: u64 },
    /// From the broker, before the answer to a registration that asked to resume `registration_id`: that session was
    /// ended before its retention window, for `reason` (what it held was let go while the client was away and the
    /// broker's memory budget was exhausted). The client is given a new session, and resynchronises. Last, so no
    /// earlier variant's tag moves.
    SessionEnded {
        registration_id: String,
        reason: String,
    },
}
