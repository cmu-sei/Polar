//! polar-agent-builder
//!
//! The real, shared, foundational sequencing every agent's own
//! pre_start has, so far, hand-rolled separately: spawn
//! HealthCheckActor, spawn RouterActor, register this agent's own
//! topics with it, spawn TcpClient via spawn_with_router sharing
//! that same router, and (optionally) spawn CryptoActor sharing both.
//! kube-consumer's own, real supervisor.rs and credentials-agent's
//! own supervisor.rs both, independently, contain a near-identical,
//! real version of this exact sequence today.
//!
//! Zero-trust by construction, not by convention: a Subscription
//! binds "register this topic with the router" and "genuinely
//! subscribe to it on the wire once connected" into one, real,
//! atomic unit -- there is no code path through this crate that
//! registers a topic without also, automatically, subscribing to it,
//! or vice versa. build() itself is the one, sole, real orchestrator;
//! nothing it produces can exist in a half-wired state.

pub mod capability;
pub use capability::Capability;
pub mod crypto;
pub use crypto::{CryptoClient, CryptoHandle};
pub mod health;
pub use health::{HealthClient, HealthHandle};
pub mod infrastructure;
pub use infrastructure::{
    InfrastructureEvent, InfrastructureSupervisor, InfrastructureSupervisorMessage,
};
pub mod message;
pub use message::{Encrypted, Outbound, Plaintext};
pub mod shutdown;
pub use shutdown::{DEFAULT_SHUTDOWN_BUDGET, SHUTDOWN_BUDGET_ENV};

use cassini_client::TcpClientMessage;
use cassini_types::ClientEvent;
use polar::cassini::TcpClient;
use polar_health::HealthCheckArgs;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use std::sync::Arc;

/// Binds a topic to the real, per-message deliver closure RouterActor
/// itself already expects -- exactly the same shape every existing,
/// hand-rolled `register_kube_topics`-style function already builds.
/// A real, atomic unit of intent: "I want this topic's messages,"
/// covering both the in-process router-registration half and the
/// real, wire-level Subscribe half together.
pub struct Subscription {
    pub topic: String,
    pub deliver: Box<dyn Fn(polar_router::IncomingMessage) + Send + Sync>,
}

impl Subscription {
    pub fn new(
        topic: impl Into<String>,
        deliver: impl Fn(polar_router::IncomingMessage) + Send + Sync + 'static,
    ) -> Self {
        Self {
            topic: topic.into(),
            deliver: Box::new(deliver),
        }
    }
}

/// The real, complete set of resources a successful build() produces
/// -- everything an agent's own pre_start needs to finish its own,
/// remaining, genuinely agent-specific wiring (spawning its own
/// domain actors, e.g. a graph controller). Every real handle here
/// is trait-based (HealthHandle, CryptoHandle), not a raw ActorRef --
/// the whole, real point of this crate is that an agent's own code
/// should never need to touch ractor primitives directly at all.
/// router and tcp_client remain genuinely, deliberately raw: they're
/// this crate's own, real, foundational building blocks, not
/// something a higher-level, trait-based wrapper genuinely adds value
/// over for every, real caller's own use.
///
/// healthcheck is never Option -- InfrastructureSupervisor's own
/// shutdown-coordination mechanism structurally depends on a real
/// HealthCheckActor existing, so build() always spawns one. Its
/// configuration is derived from the capabilities this agent
/// requires and from the deployment's own environment
/// (POLAR_HEALTH_DEP_ENDPOINTS and the other POLAR_HEALTH_* variables
/// HealthCheckActor already reads), not passed in by the agent.
#[derive(Clone)]
pub struct AgentResources {
    pub router: ActorRef<polar_router::RouterMessage>,
    pub tcp_client: TcpClient,
    pub healthcheck: crate::health::HealthHandle,
    pub crypto: Option<crate::crypto::CryptoHandle>,
    /// Every real ClientEvent this agent's own TcpClient produces --
    /// subscribe with your own, real mapping closure, exactly the
    /// same, established pattern as prepare_shutdown_port/
    /// graph_signal_port.
    pub events_output: Arc<ractor::OutputPort<ClientEvent>>,
    /// The "infra → domain" half of InfrastructureSupervisor's own,
    /// real shutdown-coordination and TCP-client-respawn protocol --
    /// see infrastructure::InfrastructureEvent's own doc comment.
    pub infrastructure_events: Arc<ractor::OutputPort<crate::infrastructure::InfrastructureEvent>>,
    /// The real, direct way to send
    /// infrastructure::InfrastructureSupervisorMessage::ProceedWithShutdown
    /// once your own, domain-specific children have genuinely, fully
    /// shut down.
    pub infrastructure: ActorRef<crate::infrastructure::InfrastructureSupervisorMessage>,
}

/// Seconds before its own certificate expires that an agent enters
/// rejuvenation mode. The same for every agent today.
const DEFAULT_REJUVENATION_THRESHOLD_SECS: i64 = 300;

pub struct AgentBuilder<M: ractor::Message> {
    myself: ActorRef<M>,
    service_name: String,
    subscriptions: Vec<Subscription>,
    requires: Vec<Capability>,
    map_lifecycle_event: Option<Box<dyn Fn(ClientEvent) -> Option<M> + Send + Sync>>,
    wants_crypto: bool,
    shutdown_budget: Option<std::time::Duration>,
}

impl<M: ractor::Message> AgentBuilder<M> {
    pub fn new(myself: ActorRef<M>, service_name: impl Into<String>) -> Self {
        Self {
            myself,
            service_name: service_name.into(),
            subscriptions: Vec::new(),
            requires: Vec::new(),
            map_lifecycle_event: None,
            wants_crypto: false,
            shutdown_budget: None,
        }
    }

    /// How long this agent's domain supervisor needs to drain, from being told to
    /// shut down to releasing the infrastructure. If it has not finished by then the
    /// process exits 1 regardless, so an agent whose drain can take longer than the
    /// default (20 s) must say so.
    ///
    /// This is a requirement, checked against what the deployment provides
    /// (`POLAR_SHUTDOWN_BUDGET_SECS`, which also sizes the pod's termination grace
    /// period): the deployment may give more, never less, and `build()` refuses to
    /// start when it gives less.
    pub fn with_shutdown_budget(mut self, budget: std::time::Duration) -> Self {
        self.shutdown_budget = Some(budget);
        self
    }

    /// Declares a capability this agent's own code needs from its
    /// deployment, beyond the ones a with_* method below already
    /// declares (for example Capability::KubeApiRead, which the builder
    /// itself does nothing for). build() checks everything declared
    /// against what the deployment says it provides
    /// (POLAR_CAPABILITIES) and refuses to start on a mismatch.
    pub fn requires(mut self, capability: Capability) -> Self {
        if !self.requires.contains(&capability) {
            self.requires.push(capability);
        }
        self
    }

    /// This agent writes to the graph over mTLS: its own health check
    /// then also watches the graph connection.
    pub fn with_graph_write(self) -> Self {
        self.requires(Capability::GraphWrite)
    }

    /// Spawns a real, live CryptoActor alongside this agent's own,
    /// real router and TcpClient once build() succeeds -- see
    /// crypto::CryptoHandle's own doc comment for what it exposes.
    /// Deliberately opt-in: an agent that never encrypts a payload
    /// (the observers) does not declare it, and credentials-agent's
    /// own, real, existing supervisor holds its own, direct escrow
    /// key, so build() genuinely can't assume every real caller
    /// wants it.
    pub fn with_encryption(mut self) -> Self {
        self.wants_crypto = true;
        self.requires(Capability::Encryption)
    }

    pub fn with_subscription(mut self, subscription: Subscription) -> Self {
        self.subscriptions.push(subscription);
        self
    }

    /// The real, required handler for every other, real
    /// ClientEvent variant this agent's own supervisor still wants
    /// to observe directly (Registered beyond subscribing,
    /// TransportError, etc.) -- exactly the same, real map_event
    /// shape lib::cassini::TcpClient::spawn_with_router itself
    /// already takes. Required: every real agent built so far
    /// genuinely needs at least CassiniConnected/CassiniDisconnected
    /// forwarded to its own healthcheck.
    pub fn with_lifecycle_handler(
        mut self,
        map_event: impl Fn(ClientEvent) -> Option<M> + Send + Sync + 'static,
    ) -> Self {
        self.map_lifecycle_event = Some(Box::new(map_event));
        self
    }

    pub async fn build(self) -> Result<AgentResources, ActorProcessingErr> {
        // Every agent built here is a Cassini client; the rest are what
        // its own code declared. Checked before anything is spawned, so
        // a mismatch with the deployment is one clear, immediate,
        // non-zero exit instead of a half-started agent.
        let mut required = self.requires.clone();
        if !required.contains(&Capability::Cassini) {
            required.insert(0, Capability::Cassini);
        }
        let provided = std::env::var(capability::PROVIDED_ENV).ok();
        match capability::check(&required, provided.as_deref()) {
            capability::CapabilityCheck::Satisfied => {}
            capability::CapabilityCheck::NotDeclared => {
                tracing::warn!(
                    "{} is not set: the deployment did not declare its capabilities, so \
                     this agent's requirements ({}) cannot be checked",
                    capability::PROVIDED_ENV,
                    required
                        .iter()
                        .map(|c| c.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            capability::CapabilityCheck::Missing(missing) => {
                return Err(ActorProcessingErr::from(capability::missing_message(
                    &missing,
                    provided.as_deref(),
                )));
            }
        }

        let shutdown_budget = shutdown::budget(
            self.shutdown_budget,
            std::env::var(SHUTDOWN_BUDGET_ENV).ok().as_deref(),
        )
        .map_err(ActorProcessingErr::from)?;

        // Derived, not passed in: whether to watch the graph follows
        // from the capabilities. The dependency endpoints are read by
        // HealthCheckActor from POLAR_HEALTH_DEP_ENDPOINTS, which the
        // deployment sets from the same `needs`, so there is no
        // endpoint list to keep in sync with the manifest here.
        let health_check_args = HealthCheckArgs {
            verdict_to: None,
            expects_graph: required.contains(&Capability::GraphWrite),
            rejuvenation_threshold_secs: DEFAULT_REJUVENATION_THRESHOLD_SECS,
            dep_cert_endpoints: Vec::new(),
        };

        let myself_cell = self.myself.get_cell();

        // The real, entire, foundational subtree -- HealthCheckActor,
        // RouterActor, this agent's own subscriptions, TcpClient, and
        // (if requested) CryptoActor -- collapsed into one, real,
        // linked child. See infrastructure::InfrastructureSupervisor's
        // own, top-level doc comment for the complete, real reasoning.
        let (infrastructure, _) = InfrastructureSupervisor::spawn_linked(
            None,
            InfrastructureSupervisor,
            infrastructure::InfrastructureSupervisorArgs {
                service_name: self.service_name.clone(),
                health_check_args,
                subscriptions: self.subscriptions,
                wants_crypto: self.wants_crypto,
                shutdown_budget,
                listen_for_signals: true,
                process_exit: shutdown::real_process_exit(),
                tcp_spawner: Arc::new(infrastructure::CassiniSpawner),
            },
            myself_cell,
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let resources: AgentResources = ractor::call!(infrastructure, |reply| {
            InfrastructureSupervisorMessage::GetResources { reply }
        })
        .map_err(|e| ActorProcessingErr::from(e.to_string()))?;

        // The caller's own, real lifecycle-handling closure --
        // subscribed directly to InfrastructureSupervisor's own,
        // real events_output, the same, established pattern every
        // other real port in this codebase already uses (matching
        // TcpClient::spawn_with_router's own, previous, direct
        // parameter exactly, just one real layer removed now).
        let map_lifecycle_event = self
            .map_lifecycle_event
            .unwrap_or_else(|| Box::new(|_event| None));
        resources
            .events_output
            .subscribe(self.myself, move |event| map_lifecycle_event(event));

        Ok(resources)
    }
}

/// The real, testable core of AgentBuilder::build()'s own lifecycle
/// wiring -- extracted specifically because build() itself, like
/// every other, real agent's own pre_start, can't genuinely be
/// tested without a live broker and valid TLS certs (TcpClientConfig::new()
/// reads real environment variables). This function's own, real
/// logic can: on ClientEvent::Registered, fire every real,
/// remembered Subscribe call, then always still forward the event to
/// the caller's own map_lifecycle_event -- proving both halves of
/// Subscription's own, atomic guarantee actually hold, and that a
/// caller's own, separate lifecycle-handling logic is never silently
/// dropped.
pub(crate) fn build_lifecycle_handler<M: ractor::Message>(
    service_name: String,
    subscribed_topics: Vec<String>,
    map_lifecycle_event: Box<dyn Fn(ClientEvent) -> Option<M> + Send + Sync>,
) -> impl Fn(ClientEvent) -> Option<M> {
    move |event: ClientEvent| -> Option<M> {
        if let ClientEvent::Registered { .. } = &event {
            // The real, live TcpClientActor genuinely, necessarily
            // already exists by the time Registered fires -- it's
            // the very actor that produced this event -- so a
            // registry lookup by its own, real, deterministic name
            // (the same one spawn_with_router itself uses) is safe
            // here, even though no direct handle to it was available
            // yet when this closure was built.
            if let Some(tcp_client_cell) = ractor::registry::where_is(format!("{service_name}.tcp"))
            {
                // Genuinely, deliberately trusted, not verified --
                // confirmed directly against ractor's own source
                // before relying on it: From<ActorCell> for
                // ActorRef<TMessage> does zero real runtime type
                // checking of its own, it's a pure PhantomData
                // wrapper. Safe here specifically because
                // TcpClient::spawn_with_router is the one, sole, real
                // thing that ever registers an actor under this
                // exact, deterministic name -- the same,
                // already-established convention this codebase's own
                // where_is call sites already rely on elsewhere (e.g.
                // credentials-agent's own dispatch_store_secret_request).
                let tcp_client_ref: ActorRef<TcpClientMessage> = tcp_client_cell.into();
                for topic in &subscribed_topics {
                    if let Err(e) = tcp_client_ref.send_message(TcpClientMessage::Subscribe {
                        topic: topic.clone(),
                        trace_ctx: None,
                    }) {
                        tracing::error!("AgentBuilder: failed to subscribe to {topic}: {e}");
                    }
                }
            } else {
                tracing::error!(
                    "AgentBuilder: TcpClientActor not found under its own, expected name while \
                     attempting to fire real, remembered subscriptions"
                );
            }
        }
        map_lifecycle_event(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ractor::{Actor, async_trait};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, PartialEq)]
    enum FakeMessage {
        Forwarded(String),
    }

    /// A minimal, real stand-in for TcpClientActor -- spawned under
    /// the exact, same, real, deterministic name
    /// build_lifecycle_handler itself expects
    /// (`{service_name}.tcp`), so where_is genuinely finds it.
    /// Records every real TcpClientMessage it receives.
    struct FakeTcpClient {
        received: Arc<Mutex<Vec<TcpClientMessage>>>,
    }

    #[async_trait]
    impl Actor for FakeTcpClient {
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
            self.received.lock().unwrap().push(message);
            Ok(())
        }
    }

    #[tokio::test]
    async fn registered_fires_every_real_remembered_subscribe_and_still_forwards_to_the_caller() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (_tcp_client, _) = Actor::spawn(
            Some("test-service.tcp".to_string()),
            FakeTcpClient {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let handler = build_lifecycle_handler(
            "test-service".to_string(),
            vec!["topic-a".to_string(), "topic-b".to_string()],
            Box::new(|event| match event {
                ClientEvent::Registered { registration_id } => {
                    Some(FakeMessage::Forwarded(registration_id))
                }
                _ => None,
            }),
        );

        let result = handler(ClientEvent::Registered {
            registration_id: "a-real-registration-id".to_string(),
        });

        // Real proof 1: the caller's own map_lifecycle_event still,
        // genuinely fired -- Subscription's own machinery never
        // silently swallows the caller's own, separate lifecycle
        // logic.
        assert_eq!(
            result,
            Some(FakeMessage::Forwarded("a-real-registration-id".to_string()))
        );

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Real proof 2: both, real, remembered topics were genuinely
        // subscribed to -- not just registered with the router
        // earlier, but actually, really subscribed on the wire.
        let messages = received.lock().unwrap();
        let subscribed_topics: Vec<String> = messages
            .iter()
            .filter_map(|m| match m {
                TcpClientMessage::Subscribe { topic, .. } => Some(topic.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(subscribed_topics.len(), 2);
        assert!(subscribed_topics.contains(&"topic-a".to_string()));
        assert!(subscribed_topics.contains(&"topic-b".to_string()));
    }

    #[tokio::test]
    async fn a_non_registered_event_never_fires_any_subscribe_but_still_forwards_to_the_caller() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (_tcp_client, _) = Actor::spawn(
            Some("test-service-2.tcp".to_string()),
            FakeTcpClient {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let handler = build_lifecycle_handler(
            "test-service-2".to_string(),
            vec!["topic-a".to_string()],
            Box::new(|event| match event {
                ClientEvent::TransportError { reason } => Some(FakeMessage::Forwarded(reason)),
                _ => None,
            }),
        );

        let result = handler(ClientEvent::TransportError {
            reason: "a real, genuine disconnect".to_string(),
        });

        assert_eq!(
            result,
            Some(FakeMessage::Forwarded(
                "a real, genuine disconnect".to_string()
            ))
        );

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(
            received.lock().unwrap().is_empty(),
            "a non-Registered event must never trigger a real subscribe call"
        );
    }

    #[tokio::test]
    async fn a_missing_tcp_client_actor_never_panics_and_still_forwards_to_the_caller() {
        // Genuinely, deliberately, no real FakeTcpClient spawned under
        // this service name at all -- proving the missing-registry-entry
        // path is a real, harmless, logged no-op, not a panic.
        let handler = build_lifecycle_handler(
            "genuinely-nonexistent-service".to_string(),
            vec!["topic-a".to_string()],
            Box::new(|event| match event {
                ClientEvent::Registered { registration_id } => {
                    Some(FakeMessage::Forwarded(registration_id))
                }
                _ => None,
            }),
        );

        let result = handler(ClientEvent::Registered {
            registration_id: "still-works".to_string(),
        });

        assert_eq!(
            result,
            Some(FakeMessage::Forwarded("still-works".to_string()))
        );
    }
}
