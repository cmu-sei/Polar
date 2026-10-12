/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

/// ractor's actor registry is process-global, so any test spawning an
/// actor under LEDGER_ACTOR_NAME or ISSUANCE_ACTOR_NAME -- both
/// hardcoded, since deserialize_and_dispatch requires them by
/// construction -- would otherwise collide nondeterministically with
/// any other such test running concurrently (Rust's default). Every
/// test that registers under these names, or assumes their absence,
/// acquires this lock first.
#[cfg(test)]
static NAMED_ACTOR_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ---- External crates ----
use cassini_types::ClientEvent;
use cert_client::handshake::HandshakeClient;
use polar::Supervisor;
use polar::cassini::{CassiniClient, TcpClient, UnsubscribeRequest};
use polar::request_reply::Envelope;
use polar_agent_builder::{
    AgentBuilder, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage, Subscription,
};
use ractor::registry::where_is;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use std::time::Duration;
use tracing::{debug, error, info, warn};

// The tests below spawn stand-ins for the Cassini client; the supervisor itself no longer
// names this type (the agent builder owns the client).
#[cfg(test)]
use cassini_client::TcpClientMessage;

// ---- credentials_agent_common: topics ----
use credentials_agent_common::{
    CREDENTIALS_ACCOUNT_PROVISIONING_TOPIC, CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC,
    CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC, CREDENTIALS_CERTIFICATE_REQUESTS_TOPIC,
    CREDENTIALS_DECRYPT_ON_BEHALF_OF_REQUESTS_TOPIC, CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC,
    CREDENTIALS_GRANTS_TOPIC, CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC,
    CREDENTIALS_ISSUANCE_REQUESTS_TOPIC, CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC,
    CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC, CREDENTIALS_REVOCATIONS_TOPIC,
    CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
};

// ---- credentials_agent_common: wire types ----
use credentials_agent_common::agent_escrow_issuance::AgentEscrowIssuanceRequest;
use credentials_agent_common::agent_escrow_key::AgentEscrowKeyRequest;
use credentials_agent_common::agent_escrow_keypair_bootstrap::RetrieveAgentEscrowKeypairRequest;
use credentials_agent_common::certificate::CertificateRequest;
use credentials_agent_common::decrypt_on_behalf_of::DecryptOnBehalfOfRequest;
use credentials_agent_common::escrow_key::EscrowKeyRequest;
use credentials_agent_common::events::CredentialsEvent;
use credentials_agent_common::identity::IdentityNormalizationRequest;
use credentials_agent_common::issuance::IssuanceRequest;
use credentials_agent_common::retrieve_secret::RetrieveSecretRequest;
use credentials_agent_common::secret::StoreSecretRequest;

// ---- Local: shared infrastructure ----
use crate::authority_publisher::{AuthorityPublisher, PublisherArgs, PublisherMessage};
use crate::introspection::IntrospectionClient;
use crate::ledger::{LedgerActor, LedgerActorArgs, LedgerMessage};
use crate::projection::{ProjectionActor, ProjectionActorArgs, ProjectionMessage};
use crate::settings::ConsumerSettings;

// ---- Local: handler + actor pairs, one per request/reply flow ----
use crate::account::AccountHandler;
use crate::account_actor::{AccountActor, AccountActorArgs};
use crate::admin_action::AdminActionHandler;
use crate::admin_action_actor::{AdminActionActor, AdminActionActorArgs};
use crate::agent_escrow_issuance::AgentEscrowIssuanceHandler;
use crate::agent_escrow_issuance_actor::{AgentEscrowIssuanceActor, AgentEscrowIssuanceActorArgs};
use crate::agent_escrow_key::AgentEscrowKeyHandler;
use crate::agent_escrow_key_actor::{AgentEscrowKeyActor, AgentEscrowKeyActorArgs};
use crate::agent_escrow_keypair_bootstrap::AgentEscrowKeypairBootstrapHandler;
use crate::agent_escrow_keypair_bootstrap_actor::{
    AgentEscrowKeypairBootstrapActor, AgentEscrowKeypairBootstrapActorArgs,
    AgentEscrowKeypairBootstrapDispatch,
};
use crate::certificate::CertificateHandler;
use crate::certificate_actor::{CertificateActor, CertificateActorArgs};
use crate::decrypt_on_behalf_of::DecryptOnBehalfOfHandler;
use crate::decrypt_on_behalf_of_actor::{
    DecryptOnBehalfOfActor, DecryptOnBehalfOfActorArgs, DecryptOnBehalfOfDispatch,
};
use crate::escrow_key::EscrowKeyHandler;
use crate::escrow_key_actor::{EscrowKeyActor, EscrowKeyActorArgs};
use crate::identity_normalization::IdentityNormalizationHandler;
use crate::identity_normalization_actor::{
    IdentityNormalizationActor, IdentityNormalizationActorArgs,
};
use crate::issuance::IssuanceHandler;
use crate::issuance_actor::{IssuanceActor, IssuanceActorArgs};
use crate::retrieve_secret::RetrieveSecretHandler;
use crate::retrieve_secret_actor::{
    RetrieveSecretActor, RetrieveSecretActorArgs, RetrieveSecretDispatch,
};
use crate::store_secret::StoreSecretHandler;
use crate::store_secret_actor::{StoreSecretActor, StoreSecretActorArgs, StoreSecretDispatch};

const LEDGER_ACTOR_NAME: &str = "polar.credentials-agent.ledger";

/// Name given to the agent builder. It registers the Cassini client as
/// "<SERVICE_NAME>.tcp".
pub const SERVICE_NAME: &str = "credentials-agent-consumer";

/// How the handler actors find the Cassini client to publish their replies:
/// by registry name, on every publish. It must equal the name the builder
/// registers (checked by a test), and because it is looked up each time, a
/// client the builder respawns under the same name is picked up with no
/// further wiring.
pub const BROKER_CLIENT_NAME: &str = "credentials-agent-consumer.tcp";

/// How long in-flight requests get to finish once shutdown has begun, with
/// new ones no longer arriving, before the ledger and projection are stopped.
const DRAIN_WINDOW_SECS: u64 = 5;

/// How long the infrastructure supervisor gives this agent to drain, from being
/// told to shut down to releasing it: the drain window, the ledger's stop (10 s)
/// and the projection's stop (30 s), plus a margin. If it is not done by then the
/// process exits 1 regardless.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(50);

/// Every topic this agent subscribes to. One subscription each; the same list
/// is unsubscribed from when shutdown begins.
pub const REQUEST_TOPICS: [&str; 15] = [
    CREDENTIALS_GRANTS_TOPIC,
    CREDENTIALS_REVOCATIONS_TOPIC,
    CREDENTIALS_ACCOUNT_PROVISIONING_TOPIC,
    credentials_agent_common::CREDENTIALS_ADMIN_ACTION_REQUESTS_TOPIC,
    credentials_agent_common::CREDENTIALS_ACCOUNT_REQUESTS_TOPIC,
    CREDENTIALS_ISSUANCE_REQUESTS_TOPIC,
    CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC,
    CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC,
    CREDENTIALS_CERTIFICATE_REQUESTS_TOPIC,
    CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
    CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC,
    CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC,
    CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC,
    CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC,
    CREDENTIALS_DECRYPT_ON_BEHALF_OF_REQUESTS_TOPIC,
];

const ISSUANCE_ACTOR_NAME: &str = "polar.credentials-agent.issuance";
const IDENTITY_NORMALIZATION_ACTOR_NAME: &str = "polar.credentials-agent.identity-normalization";
const ESCROW_KEY_ACTOR_NAME: &str = "polar.credentials-agent.escrow-key";
const CERTIFICATE_ACTOR_NAME: &str = "polar.credentials-agent.certificate";
const STORE_SECRET_ACTOR_NAME: &str = "polar.credentials-agent.store-secret";
const ADMIN_ACTION_ACTOR_NAME: &str = "polar.credentials-agent.admin-action";
const ACCOUNT_ACTOR_NAME: &str = "polar.credentials-agent.account";
const RETRIEVE_SECRET_ACTOR_NAME: &str = "polar.credentials-agent.retrieve-secret";
const AGENT_ESCROW_KEYPAIR_BOOTSTRAP_ACTOR_NAME: &str =
    "polar.credentials-agent.agent-escrow-keypair-bootstrap";
const AGENT_ESCROW_ISSUANCE_ACTOR_NAME: &str = "polar.credentials-agent.agent-escrow-issuance";
const AGENT_ESCROW_KEY_ACTOR_NAME: &str = "polar.credentials-agent.agent-escrow-key";
const DECRYPT_ON_BEHALF_OF_ACTOR_NAME: &str = "polar.credentials-agent.decrypt-on-behalf-of";

/// This supervisor's own message type. The shared `polar::SupervisorMessage`
/// cannot carry the infrastructure signals (respawn, prepare-shutdown) without
/// `polar-agent-builder` depending back on `polar`, so, like the other
/// builder-based supervisors, this one defines exactly the messages it handles.
pub enum ConsumerSupervisorMessage {
    /// Connection lifecycle events from the Cassini client, and, re-wrapped by
    /// the subscriptions, every message published on a request topic.
    ClientEvent { event: ClientEvent },
    /// The health check ended for rejuvenation. Drain, stop this agent's own
    /// children, then answer `ProceedWithShutdown`.
    PrepareShutdown,
    /// The drain window has elapsed.
    DrainElapsed,
    /// Time to say how many requests have arrived since the last time.
    ReportActivity,
    /// A runtime section was replaced: if it is one that is published, read it and hand it on.
    RuntimeChanged { section: String },
    /// An agent asked for the current authority.
    AuthorityRequested,
}

/// How often the consumer says how many requests it has seen.
const ACTIVITY_REPORT_EVERY: Duration = Duration::from_secs(60);

pub struct ConsumerSupervisor;

pub struct ConsumerSupervisorState {
    /// Owns the graph projection, and its own GraphControllerActor as
    /// a direct child -- see projection.rs's own module doc. The other
    /// handler actors are tied to this supervisor by spawn_linked and nothing
    /// here reads them, so only the two the shutdown sequence stops by hand
    /// are held.
    projection_actor: ActorRef<ProjectionMessage>,
    /// The source of truth: stopped first at shutdown, which waits for the
    /// append in progress.
    ledger: ActorRef<LedgerMessage>,
    healthcheck: HealthHandle,
    /// Signs, numbers and publishes the policy, the groups and the revoked certificates.
    publisher: ActorRef<PublisherMessage>,
    /// Where the policy and groups sections are read from when one changes.
    configuration: polar_agent_builder::ConfigurationHandle,
    /// Used only to unsubscribe at shutdown; replaced when the builder
    /// respawns the client.
    broker_client: TcpClient,
    /// Answered with `ProceedWithShutdown` once this agent's own children are
    /// down.
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    /// Requests seen, reported periodically so that silence can be told from idleness.
    activity: crate::activity::Activity,
    draining: bool,
    /// The drain was started by `PrepareShutdown` (as opposed to a failure),
    /// so it ends by releasing the infrastructure and not by exiting.
    rejuvenating: bool,
}

impl Supervisor for ConsumerSupervisor {
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>) {
        if topic == credentials_agent_common::CREDENTIALS_ADMIN_ACTION_REQUESTS_TOPIC {
            match rkyv::from_bytes::<
                Envelope<credentials_agent_common::admin_action::SignedAdminAction>,
                rkyv::rancor::Error,
            >(&payload)
            {
                Ok(envelope) => {
                    if let Some(actor) = where_is(ADMIN_ACTION_ACTOR_NAME.to_string()) {
                        if let Err(e) = actor.send_message(envelope) {
                            warn!("Failed to forward admin action to actor: {e}");
                        }
                    } else {
                        warn!("Admin action actor not yet registered, dropping admin action");
                    }
                }
                Err(e) => {
                    warn!("Failed to deserialize admin action request: {e}");
                }
            }
        } else if topic == credentials_agent_common::CREDENTIALS_ACCOUNT_REQUESTS_TOPIC {
            match rkyv::from_bytes::<
                Envelope<credentials_agent_common::account_claim::AccountRequest>,
                rkyv::rancor::Error,
            >(&payload)
            {
                Ok(envelope) => {
                    if let Some(actor) = where_is(ACCOUNT_ACTOR_NAME.to_string()) {
                        if let Err(e) = actor.send_message(envelope) {
                            warn!("Failed to forward account request to actor: {e}");
                        }
                    } else {
                        warn!("Account actor not yet registered, dropping account request");
                    }
                }
                Err(e) => {
                    warn!("Failed to deserialize account request: {e}");
                }
            }
        } else if topic == CREDENTIALS_ISSUANCE_REQUESTS_TOPIC {
            match rkyv::from_bytes::<Envelope<IssuanceRequest>, rkyv::rancor::Error>(&payload) {
                Ok(envelope) => {
                    if let Some(issuance_actor) = where_is(ISSUANCE_ACTOR_NAME.to_string()) {
                        if let Err(e) = issuance_actor.send_message(envelope) {
                            warn!("Failed to forward issuance request to actor: {e}");
                        }
                    } else {
                        warn!("Issuance actor not yet registered, dropping issuance request");
                    }
                }
                Err(e) => {
                    warn!("Failed to deserialize issuance request from topic {topic}: {e}");
                }
            }
            return;
        }

        if topic == CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC {
            match rkyv::from_bytes::<Envelope<AgentEscrowIssuanceRequest>, rkyv::rancor::Error>(
                &payload,
            ) {
                Ok(envelope) => {
                    if let Some(actor) = where_is(AGENT_ESCROW_ISSUANCE_ACTOR_NAME.to_string()) {
                        if let Err(e) = actor.send_message(envelope) {
                            warn!("Failed to forward agent escrow-issuance request to actor: {e}");
                        }
                    } else {
                        warn!("Agent escrow-issuance actor not yet registered, dropping request");
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to deserialize agent escrow-issuance request from topic {topic}: {e}"
                    );
                }
            }
            return;
        }

        if topic == CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC {
            match rkyv::from_bytes::<Envelope<AgentEscrowKeyRequest>, rkyv::rancor::Error>(&payload)
            {
                Ok(envelope) => {
                    if let Some(actor) = where_is(AGENT_ESCROW_KEY_ACTOR_NAME.to_string()) {
                        if let Err(e) = actor.send_message(envelope) {
                            warn!("Failed to forward agent escrow-key request to actor: {e}");
                        }
                    } else {
                        warn!("Agent escrow-key actor not yet registered, dropping request");
                    }
                }
                Err(e) => {
                    warn!("Failed to deserialize agent escrow-key request from topic {topic}: {e}");
                }
            }
            return;
        }

        if topic == CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC {
            match rkyv::from_bytes::<Envelope<IdentityNormalizationRequest>, rkyv::rancor::Error>(
                &payload,
            ) {
                Ok(envelope) => {
                    if let Some(actor) = where_is(IDENTITY_NORMALIZATION_ACTOR_NAME.to_string()) {
                        if let Err(e) = actor.send_message(envelope) {
                            warn!("Failed to forward identity-normalization request to actor: {e}");
                        }
                    } else {
                        warn!("Identity-normalization actor not yet registered, dropping request");
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to deserialize identity-normalization request from topic {topic}: {e}"
                    );
                }
            }
            return;
        }

        if topic == CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC {
            match rkyv::from_bytes::<Envelope<EscrowKeyRequest>, rkyv::rancor::Error>(&payload) {
                Ok(envelope) => {
                    if let Some(actor) = where_is(ESCROW_KEY_ACTOR_NAME.to_string()) {
                        if let Err(e) = actor.send_message(envelope) {
                            warn!("Failed to forward escrow-key request to actor: {e}");
                        }
                    } else {
                        warn!("Escrow-key actor not yet registered, dropping request");
                    }
                }
                Err(e) => {
                    warn!("Failed to deserialize escrow-key request from topic {topic}: {e}");
                }
            }
            return;
        }

        if topic == CREDENTIALS_CERTIFICATE_REQUESTS_TOPIC {
            match rkyv::from_bytes::<Envelope<CertificateRequest>, rkyv::rancor::Error>(&payload) {
                Ok(envelope) => {
                    if let Some(actor) = where_is(CERTIFICATE_ACTOR_NAME.to_string()) {
                        if let Err(e) = actor.send_message(envelope) {
                            warn!("Failed to forward certificate request to actor: {e}");
                        }
                    } else {
                        warn!("Certificate actor not yet registered, dropping request");
                    }
                }
                Err(e) => {
                    warn!("Failed to deserialize certificate request from topic {topic}: {e}");
                }
            }
            return;
        }

        // Both credentials-grants and credentials-revocations carry
        // the same CredentialsEvent type -- unlike jira/process's
        // convention, topic name isn't used to select a different
        // actor, since there's only ever one writer (LedgerActor)
        // here at all.
        match rkyv::from_bytes::<CredentialsEvent, rkyv::rancor::Error>(&payload) {
            Ok(event) => {
                if let Some(ledger) = where_is(LEDGER_ACTOR_NAME.to_string()) {
                    if let Err(e) = ledger.send_message(LedgerMessage::AppendNoReply(event)) {
                        warn!("Failed to forward event from topic {topic} to ledger: {e}");
                    }
                } else {
                    warn!("Ledger actor not yet registered, dropping event from topic {topic}");
                }
            }
            Err(e) => {
                warn!("Failed to deserialize CredentialsEvent from topic {topic}: {e}");
            }
        }
    }
}

impl ConsumerSupervisor {
    /// Not part of the shared Supervisor trait -- see its own call
    /// site's comment for why. Deserializes and forwards to
    /// StoreSecretActor, bundled with the broker-verified
    /// published_by this dispatch path exists specifically to carry.
    fn dispatch_store_secret_request(payload: Vec<u8>, published_by: Option<String>) {
        match rkyv::from_bytes::<Envelope<StoreSecretRequest>, rkyv::rancor::Error>(&payload) {
            Ok(envelope) => {
                if let Some(actor) = where_is(STORE_SECRET_ACTOR_NAME.to_string()) {
                    if let Err(e) = actor.send_message(StoreSecretDispatch {
                        envelope,
                        published_by,
                    }) {
                        warn!("Failed to forward store-secret request to actor: {e}");
                    }
                } else {
                    warn!("Store-secret actor not yet registered, dropping request");
                }
            }
            Err(e) => {
                warn!("Failed to deserialize store-secret request: {e}");
            }
        }
    }

    /// Mirrors dispatch_store_secret_request's own reasoning exactly
    /// -- see its own doc comment. Deserializes and forwards to
    /// RetrieveSecretActor, bundled with the broker-verified
    /// published_by this dispatch path exists specifically to carry.
    fn dispatch_retrieve_secret_request(payload: Vec<u8>, published_by: Option<String>) {
        match rkyv::from_bytes::<Envelope<RetrieveSecretRequest>, rkyv::rancor::Error>(&payload) {
            Ok(envelope) => {
                if let Some(actor) = where_is(RETRIEVE_SECRET_ACTOR_NAME.to_string()) {
                    if let Err(e) = actor.send_message(RetrieveSecretDispatch {
                        envelope,
                        published_by,
                    }) {
                        warn!("Failed to forward retrieve-secret request to actor: {e}");
                    }
                } else {
                    warn!("Retrieve-secret actor not yet registered, dropping request");
                }
            }
            Err(e) => {
                warn!("Failed to deserialize retrieve-secret request: {e}");
            }
        }
    }

    /// Mirrors dispatch_store_secret_request's own reasoning exactly
    /// -- see its own doc comment. Deserializes and forwards to
    /// AgentEscrowKeypairBootstrapActor, bundled with the
    /// broker-verified published_by this dispatch path exists
    /// specifically to carry.
    fn dispatch_agent_escrow_keypair_bootstrap_request(
        payload: Vec<u8>,
        published_by: Option<String>,
    ) {
        match rkyv::from_bytes::<Envelope<RetrieveAgentEscrowKeypairRequest>, rkyv::rancor::Error>(
            &payload,
        ) {
            Ok(envelope) => {
                if let Some(actor) = where_is(AGENT_ESCROW_KEYPAIR_BOOTSTRAP_ACTOR_NAME.to_string())
                {
                    if let Err(e) = actor.send_message(AgentEscrowKeypairBootstrapDispatch {
                        envelope,
                        published_by,
                    }) {
                        warn!(
                            "Failed to forward agent escrow-keypair bootstrap request to actor: {e}"
                        );
                    }
                } else {
                    warn!(
                        "Agent escrow-keypair bootstrap actor not yet registered, dropping request"
                    );
                }
            }
            Err(e) => {
                warn!("Failed to deserialize agent escrow-keypair bootstrap request: {e}");
            }
        }
    }

    /// Mirrors dispatch_retrieve_secret_request's own reasoning
    /// exactly -- see its own doc comment. Deserializes and forwards
    /// to DecryptOnBehalfOfActor, bundled with the broker-verified
    /// published_by this dispatch path exists specifically to carry
    /// -- the requesting identity PolicyActor's own resolution is
    /// derived from, never self-reported.
    fn dispatch_decrypt_on_behalf_of_request(payload: Vec<u8>, published_by: Option<String>) {
        match rkyv::from_bytes::<Envelope<DecryptOnBehalfOfRequest>, rkyv::rancor::Error>(&payload)
        {
            Ok(envelope) => {
                if let Some(actor) = where_is(DECRYPT_ON_BEHALF_OF_ACTOR_NAME.to_string()) {
                    if let Err(e) = actor.send_message(DecryptOnBehalfOfDispatch {
                        envelope,
                        published_by,
                    }) {
                        warn!("Failed to forward decrypt-on-behalf-of request to actor: {e}");
                    }
                } else {
                    warn!("Decrypt-on-behalf-of actor not yet registered, dropping request");
                }
            }
            Err(e) => {
                warn!("Failed to deserialize decrypt-on-behalf-of request: {e}");
            }
        }
    }
}

impl ConsumerSupervisor {
    /// One subscription per request topic. Each re-wraps what the router
    /// delivers (topic, payload, the broker-verified `published_by`, trace
    /// context) as a `ClientEvent::MessagePublished` for this supervisor, so
    /// the draining check and the dispatch below stay where they were and a
    /// request can never be dispatched without having gone through them.
    /// A subscription binds registering the topic with the router to
    /// subscribing on the wire, so neither can be forgotten.
    fn request_subscriptions(
        supervisor: &ActorRef<ConsumerSupervisorMessage>,
    ) -> Vec<Subscription> {
        REQUEST_TOPICS
            .iter()
            .map(|topic| {
                let supervisor = supervisor.clone();
                Subscription::new(*topic, move |incoming| {
                    let event = ClientEvent::MessagePublished {
                        topic: incoming.topic,
                        payload: incoming.payload,
                        trace_ctx: incoming.trace_ctx,
                        published_by: incoming.published_by,
                    };
                    if let Err(e) =
                        supervisor.send_message(ConsumerSupervisorMessage::ClientEvent { event })
                    {
                        warn!("AgentBuilder: failed to deliver routed message to supervisor: {e}");
                    }
                })
            })
            .collect()
    }

    /// Agents ask for the current authority on this topic; this agent is the only one that answers.
    fn authority_request_subscription(
        supervisor: &ActorRef<ConsumerSupervisorMessage>,
    ) -> Subscription {
        let supervisor = supervisor.clone();
        Subscription::new(
            credentials_agent_common::authority_update::AUTHORITY_SNAPSHOT_REQUESTS_TOPIC,
            move |_| {
                if let Err(e) =
                    supervisor.send_message(ConsumerSupervisorMessage::AuthorityRequested)
                {
                    warn!("could not pass on a request for the authority: {e}");
                }
            },
        )
    }

    /// Reads a replaced section and hands it to the publisher. A section that cannot be read says so and is
    /// left for the next republish; it is never published half-read.
    async fn pass_on_section(state: &ConsumerSupervisorState, section: &str) {
        match state.configuration.runtime(section).await {
            Ok(Some(value)) => {
                let _ = state.publisher.cast(PublisherMessage::SectionChanged {
                    section: section.to_string(),
                    version: value.version,
                    body: value.body,
                    authored_by: Some(value.signer),
                });
            }
            Ok(None) => {}
            Err(e) => warn!(%section, "could not read the section to publish it: {e}"),
        }
    }

    /// Routes one request to its handler actor. Four topics carry the
    /// broker-verified `published_by`, which the shared `Supervisor` trait
    /// cannot (it is cross-cutting and cannot gain a parameter for one
    /// agent's need): stored_by, requested_by and the like must be verified by
    /// the broker, never self-reported.
    fn dispatch_request(topic: String, payload: Vec<u8>, published_by: Option<String>) {
        if topic == CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC {
            ConsumerSupervisor::dispatch_store_secret_request(payload, published_by);
        } else if topic == CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC {
            ConsumerSupervisor::dispatch_retrieve_secret_request(payload, published_by);
        } else if topic == CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC {
            ConsumerSupervisor::dispatch_agent_escrow_keypair_bootstrap_request(
                payload,
                published_by,
            );
        } else if topic == CREDENTIALS_DECRYPT_ON_BEHALF_OF_REQUESTS_TOPIC {
            ConsumerSupervisor::dispatch_decrypt_on_behalf_of_request(payload, published_by);
        } else {
            ConsumerSupervisor::deserialize_and_dispatch(topic, payload);
        }
    }

    /// Schedules `DrainElapsed`. Not awaited: the timer runs detached, so this
    /// supervisor keeps handling (and logging) messages while the window is open.
    fn schedule_drain_end(myself: &ActorRef<ConsumerSupervisorMessage>) {
        // Dropping the handle detaches the timer; it still fires.
        drop(
            myself.send_after(Duration::from_secs(DRAIN_WINDOW_SECS), || {
                ConsumerSupervisorMessage::DrainElapsed
            }),
        );
    }

    /// Tells the broker this agent is going dark, so it stops routing requests
    /// here. Failures are logged and do not stop the shutdown.
    fn unsubscribe_from_topics(client: &TcpClient) {
        for topic in REQUEST_TOPICS {
            if let Err(e) = client.unsubscribe(UnsubscribeRequest {
                topic: topic.to_string(),
                trace_ctx: None,
            }) {
                warn!("CONSUMER_SUPERVISOR: unsubscribe from '{topic}' failed (continuing): {e}");
            }
        }
    }
}

#[async_trait]
impl Actor for ConsumerSupervisor {
    type Msg = ConsumerSupervisorMessage;
    type State = ConsumerSupervisorState;
    /// The configuration `run` judged; the builder is made from it.
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        judged: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        // Made before the builder, which keeps them current from the credentials agent's own
        // published authority (`with_authority_actors`): the handlers below decide with them.
        // PermissionsActor is the sole, real, authoritative source of
        // what permissions an identity actually holds -- PolicyActor
        // must never itself resolve permissions, only consume this
        // actor's own, real resolutions. See polar-permissions's own
        // crate documentation for the complete, real reasoning.
        let (permissions_actor, _) = polar_permissions::PermissionsActor::spawn_linked(
            None,
            polar_permissions::PermissionsActor,
            (),
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        // PolicyActor never exists without a real PermissionsActor --
        // construction requires one, structurally, not optionally.
        // See polar-policy's own crate documentation for the
        // complete, real reasoning behind this coupling.
        let (policy_actor, _) = polar_policy::PolicyActor::spawn_linked(
            None,
            polar_policy::PolicyActor,
            polar_policy::PolicyActorArgs {
                permissions_actor: permissions_actor.clone(),
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        // The environment is read and judged first, before any actor is spawned, and the
        // agent's own settings come from that one snapshot. Declaring the graph section here
        // rather than requiring the graph-write capability is deliberate: this agent
        // projects to the graph but its health must not depend on Neo4j (the ledger is its
        // source of truth and the graph only a projection of it). Deliberately NOT
        // `with_encryption()` either: CryptoActor is the gateway to the Credentials Agent,
        // and its requests go to the very topics this agent serves, so this agent would be
        // asking itself for its own keys (and its polar-init must never run the escrow
        // bootstrap against itself). The dependency endpoints (Cassini and Neo4j
        // certificates) come from POLAR_HEALTH_DEP_ENDPOINTS in the deployment.
        let mut builder = AgentBuilder::new(myself.clone(), SERVICE_NAME)
            .with_lifecycle_handler(|event| Some(ConsumerSupervisorMessage::ClientEvent { event }))
            .with_authority_actors(permissions_actor.clone(), policy_actor.clone());
        builder = builder.with_subscription(Self::authority_request_subscription(&myself));
        for subscription in Self::request_subscriptions(&myself) {
            builder = builder.with_subscription(subscription);
        }
        let configured = builder.into_configured(judged);
        let graph_settings =
            polar::graph::environment::GraphSettings::from_env(&**configured.config())
                .map_err(ActorProcessingErr::from)?;

        let settings =
            ConsumerSettings::read(configured.config()).map_err(ActorProcessingErr::from)?;
        let log_path = settings.event_log_path.clone();

        // ProjectionActor owns the graph projection as its own,
        // genuinely separate concern -- see projection.rs's own
        // module doc for why this isn't LedgerActor's job despite
        // being where the events originate. Spawned eagerly, unlike
        // every other graph-writing agent in this codebase -- the
        // Credentials Agent's own health must never depend on Neo4j's
        // availability at all. No technical dependency requires
        // waiting for Cassini registration first; a Bolt connection
        // to Neo4j is entirely independent of Cassini/TCP client
        // registration.
        //
        // Spawned before LedgerActor, with ledger: None -- the two
        // actors need references to each other, which can't both be
        // constructor arguments (neither ActorRef exists before its
        // own actor is spawned). ProjectionActor's need for the
        // ledger reference is deferred via SetLedger below, since
        // nothing calls into it until an actual event or catch-up
        // happens, well after both actors exist.
        let (projection_actor, _) = ProjectionActor::spawn_linked(
            Some("credentials-agent.consumer.projection".to_string()),
            ProjectionActor,
            ProjectionActorArgs {
                ledger: None,
                graph: graph_settings,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let (ledger, _) = LedgerActor::spawn_linked(
            Some(LEDGER_ACTOR_NAME.to_string()),
            LedgerActor,
            LedgerActorArgs {
                log_path,
                projection_actor: projection_actor.clone(),
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let _ = projection_actor.cast(ProjectionMessage::SetLedger(ledger.clone()));

        let cert_issuer_url = settings.cert_issuer_url.clone();

        // How this consumer trusts cert-issuer. With the CA path set it
        // verifies against that file from its first request; without it,
        // the CA is fetched once from GET /ca and pinned (the degraded
        // self-bootstrap mode). Each handler owns its own client, so the
        // trust setup runs once per handler.
        let cert_issuer_ca_path = settings.ca_cert.clone();
        let make_cert_issuer_client = || async {
            HandshakeClient::for_trust_mode(cert_issuer_url.clone(), cert_issuer_ca_path.as_deref())
                .await
                .map_err(|e| {
                    ActorProcessingErr::from(format!("cannot establish trust in cert-issuer: {e}"))
                })
        };

        let escrow_keypair = {
            std::sync::Arc::new(
                crate::escrow::EscrowKeypair::load_or_generate(&settings.escrow_key_path)
                    .map_err(ActorProcessingErr::from)?,
            )
        };

        let issuance_handler = IssuanceHandler {
            ledger: ledger.clone(),
            cert_issuer: make_cert_issuer_client().await?,
            // Introspection is genuinely optional at the deployment
            // level -- a cluster that never has LikelyIdp credentials
            // in practice doesn't need it configured. If one does
            // arrive without introspection configured, the handler
            // itself refuses it (TokenIntrospectionFailed) rather
            // than silently proceeding unintrospected.
            introspection: settings.introspection.as_ref().map(|i| {
                IntrospectionClient::new(
                    i.endpoint.clone(),
                    i.client_id.clone(),
                    i.client_secret.clone(),
                )
            }),
            known_kubernetes_issuers: settings.kubernetes_issuers.clone(),
            escrow_keypair: escrow_keypair.clone(),
        };

        let (_issuance_actor, _) = IssuanceActor::spawn_linked(
            Some(ISSUANCE_ACTOR_NAME.to_string()),
            IssuanceActor,
            IssuanceActorArgs {
                handler: issuance_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let agent_escrow_issuance_handler = AgentEscrowIssuanceHandler {
            ledger: ledger.clone(),
            cert_issuer: make_cert_issuer_client().await?,
            known_kubernetes_issuers: settings.kubernetes_issuers.clone(),
            escrow_keypair: escrow_keypair.clone(),
        };
        let (_agent_escrow_issuance_actor, _) = AgentEscrowIssuanceActor::spawn_linked(
            Some(AGENT_ESCROW_ISSUANCE_ACTOR_NAME.to_string()),
            AgentEscrowIssuanceActor,
            AgentEscrowIssuanceActorArgs {
                handler: agent_escrow_issuance_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let agent_escrow_key_handler = AgentEscrowKeyHandler {
            ledger: ledger.clone(),
        };
        let (_agent_escrow_key_actor, _) = AgentEscrowKeyActor::spawn_linked(
            Some(AGENT_ESCROW_KEY_ACTOR_NAME.to_string()),
            AgentEscrowKeyActor,
            AgentEscrowKeyActorArgs {
                handler: agent_escrow_key_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let identity_normalization_handler = IdentityNormalizationHandler {
            cert_issuer: make_cert_issuer_client().await?,
        };

        let (_identity_normalization_actor, _) = IdentityNormalizationActor::spawn_linked(
            Some(IDENTITY_NORMALIZATION_ACTOR_NAME.to_string()),
            IdentityNormalizationActor,
            IdentityNormalizationActorArgs {
                handler: identity_normalization_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let escrow_key_handler = EscrowKeyHandler {
            escrow_keypair: escrow_keypair.clone(),
        };
        let (_escrow_key_actor, _) = EscrowKeyActor::spawn_linked(
            Some(ESCROW_KEY_ACTOR_NAME.to_string()),
            EscrowKeyActor,
            EscrowKeyActorArgs {
                handler: escrow_key_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let certificate_handler = CertificateHandler {
            ledger: ledger.clone(),
        };
        let (_certificate_actor, _) = CertificateActor::spawn_linked(
            Some(CERTIFICATE_ACTOR_NAME.to_string()),
            CertificateActor,
            CertificateActorArgs {
                handler: certificate_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let store_secret_handler = StoreSecretHandler {
            escrow_keypair: escrow_keypair.clone(),
            ledger: ledger.clone(),
        };
        let (_store_secret_actor, _) = StoreSecretActor::spawn_linked(
            Some(STORE_SECRET_ACTOR_NAME.to_string()),
            StoreSecretActor,
            StoreSecretActorArgs {
                handler: store_secret_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let admin_action_handler = AdminActionHandler {
            ledger: ledger.clone(),
        };
        let (_admin_action_actor, _) = AdminActionActor::spawn_linked(
            Some(ADMIN_ACTION_ACTOR_NAME.to_string()),
            AdminActionActor,
            AdminActionActorArgs {
                handler: admin_action_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        // The account claim signs through /internal/issue, which identifies
        // its caller by the client certificate on the connection: this is
        // the agent's own mTLS identity, the same files its Cassini client
        // uses.
        let own_cert_path = settings.own_cert_path.clone();
        let own_key_path = settings.own_key_path.clone();
        let account_cert_issuer = HandshakeClient::for_trust_mode_with_identity(
            cert_issuer_url.clone(),
            cert_issuer_ca_path.as_deref(),
            &own_cert_path,
            &own_key_path,
        )
        .await
        .map_err(|e| {
            ActorProcessingErr::from(format!("cannot establish trust in cert-issuer: {e}"))
        })?;
        let account_handler = AccountHandler {
            ledger: ledger.clone(),
            cert_issuer: account_cert_issuer,
            bootstrap_identity: settings.bootstrap_identity.clone(),
        };
        let (_account_actor, _) = AccountActor::spawn_linked(
            Some(ACCOUNT_ACTOR_NAME.to_string()),
            AccountActor,
            AccountActorArgs {
                handler: account_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let agent_escrow_keypair_bootstrap_handler = AgentEscrowKeypairBootstrapHandler {
            escrow_keypair: escrow_keypair.clone(),
            ledger: ledger.clone(),
        };
        let (_agent_escrow_keypair_bootstrap_actor, _) =
            AgentEscrowKeypairBootstrapActor::spawn_linked(
                Some(AGENT_ESCROW_KEYPAIR_BOOTSTRAP_ACTOR_NAME.to_string()),
                AgentEscrowKeypairBootstrapActor,
                AgentEscrowKeypairBootstrapActorArgs {
                    handler: agent_escrow_keypair_bootstrap_handler,
                },
                myself.get_cell(),
            )
            .await
            .map_err(ActorProcessingErr::from)?;

        let retrieve_secret_handler = RetrieveSecretHandler {
            escrow_keypair: escrow_keypair.clone(),
            ledger: ledger.clone(),
            policy_actor: policy_actor.clone(),
        };
        let (_retrieve_secret_actor, _) = RetrieveSecretActor::spawn_linked(
            Some(RETRIEVE_SECRET_ACTOR_NAME.to_string()),
            RetrieveSecretActor,
            RetrieveSecretActorArgs {
                handler: retrieve_secret_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let decrypt_on_behalf_of_handler = DecryptOnBehalfOfHandler {
            escrow_keypair: escrow_keypair.clone(),
            ledger: ledger.clone(),
            policy_actor,
        };
        let (_decrypt_on_behalf_of_actor, _) = DecryptOnBehalfOfActor::spawn_linked(
            Some(DECRYPT_ON_BEHALF_OF_ACTOR_NAME.to_string()),
            DecryptOnBehalfOfActor,
            DecryptOnBehalfOfActorArgs {
                handler: decrypt_on_behalf_of_handler,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        // The health check, the router, the request subscriptions and the Cassini
        // client come from the agent builder, spawned last so every handler
        // actor exists before a request can arrive. It was configured first, above.
        let configured_authority_publisher = configured
            .config()
            .read(&polar_agent_builder::environment::AUTHORITY_PUBLISHER)
            .map_err(|e| ActorProcessingErr::from(e.to_string()))?;
        let resources = configured.build().await?;

        // The "infrastructure -> domain" half of the respawn/shutdown protocol.
        // Without this subscription the infrastructure supervisor would wait
        // forever for a ProceedWithShutdown after rejuvenation.
        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => {
                    Some(ConsumerSupervisorMessage::PrepareShutdown)
                }
            });

        // The authority publisher. The sections it publishes arrive as signed documents, so it is told when
        // one is replaced, and once now for any already held; the ledger tells it when a certificate is
        // revoked.
        let own_identity = configured_authority_publisher;
        let broker = BROKER_CLIENT_NAME.to_string();
        let (publisher, _) = AuthorityPublisher::spawn_linked(
            None,
            AuthorityPublisher,
            PublisherArgs {
                ledger: ledger.clone(),
                key: escrow_keypair.private_key().clone(),
                publish: std::sync::Arc::new(move |topic: &str, payload: Vec<u8>| {
                    match where_is(broker.clone()) {
                        Some(client) => {
                            if let Err(e) =
                                client.send_message(cassini_client::TcpClientMessage::publish(
                                    topic.to_string(),
                                    payload,
                                    None,
                                ))
                            {
                                error!("could not publish on '{topic}': {e}");
                            }
                        }
                        None => error!("no broker client to publish on '{topic}'"),
                    }
                }),
                local: resources.authority.clone(),
                own_identity,
            },
            myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;
        match ractor::call!(ledger, LedgerMessage::RevocationChanges) {
            Ok(port) => port.subscribe(publisher.clone(), |_| {
                Some(PublisherMessage::RevocationsChanged)
            }),
            Err(e) => {
                return Err(ActorProcessingErr::from(format!(
                    "the ledger did not give its revocation port: {e}"
                )));
            }
        }
        resources
            .configuration
            .runtime_changes()
            .subscribe(myself.clone(), |changed| {
                Some(ConsumerSupervisorMessage::RuntimeChanged {
                    section: changed.section,
                })
            });
        for section in ["policy", "groups"] {
            let _ = myself.cast(ConsumerSupervisorMessage::RuntimeChanged {
                section: section.to_string(),
            });
        }
        publisher.cast(PublisherMessage::Start)?;

        drop(myself.send_interval(ACTIVITY_REPORT_EVERY, || {
            ConsumerSupervisorMessage::ReportActivity
        }));

        Ok(ConsumerSupervisorState {
            projection_actor,
            ledger,
            healthcheck: resources.healthcheck,
            publisher,
            configuration: resources.configuration,
            broker_client: resources.tcp_client,
            infrastructure: resources.infrastructure,
            activity: crate::activity::Activity::new(),
            draining: false,
            rejuvenating: false,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ConsumerSupervisorMessage::PrepareShutdown => {
                if state.draining {
                    // A failure already started a drain that ends in exit(1);
                    // the handshake is moot.
                    warn!(
                        "CONSUMER_SUPERVISOR: PrepareShutdown received while already draining; \
                         the pending exit will end the process"
                    );
                } else {
                    state.draining = true;
                    state.rejuvenating = true;
                    info!(
                        "CONSUMER_SUPERVISOR: PrepareShutdown received (rejuvenation), \
                         unsubscribing from Cassini and entering drain window"
                    );
                    Self::unsubscribe_from_topics(&state.broker_client);
                    Self::schedule_drain_end(&myself);
                }
            }

            ConsumerSupervisorMessage::RuntimeChanged { section } => {
                Self::pass_on_section(state, &section).await;
            }

            ConsumerSupervisorMessage::AuthorityRequested => {
                let _ = state.publisher.cast(PublisherMessage::SnapshotRequested);
            }

            ConsumerSupervisorMessage::ReportActivity => {
                let report = state.activity.take_report(std::time::Instant::now());
                info!("CONSUMER_SUPERVISOR: {report}");
            }

            ConsumerSupervisorMessage::DrainElapsed => {
                warn!("CONSUMER_SUPERVISOR: drain window elapsed, beginning shutdown");

                // Nothing more is published once the drain is over.
                state
                    .publisher
                    .stop(Some("supervisor shutting down".into()));

                // The ledger first: it is the source of truth, and stopping it
                // waits for the append in progress. The projection is derived
                // and rebuilt from the ledger at startup, so anything still
                // queued for it is not lost.
                match state
                    .ledger
                    .stop_and_wait(
                        Some("supervisor shutting down".into()),
                        Some(Duration::from_secs(10)),
                    )
                    .await
                {
                    Ok(()) => info!("CONSUMER_SUPERVISOR: ledger stopped cleanly"),
                    Err(e) => error!(
                        "CONSUMER_SUPERVISOR: ledger did not stop cleanly within 10s, \
                         exiting anyway: {e:?}"
                    ),
                }

                // The projection owns its own graph controller as a linked
                // child, so stopping it flushes the graph controller too.
                match state
                    .projection_actor
                    .stop_and_wait(
                        Some("supervisor shutting down".into()),
                        Some(Duration::from_secs(30)),
                    )
                    .await
                {
                    Ok(()) => info!("CONSUMER_SUPERVISOR: projection actor stopped cleanly"),
                    Err(e) => error!(
                        "CONSUMER_SUPERVISOR: projection actor did not stop cleanly \
                         within 30s, exiting anyway: {e:?}"
                    ),
                }

                if state.rejuvenating {
                    // Our own children are down; let the infrastructure stop.
                    // Its termination (handle_supervisor_evt) exits the process
                    // with a non-zero status.
                    info!(
                        "CONSUMER_SUPERVISOR: domain shutdown complete, releasing infrastructure"
                    );
                    if let Err(e) = state
                        .infrastructure
                        .cast(InfrastructureSupervisorMessage::ProceedWithShutdown)
                    {
                        error!(
                            "CONSUMER_SUPERVISOR: could not tell infrastructure to proceed ({e}); \
                             exiting now"
                        );
                        std::process::exit(1);
                    }
                } else {
                    warn!("CONSUMER_SUPERVISOR: exiting now");
                    std::process::exit(1);
                }
            }

            ConsumerSupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { registration_id } => {
                    // The builder has already (re)subscribed to every request
                    // topic; this is only the health signal. No graph signal:
                    // this agent's health never depends on graph availability.
                    info!(
                        %registration_id,
                        topics = REQUEST_TOPICS.len(),
                        "CONSUMER_SUPERVISOR: registered; the builder subscribes to every request topic"
                    );
                    let _ = state.healthcheck.cassini_connected();
                }
                ClientEvent::MessagePublished {
                    topic,
                    payload,
                    published_by,
                    ..
                } => {
                    state.activity.record(&topic, std::time::Instant::now());
                    info!(
                        %topic,
                        bytes = payload.len(),
                        requester = published_by.as_deref().unwrap_or("unverified"),
                        draining = state.draining,
                        "CONSUMER_SUPERVISOR: request received"
                    );
                    if state.draining {
                        warn!(
                            "CONSUMER_SUPERVISOR: draining -- logging message on topic \
                             '{topic}' ({} bytes) instead of dispatching; will be lost on \
                             exit",
                            payload.len()
                        );
                        // TODO: publish to dead-letter queue once Cassini supports one.
                    } else {
                        Self::dispatch_request(topic, payload, published_by);
                    }
                }
                ClientEvent::TransportError { reason } => {
                    // Reported so the health check can fail and the Job restart
                    // this pod. Without it a consumer that lost its Cassini
                    // session stays up but subscribed to nothing.
                    warn!("Transport error (awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => {}
            },
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        message: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisionEvent::ActorTerminated(actor, _, reason) => {
                // The infrastructure supervisor is a direct child (the builder
                // spawned it linked; the health check is its child, so
                // rejuvenation arrives as PrepareShutdown, not as a termination
                // here). Its termination is the end of the shutdown handshake,
                // or the infrastructure dying on its own; either way the process
                // exits so the Job starts a pod with fresh certificates. It is
                // spawned unnamed, so match by id.
                if actor.get_id() == state.infrastructure.get_id() {
                    info!(
                        "CONSUMER_SUPERVISOR: infrastructure terminated ({reason:?}), exiting so the Job restarts the pod"
                    );
                    std::process::exit(1);
                } else if actor.get_id() == state.ledger.get_id() && !state.draining {
                    // Not our own doing: the ledger is this agent's source of
                    // truth and nothing can safely continue without it.
                    error!(
                        "CONSUMER_SUPERVISOR: ledger actor terminated ({reason:?}) unexpectedly -- exiting"
                    );
                    std::process::exit(1);
                }
            }
            SupervisionEvent::ActorFailed(actor, err) => {
                error!("CONSUMER_SUPERVISOR: child {actor:?} failed: {err}");
                if actor.get_id() == state.ledger.get_id() {
                    error!("CONSUMER_SUPERVISOR: the ledger failed -- exiting");
                    std::process::exit(1);
                }
                // Any other child failure is fatal too, but give in-flight
                // work a bounded window first (handle() logs, not dispatches,
                // while draining), then DrainElapsed stops the ledger and
                // projection and exits.
                if !state.draining {
                    state.draining = true;
                    warn!(
                        "CONSUMER_SUPERVISOR: entering drain window before exit; any \
                         requests that arrive will be logged, not processed"
                    );
                    Self::schedule_drain_end(&myself);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::GrantRecord;
    use credentials_agent_common::events::{GrantReason, RevocationReason};

    async fn spawn_ledger_registered(log_path: &str) -> ActorRef<LedgerMessage> {
        let (ledger, _) = Actor::spawn(
            Some(LEDGER_ACTOR_NAME.to_string()),
            LedgerActor,
            LedgerActorArgs {
                log_path: log_path.to_string(),
                projection_actor: crate::projection::tests::fake_projection_actor().await,
            },
        )
        .await
        .expect("ledger should spawn cleanly");
        ledger
    }

    async fn check_grant(ledger: &ActorRef<LedgerMessage>, identity: &str) -> Option<GrantRecord> {
        match ractor::rpc::call(
            ledger,
            |reply| LedgerMessage::CheckGrant {
                identity: identity.to_string(),
                reply,
            },
            None,
        )
        .await
        .expect("call should succeed")
        {
            ractor::rpc::CallResult::Success(result) => result,
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn deserialize_and_dispatch_forwards_a_real_event_to_the_real_ledger() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let ledger = spawn_ledger_registered(path.to_str().unwrap()).await;

        let event = CredentialsEvent::Grant {
            target_identity: "alice".to_string(),
            permissions: vec![cert_issuer_common::permission::Permission {
                scope: cert_issuer_common::permission::PermissionScope::ServiceToService,
                target: None,
            }],
            granted_by: "admin".to_string(),
            timestamp: 1000,
            reason: GrantReason::UserOnboarding,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&event)
            .unwrap()
            .to_vec();

        // The real function every incoming Cassini message actually
        // goes through -- not a reimplementation of it.
        ConsumerSupervisor::deserialize_and_dispatch(CREDENTIALS_GRANTS_TOPIC.to_string(), bytes);

        // A subsequent call() to the same actor only returns after
        // every prior message (including our dispatch above) has
        // already been processed, by ractor's own sequential-mailbox
        // guarantee -- no arbitrary sleep needed to synchronize.
        let grant = check_grant(&ledger, "alice").await;
        assert!(grant.is_some());
        assert_eq!(grant.unwrap().granted_by, "admin");

        ledger.stop_and_wait(None, None).await.unwrap();
    }

    #[tokio::test]
    async fn deserialize_and_dispatch_handles_missing_ledger_gracefully() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        // No ledger registered at all -- must not panic, just log and
        // drop, matching the documented behavior for this case.
        let event = CredentialsEvent::Revoke {
            serial: "abc123".to_string(),
            reason: RevocationReason::Compromised,
            revoked_by: "admin".to_string(),
            timestamp: 1000,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&event)
            .unwrap()
            .to_vec();

        ConsumerSupervisor::deserialize_and_dispatch(
            CREDENTIALS_REVOCATIONS_TOPIC.to_string(),
            bytes,
        );
        // Reaching this line at all is the assertion -- no panic.
    }
}

#[cfg(test)]
mod issuance_wiring_tests {
    use super::*;
    use crate::state::GrantRecord;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_token(iss: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"iss":"{iss}"}}"#));
        format!("{header}.{payload}.")
    }

    async fn check_grant(ledger: &ActorRef<LedgerMessage>, identity: &str) -> Option<GrantRecord> {
        match ractor::rpc::call(
            ledger,
            |reply| LedgerMessage::CheckGrant {
                identity: identity.to_string(),
                reply,
            },
            None,
        )
        .await
        .unwrap()
        {
            ractor::rpc::CallResult::Success(result) => result,
            other => panic!("expected Success, got {other:?}"),
        }
    }

    async fn check_renewal(
        ledger: &ActorRef<LedgerMessage>,
        identity: &str,
        instance_binding: &str,
    ) -> bool {
        match ractor::rpc::call(
            ledger,
            |reply| LedgerMessage::CheckRenewal {
                identity: identity.to_string(),
                instance_binding: instance_binding.to_string(),
                reply,
            },
            None,
        )
        .await
        .unwrap()
        {
            ractor::rpc::CallResult::Success(result) => result,
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn full_pipeline_from_raw_bytes_through_dispatch_to_a_recorded_issuance() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        // Real, end-to-end proof: raw bytes arriving on
        // CREDENTIALS_ISSUANCE_REQUESTS_TOPIC -> deserialize_and_dispatch
        // -> IssuanceActor's real async handle() -> IssuanceHandler's
        // real logic (classify, validate, grant/renewal checks, sign)
        // -> a genuinely recorded IssuanceCompleted event in the ledger.
        // No shortcuts -- every real component in the actual chain.

        let cert_issuer = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "system:serviceaccount:polar:admin-tool".to_string(),
                    instance_binding: Some("admin-tool-uid".to_string()),
                    jti: None,
                    normalized_identity: None,
                },
            ))
            .mount(&cert_issuer)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::IssueResponse {
                    certificate_pem: "-----BEGIN CERTIFICATE-----\nfake\n-----END CERTIFICATE-----\n"
                        .to_string(),
                    ca_chain_pem: "-----BEGIN CERTIFICATE-----\nfake-ca\n-----END CERTIFICATE-----\n"
                        .to_string(),
                    serial_number: "full-pipeline-test-serial".to_string(),
                    session_id: "00000000-0000-0000-0000-000000000099".to_string(),
                    expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
                },
            ))
            .mount(&cert_issuer)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.jsonl");
        let (ledger, _) = LedgerActor::spawn(
            Some(LEDGER_ACTOR_NAME.to_string()),
            LedgerActor,
            LedgerActorArgs {
                log_path: log_path.to_str().unwrap().to_string(),
                projection_actor: crate::projection::tests::fake_projection_actor().await,
            },
        )
        .await
        .unwrap();

        let handler = IssuanceHandler {
            ledger: ledger.clone(),
            cert_issuer: HandshakeClient::new(cert_issuer.uri()),
            introspection: None,
            known_kubernetes_issuers: vec![
                "https://kubernetes.default.svc.cluster.local".to_string(),
            ],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let (issuance_actor, _) = IssuanceActor::spawn(
            Some(ISSUANCE_ACTOR_NAME.to_string()),
            IssuanceActor,
            IssuanceActorArgs { handler },
        )
        .await
        .unwrap();

        let request = IssuanceRequest {
            csr_pem: "fake-csr".to_string(),
            credential: make_token("https://kubernetes.default.svc.cluster.local"),
        };
        let envelope = Envelope {
            correlation_id: "test-correlation-id".to_string(),
            payload: request,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
            .unwrap()
            .to_vec();

        // The real function every incoming Cassini message on this
        // topic actually goes through.
        ConsumerSupervisor::deserialize_and_dispatch(
            CREDENTIALS_ISSUANCE_REQUESTS_TOPIC.to_string(),
            bytes,
        );

        // IssuanceActor processes on its own independent mailbox, so
        // (unlike LedgerActor's own sequential-mailbox synchronization
        // trick) there's no single deterministic checkpoint to await
        // here -- a short sleep is a real, honest compromise, not a
        // fully deterministic wait.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        assert!(
            check_renewal(
                &ledger,
                "system:serviceaccount:polar:admin-tool",
                "admin-tool-uid"
            )
            .await,
            "IssuanceCompleted must have actually been recorded in the ledger by now"
        );
        assert!(
            check_grant(&ledger, "system:serviceaccount:polar:admin-tool")
                .await
                .is_none()
        );

        issuance_actor.stop_and_wait(None, None).await.unwrap();
        ledger.stop_and_wait(None, None).await.unwrap();
    }
}

#[cfg(test)]
mod agent_escrow_wiring_tests {
    use super::*;
    use crate::agent_escrow_issuance::AgentEscrowIssuanceHandler;
    use crate::agent_escrow_issuance_actor::{
        AgentEscrowIssuanceActor, AgentEscrowIssuanceActorArgs,
    };
    use crate::agent_escrow_key::AgentEscrowKeyHandler;
    use crate::agent_escrow_key_actor::{AgentEscrowKeyActor, AgentEscrowKeyActorArgs};
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use credentials_agent_common::agent_escrow_issuance::AgentEscrowIssuanceRequest;
    use credentials_agent_common::agent_escrow_key::{AgentEscrowKeyReply, AgentEscrowKeyRequest};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_token(iss: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"iss":"{iss}"}}"#));
        format!("{header}.{payload}.")
    }

    async fn check_agent_escrow_key(
        ledger: &ActorRef<LedgerMessage>,
        identity: &str,
    ) -> Option<crate::state::AgentEscrowCertificateRecord> {
        match ractor::rpc::call(
            ledger,
            |reply| LedgerMessage::CheckAgentEscrowKey {
                identity: identity.to_string(),
                reply,
            },
            None,
        )
        .await
        .unwrap()
        {
            ractor::rpc::CallResult::Success(result) => result,
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn full_pipeline_from_raw_bytes_through_dispatch_to_a_recorded_agent_escrow_issuance() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        // Mirrors issuance_wiring_tests's own
        // full_pipeline_from_raw_bytes_through_dispatch_to_a_recorded_issuance
        // exactly, proving the same real, end-to-end chain for the new
        // topic/actor pair specifically: raw bytes on
        // CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC ->
        // deserialize_and_dispatch -> AgentEscrowIssuanceActor's real
        // async handle() -> AgentEscrowIssuanceHandler's real logic ->
        // a genuinely recorded AgentEscrowIssuanceCompleted event.

        let cert_issuer = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "system:serviceaccount:polar:git-observer".to_string(),
                    instance_binding: Some("pod-uid-1".to_string()),
                    jti: None,
                    normalized_identity: None,
                },
            ))
            .mount(&cert_issuer)
            .await;
        Mock::given(method("POST"))
            .and(path("/issue"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(cert_issuer_common::IssueResponse {
                    certificate_pem:
                        "-----BEGIN CERTIFICATE-----\nfake-service\n-----END CERTIFICATE-----\n"
                            .to_string(),
                    ca_chain_pem:
                        "-----BEGIN CERTIFICATE-----\nfake-ca\n-----END CERTIFICATE-----\n"
                            .to_string(),
                    serial_number: "agent-escrow-wiring-test-serial".to_string(),
                    session_id: "00000000-0000-0000-0000-000000000098".to_string(),
                    expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
                }),
            )
            .mount(&cert_issuer)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.jsonl");
        let (ledger, _) = LedgerActor::spawn(
            Some(LEDGER_ACTOR_NAME.to_string()),
            LedgerActor,
            LedgerActorArgs {
                log_path: log_path.to_str().unwrap().to_string(),
                projection_actor: crate::projection::tests::fake_projection_actor().await,
            },
        )
        .await
        .unwrap();

        let handler = AgentEscrowIssuanceHandler {
            ledger: ledger.clone(),
            cert_issuer: HandshakeClient::new(cert_issuer.uri()),
            known_kubernetes_issuers: vec![
                "https://kubernetes.default.svc.cluster.local".to_string(),
            ],
            escrow_keypair: crate::escrow::tests::test_keypair(),
        };

        let (agent_escrow_issuance_actor, _) = AgentEscrowIssuanceActor::spawn(
            Some(AGENT_ESCROW_ISSUANCE_ACTOR_NAME.to_string()),
            AgentEscrowIssuanceActor,
            AgentEscrowIssuanceActorArgs { handler },
        )
        .await
        .unwrap();

        let request = AgentEscrowIssuanceRequest {
            csr_pem: "fake-csr".to_string(),
            credential: make_token("https://kubernetes.default.svc.cluster.local"),
        };
        let envelope = Envelope {
            correlation_id: "test-correlation-id".to_string(),
            payload: request,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
            .unwrap()
            .to_vec();

        // The real function every incoming Cassini message on this
        // topic actually goes through.
        ConsumerSupervisor::deserialize_and_dispatch(
            CREDENTIALS_AGENT_ESCROW_ISSUANCE_REQUESTS_TOPIC.to_string(),
            bytes,
        );

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let record =
            check_agent_escrow_key(&ledger, "system:serviceaccount:polar:git-observer").await;
        assert!(
            record.is_some(),
            "AgentEscrowIssuanceCompleted must have actually been recorded in the ledger by now"
        );
        assert_eq!(record.unwrap().serial, "agent-escrow-wiring-test-serial");

        agent_escrow_issuance_actor
            .stop_and_wait(None, None)
            .await
            .unwrap();
        ledger.stop_and_wait(None, None).await.unwrap();
    }

    struct CapturingBrokerClient {
        captured: std::sync::Arc<tokio::sync::Mutex<Option<Vec<u8>>>>,
    }

    #[ractor::async_trait]
    impl ractor::Actor for CapturingBrokerClient {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let TcpClientMessage::Publish { payload, .. } = message {
                *self.captured.lock().await = Some(payload);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn full_pipeline_from_raw_bytes_through_dispatch_answers_the_agent_escrow_key_query() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        // Same real-chain proof as the issuance test above, for the
        // agent escrow-key query specifically -- a genuinely separate
        // actor and topic pair. A real, capturing fake broker client
        // (registered under BROKER_CLIENT_NAME, the exact name
        // AgentEscrowKeyActor's own handle() looks up via where_is)
        // lets this test assert on the actual, real published reply
        // bytes, not merely that dispatch didn't panic -- this query
        // has no ledger side effect of its own to poll for the way
        // the issuance test above does.
        let captured = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let (broker_client, _) = ractor::Actor::spawn(
            Some(BROKER_CLIENT_NAME.to_string()),
            CapturingBrokerClient {
                captured: captured.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.jsonl");
        let (ledger, _) = LedgerActor::spawn(
            Some(LEDGER_ACTOR_NAME.to_string()),
            LedgerActor,
            LedgerActorArgs {
                log_path: log_path.to_str().unwrap().to_string(),
                projection_actor: crate::projection::tests::fake_projection_actor().await,
            },
        )
        .await
        .unwrap();

        // Seed a real requester certificate.
        let certificate_pem = {
            let mut rng = rand::rng();
            let private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
            let private_key_pem = rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(
                &private_key,
                rsa::pkcs8::LineEnding::LF,
            )
            .unwrap();
            let key_pair = rcgen::KeyPair::from_pem(&private_key_pem).unwrap();
            let params = rcgen::CertificateParams::new(vec![
                "git-observer.polar.svc.cluster.local".to_string(),
            ])
            .unwrap();
            params.self_signed(&key_pair).unwrap().pem()
        };
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: credentials_agent_common::events::CredentialsEvent::AgentEscrowIssuanceCompleted {
                    identity: "git-observer".to_string(),
                    instance_binding: "pod-uid-1".to_string(),
                    serial: "agent-escrow-key-wiring-test-serial".to_string(),
                    timestamp: 2000,
                    certificate_pem: certificate_pem.clone(),
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let handler = AgentEscrowKeyHandler {
            ledger: ledger.clone(),
        };
        let (agent_escrow_key_actor, _) = AgentEscrowKeyActor::spawn(
            Some(AGENT_ESCROW_KEY_ACTOR_NAME.to_string()),
            AgentEscrowKeyActor,
            AgentEscrowKeyActorArgs { handler },
        )
        .await
        .unwrap();

        let request = AgentEscrowKeyRequest {
            identity: "git-observer".to_string(),
        };
        let envelope = Envelope {
            correlation_id: "test-correlation-id-2".to_string(),
            payload: request,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
            .unwrap()
            .to_vec();

        ConsumerSupervisor::deserialize_and_dispatch(
            CREDENTIALS_AGENT_ESCROW_KEY_REQUESTS_TOPIC.to_string(),
            bytes,
        );

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let captured_bytes = captured
            .lock()
            .await
            .clone()
            .expect("the actor's own reply must have actually been published");
        let reply_envelope =
            rkyv::from_bytes::<Envelope<AgentEscrowKeyReply>, rkyv::rancor::Error>(&captured_bytes)
                .unwrap();
        assert_eq!(reply_envelope.correlation_id, "test-correlation-id-2");
        match reply_envelope.payload {
            AgentEscrowKeyReply::Found { public_key_pem } => {
                let expected =
                    cert_issuer_common::public_key::extract_public_key_pem(&certificate_pem)
                        .unwrap();
                assert_eq!(public_key_pem, expected);
            }
            other => panic!("expected Found, got {other:?}"),
        }

        agent_escrow_key_actor
            .stop_and_wait(None, None)
            .await
            .unwrap();
        broker_client.stop_and_wait(None, None).await.unwrap();
        ledger.stop_and_wait(None, None).await.unwrap();
    }
}

#[cfg(test)]
mod retrieve_secret_wiring_tests {
    use super::*;
    use crate::retrieve_secret::RetrieveSecretHandler;
    use crate::retrieve_secret_actor::{RetrieveSecretActor, RetrieveSecretActorArgs};
    use credentials_agent_common::escrow_secret::EscrowedSecret;
    use credentials_agent_common::retrieve_secret::{RetrieveSecretReply, RetrieveSecretRequest};

    struct CapturingBrokerClient {
        captured: std::sync::Arc<tokio::sync::Mutex<Option<Vec<u8>>>>,
    }

    #[ractor::async_trait]
    impl ractor::Actor for CapturingBrokerClient {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let TcpClientMessage::Publish { payload, .. } = message {
                *self.captured.lock().await = Some(payload);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn full_pipeline_from_raw_bytes_through_the_special_dispatch_path_to_a_real_reply() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        // Proves the actual thing dispatch_store_secret_request's own
        // doc comment named as the real reason this dispatch path
        // exists outside the shared Supervisor trait: published_by
        // genuinely reaches the handler, derived from this call's own
        // argument, not from anything in the serialized request
        // bytes -- the same real, end-to-end proof
        // agent_escrow_wiring_tests already established for the
        // ordinary-path actors, applied here to the one, special
        // dispatch path this crate has that they don't use.

        let captured = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let (broker_client, _) = ractor::Actor::spawn(
            Some(BROKER_CLIENT_NAME.to_string()),
            CapturingBrokerClient {
                captured: captured.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.jsonl");
        let (ledger, _) = LedgerActor::spawn(
            Some(LEDGER_ACTOR_NAME.to_string()),
            LedgerActor,
            LedgerActorArgs {
                log_path: log_path.to_str().unwrap().to_string(),
                projection_actor: crate::projection::tests::fake_projection_actor().await,
            },
        )
        .await
        .unwrap();

        // As in a real cluster: the requester's mTLS certificate cannot encrypt, its RSA agent escrow
        // key can, and a policy permits it this one secret.
        use crate::retrieve_secret::tests as fixtures;
        fixtures::seed_mtls_certificate(&ledger, "requester").await;
        let requester_private_key = fixtures::seed_escrow_certificate(&ledger, "requester").await;
        let escrow_keypair = crate::escrow::tests::test_keypair();
        let original_plaintext = b"a real secret via the real dispatch path";
        fixtures::store_secret(&ledger, &escrow_keypair, "gitlab-token", original_plaintext).await;

        let handler = RetrieveSecretHandler {
            escrow_keypair,
            ledger: ledger.clone(),
            policy_actor: fixtures::policy_actor(Some(("requester", "gitlab-token"))).await,
        };
        let (retrieve_secret_actor, _) = RetrieveSecretActor::spawn(
            Some(RETRIEVE_SECRET_ACTOR_NAME.to_string()),
            RetrieveSecretActor,
            RetrieveSecretActorArgs { handler },
        )
        .await
        .unwrap();

        let request = RetrieveSecretRequest {
            name: "gitlab-token".to_string(),
        };
        let envelope = Envelope {
            correlation_id: "test-correlation-id-retrieve".to_string(),
            payload: request,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
            .unwrap()
            .to_vec();

        // The actual, real, private dispatch function this test
        // exists to exercise -- not deserialize_and_dispatch, since
        // this specific topic is deliberately routed elsewhere (see
        // ConsumerSupervisor's own ClientEvent::MessagePublished
        // handler and dispatch_retrieve_secret_request's own doc
        // comment for why).
        ConsumerSupervisor::dispatch_retrieve_secret_request(bytes, Some("requester".to_string()));

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let captured_bytes = captured
            .lock()
            .await
            .clone()
            .expect("the actor's own reply must have actually been published");
        let reply_envelope =
            rkyv::from_bytes::<Envelope<RetrieveSecretReply>, rkyv::rancor::Error>(&captured_bytes)
                .unwrap();
        assert_eq!(
            reply_envelope.correlation_id,
            "test-correlation-id-retrieve"
        );
        match reply_envelope.payload {
            RetrieveSecretReply::Found { secret } => {
                let decrypted = secret.decrypt(&requester_private_key).unwrap();
                assert_eq!(&*decrypted, original_plaintext);
            }
            other => panic!("expected Found, got {other:?}"),
        }

        retrieve_secret_actor
            .stop_and_wait(None, None)
            .await
            .unwrap();
        broker_client.stop_and_wait(None, None).await.unwrap();
        ledger.stop_and_wait(None, None).await.unwrap();
    }
}

#[cfg(test)]
mod agent_escrow_keypair_bootstrap_wiring_tests {
    use super::*;
    use crate::agent_escrow_keypair_bootstrap::AgentEscrowKeypairBootstrapHandler;
    use crate::agent_escrow_keypair_bootstrap_actor::{
        AgentEscrowKeypairBootstrapActor, AgentEscrowKeypairBootstrapActorArgs,
    };
    use credentials_agent_common::agent_escrow_keypair_bootstrap::{
        RetrieveAgentEscrowKeypairReply, RetrieveAgentEscrowKeypairRequest,
        agent_escrow_keypair_secret_name,
    };
    use credentials_agent_common::escrow_secret::EscrowedSecret;
    use x25519_dalek::{EphemeralSecret, PublicKey};

    struct CapturingBrokerClient {
        captured: std::sync::Arc<tokio::sync::Mutex<Option<Vec<u8>>>>,
    }

    #[ractor::async_trait]
    impl ractor::Actor for CapturingBrokerClient {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let TcpClientMessage::Publish { payload, .. } = message {
                *self.captured.lock().await = Some(payload);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn full_pipeline_from_raw_bytes_through_the_special_dispatch_path_to_a_real_reply() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        // Mirrors retrieve_secret_wiring_tests's own test of the
        // identical shape -- proves published_by genuinely reaches
        // this handler too, via this crate's second dispatch path
        // outside the shared Supervisor trait.

        let captured = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let (broker_client, _) = ractor::Actor::spawn(
            Some(BROKER_CLIENT_NAME.to_string()),
            CapturingBrokerClient {
                captured: captured.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.jsonl");
        let (ledger, _) = LedgerActor::spawn(
            Some(LEDGER_ACTOR_NAME.to_string()),
            LedgerActor,
            LedgerActorArgs {
                log_path: log_path.to_str().unwrap().to_string(),
                projection_actor: crate::projection::tests::fake_projection_actor().await,
            },
        )
        .await
        .unwrap();

        // Seed a real, stored "escrow keypair" secret.
        let escrow_keypair = crate::escrow::tests::test_keypair();
        let original_plaintext = b"a real escrowed keypair, via the real dispatch path";
        let stored_secret =
            EscrowedSecret::encrypt(original_plaintext, escrow_keypair.public_key_pem()).unwrap();
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: credentials_agent_common::events::CredentialsEvent::SecretStored {
                    name: agent_escrow_keypair_secret_name("git-observer"),
                    secret: stored_secret,
                    stored_by: "git-observer".to_string(),
                    timestamp: 1000,
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let handler = AgentEscrowKeypairBootstrapHandler {
            escrow_keypair,
            ledger: ledger.clone(),
        };
        let (agent_escrow_keypair_bootstrap_actor, _) = AgentEscrowKeypairBootstrapActor::spawn(
            Some(AGENT_ESCROW_KEYPAIR_BOOTSTRAP_ACTOR_NAME.to_string()),
            AgentEscrowKeypairBootstrapActor,
            AgentEscrowKeypairBootstrapActorArgs { handler },
        )
        .await
        .unwrap();

        let requester_secret = EphemeralSecret::random();
        let requester_public_key = PublicKey::from(&requester_secret).to_bytes();

        let request = RetrieveAgentEscrowKeypairRequest {
            ephemeral_public_key: requester_public_key,
        };
        let envelope = Envelope {
            correlation_id: "test-correlation-id-bootstrap".to_string(),
            payload: request,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
            .unwrap()
            .to_vec();

        // The actual, real, private dispatch function this test
        // exists to exercise.
        ConsumerSupervisor::dispatch_agent_escrow_keypair_bootstrap_request(
            bytes,
            Some("git-observer".to_string()),
        );

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let captured_bytes = captured
            .lock()
            .await
            .clone()
            .expect("the actor's own reply must have actually been published");
        let reply_envelope = rkyv::from_bytes::<
            Envelope<RetrieveAgentEscrowKeypairReply>,
            rkyv::rancor::Error,
        >(&captured_bytes)
        .unwrap();
        assert_eq!(
            reply_envelope.correlation_id,
            "test-correlation-id-bootstrap"
        );
        match reply_envelope.payload {
            RetrieveAgentEscrowKeypairReply::Found { payload } => {
                let decrypted = payload.decrypt(requester_secret).unwrap();
                assert_eq!(decrypted.as_slice(), original_plaintext);
            }
            other => panic!("expected Found, got {other:?}"),
        }

        agent_escrow_keypair_bootstrap_actor
            .stop_and_wait(None, None)
            .await
            .unwrap();
        broker_client.stop_and_wait(None, None).await.unwrap();
        ledger.stop_and_wait(None, None).await.unwrap();
    }
}

#[cfg(test)]
mod identity_normalization_wiring_tests {
    use super::*;
    use crate::identity_normalization::IdentityNormalizationHandler;
    use crate::identity_normalization_actor::{
        IdentityNormalizationActor, IdentityNormalizationActorArgs,
    };
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn full_pipeline_from_raw_bytes_through_dispatch_reaches_cert_issuer() {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;

        let cert_issuer = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/validate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                cert_issuer_common::ValidateResponse {
                    workload_identity: "testuser".to_string(),
                    instance_binding: None,
                    jti: None,
                    normalized_identity: Some("testuser.polar.keycloak-issuer".to_string()),
                },
            ))
            .mount(&cert_issuer)
            .await;

        let handler = IdentityNormalizationHandler {
            cert_issuer: HandshakeClient::new(cert_issuer.uri()),
        };

        let (_actor, _) = IdentityNormalizationActor::spawn(
            Some(IDENTITY_NORMALIZATION_ACTOR_NAME.to_string()),
            IdentityNormalizationActor,
            IdentityNormalizationActorArgs { handler },
        )
        .await
        .unwrap();

        let request = IdentityNormalizationRequest {
            credential: "any-token".to_string(),
        };
        let envelope = Envelope {
            correlation_id: "test-correlation-id".to_string(),
            payload: request,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
            .unwrap()
            .to_vec();

        // The real function every incoming Cassini message on this
        // topic actually goes through.
        ConsumerSupervisor::deserialize_and_dispatch(
            CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC.to_string(),
            bytes,
        );

        // The actor processes on its own independent mailbox -- same
        // honest compromise as issuance's own full-pipeline test, a
        // short sleep rather than a fully deterministic wait.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // No real broker is registered in this test, so the actor's
        // final publish-the-reply step is expected to fail/log (no
        // BROKER_CLIENT_NAME actor exists here) -- what this proves
        // instead is that the full chain up to that point genuinely
        // ran: dispatch -> actor -> handler -> a real HTTP call to
        // cert-issuer's /validate, confirmed by the mock's own
        // request count, not just assumed from no panic occurring.
        let received = cert_issuer.received_requests().await.unwrap();
        assert_eq!(
            received.len(),
            1,
            "cert-issuer's /validate must have been called exactly once"
        );
    }
}

#[cfg(test)]
mod decrypt_on_behalf_of_wiring_tests {
    use super::*;
    use crate::decrypt_on_behalf_of::DecryptOnBehalfOfHandler;
    use crate::decrypt_on_behalf_of_actor::{DecryptOnBehalfOfActor, DecryptOnBehalfOfActorArgs};
    use credentials_agent_common::decrypt_on_behalf_of::{
        DecryptOnBehalfOfReply, DecryptOnBehalfOfRequest,
    };
    use credentials_agent_common::escrow_secret::EscrowedSecret;

    struct CapturingBrokerClient {
        captured: std::sync::Arc<tokio::sync::Mutex<Option<Vec<u8>>>>,
    }

    #[ractor::async_trait]
    impl ractor::Actor for CapturingBrokerClient {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let TcpClientMessage::Publish { payload, .. } = message {
                *self.captured.lock().await = Some(payload);
            }
            Ok(())
        }
    }

    async fn spawn_policy_actor_permitting(
        identity: &str,
        resource_id: &str,
    ) -> ActorRef<polar_policy::PolicyMessage> {
        let (permissions_actor, _) =
            ractor::Actor::spawn(None, polar_permissions::PermissionsActor, ())
                .await
                .unwrap();
        let (policy_actor, _) = ractor::Actor::spawn(
            None,
            polar_policy::PolicyActor,
            polar_policy::PolicyActorArgs { permissions_actor },
        )
        .await
        .unwrap();

        let policy = format!(
            r#"
            permit(
                principal == Identity::"{identity}",
                action == Action::"decrypt",
                resource == {resource_id}
            );
            "#
        );
        let result = ractor::call!(policy_actor, |reply| {
            polar_policy::PolicyMessage::UpdatePolicy {
                policy_source: policy,
                reply,
            }
        })
        .unwrap();
        result.unwrap();

        policy_actor
    }

    #[tokio::test]
    async fn full_pipeline_from_raw_bytes_through_the_special_dispatch_path_to_a_real_allowed_reply()
     {
        let _guard = super::NAMED_ACTOR_TEST_LOCK.lock().await;
        // Mirrors retrieve_secret_wiring_tests's own test of the
        // identical shape -- proves published_by genuinely reaches
        // this handler too, via this crate's real, new
        // dispatch_decrypt_on_behalf_of_request path, and that the
        // real, complete flow (certificate lookup, live PolicyActor
        // query, decrypt, re-encrypt) actually works end-to-end
        // through the real dispatch function, not merely the
        // handler in isolation (already proven directly in
        // decrypt_on_behalf_of.rs's own test module).

        let captured = std::sync::Arc::new(tokio::sync::Mutex::new(None));
        let (broker_client, _) = ractor::Actor::spawn(
            Some(BROKER_CLIENT_NAME.to_string()),
            CapturingBrokerClient {
                captured: captured.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.jsonl");
        let (ledger, _) = LedgerActor::spawn(
            Some(LEDGER_ACTOR_NAME.to_string()),
            LedgerActor,
            LedgerActorArgs {
                log_path: log_path.to_str().unwrap().to_string(),
                projection_actor: crate::projection::tests::fake_projection_actor().await,
            },
        )
        .await
        .unwrap();

        // Seed a real requester certificate.
        let mut rng = rand::rng();
        let requester_private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let requester_private_key_pem = rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(
            &requester_private_key,
            rsa::pkcs8::LineEnding::LF,
        )
        .unwrap();
        let requester_key_pair = rcgen::KeyPair::from_pem(&requester_private_key_pem).unwrap();
        let requester_params =
            rcgen::CertificateParams::new(vec!["requester.polar.svc.cluster.local".to_string()])
                .unwrap();
        let requester_certificate_pem = requester_params
            .self_signed(&requester_key_pair)
            .unwrap()
            .pem();
        let _ = ractor::rpc::call(
            &ledger,
            |reply| LedgerMessage::Append {
                event: credentials_agent_common::events::CredentialsEvent::IssuanceCompleted {
                    identity: "requester".to_string(),
                    instance_binding: "pod-uid-1".to_string(),
                    serial: "requester-serial-1".to_string(),
                    timestamp: 1000,
                    certificate_pem: requester_certificate_pem,
                },
                reply,
            },
            None,
        )
        .await
        .unwrap();

        let resource_id = r#"KubeSecret::"namespace/pod/ENV_VAR""#;
        let policy_actor = spawn_policy_actor_permitting("requester", resource_id).await;

        let escrow_keypair = crate::escrow::tests::test_keypair();
        let original_plaintext = b"a real, plaintext env var value, via the real dispatch path";
        let ciphertext =
            EscrowedSecret::encrypt(original_plaintext, escrow_keypair.public_key_pem()).unwrap();

        let handler = DecryptOnBehalfOfHandler {
            escrow_keypair,
            ledger: ledger.clone(),
            policy_actor,
        };
        let (decrypt_on_behalf_of_actor, _) = DecryptOnBehalfOfActor::spawn(
            Some(DECRYPT_ON_BEHALF_OF_ACTOR_NAME.to_string()),
            DecryptOnBehalfOfActor,
            DecryptOnBehalfOfActorArgs { handler },
        )
        .await
        .unwrap();

        let request = DecryptOnBehalfOfRequest {
            ciphertext,
            resource_id: resource_id.to_string(),
        };
        let envelope = Envelope {
            correlation_id: "test-correlation-id-decrypt-on-behalf-of".to_string(),
            payload: request,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
            .unwrap()
            .to_vec();

        // The actual, real, private dispatch function this test
        // exists to exercise.
        ConsumerSupervisor::dispatch_decrypt_on_behalf_of_request(
            bytes,
            Some("requester".to_string()),
        );

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let captured_bytes = captured
            .lock()
            .await
            .clone()
            .expect("the actor's own reply must have actually been published");
        let reply_envelope = rkyv::from_bytes::<
            Envelope<DecryptOnBehalfOfReply>,
            rkyv::rancor::Error,
        >(&captured_bytes)
        .unwrap();
        assert_eq!(
            reply_envelope.correlation_id,
            "test-correlation-id-decrypt-on-behalf-of"
        );
        match reply_envelope.payload {
            DecryptOnBehalfOfReply::Allowed { secret } => {
                let decrypted = secret.decrypt(&requester_private_key).unwrap();
                assert_eq!(&*decrypted, original_plaintext);
            }
            other => panic!("expected Allowed, got {other:?}"),
        }

        decrypt_on_behalf_of_actor
            .stop_and_wait(None, None)
            .await
            .unwrap();
        broker_client.stop_and_wait(None, None).await.unwrap();
        ledger.stop_and_wait(None, None).await.unwrap();
    }
}

#[cfg(test)]
mod builder_wiring_tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    /// The handler actors publish their replies through
    /// `where_is(BROKER_CLIENT_NAME)`. If this name and the one the agent
    /// builder registers ever differ, every reply is silently dropped: the
    /// lookup finds nothing, and nothing is logged.
    #[test]
    fn handler_actors_find_the_client_under_the_name_the_builder_registers() {
        assert_eq!(BROKER_CLIENT_NAME, format!("{SERVICE_NAME}.tcp"));
    }

    #[test]
    fn there_are_fifteen_distinct_request_topics() {
        let distinct: HashSet<&str> = REQUEST_TOPICS.iter().copied().collect();
        assert_eq!(REQUEST_TOPICS.len(), 15);
        assert_eq!(
            distinct.len(),
            15,
            "a topic is listed twice: {REQUEST_TOPICS:?}"
        );
    }

    /// The topics whose requester must be broker-verified are all subscribed
    /// to; dropping one from the list would silently stop that flow.
    #[test]
    fn every_topic_that_needs_the_verified_sender_is_subscribed() {
        for topic in [
            CREDENTIALS_STORE_SECRET_REQUESTS_TOPIC,
            CREDENTIALS_RETRIEVE_SECRET_REQUESTS_TOPIC,
            CREDENTIALS_RETRIEVE_AGENT_ESCROW_KEYPAIR_REQUESTS_TOPIC,
            CREDENTIALS_DECRYPT_ON_BEHALF_OF_REQUESTS_TOPIC,
        ] {
            assert!(
                REQUEST_TOPICS.contains(&topic),
                "{topic} is not subscribed to"
            );
        }
    }

    /// A minimal stand-in for the supervisor: records the client events it is
    /// sent, so the subscriptions' delivery closures can be exercised without
    /// a broker.
    struct FakeSupervisor {
        received: Arc<Mutex<Vec<ClientEvent>>>,
    }

    #[async_trait]
    impl Actor for FakeSupervisor {
        type Msg = ConsumerSupervisorMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let ConsumerSupervisorMessage::ClientEvent { event } = message {
                self.received.lock().unwrap().push(event);
            }
            Ok(())
        }
    }

    /// Every subscription hands its message to the supervisor as a
    /// `MessagePublished` with the topic, the payload and, above all, the
    /// broker-verified sender intact: the handlers derive who is asking from
    /// it, so losing or rewriting it would be a security bug.
    #[tokio::test]
    async fn each_subscription_forwards_its_topic_payload_and_verified_sender() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (supervisor, _) = Actor::spawn(
            None,
            FakeSupervisor {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let subscriptions = ConsumerSupervisor::request_subscriptions(&supervisor);
        let subscribed: HashSet<&str> = subscriptions.iter().map(|s| s.topic.as_str()).collect();
        let expected: HashSet<&str> = REQUEST_TOPICS.iter().copied().collect();
        assert_eq!(
            subscribed, expected,
            "one subscription per request topic, and no others"
        );

        for subscription in &subscriptions {
            (subscription.deliver)(polar_router::IncomingMessage {
                topic: subscription.topic.clone(),
                payload: vec![1, 2, 3],
                published_by: Some("a.verified.identity".to_string()),
                trace_ctx: None,
            });
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        let events = received.lock().unwrap();
        assert_eq!(events.len(), REQUEST_TOPICS.len());
        let mut forwarded = HashSet::new();
        for event in events.iter() {
            match event {
                ClientEvent::MessagePublished {
                    topic,
                    payload,
                    published_by,
                    ..
                } => {
                    assert_eq!(payload, &vec![1, 2, 3]);
                    assert_eq!(published_by.as_deref(), Some("a.verified.identity"));
                    forwarded.insert(topic.as_str());
                }
                other => panic!("expected MessagePublished, got {other:?}"),
            }
        }
        assert_eq!(forwarded, expected);
    }
}
