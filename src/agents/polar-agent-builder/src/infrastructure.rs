//! InfrastructureSupervisor -- owns HealthCheckActor, RouterActor,
//! TcpClientActor, and (if requested) CryptoActor as real, direct,
//! linked children, in one, real, cohesive subtree. A service that is not a Cassini client (the
//! broker itself) has only the health actor: no router, client or crypto, and nothing to
//! subscribe to. Everything about how it ends is the same. Spawned by
//! AgentBuilder::build() itself as a real, linked child of the
//! domain supervisor; the domain supervisor never spawns any of
//! these four actors directly at all.
//!
//! Exists to solve one, real, genuine problem CryptoActor's own
//! design creates: CryptoActorArgs bakes in a real
//! ActorRef<TcpClientMessage> at spawn time. If TcpClientActor
//! genuinely, unexpectedly terminates and a fresh one spawns, that
//! baked-in reference is now stale. This actor is the one, real
//! place that detects that and respawns both together, atomically,
//! sharing the same, still-live RouterActor.
//!
//! It also owns how the agent ends. Every reason to end goes through one
//! funnel, `begin_shutdown`: a verdict from the health actor, a SIGTERM or SIGINT,
//! or any child (other than a Cassini client that stopped cleanly, which is
//! replaced) ending unexpectedly. The funnel is idempotent and logs the reason.
//! The domain supervisor's own, separate children (a graph controller, watcher
//! trees) may still need the router and TCP client alive while they shut down
//! cleanly, so the funnel runs a two-way handshake with it: see
//! InfrastructureEvent::PrepareShutdown and
//! InfrastructureSupervisorMessage::ProceedWithShutdown. A watchdog bounds the
//! handshake: if the process is still alive when the shutdown budget runs out, it
//! exits 1 regardless. See shutdown.rs for the decisions themselves.

use crate::configuration::ConfigurationHandle;
use crate::crypto::CryptoHandle;
use crate::health::HealthHandle;
use crate::shutdown::{self, Action, Child, Death, ProcessExit, ShutdownMachine};
use crate::{AgentResources, Subscription, build_lifecycle_handler};
use cassini_client::TCPClientConfig;
use cassini_types::ClientEvent;
use polar::cassini::TcpClient;
use polar_health::{HealthCheckActor, HealthCheckArgs, HealthVerdict};
use ractor::{
    Actor, ActorCell, ActorProcessingErr, ActorRef, OutputPort, RpcReplyPort, SupervisionEvent,
    async_trait,
};
use std::sync::Arc;
use std::time::Duration;

/// How long the supervisor waits for its children to stop once told to proceed.
const STOP_CHILDREN_TIMEOUT: Duration = Duration::from_secs(10);

/// The "infra → domain" direction -- a real agent subscribes to this
/// via AgentResources's own events_output/infrastructure_events
/// fields, using its own, real mapping closure, exactly mirroring
/// the established prepare_shutdown_port/graph_signal_port pattern.
#[derive(Clone)]
pub enum InfrastructureEvent {
    /// The agent is to end, for whatever reason: the health actor gave a verdict,
    /// Kubernetes sent SIGTERM, or a child of this supervisor died. The domain
    /// supervisor should begin coordinating its own shutdown now (its own domain
    /// children first), then send
    /// InfrastructureSupervisorMessage::ProceedWithShutdown once
    /// that's genuinely, fully done. Sent at most once. If it does not answer
    /// within the shutdown budget the process exits 1 regardless.
    PrepareShutdown,
}

pub enum InfrastructureSupervisorMessage {
    /// Internal: the real, raw ClientEvent forwarded from
    /// TcpClientActor's own lifecycle callback -- immediately,
    /// directly re-sent via this actor's own, real
    /// Arc<OutputPort<ClientEvent>>, so a domain supervisor's own,
    /// real lifecycle-handling closure still, genuinely works exactly
    /// as it always has, subscribed to that same, real port directly.
    ClientEvent(ClientEvent),
    /// Sent once the domain supervisor's own, direct children
    /// (everything outside this infra subtree) have genuinely, fully
    /// terminated -- tells infra it's now safe to stop its own
    /// remaining children and terminate itself.
    ProceedWithShutdown,
    /// The one, real way AgentBuilder::build() itself retrieves the
    /// initial, real AgentResources this actor's own pre_start
    /// already, genuinely spawned internally. `None` when this supervisor has no Cassini
    /// client, which is what `ServiceBuilder` builds.
    GetResources {
        reply: RpcReplyPort<Option<AgentResources>>,
    },
    /// What a service without a Cassini client is given: the health handle, the shutdown
    /// events and the snapshot.
    GetService {
        reply: RpcReplyPort<crate::ServiceResources>,
    },
    /// Internal: the health actor's verdict, delivered through the port this
    /// supervisor gave it.
    Health(HealthVerdict),
    /// Ask for an orderly shutdown, for this reason. Sent by the signal listener;
    /// it can be sent by anything holding AgentResources::infrastructure. If a
    /// shutdown is already under way this changes nothing.
    BeginShutdown { reason: String },
    /// Internal: time for the self-test to look.
    SelfTestTick,
}

/// The lifecycle callback the Cassini client is given.
pub(crate) type LifecycleHandler =
    Box<dyn Fn(ClientEvent) -> Option<InfrastructureSupervisorMessage> + Send + Sync>;

/// How the supervisor makes its Cassini client. A seam: the real one connects to
/// a broker, a test's does not.
#[async_trait]
pub(crate) trait TcpClientSpawner: Send + Sync {
    async fn spawn(
        &self,
        service_name: &str,
        supervisor: ActorRef<InfrastructureSupervisorMessage>,
        lifecycle: LifecycleHandler,
        router: ActorRef<polar_router::RouterMessage>,
        config: TCPClientConfig,
    ) -> Result<TcpClient, ActorProcessingErr>;
}

pub(crate) struct CassiniSpawner;

#[async_trait]
impl TcpClientSpawner for CassiniSpawner {
    async fn spawn(
        &self,
        service_name: &str,
        supervisor: ActorRef<InfrastructureSupervisorMessage>,
        lifecycle: LifecycleHandler,
        router: ActorRef<polar_router::RouterMessage>,
        config: TCPClientConfig,
    ) -> Result<TcpClient, ActorProcessingErr> {
        TcpClient::spawn_with_router_config(service_name, supervisor, lifecycle, router, config)
            .await
    }
}

pub struct InfrastructureSupervisorArgs {
    pub service_name: String,
    pub health_check_args: HealthCheckArgs,
    /// The one snapshot of the environment the builder took. Held for the life of the
    /// agent, and handed out through `AgentResources::config`.
    pub config: Arc<polar_env::Config>,
    pub subscriptions: Vec<Subscription>,
    pub wants_crypto: bool,
    /// Whether this is a Cassini client: a router, a client and its subscriptions. A service
    /// that is the broker, or otherwise not a client, has none of them.
    pub(crate) with_client: bool,
    /// How long the domain supervisor has to drain before the process exits anyway.
    pub(crate) shutdown_budget: Duration,
    /// Listen for SIGTERM and SIGINT. Off in tests, because signals belong to the
    /// whole process.
    pub(crate) listen_for_signals: bool,
    pub(crate) process_exit: ProcessExit,
    pub(crate) tcp_spawner: Arc<dyn TcpClientSpawner>,
    /// Publish probes to the agent itself and expect them back; see `selftest`.
    pub(crate) self_test: bool,
}

/// How often the self-test looks at the clock; a probe is sent every `SELF_TEST_INTERVAL`, and
/// the agent is judged deaf when one has gone `SELF_TEST_TIMEOUT` unanswered.
const SELF_TEST_TICK: Duration = Duration::from_secs(5);
const SELF_TEST_INTERVAL: Duration = Duration::from_secs(30);
const SELF_TEST_TIMEOUT: Duration = Duration::from_secs(20);

/// The topic an agent's probes travel on. Shared by every instance of the service: each tells its
/// own probes from the others by the instance id they carry.
fn self_test_topic(service_name: &str) -> String {
    // The broker names a subscription `{session}:{topic}` and splits it at the first colon, and
    // some services are named with colons.
    format!("selftest.{}", service_name.replace(':', "-"))
}

/// A probe is the instance id followed by its number.
fn probe_payload(instance: &[u8; 16], number: u64) -> Vec<u8> {
    let mut payload = instance.to_vec();
    payload.extend_from_slice(&number.to_be_bytes());
    payload
}

/// The number in a probe sent by `instance`, if that is what this is.
fn probe_number(instance: &[u8; 16], payload: &[u8]) -> Option<u64> {
    let (id, number) = payload.split_at_checked(16)?;
    (id == instance).then_some(())?;
    Some(u64::from_be_bytes(number.try_into().ok()?))
}

struct SelfTestState {
    machine: crate::selftest::SelfTest,
    instance: [u8; 16],
    topic: String,
    /// The highest probe number the router has delivered back.
    heard: Arc<std::sync::atomic::AtomicU64>,
    began: tokio::time::Instant,
    /// Whether the Cassini client is registered, as far as its events say.
    connected: bool,
}

/// What the supervisor needs to make a Cassini client.
struct ClientSetup {
    subscriptions: Vec<Subscription>,
    wants_crypto: bool,
    self_test: bool,
    tcp_spawner: Arc<dyn TcpClientSpawner>,
    config: Arc<polar_env::Config>,
    configuration: ConfigurationHandle,
}

/// What a Cassini client brings with it.
struct ClientParts {
    events_output: Arc<OutputPort<ClientEvent>>,
    router: ActorRef<polar_router::RouterMessage>,
    tcp_client: TcpClient,
    crypto: Option<CryptoHandle>,
}

pub struct InfrastructureSupervisorState {
    config: Arc<polar_env::Config>,
    configuration: ConfigurationHandle,
    infrastructure_events: Arc<OutputPort<InfrastructureEvent>>,
    client: Option<ClientParts>,
    healthcheck: HealthHandle,
    service_name: String,
    shutdown: ShutdownMachine,
    shutdown_budget: Duration,
    process_exit: ProcessExit,
    self_test: Option<SelfTestState>,
}

pub struct InfrastructureSupervisor;

impl InfrastructureSupervisor {
    /// The one entry point to ending the agent. Idempotent: the first reason
    /// starts the shutdown, and any later one is logged and changes nothing.
    fn begin_shutdown(state: &mut InfrastructureSupervisorState, reason: String) {
        if !state.shutdown.begin(&reason) {
            tracing::info!(
                "InfrastructureSupervisor: already shutting down ({}); ignoring a further \
                 reason: {reason}",
                state.shutdown.reason().unwrap_or("no reason recorded")
            );
            return;
        }

        tracing::warn!("InfrastructureSupervisor: beginning an orderly shutdown: {reason}");
        Self::start_watchdog(state.shutdown_budget, state.process_exit.clone(), &reason);
        state
            .infrastructure_events
            .send(InfrastructureEvent::PrepareShutdown);
    }

    /// Looks at what has come back, then at the clock, and does what the self-test says.
    fn self_test_tick(state: &mut InfrastructureSupervisorState) {
        use crate::health::HealthClient;
        use crate::selftest::Action;

        let (Some(test), Some(client)) = (&mut state.self_test, &state.client) else {
            return;
        };
        let heard = test.heard.load(std::sync::atomic::Ordering::SeqCst);
        if heard > 0 && let Some(Action::Hearing) = test.machine.heard(heard) {
            tracing::info!(
                "InfrastructureSupervisor: self-test: a probe came back, the agent hears the \
                 broker again"
            );
            let _ = state.healthcheck.cassini_connected();
        }
        for action in test.machine.tick(test.began.elapsed(), test.connected) {
            match action {
                Action::Probe(number) => {
                    tracing::debug!(number, "InfrastructureSupervisor: self-test: probing");
                    let _ = client
                        .tcp_client
                        .as_actor_ref()
                        .send_message(cassini_client::TcpClientMessage::publish(
                            test.topic.clone(),
                            probe_payload(&test.instance, number),
                            None,
                        ));
                }
                Action::Deaf { unanswered_for } => {
                    tracing::error!(
                        unanswered_secs = unanswered_for.as_secs(),
                        topic = %test.topic,
                        "InfrastructureSupervisor: self-test: no probe has come back, the agent \
                         is registered but cannot hear the broker; reporting the Cassini \
                         connection as lost"
                    );
                    let _ = state.healthcheck.cassini_disconnected();
                }
                Action::Hearing => {}
            }
        }
    }

    /// The last resort: if the process is still alive when the budget runs out, the
    /// domain supervisor never finished, so exit 1 and let the Job replace the pod.
    ///
    /// A detached task, not a message to this actor, so it does not depend on the
    /// actor still being alive. Without it, a domain supervisor that never answers
    /// leaves only the Kubernetes liveness probe to end the pod.
    fn start_watchdog(budget: Duration, process_exit: ProcessExit, reason: &str) {
        let reason = reason.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(budget).await;
            tracing::error!(
                "InfrastructureSupervisor: the process is still running {}s after the shutdown \
                 began ({reason}); the domain supervisor never finished, exiting 1",
                budget.as_secs()
            );
            process_exit(1);
        });
    }

    /// The router, the subscriptions, the Cassini client and, if asked, the crypto actor: what
    /// makes this agent a client. Moved out of `pre_start` so a service without one skips it all.
    async fn spawn_client_parts(
        myself: &ActorRef<InfrastructureSupervisorMessage>,
        service_name: &str,
        setup: ClientSetup,
    ) -> Result<(ClientParts, Option<SelfTestState>), ActorProcessingErr> {
        let myself_cell = myself.get_cell();
        let (router, _) = polar_router::RouterActor::spawn_linked(
            None,
            polar_router::RouterActor,
            (),
            myself_cell.clone(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let mut subscriptions = setup.subscriptions;
        // Every agent hears its own signed configuration, and the actor verifies it.
        subscriptions.push(crate::configuration::subscription(
            service_name,
            setup.configuration.clone(),
        ));
        let self_test = if setup.self_test {
            let instance: [u8; 16] = rand::random();
            let topic = self_test_topic(service_name);
            let heard = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let heard_by_router = heard.clone();
            subscriptions.push(Subscription::new(topic.clone(), move |incoming| {
                if let Some(number) = probe_number(&instance, &incoming.payload) {
                    heard_by_router.fetch_max(number, std::sync::atomic::Ordering::SeqCst);
                }
            }));
            drop(myself.send_interval(SELF_TEST_TICK, || {
                InfrastructureSupervisorMessage::SelfTestTick
            }));
            Some(SelfTestState {
                machine: crate::selftest::SelfTest::new(SELF_TEST_INTERVAL, SELF_TEST_TIMEOUT),
                instance,
                topic,
                heard,
                began: tokio::time::Instant::now(),
                connected: false,
            })
        } else {
            None
        };

        let subscribed_topics: Vec<String> = subscriptions.iter().map(|s| s.topic.clone()).collect();

        tracing::info!(
            "InfrastructureSupervisor: expecting {} subscription(s): {}",
            subscribed_topics.len(),
            subscribed_topics.join(", ")
        );

        for subscription in subscriptions {
            router
                .send_message(polar_router::RouterMessage::Register {
                    topic: subscription.topic,
                    deliver: subscription.deliver,
                })
                .map_err(ActorProcessingErr::from)?;
        }

        let events_output = Arc::new(OutputPort::default());

        // Every real ClientEvent this agent's own TcpClient ever
        // produces is immediately, directly re-sent out via
        // events_output above -- this actor never, itself, tries to
        // interpret them (Registered vs TransportError, etc); a real
        // domain supervisor's own, real lifecycle-handling closure,
        // subscribed to that port, does that.
        let lifecycle_handler: LifecycleHandler = Box::new(build_lifecycle_handler(
            service_name.to_string(),
            subscribed_topics.clone(),
            Box::new(|event: ClientEvent| {
                Some(InfrastructureSupervisorMessage::ClientEvent(event))
            }),
        ));

        let client_config =
            TCPClientConfig::from_env(&*setup.config).map_err(ActorProcessingErr::from)?;
        let tcp_client = setup
            .tcp_spawner
            .spawn(
                service_name,
                myself.clone(),
                lifecycle_handler,
                router.clone(),
                client_config,
            )
            .await?;

        let crypto = if setup.wants_crypto {
            let (crypto_actor, _) = polar_crypto::CryptoActor::spawn_linked(
                None,
                polar_crypto::CryptoActor,
                polar_crypto::CryptoActorArgs::new(tcp_client.as_actor_ref(), router.clone())
                    .with_settings(
                        polar_crypto::environment::CryptoSettings::from_env(&*setup.config)
                            .map_err(ActorProcessingErr::from)?,
                    ),
                myself_cell,
            )
            .await
            .map_err(ActorProcessingErr::from)?;
            Some(CryptoHandle::new(crypto_actor))
        } else {
            None
        };

        Ok((
            ClientParts {
                events_output,
                router,
                tcp_client,
                crypto,
            },
            self_test,
        ))
    }

    fn classify(state: &InfrastructureSupervisorState, cell: &ActorCell) -> Child {
        let id = cell.get_id();
        if id == state.healthcheck.as_actor_ref().get_id() {
            Child::Health
        } else if state.client.is_some()
            && cell.get_name() == Some(format!("{}.tcp", state.service_name))
        {
            // TcpClientActor is spawned under its own deterministic name; see
            // TcpClient::spawn_with_router.
            Child::TcpClient
        } else if state
            .client
            .as_ref()
            .is_some_and(|client| id == client.router.get_id())
        {
            Child::Other("the router".to_string())
        } else if state
            .client
            .as_ref()
            .and_then(|client| client.crypto.as_ref())
            .is_some_and(|crypto| crypto.as_actor_ref().get_id() == id)
        {
            Child::Other("the crypto actor".to_string())
        } else {
            Child::Other(match cell.get_name() {
                Some(name) => name,
                None => format!("child {id}"),
            })
        }
    }

    fn child_ended(state: &mut InfrastructureSupervisorState, cell: ActorCell, death: Death) {
        let child = Self::classify(state, &cell);
        match shutdown::on_child_death(state.shutdown.is_running(), &child, &death) {
            Action::Ignore => {
                tracing::debug!(
                    "InfrastructureSupervisor: {child:?} ended during the shutdown ({death:?})"
                );
            }
            Action::Shutdown(reason) => {
                tracing::error!("InfrastructureSupervisor: {reason}");
                Self::begin_shutdown(state, reason);
            }
        }
    }
}

#[async_trait]
impl Actor for InfrastructureSupervisor {
    type Msg = InfrastructureSupervisorMessage;
    type State = InfrastructureSupervisorState;
    type Arguments = InfrastructureSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: InfrastructureSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        let myself_cell = myself.get_cell();

        // The health actor reports its decision to end the agent here, as a
        // message, and stays alive to keep the health file fresh while the agent
        // drains. Whatever recipient the caller put in the args is replaced: this
        // supervisor is the only place that acts on it.
        let verdicts = Arc::new(OutputPort::default());
        verdicts.subscribe(myself.clone(), |verdict| {
            Some(InfrastructureSupervisorMessage::Health(verdict))
        });
        let health_check_args = args.health_check_args.with_verdict_to(verdicts);

        let (healthcheck, _) = HealthCheckActor::spawn_linked(
            None,
            HealthCheckActor,
            health_check_args,
            myself_cell.clone(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        // Owns the configuration the agent runs on from here, and takes updates to it. It needs
        // nothing but the snapshot, so it starts before, and without, the Cassini client.
        // Signed documents are checked against the CA this agent already pins for Cassini. Without a
        // readable one there is nothing to verify against and every document is refused.
        let verifier =
            match crate::signed::UpdateVerifier::from_config(&args.config, &args.service_name) {
                Ok(verifier) => Some(verifier),
                Err(why) => {
                    tracing::warn!(
                        "InfrastructureSupervisor: signed configuration will be refused: {why}"
                    );
                    None
                }
            };
        let configuration =
            ConfigurationHandle::spawn(None, (*args.config).clone(), verifier, myself_cell.clone())
                .await?;

        let infrastructure_events = Arc::new(OutputPort::default());

        let (client, self_test) = if args.with_client {
            let (client, self_test) = Self::spawn_client_parts(
                &myself,
                &args.service_name,
                ClientSetup {
                    subscriptions: args.subscriptions,
                    wants_crypto: args.wants_crypto,
                    self_test: args.self_test,
                    tcp_spawner: args.tcp_spawner,
                    config: args.config.clone(),
                    configuration: configuration.clone(),
                },
            )
            .await?;
            (Some(client), self_test)
        } else {
            // A service that is not a Cassini client has no connection to wait for. The health
            // actor starts its ticks when it is told Cassini is connected, so tell it, once.
            use crate::health::HealthClient;
            let _ = HealthHandle::new(healthcheck.clone()).cassini_connected();
            (None, None)
        };

        if args.listen_for_signals {
            shutdown::spawn_signal_listener(myself.clone());
        }

        Ok(InfrastructureSupervisorState {
            config: args.config,
            configuration,
            infrastructure_events,
            client,
            healthcheck: HealthHandle::new(healthcheck),
            service_name: args.service_name,
            shutdown: ShutdownMachine::new(),
            shutdown_budget: args.shutdown_budget,
            process_exit: args.process_exit,
            self_test,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            InfrastructureSupervisorMessage::ClientEvent(event) => {
                // CryptoActor subscribes to its reply topics only once told
                // the client has registered; the client drops any Subscribe
                // sent earlier. Sent on every Registered, so a reconnect
                // restores them too.
                match &event {
                    ClientEvent::Registered { .. } => {
                        if let Some(test) = &mut state.self_test {
                            test.connected = true;
                            test.machine.registered();
                        }
                    }
                    ClientEvent::TransportError { .. } => {
                        if let Some(test) = &mut state.self_test {
                            test.connected = false;
                        }
                    }
                    _ => {}
                }
                if let Some(client) = &state.client {
                    if let ClientEvent::Registered { .. } = &event
                        && let Some(crypto) = &client.crypto
                        && let Err(e) = crypto
                            .as_actor_ref()
                            .send_message(polar_crypto::CryptoMessage::ClientRegistered)
                    {
                        tracing::warn!("failed to tell CryptoActor the client registered: {e}");
                    }
                    client.events_output.send(event);
                }
            }
            InfrastructureSupervisorMessage::Health(verdict) => {
                match &verdict {
                    HealthVerdict::Rejuvenate => {
                        tracing::info!("InfrastructureSupervisor: the health actor says rejuvenate")
                    }
                    HealthVerdict::Unhealthy(_) => tracing::error!(
                        "InfrastructureSupervisor: the health actor says the agent is unhealthy: {}",
                        verdict.reason()
                    ),
                }
                Self::begin_shutdown(state, verdict.reason());
            }
            InfrastructureSupervisorMessage::BeginShutdown { reason } => {
                Self::begin_shutdown(state, reason);
            }
            InfrastructureSupervisorMessage::SelfTestTick => Self::self_test_tick(state),
            InfrastructureSupervisorMessage::ProceedWithShutdown => {
                if !state.shutdown.proceed() {
                    tracing::info!(
                        "InfrastructureSupervisor: already stopping, ignoring a further \
                         ProceedWithShutdown"
                    );
                    return Ok(());
                }
                tracing::info!("InfrastructureSupervisor: proceeding with its own shutdown");
                myself
                    .stop_children_and_wait(
                        Some("infrastructure shutdown".to_string()),
                        Some(STOP_CHILDREN_TIMEOUT),
                    )
                    .await;
                myself.stop(Some("infrastructure shutdown complete".to_string()));
            }
            InfrastructureSupervisorMessage::GetResources { reply } => {
                let _ = reply.send(state.client.as_ref().map(|client| AgentResources {
                    router: client.router.clone(),
                    tcp_client: client.tcp_client.clone(),
                    healthcheck: state.healthcheck.clone(),
                    crypto: client.crypto.clone(),
                    events_output: client.events_output.clone(),
                    infrastructure_events: state.infrastructure_events.clone(),
                    infrastructure: myself.clone(),
                    config: state.config.clone(),
                    configuration: state.configuration.clone(),
                }));
            }
            InfrastructureSupervisorMessage::GetService { reply } => {
                let _ = reply.send(crate::ServiceResources {
                    healthcheck: state.healthcheck.clone(),
                    infrastructure_events: state.infrastructure_events.clone(),
                    infrastructure: myself.clone(),
                    config: state.config.clone(),
                    configuration: state.configuration.clone(),
                });
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                Self::child_ended(state, cell, Death::Terminated(reason));
            }
            SupervisionEvent::ActorFailed(cell, error) => {
                Self::child_ended(state, cell, Death::Failed(error.to_string()));
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cassini_client::TcpClientMessage;
    use ractor::ActorStatus;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // -- a Cassini client that needs no broker ---------------------------------

    struct FakeTcp {
        fail_on_message: bool,
        /// If set, whatever is published comes straight back through the router, as a broker
        /// would deliver it to a subscriber of the topic.
        echo_to: Option<ActorRef<polar_router::RouterMessage>>,
    }

    #[async_trait]
    impl Actor for FakeTcp {
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
            if let (Some(router), TcpClientMessage::Publish { topic, payload, .. }) =
                (&self.echo_to, message)
            {
                let _ = router.send_message(polar_router::RouterMessage::Route(
                    polar_router::IncomingMessage {
                        topic,
                        payload,
                        published_by: None,
                        trace_ctx: None,
                    },
                ));
            }
            if self.fail_on_message {
                Err(ActorProcessingErr::from("the fake client was made to fail"))
            } else {
                Ok(())
            }
        }
    }

    #[derive(Default)]
    struct FakeSpawner {
        spawned: AtomicUsize,
        fail_next_spawn: AtomicBool,
        clients_fail_on_message: AtomicBool,
        /// Clients hand what they publish back to the router.
        clients_echo: AtomicBool,
        /// The configuration each client was started with, in order.
        configs: Mutex<Vec<TCPClientConfig>>,
    }

    #[async_trait]
    impl TcpClientSpawner for FakeSpawner {
        async fn spawn(
            &self,
            service_name: &str,
            supervisor: ActorRef<InfrastructureSupervisorMessage>,
            _lifecycle: LifecycleHandler,
            router: ActorRef<polar_router::RouterMessage>,
            config: TCPClientConfig,
        ) -> Result<TcpClient, ActorProcessingErr> {
            self.configs.lock().unwrap().push(config);
            if self.fail_next_spawn.swap(false, Ordering::SeqCst) {
                return Err(ActorProcessingErr::from(
                    "the fake spawner was made to fail",
                ));
            }
            self.spawned.fetch_add(1, Ordering::SeqCst);
            let (actor, _) = Actor::spawn_linked(
                Some(format!("{service_name}.tcp")),
                FakeTcp {
                    fail_on_message: self.clients_fail_on_message.load(Ordering::SeqCst),
                    echo_to: self
                        .clients_echo
                        .load(Ordering::SeqCst)
                        .then(|| router.clone()),
                },
                (),
                supervisor.get_cell(),
            )
            .await?;
            Ok(TcpClient::from_actor_ref(actor))
        }
    }

    // -- a record of what the domain supervisor is told ----------------------------

    struct EventLog;

    #[async_trait]
    impl Actor for EventLog {
        type Msg = InfrastructureEvent;
        type State = Arc<Mutex<Vec<String>>>;
        type Arguments = Self::State;

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            seen: Self::State,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(seen)
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            event: Self::Msg,
            seen: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            seen.lock().unwrap().push(match event {
                InfrastructureEvent::PrepareShutdown => "prepare_shutdown".to_string(),
            });
            Ok(())
        }
    }

    use std::collections::HashMap;

    // -- the rig -----------------------------------------------------------------------
    //
    // The real supervisor with a real router and a real health actor. The health actor is
    // handed its settings from a snapshot of a map, as the builder hands them from the
    // process, so rigs share nothing and run side by side.

    struct Rig {
        infra: ActorRef<InfrastructureSupervisorMessage>,
        done: Option<ractor::concurrency::JoinHandle<()>>,
        resources: AgentResources,
        events: Arc<Mutex<Vec<String>>>,
        exits: Arc<Mutex<Vec<i32>>>,
        spawner: Arc<FakeSpawner>,
        _dir: tempfile::TempDir,
    }

    impl Rig {
        /// `health_env` is applied on top of a health file in a temp directory and a
        /// Cassini deadline too long to matter, so only the tests about the
        /// health actor's own verdicts see one.
        async fn new(
            service: &str,
            budget: Duration,
            health_env: &[(&str, &str)],
            listen_for_signals: bool,
        ) -> Self {
            Self::build(service, budget, health_env, listen_for_signals, false, None).await
        }

        /// A rig whose agent tests itself. `echo` is whether its Cassini client hands the
        /// probes back, as a working broker connection would.
        async fn self_testing(service: &str, echo: bool) -> Self {
            Self::build(
                service,
                Duration::from_secs(20),
                &[],
                false,
                false,
                Some(echo),
            )
            .await
        }

        /// A rig whose agent encrypts: it has a real certificate for the crypto actor to take
        /// its identity from.
        async fn with_crypto(service: &str) -> Self {
            Self::build(service, Duration::from_secs(20), &[], false, true, None).await
        }

        async fn build(
            service: &str,
            budget: Duration,
            health_env: &[(&str, &str)],
            listen_for_signals: bool,
            crypto: bool,
            self_test: Option<bool>,
        ) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let certificate = dir.path().join("cert.pem");
            if crypto {
                let params = rcgen::CertificateParams::new(vec![format!("{service}-identity")])
                    .expect("certificate parameters");
                let key = rcgen::KeyPair::generate().expect("key");
                std::fs::write(
                    &certificate,
                    params.self_signed(&key).expect("self-sign").pem(),
                )
                .expect("write the certificate");
            }
            let mut vars: HashMap<String, String> = [
                (
                    "POLAR_HEALTH_FILE",
                    dir.path().join("health.json").to_str().unwrap(),
                ),
                ("POLAR_HEALTH_CASSINI_DEADLINE_SECS", "100000"),
            ]
            .into_iter()
            .chain(crate::environment::fixtures::CASSINI_ENV.iter().copied())
            .chain(health_env.iter().copied())
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
            let mut required = vec![crate::Capability::Cassini];
            if crypto {
                vars.insert(
                    "TLS_CLIENT_CERT".to_string(),
                    certificate.to_string_lossy().into_owned(),
                );
                required.push(crate::Capability::Encryption);
            }
            let startup = crate::startup::prepare(&vars, &[], &required, None).unwrap();

            let exits = Arc::new(Mutex::new(Vec::new()));
            let recorded = exits.clone();
            let spawner = Arc::new(FakeSpawner::default());
            if self_test == Some(true) {
                spawner.clients_echo.store(true, Ordering::SeqCst);
            }

            let (infra, done) = Actor::spawn(
                None,
                InfrastructureSupervisor,
                InfrastructureSupervisorArgs {
                    service_name: service.to_string(),
                    health_check_args: HealthCheckArgs::new(false, 300, vec![])
                        .with_config(startup.health),
                    config: startup.config,
                    subscriptions: vec![],
                    wants_crypto: crypto,
                    with_client: true,
                    shutdown_budget: budget,
                    listen_for_signals,
                    process_exit: Arc::new(move |code| recorded.lock().unwrap().push(code)),
                    tcp_spawner: spawner.clone(),
                    self_test: self_test.is_some(),
                },
            )
            .await
            .unwrap();

            let resources: AgentResources = ractor::call!(infra, |reply| {
                InfrastructureSupervisorMessage::GetResources { reply }
            })
            .unwrap()
            .expect("a supervisor with a Cassini client");

            let events = Arc::new(Mutex::new(Vec::new()));
            let (log, _) = Actor::spawn(None, EventLog, events.clone()).await.unwrap();
            resources.infrastructure_events.subscribe(log, Some);

            Self {
                infra,
                done: Some(done),
                resources,
                events,
                exits,
                spawner,
                _dir: dir,
            }
        }

        async fn standard(service: &str) -> Self {
            Self::new(service, Duration::from_secs(20), &[], false).await
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }

        /// What the health actor last wrote about the agent.
        fn health(&self) -> polar_health::HealthState {
            let text = std::fs::read_to_string(self._dir.path().join("health.json"))
                .expect("a health file");
            serde_json::from_str(&text).expect("a health file that parses")
        }

        /// The agent registers, as its Cassini client reports it.
        async fn registered(&self) {
            use crate::health::HealthClient;
            self.resources.healthcheck.cassini_connected().unwrap();
            self.infra
                .cast(InfrastructureSupervisorMessage::ClientEvent(
                    ClientEvent::Registered {
                        registration_id: "r".to_string(),
                    },
                ))
                .unwrap();
        }

        fn exits(&self) -> Vec<i32> {
            self.exits.lock().unwrap().clone()
        }

        fn begin(&self, reason: &str) {
            self.infra
                .cast(InfrastructureSupervisorMessage::BeginShutdown {
                    reason: reason.to_string(),
                })
                .unwrap();
        }

        /// Lets queued messages and the output ports' forwarding tasks run.
        async fn settle(&self) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // -- the funnel ----------------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn the_resources_carry_the_snapshot_the_supervisor_holds() {
        let rig = Rig::standard("infra-config").await;
        // Declared by the health section, set in the rig's map: in the snapshot.
        assert_eq!(
            rig.resources
                .config
                .read(&polar_health::vars::CASSINI_DEADLINE_SECS)
                .unwrap(),
            100000
        );
        // Not declared by anything: refused, not read from wherever it might be.
        assert!(matches!(
            rig.resources
                .config
                .read(&polar_env::Optional::<String>::text(
                    "NOT_DECLARED",
                    "nothing declares this"
                )),
            Err(polar_env::ConfigError::Undeclared(_))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn the_resources_carry_a_live_configuration_that_refuses_to_change_the_health_invariants() {
        let rig = Rig::standard("infra-live-config").await;
        let configuration = &rig.resources.configuration;
        let name = polar_health::vars::CASSINI_DEADLINE_SECS.name();

        // It starts as the snapshot.
        assert_eq!(
            configuration
                .current()
                .await
                .unwrap()
                .read(&polar_health::vars::CASSINI_DEADLINE_SECS)
                .unwrap(),
            100000
        );
        // A health safety invariant is static: an update to it is refused by name and the value
        // in force is unchanged.
        let outcome = configuration
            .apply_unchecked(1, vec![(name.to_string(), "1".to_string())])
            .await
            .unwrap();
        let crate::configuration::ApplyOutcome::Refused(crate::configuration::Refusal::Invalid(
            problems,
        )) = outcome
        else {
            panic!("the health deadline must not be changeable at runtime");
        };
        assert!(problems.iter().any(|p| p.contains(name)), "{problems:?}");
        assert_eq!(
            configuration
                .current()
                .await
                .unwrap()
                .read(&polar_health::vars::CASSINI_DEADLINE_SECS)
                .unwrap(),
            100000
        );
    }

    #[tokio::test(start_paused = true)]
    async fn signed_configuration_is_checked_against_the_ca_the_agent_pins_and_refused_without_one()
    {
        use crate::configuration::{ApplyOutcome, Refusal};
        use crate::signed::Rejection;
        use polar_signing::testing::{Spec, root, signer};

        // An agent that pins a real CA accepts what that CA's signers sign, and the document goes on
        // to the configuration checks: HOST-style variables of the health section are static.
        let ca = root("Polar Internal CA");
        let dir = tempfile::tempdir().unwrap();
        let ca_file = dir.path().join("ca.pem");
        std::fs::write(&ca_file, &ca.pem).unwrap();
        let rig = Rig::new(
            "infra-signed",
            Duration::from_secs(20),
            &[("TLS_CA_CERT", ca_file.to_str().unwrap())],
            false,
        )
        .await;
        let s = signer(
            &ca,
            Spec {
                not_after_year: 2090,
                ..Spec::default()
            },
        );
        let name = polar_health::vars::CASSINI_DEADLINE_SECS.name();
        let document = polar_signing::sign(
            serde_json::json!({
                "id": "u",
                "target": {"agent": "infra-signed"},
                "version": 1,
                "changes": [{"name": name, "value": "1"}]
            }),
            std::slice::from_ref(&s.cert_pem),
            &s.key_pem,
        )
        .unwrap()
        .to_value();
        // The agent hears its own configuration topic through the real router, and the document goes to
        // the same checks.
        let notification = polar_scheduler_common::ConfigurationNotification {
            agent: "infra-signed".to_string(),
            document_json: document.to_string(),
        };
        rig.resources
            .router
            .send_message(polar_router::RouterMessage::Route(
                polar_router::IncomingMessage {
                    topic: polar_scheduler_common::configuration_topic("infra-signed"),
                    payload: rkyv::to_bytes::<rkyv::rancor::Error>(&notification)
                        .unwrap()
                        .to_vec(),
                    published_by: None,
                    trace_ctx: None,
                },
            ))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let history = rig.resources.configuration.history().await.unwrap();
        assert_eq!(history.len(), 1, "{history:?}");
        assert!(
            history[0].signer.is_some(),
            "it was verified, then refused for what it asks"
        );

        let outcome = rig
            .resources
            .configuration
            .submit(document.clone())
            .await
            .unwrap();
        // Verified (signature, signer, target), then refused for what it asks: a static variable.
        assert!(
            matches!(outcome, ApplyOutcome::Refused(Refusal::Invalid(_))),
            "{outcome:?}"
        );

        // An agent whose CA file is not there refuses the very same document, and says why.
        let rig = Rig::new(
            "infra-unpinned",
            Duration::from_secs(20),
            &[("TLS_CA_CERT", "/nonexistent/ca.pem")],
            false,
        )
        .await;
        assert_eq!(
            rig.resources.configuration.submit(document).await.unwrap(),
            ApplyOutcome::Refused(Refusal::Unverified(Rejection::NotConfigured))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_to_shut_down_tells_the_domain_supervisor_once() {
        let rig = Rig::standard("infra-begin").await;
        rig.begin("a test asked");
        rig.settle().await;
        assert_eq!(rig.events(), ["prepare_shutdown"]);
    }

    #[tokio::test(start_paused = true)]
    async fn every_further_trigger_is_ignored() {
        let rig = Rig::standard("infra-idempotent").await;
        rig.begin("first");
        rig.begin("second");
        rig.infra
            .cast(InfrastructureSupervisorMessage::Health(
                HealthVerdict::Rejuvenate,
            ))
            .unwrap();
        rig.infra
            .cast(InfrastructureSupervisorMessage::Health(
                HealthVerdict::Unhealthy("late".into()),
            ))
            .unwrap();
        rig.settle().await;
        assert_eq!(rig.events(), ["prepare_shutdown"], "exactly one");
    }

    #[tokio::test(start_paused = true)]
    async fn a_rejuvenation_verdict_starts_the_shutdown() {
        let rig = Rig::standard("infra-rejuvenate").await;
        rig.infra
            .cast(InfrastructureSupervisorMessage::Health(
                HealthVerdict::Rejuvenate,
            ))
            .unwrap();
        rig.settle().await;
        assert_eq!(rig.events(), ["prepare_shutdown"]);
    }

    #[tokio::test(start_paused = true)]
    async fn an_unhealthy_verdict_starts_the_shutdown() {
        let rig = Rig::standard("infra-unhealthy").await;
        rig.infra
            .cast(InfrastructureSupervisorMessage::Health(
                HealthVerdict::Unhealthy("the graph went dark".into()),
            ))
            .unwrap();
        rig.settle().await;
        assert_eq!(rig.events(), ["prepare_shutdown"]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_real_health_actor_reaches_the_funnel_through_its_verdict_port() {
        // Cassini never connects, so the actor's own deadline (60 s) gives a
        // verdict at its third tick. Nothing here sends one by hand.
        let rig = Rig::new(
            "infra-end-to-end",
            Duration::from_secs(20),
            &[("POLAR_HEALTH_CASSINI_DEADLINE_SECS", "60")],
            false,
        )
        .await;

        tokio::time::sleep(Duration::from_secs(100)).await;
        assert_eq!(rig.events(), ["prepare_shutdown"]);

        // And the health actor stayed alive to keep the file fresh while draining.
        assert_eq!(
            rig.resources.healthcheck.as_actor_ref().get_status(),
            ActorStatus::Running
        );
        let file = rig._dir.path().join("health.json");
        let state: polar_health::HealthState =
            serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
        assert_eq!(state.phase, polar_health::HealthPhase::ShuttingDown);
    }

    // -- the handshake and the watchdog ---------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn proceeding_stops_the_children_and_ends_the_supervisor() {
        let mut rig = Rig::standard("infra-proceed").await;
        rig.begin("test");
        rig.settle().await;
        rig.infra
            .cast(InfrastructureSupervisorMessage::ProceedWithShutdown)
            .unwrap();

        let ended = tokio::time::timeout(Duration::from_secs(30), rig.done.take().unwrap()).await;
        assert!(ended.is_ok(), "the supervisor never ended");

        assert_eq!(rig.resources.router.get_status(), ActorStatus::Stopped);
        assert_eq!(
            rig.resources.healthcheck.as_actor_ref().get_status(),
            ActorStatus::Stopped
        );
        assert_eq!(
            rig.resources.tcp_client.as_actor_ref().get_status(),
            ActorStatus::Stopped
        );
        // Stopping the health actor is expected now and must not start another
        // shutdown, as it used to.
        assert_eq!(rig.events(), ["prepare_shutdown"]);
        assert!(
            rig.exits().is_empty(),
            "a clean handshake needs no watchdog"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_proceed_is_harmless() {
        let mut rig = Rig::standard("infra-proceed-twice").await;
        rig.begin("test");
        rig.infra
            .cast(InfrastructureSupervisorMessage::ProceedWithShutdown)
            .unwrap();
        let _ = rig
            .infra
            .cast(InfrastructureSupervisorMessage::ProceedWithShutdown);
        let ended = tokio::time::timeout(Duration::from_secs(30), rig.done.take().unwrap()).await;
        assert!(ended.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn the_watchdog_exits_1_when_the_domain_supervisor_never_answers() {
        let rig = Rig::new("infra-watchdog", Duration::from_secs(20), &[], false).await;
        rig.begin("test");
        rig.settle().await;

        tokio::time::sleep(Duration::from_secs(19)).await;
        assert!(rig.exits().is_empty(), "not before the budget runs out");

        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(rig.exits(), [1], "exit 1, never 0");
    }

    #[tokio::test(start_paused = true)]
    async fn the_budget_is_the_one_the_agent_was_given() {
        let rig = Rig::new("infra-budget", Duration::from_secs(50), &[], false).await;
        rig.begin("test");
        tokio::time::sleep(Duration::from_secs(49)).await;
        assert!(rig.exits().is_empty());
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(rig.exits(), [1]);
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_running_on_a_timer_until_a_shutdown_begins() {
        let rig = Rig::standard("infra-no-watchdog").await;
        tokio::time::sleep(Duration::from_secs(600)).await;
        assert!(rig.exits().is_empty());
        assert!(rig.events().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn the_health_verdict_runs_the_whole_pipeline_to_the_watchdog() {
        let rig = Rig::new(
            "infra-whole-pipeline",
            Duration::from_secs(20),
            &[("POLAR_HEALTH_CASSINI_DEADLINE_SECS", "60")],
            false,
        )
        .await;
        // The verdict comes at the third tick (90 s); 100 s is 10 s into the budget.
        tokio::time::sleep(Duration::from_secs(100)).await;
        assert!(rig.exits().is_empty());
        // The domain supervisor never answers, so 20 s after the verdict: exit 1.
        tokio::time::sleep(Duration::from_secs(15)).await;
        assert_eq!(rig.exits(), [1]);
    }

    // -- children dying -----------------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn the_router_dying_starts_an_orderly_shutdown() {
        let rig = Rig::standard("infra-router-dies").await;
        rig.resources.router.kill();
        rig.settle().await;
        assert_eq!(rig.events(), ["prepare_shutdown"]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_health_actor_dying_starts_an_orderly_shutdown() {
        let rig = Rig::standard("infra-health-dies").await;
        rig.resources.healthcheck.as_actor_ref().kill();
        rig.settle().await;
        assert_eq!(rig.events(), ["prepare_shutdown"]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_cassini_client_ending_starts_an_orderly_shutdown_and_is_not_replaced() {
        // The client front keeps the connection up itself, so what ends here is the front,
        // which only happens when it cannot go on. The supervisor does not make another.
        let rig = Rig::standard("infra-tcp-ends").await;
        assert_eq!(rig.spawner.spawned.load(Ordering::SeqCst), 1);

        rig.resources.tcp_client.as_actor_ref().kill();
        rig.settle().await;

        assert_eq!(rig.events(), ["prepare_shutdown"]);
        assert_eq!(
            rig.spawner.spawned.load(Ordering::SeqCst),
            1,
            "no replacement"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_crypto_actor_dying_starts_an_orderly_shutdown() {
        let rig = Rig::with_crypto("infra-crypto-dies").await;
        let crypto = rig
            .resources
            .crypto
            .clone()
            .expect("an encrypting agent has one");
        crypto.as_actor_ref().kill();
        rig.settle().await;
        assert_eq!(rig.events(), ["prepare_shutdown"]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_client_is_started_with_the_snapshots_coordinates() {
        // The rig's process has no BROKER_ADDR; the snapshot is built from a map that does.
        let rig = Rig::standard("infra-tcp-config").await;
        let configs = rig.spawner.configs.lock().unwrap().clone();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].broker_endpoint, "cassini:8080");
        assert_eq!(configs[0].server_name, "cassini.local");
        assert_eq!(configs[0].client_certificate_path, "/c/cert.pem");
    }

    #[tokio::test(start_paused = true)]
    async fn children_ending_during_the_drain_change_nothing() {
        let rig = Rig::standard("infra-drain-deaths").await;
        rig.begin("test");
        rig.settle().await;

        rig.resources.router.kill();
        rig.resources.tcp_client.as_actor_ref().kill();
        rig.settle().await;

        assert_eq!(rig.events(), ["prepare_shutdown"], "no second event");
        assert_eq!(
            rig.spawner.spawned.load(Ordering::SeqCst),
            1,
            "no replacement"
        );
        assert_eq!(
            rig.infra.get_status(),
            ActorStatus::Running,
            "still up for the drain"
        );
    }

    // -- signals ---------------------------------------------------------------------------------

    /// Real signals belong to the whole process, so only one test installs a
    /// listener, and it cannot overlap another rig.
    #[tokio::test]
    async fn sigterm_starts_an_orderly_shutdown() {
        let rig = Rig::new("infra-sigterm", Duration::from_secs(20), &[], true).await;
        // Give the listener task time to register its handler.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(rig.events().is_empty());

        // SAFETY: raising a signal that the listener above has a handler for.
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
        for _ in 0..50 {
            if !rig.events().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(rig.events(), ["prepare_shutdown"]);

        // A second one, as Kubernetes sends when it grows impatient, is ignored.
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(rig.events(), ["prepare_shutdown"]);
    }

    // -- the self-test -----------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn an_agent_that_hears_its_probes_stays_connected() {
        let rig = Rig::self_testing("selftest-hearing", true).await;
        rig.registered().await;
        tokio::time::sleep(Duration::from_secs(600)).await;
        assert!(rig.health().cassini_connected, "{:?}", rig.health());
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_registered_but_deaf_reports_its_connection_lost() {
        let rig = Rig::self_testing("selftest-deaf", false).await;
        rig.registered().await;
        // Past the timeout, with room for the health actor's own tick to write the file.
        tokio::time::sleep(Duration::from_secs(120)).await;
        let health = rig.health();
        assert!(!health.cassini_connected, "{health:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_without_the_self_test_is_not_judged_by_silence() {
        let rig = Rig::standard("selftest-off").await;
        rig.registered().await;
        tokio::time::sleep(Duration::from_secs(600)).await;
        assert!(rig.health().cassini_connected, "{:?}", rig.health());
    }

    #[test]
    fn the_probe_topic_has_no_colon_in_it() {
        // The broker names a subscription `{session}:{topic}` and splits it at the first colon;
        // some services are named `jira:observer:web_client`.
        assert_eq!(
            self_test_topic("jira:observer:web_client"),
            "selftest.jira-observer-web_client"
        );
        assert_eq!(
            self_test_topic("credentials-agent-consumer"),
            "selftest.credentials-agent-consumer"
        );
    }

    #[test]
    fn a_probe_is_recognised_only_by_the_instance_that_sent_it() {
        let ours = [1u8; 16];
        let theirs = [2u8; 16];
        assert_eq!(probe_number(&ours, &probe_payload(&ours, 7)), Some(7));
        assert_eq!(probe_number(&ours, &probe_payload(&theirs, 7)), None);
        assert_eq!(probe_number(&ours, b"short"), None);
    }
}

/// A service that is not a Cassini client: the same funnel, the health actor and the watchdog,
/// with no router, client or crypto.
#[cfg(test)]
mod service_tests {
    use super::*;
    use ractor::ActorStatus;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct Seen;

    #[async_trait]
    impl Actor for Seen {
        type Msg = InfrastructureEvent;
        type State = Arc<Mutex<Vec<String>>>;
        type Arguments = Self::State;
        async fn pre_start(
            &self,
            _: ActorRef<Self::Msg>,
            seen: Self::State,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(seen)
        }
        async fn handle(
            &self,
            _: ActorRef<Self::Msg>,
            _: Self::Msg,
            seen: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            seen.lock().unwrap().push("prepare_shutdown".to_string());
            Ok(())
        }
    }

    struct ServiceRig {
        infra: ActorRef<InfrastructureSupervisorMessage>,
        done: Option<ractor::concurrency::JoinHandle<()>>,
        service: crate::ServiceResources,
        events: Arc<Mutex<Vec<String>>>,
        exits: Arc<Mutex<Vec<i32>>>,
        dir: tempfile::TempDir,
    }

    impl ServiceRig {
        async fn new(budget: Duration, health_env: &[(&str, &str)]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let vars: HashMap<String, String> = [(
                "POLAR_HEALTH_FILE",
                dir.path().join("health.json").to_str().unwrap(),
            )]
            .into_iter()
            .chain(health_env.iter().copied())
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
            // No Cassini capability: a service is not a client.
            let startup = crate::startup::prepare(&vars, &[], &[], None).unwrap();
            let exits = Arc::new(Mutex::new(Vec::new()));
            let recorded = exits.clone();
            let (infra, done) = Actor::spawn(
                None,
                InfrastructureSupervisor,
                InfrastructureSupervisorArgs {
                    service_name: "a-service".to_string(),
                    health_check_args: HealthCheckArgs::new(false, 300, vec![])
                        .with_config(startup.health),
                    config: startup.config,
                    subscriptions: vec![],
                    wants_crypto: false,
                    with_client: false,
                    shutdown_budget: budget,
                    listen_for_signals: false,
                    process_exit: Arc::new(move |code| recorded.lock().unwrap().push(code)),
                    tcp_spawner: Arc::new(CassiniSpawner),
                    self_test: false,
                },
            )
            .await
            .unwrap();
            let service = ractor::call!(infra, |reply| InfrastructureSupervisorMessage::GetService {
                reply
            })
            .unwrap();
            let events = Arc::new(Mutex::new(Vec::new()));
            let (log, _) = Actor::spawn(None, Seen, events.clone()).await.unwrap();
            service.infrastructure_events.subscribe(log, Some);
            Self {
                infra,
                done: Some(done),
                service,
                events,
                exits,
                dir,
            }
        }

        async fn settle(&self) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_service_has_no_cassini_client_and_says_so() {
        let rig = ServiceRig::new(Duration::from_secs(20), &[]).await;
        let resources = ractor::call!(rig.infra, |reply| {
            InfrastructureSupervisorMessage::GetResources { reply }
        })
        .unwrap();
        assert!(resources.is_none(), "no router, client or crypto to give");
    }

    #[tokio::test(start_paused = true)]
    async fn a_service_is_not_failed_for_a_connection_it_never_makes() {
        // An agent that never connects is ended at its Cassini deadline. A service has nothing
        // to connect to, so the health actor is told it is connected and never gives that verdict.
        let rig = ServiceRig::new(
            Duration::from_secs(20),
            &[("POLAR_HEALTH_CASSINI_DEADLINE_SECS", "60")],
        )
        .await;
        tokio::time::sleep(Duration::from_secs(200)).await;
        assert!(rig.events.lock().unwrap().is_empty(), "no verdict, no shutdown");
        assert_eq!(
            rig.service.healthcheck.as_actor_ref().get_status(),
            ActorStatus::Running
        );
        // And it is keeping its health file, which is what the probe reads.
        assert!(rig.dir.path().join("health.json").exists());
    }

    #[tokio::test(start_paused = true)]
    async fn a_health_verdict_tells_the_service_once() {
        let rig = ServiceRig::new(Duration::from_secs(20), &[]).await;
        for verdict in [HealthVerdict::Rejuvenate, HealthVerdict::Unhealthy("late".into())] {
            rig.infra
                .cast(InfrastructureSupervisorMessage::Health(verdict))
                .unwrap();
        }
        rig.settle().await;
        assert_eq!(*rig.events.lock().unwrap(), ["prepare_shutdown"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_service_asking_to_end_goes_through_the_same_funnel() {
        // A shutdown requested over the wire, as the broker's control manager does.
        let rig = ServiceRig::new(Duration::from_secs(20), &[]).await;
        rig.infra
            .cast(InfrastructureSupervisorMessage::BeginShutdown {
                reason: "asked over the wire".into(),
            })
            .unwrap();
        rig.settle().await;
        assert_eq!(*rig.events.lock().unwrap(), ["prepare_shutdown"]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_watchdog_ends_a_service_that_never_finishes_draining() {
        let rig = ServiceRig::new(Duration::from_secs(20), &[]).await;
        rig.infra
            .cast(InfrastructureSupervisorMessage::BeginShutdown { reason: "test".into() })
            .unwrap();
        rig.settle().await;
        tokio::time::sleep(Duration::from_secs(19)).await;
        assert!(rig.exits.lock().unwrap().is_empty());
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(*rig.exits.lock().unwrap(), [1]);
    }

    #[tokio::test(start_paused = true)]
    async fn proceeding_ends_the_service_infrastructure() {
        let mut rig = ServiceRig::new(Duration::from_secs(20), &[]).await;
        rig.infra
            .cast(InfrastructureSupervisorMessage::BeginShutdown { reason: "test".into() })
            .unwrap();
        rig.settle().await;
        rig.infra
            .cast(InfrastructureSupervisorMessage::ProceedWithShutdown)
            .unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(30), rig.done.take().unwrap()).await;
        assert!(ended.is_ok(), "the supervisor never ended");
        assert_eq!(
            rig.service.healthcheck.as_actor_ref().get_status(),
            ActorStatus::Stopped
        );
        assert!(rig.exits.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn the_health_actor_dying_ends_a_service_in_order() {
        let rig = ServiceRig::new(Duration::from_secs(20), &[]).await;
        rig.service.healthcheck.as_actor_ref().kill();
        rig.settle().await;
        assert_eq!(*rig.events.lock().unwrap(), ["prepare_shutdown"]);
    }
}
