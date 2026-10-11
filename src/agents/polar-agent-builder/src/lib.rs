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
// Reading the process environment goes through polar-env (see clippy.toml).
#![deny(clippy::disallowed_methods)]

pub mod agent;
pub use agent::{Configured, PolarAgent, StartError, run};
pub mod capability;
pub use capability::Capability;
pub mod configuration;
pub use configuration::{ConfigChanged, ConfigurationHandle};
pub mod crypto;
pub mod environment;
pub use crypto::{CryptoClient, CryptoHandle};
pub mod health;
pub use health::{HealthClient, HealthHandle};
pub mod infrastructure;
pub use infrastructure::{
    InfrastructureEvent, InfrastructureSupervisor, InfrastructureSupervisorMessage,
};
pub mod message;
pub use message::{Encrypted, Outbound, Plaintext};
pub mod selftest;
pub mod shutdown;
pub(crate) mod startup;
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
    /// The one snapshot of the environment the builder took: the variables the builder, its
    /// components and this agent declared, and nothing else. Read a declared variable with
    /// `config.read(&VAR)`; one nobody declared is refused.
    pub config: Arc<polar_env::Config>,
    /// The same configuration, live: it takes versioned updates to the variables the code
    /// declared `Dynamic`, and tells subscribers when one changes. `config` above stays the
    /// startup snapshot; read `configuration.current()` for what is in force now.
    pub configuration: crate::configuration::ConfigurationHandle,
}

pub struct AgentBuilder<M: ractor::Message> {
    myself: ActorRef<M>,
    service_name: String,
    subscriptions: Vec<Subscription>,
    requires: Vec<Capability>,
    map_lifecycle_event: Option<Box<dyn Fn(ClientEvent) -> Option<M> + Send + Sync>>,
    wants_crypto: bool,
    self_test: bool,
    shutdown_budget: Option<std::time::Duration>,
    environment: Vec<&'static polar_env::Section>,
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
            self_test: false,
            shutdown_budget: None,
            environment: Vec::new(),
        }
    }

    /// Declares the variables this agent's own code reads. They join the builder's and its
    /// components' in the one snapshot, are checked with them before anything spawns (so a
    /// missing or malformed one is named at startup, together with every other problem),
    /// and are read afterwards from `AgentResources::config`. A variable that is not
    /// declared is not in the snapshot, so the agent cannot read it by accident.
    pub fn with_environment(mut self, section: &'static polar_env::Section) -> Self {
        self.environment.push(section);
        self
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

    /// The agent publishes probes to itself and expects to hear them: if it is registered but
    /// no probe comes back, it reports its Cassini connection as lost, and the health check
    /// ends it. For agents that wait on requests, whose silence would otherwise look like
    /// having nothing to do. See `selftest`.
    pub fn with_self_test(mut self) -> Self {
        self.self_test = true;
        self
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

    /// Reads and judges the environment, and spawns nothing.
    ///
    /// This is the first half of [`build`](Self::build). Take it first when the domain
    /// supervisor needs its own settings (from [`ConfiguredAgent::config`]) before it starts
    /// the actors that use them, and spawn the infrastructure afterwards with
    /// [`ConfiguredAgent::build`]. An unsound environment fails here, with every problem, before
    /// anything has been started.
    pub fn configure(self) -> Result<ConfiguredAgent<M>, ActorProcessingErr> {
        self.configure_from(&polar_env::ProcessEnv)
    }

    /// As [`configure`](Self::configure), reading `env`.
    pub(crate) fn configure_from(
        self,
        env: &dyn polar_env::Env,
    ) -> Result<ConfiguredAgent<M>, ActorProcessingErr> {
        // Every agent built here is a Cassini client; the rest are what its own code
        // declared.
        let mut required = self.requires.clone();
        if !required.contains(&Capability::Cassini) {
            required.insert(0, Capability::Cassini);
        }

        // One snapshot of the environment, judged as a whole before anything else happens.
        // Everything after this reads the snapshot.
        let startup = startup::prepare(env, &self.environment, &required, self.shutdown_budget)
            .map_err(ActorProcessingErr::from)?;

        Ok(ConfiguredAgent {
            builder: self,
            startup,
        })
    }

    /// For an agent that implements [`PolarAgent`]: takes what `run` already judged instead of
    /// reading the environment again, and declares from it what the agent declared (its
    /// capabilities, encryption, the self-test), so the builder cannot be told anything that
    /// contradicts the trait. The subscriptions and the lifecycle handler are still the
    /// agent's to add before this.
    pub fn into_configured(mut self, configured: Configured) -> ConfiguredAgent<M> {
        self.requires = configured.required.clone();
        self.wants_crypto = configured.required.contains(&Capability::Encryption);
        self.self_test = configured.self_test;
        ConfiguredAgent {
            builder: self,
            startup: configured.startup,
        }
    }

    /// Reads the environment and spawns the infrastructure: [`configure`](Self::configure)
    /// then [`ConfiguredAgent::build`].
    pub async fn build(self) -> Result<AgentResources, ActorProcessingErr> {
        self.configure()?.build().await
    }
}

/// An agent whose environment has been read and judged, and whose infrastructure is not yet
/// spawned. From [`AgentBuilder::configure`].
pub struct ConfiguredAgent<M: ractor::Message> {
    builder: AgentBuilder<M>,
    startup: startup::Startup,
}

impl<M: ractor::Message> ConfiguredAgent<M> {
    /// The snapshot the infrastructure will run on. Read the agent's own settings from it
    /// with `config.read(&VAR)`, or parse a component's settings with its own
    /// `from_env(&*config)`.
    pub fn config(&self) -> &Arc<polar_env::Config> {
        &self.startup.config
    }

    /// Spawns the infrastructure: the health check, the router, the subscriptions and the
    /// Cassini client, on the snapshot taken when this was configured.
    pub async fn build(self) -> Result<AgentResources, ActorProcessingErr> {
        let ConfiguredAgent {
            builder: this,
            startup,
        } = self;
        let health_check_args = HealthCheckArgs::new(
            startup.health.expects_graph,
            startup.health.rejuvenation_threshold_secs,
            startup.health.dep_cert_endpoints.clone(),
        )
        .with_config(startup.health);

        let myself_cell = this.myself.get_cell();

        // The real, entire, foundational subtree -- HealthCheckActor,
        // RouterActor, this agent's own subscriptions, TcpClient, and
        // (if requested) CryptoActor -- collapsed into one, real,
        // linked child. See infrastructure::InfrastructureSupervisor's
        // own, top-level doc comment for the complete, real reasoning.
        let (infrastructure, _) = InfrastructureSupervisor::spawn_linked(
            None,
            InfrastructureSupervisor,
            infrastructure::InfrastructureSupervisorArgs {
                service_name: this.service_name.clone(),
                health_check_args,
                config: startup.config,
                subscriptions: this.subscriptions,
                wants_crypto: this.wants_crypto,
                with_client: true,
                self_test: this.self_test,
                shutdown_budget: startup.shutdown_budget,
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
        .map_err(|e| ActorProcessingErr::from(e.to_string()))?
        .ok_or_else(|| ActorProcessingErr::from("the infrastructure has no Cassini client"))?;

        // The caller's own, real lifecycle-handling closure --
        // subscribed directly to InfrastructureSupervisor's own,
        // real events_output, the same, established pattern every
        // other real port in this codebase already uses (matching
        // TcpClient::spawn_with_router's own, previous, direct
        // parameter exactly, just one real layer removed now).
        let map_lifecycle_event = this
            .map_lifecycle_event
            .unwrap_or_else(|| Box::new(|_event| None));
        resources
            .events_output
            .subscribe(this.myself, move |event| map_lifecycle_event(event));

        Ok(resources)
    }
}

/// What a service that is not a Cassini client is given: the health handle, the shutdown
/// events, the shutdown handshake and the snapshot. The broker is one.
#[derive(Clone)]
pub struct ServiceResources {
    pub healthcheck: crate::health::HealthHandle,
    /// The infrastructure's side of the shutdown handshake: `PrepareShutdown` arrives here, for
    /// whatever reason the service is to end (a health verdict, SIGTERM, a child's death).
    pub infrastructure_events: Arc<ractor::OutputPort<crate::infrastructure::InfrastructureEvent>>,
    /// Where to send `ProceedWithShutdown` once the service has drained, and `BeginShutdown` to
    /// ask for the service to end (a shutdown requested over the wire, for example).
    pub infrastructure: ActorRef<crate::infrastructure::InfrastructureSupervisorMessage>,
    pub config: Arc<polar_env::Config>,
    /// The configuration, live. See [`AgentResources::configuration`].
    pub configuration: crate::configuration::ConfigurationHandle,
}

/// The infrastructure of a service that is not a Cassini client: the broker itself, or any
/// future service that serves instead of consuming. It has the health check, the signals, the
/// shutdown handshake with its budget and watchdog, and the one configuration snapshot, and none
/// of the router, the client or the subscriptions that `AgentBuilder` adds.
pub struct ServiceBuilder<M: ractor::Message> {
    myself: ActorRef<M>,
    service_name: String,
    requires: Vec<Capability>,
    shutdown_budget: Option<std::time::Duration>,
    environment: Vec<&'static polar_env::Section>,
}

impl<M: ractor::Message> ServiceBuilder<M> {
    pub fn new(myself: ActorRef<M>, service_name: impl Into<String>) -> Self {
        Self {
            myself,
            service_name: service_name.into(),
            requires: Vec::new(),
            shutdown_budget: None,
            environment: Vec::new(),
        }
    }

    /// Declares the variables the service's own code reads, as `AgentBuilder::with_environment`.
    pub fn with_environment(mut self, section: &'static polar_env::Section) -> Self {
        self.environment.push(section);
        self
    }

    /// The least the service needs to drain, as `AgentBuilder::with_shutdown_budget`.
    pub fn with_shutdown_budget(mut self, budget: std::time::Duration) -> Self {
        self.shutdown_budget = Some(budget);
        self
    }

    /// For a service that implements [`PolarAgent`] with `CASSINI_CLIENT = false`: takes what
    /// `run` already judged.
    pub fn into_configured(mut self, configured: Configured) -> ConfiguredService<M> {
        self.requires = configured.required.clone();
        ConfiguredService {
            builder: self,
            startup: configured.startup,
            hooks: configured.hooks,
        }
    }
}

/// A service whose configuration has been judged and whose infrastructure is not yet spawned.
pub struct ConfiguredService<M: ractor::Message> {
    builder: ServiceBuilder<M>,
    startup: startup::Startup,
    hooks: agent::Hooks,
}

impl<M: ractor::Message> ConfiguredService<M> {
    pub fn config(&self) -> &Arc<polar_env::Config> {
        &self.startup.config
    }

    /// Spawns the infrastructure: the health actor, the signal listener and the shutdown
    /// handshake, on the snapshot taken when this was configured.
    pub async fn build(self) -> Result<ServiceResources, ActorProcessingErr> {
        let ConfiguredService {
            builder: this,
            startup,
            hooks,
        } = self;
        if this.requires.contains(&Capability::Encryption) {
            return Err(ActorProcessingErr::from(
                "a service without a Cassini client cannot encrypt: the crypto actor works \
                 through the client",
            ));
        }
        let health_check_args = HealthCheckArgs::new(
            startup.health.expects_graph,
            startup.health.rejuvenation_threshold_secs,
            startup.health.dep_cert_endpoints.clone(),
        )
        .with_config(startup.health);

        let (infrastructure, _) = InfrastructureSupervisor::spawn_linked(
            None,
            InfrastructureSupervisor,
            infrastructure::InfrastructureSupervisorArgs {
                service_name: this.service_name.clone(),
                health_check_args,
                config: startup.config,
                subscriptions: Vec::new(),
                wants_crypto: false,
                with_client: false,
                self_test: false,
                shutdown_budget: startup.shutdown_budget,
                listen_for_signals: hooks.listen_for_signals,
                process_exit: hooks.process_exit.clone(),
                tcp_spawner: Arc::new(infrastructure::CassiniSpawner),
            },
            this.myself.get_cell(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let resources: ServiceResources = ractor::call!(infrastructure, |reply| {
            InfrastructureSupervisorMessage::GetService { reply }
        })
        .map_err(|e| ActorProcessingErr::from(e.to_string()))?;
        if let Some(out) = &hooks.resources_out {
            *out.lock().unwrap() = Some(resources.clone());
        }
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
        if let ClientEvent::Registered { registration_id } = &event {
            tracing::info!(
                %registration_id,
                topics = subscribed_topics.len(),
                "AgentBuilder: registered with Cassini, subscribing to every request topic"
            );
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
                    match tcp_client_ref.send_message(TcpClientMessage::Subscribe {
                        topic: topic.clone(),
                        trace_ctx: None,
                    }) {
                        Ok(()) => tracing::info!("AgentBuilder: subscribing to {topic}"),
                        Err(e) => {
                            tracing::error!("AgentBuilder: failed to subscribe to {topic}: {e}")
                        }
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
    use polar_env::Env;
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

    // -- configuring is reading the environment, and spawns nothing --------------------

    struct Idle;

    #[async_trait]
    impl Actor for Idle {
        type Msg = FakeMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }
    }

    fn map(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[tokio::test]
    async fn configuring_takes_the_snapshot_before_anything_is_spawned() {
        let (idle, _) = Actor::spawn(None, Idle, ()).await.unwrap();
        let env = map(environment::fixtures::CASSINI_ENV);
        let configured = AgentBuilder::new(idle, "configure-test")
            .configure_from(&env)
            .unwrap();

        // The snapshot is there to read, so a domain supervisor can take its own settings
        // from it before it starts the actors that need them.
        assert_eq!(
            configured.config().var("BROKER_ADDR").as_deref(),
            Some("cassini:8080")
        );
        // Nothing was spawned: the client's name is not registered.
        assert!(ractor::registry::where_is("configure-test.tcp".to_string()).is_none());
    }

    #[tokio::test]
    async fn an_agent_built_from_the_trait_declares_to_the_builder_what_the_trait_declared() {
        let (idle, _) = Actor::spawn(None, Idle, ()).await.unwrap();
        let env = map(environment::fixtures::CASSINI_ENV);
        let required = vec![Capability::Cassini, Capability::Encryption];
        let startup = startup::prepare(&env, &[], &required, None).unwrap();
        let configured = Configured {
            startup,
            required: required.clone(),
            self_test: false,
            hooks: crate::agent::Hooks::real(),
        };

        let built = AgentBuilder::new(idle, "from-the-trait")
            // What a hand-written chain might also have said: it is overruled.
            .with_graph_write()
            .with_self_test()
            .into_configured(configured);

        assert_eq!(built.builder.requires, required);
        assert!(built.builder.wants_crypto, "encryption was needed");
        assert!(!built.builder.self_test, "the trait said no self-test");
    }

    #[tokio::test]
    async fn configuring_refuses_an_unsound_environment_with_every_problem() {
        let (idle, _) = Actor::spawn(None, Idle, ()).await.unwrap();
        let error = AgentBuilder::new(idle, "configure-bad")
            .configure_from(&map(&[("POLAR_HEALTH_TICK_SECS", "soon")]))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("POLAR_HEALTH_TICK_SECS"), "{error}");
        assert!(error.contains("BROKER_ADDR"), "the client's too: {error}");
    }

    #[tokio::test]
    async fn what_the_agent_declares_is_judged_when_configuring() {
        let (idle, _) = Actor::spawn(None, Idle, ()).await.unwrap();
        let error = AgentBuilder::new(idle, "configure-graph")
            .with_graph_write()
            .configure_from(&map(environment::fixtures::CASSINI_ENV))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("GRAPH_ENDPOINT"), "{error}");
    }

    #[tokio::test]
    async fn an_agent_can_take_the_graph_settings_without_making_its_health_depend_on_the_graph() {
        // The credentials consumer's case: it projects to the graph but must not have its
        // health watch Neo4j, so it declares the graph section itself instead of requiring
        // the graph-write capability.
        let (idle, _) = Actor::spawn(None, Idle, ()).await.unwrap();
        let mut vars = environment::fixtures::CASSINI_ENV.to_vec();
        vars.extend_from_slice(environment::fixtures::GRAPH_ENV);
        let configured = AgentBuilder::new(idle, "configure-projection")
            .with_environment(&polar::graph::environment::SECTION)
            .configure_from(&map(&vars))
            .unwrap();

        let settings =
            polar::graph::environment::GraphSettings::from_env(&**configured.config()).unwrap();
        assert_eq!(settings.endpoint, "bolt+s://db:7687");
        assert!(
            !configured.startup.health.expects_graph,
            "health does not watch the graph"
        );
    }
}
