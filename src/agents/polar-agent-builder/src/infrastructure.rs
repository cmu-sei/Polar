//! InfrastructureSupervisor -- owns HealthCheckActor, RouterActor,
//! TcpClientActor, and (if requested) CryptoActor as real, direct,
//! linked children, in one, real, cohesive subtree. Spawned by
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
    /// The TCP client terminated unexpectedly and was genuinely,
    /// freshly respawned (only when not already shutting down) --
    /// fresh, real handles for the domain supervisor's own state.
    Respawned {
        tcp_client: TcpClient,
        crypto: Option<CryptoHandle>,
    },
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
    /// already, genuinely spawned internally.
    GetResources { reply: RpcReplyPort<AgentResources> },
    /// Internal: the health actor's verdict, delivered through the port this
    /// supervisor gave it.
    Health(HealthVerdict),
    /// Ask for an orderly shutdown, for this reason. Sent by the signal listener;
    /// it can be sent by anything holding AgentResources::infrastructure. If a
    /// shutdown is already under way this changes nothing.
    BeginShutdown { reason: String },
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
    /// How long the domain supervisor has to drain before the process exits anyway.
    pub(crate) shutdown_budget: Duration,
    /// Listen for SIGTERM and SIGINT. Off in tests, because signals belong to the
    /// whole process.
    pub(crate) listen_for_signals: bool,
    pub(crate) process_exit: ProcessExit,
    pub(crate) tcp_spawner: Arc<dyn TcpClientSpawner>,
}

pub struct InfrastructureSupervisorState {
    config: Arc<polar_env::Config>,
    events_output: Arc<OutputPort<ClientEvent>>,
    infrastructure_events: Arc<OutputPort<InfrastructureEvent>>,
    router: ActorRef<polar_router::RouterMessage>,
    tcp_client: TcpClient,
    crypto: Option<CryptoHandle>,
    healthcheck: HealthHandle,
    service_name: String,
    subscribed_topics: Vec<String>,
    wants_crypto: bool,
    shutdown: ShutdownMachine,
    shutdown_budget: Duration,
    process_exit: ProcessExit,
    tcp_spawner: Arc<dyn TcpClientSpawner>,
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

    fn classify(state: &InfrastructureSupervisorState, cell: &ActorCell) -> Child {
        let id = cell.get_id();
        if id == state.healthcheck.as_actor_ref().get_id() {
            Child::Health
        } else if cell.get_name() == Some(format!("{}.tcp", state.service_name)) {
            // TcpClientActor is spawned under its own deterministic name; see
            // TcpClient::spawn_with_router.
            Child::TcpClient
        } else if id == state.router.get_id() {
            Child::Other("the router".to_string())
        } else if state
            .crypto
            .as_ref()
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

    async fn child_ended(
        myself: &ActorRef<InfrastructureSupervisorMessage>,
        state: &mut InfrastructureSupervisorState,
        cell: ActorCell,
        death: Death,
    ) {
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
            Action::RespawnTcpClient => {
                tracing::warn!(
                    "InfrastructureSupervisor: TcpClientActor terminated unexpectedly \
                     ({death:?}) -- respawning it, and CryptoActor if requested, together"
                );
                if let Err(e) = Self::respawn_tcp_client(myself, state).await {
                    let reason = format!("the Cassini client could not be respawned: {e}");
                    tracing::error!("InfrastructureSupervisor: {reason}");
                    Self::begin_shutdown(state, reason);
                }
            }
        }
    }

    /// Replaces the Cassini client, and the crypto actor that holds a reference to
    /// it, together, sharing the same router.
    async fn respawn_tcp_client(
        myself: &ActorRef<InfrastructureSupervisorMessage>,
        state: &mut InfrastructureSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        let lifecycle_handler: LifecycleHandler = Box::new(build_lifecycle_handler(
            state.service_name.clone(),
            state.subscribed_topics.clone(),
            Box::new(|event: ClientEvent| {
                Some(InfrastructureSupervisorMessage::ClientEvent(event))
            }),
        ));

        // From the snapshot, as at the first start: a respawn is not a second reading of
        // the process.
        let client_config =
            TCPClientConfig::from_env(&*state.config).map_err(ActorProcessingErr::from)?;
        let new_tcp_client = state
            .tcp_spawner
            .spawn(
                &state.service_name,
                myself.clone(),
                lifecycle_handler,
                state.router.clone(),
                client_config,
            )
            .await?;

        let new_crypto = if state.wants_crypto {
            let (crypto_actor, _) = polar_crypto::CryptoActor::spawn_linked(
                None,
                polar_crypto::CryptoActor,
                polar_crypto::CryptoActorArgs {
                    tcp_client: new_tcp_client.as_actor_ref(),
                    router: state.router.clone(),
                },
                myself.get_cell(),
            )
            .await
            .map_err(ActorProcessingErr::from)?;
            Some(CryptoHandle::new(crypto_actor))
        } else {
            None
        };

        state.tcp_client = new_tcp_client.clone();
        state.crypto = new_crypto.clone();

        state
            .infrastructure_events
            .send(InfrastructureEvent::Respawned {
                tcp_client: new_tcp_client,
                crypto: new_crypto,
            });
        Ok(())
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

        let (router, _) = polar_router::RouterActor::spawn_linked(
            None,
            polar_router::RouterActor,
            (),
            myself_cell.clone(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        let subscribed_topics: Vec<String> =
            args.subscriptions.iter().map(|s| s.topic.clone()).collect();

        for subscription in args.subscriptions {
            router
                .send_message(polar_router::RouterMessage::Register {
                    topic: subscription.topic,
                    deliver: subscription.deliver,
                })
                .map_err(ActorProcessingErr::from)?;
        }

        let events_output = Arc::new(OutputPort::default());
        let infrastructure_events = Arc::new(OutputPort::default());

        // Every real ClientEvent this agent's own TcpClient ever
        // produces is immediately, directly re-sent out via
        // events_output above -- this actor never, itself, tries to
        // interpret them (Registered vs TransportError, etc); a real
        // domain supervisor's own, real lifecycle-handling closure,
        // subscribed to that port, does that.
        let lifecycle_handler: LifecycleHandler = Box::new(build_lifecycle_handler(
            args.service_name.clone(),
            subscribed_topics.clone(),
            Box::new(|event: ClientEvent| {
                Some(InfrastructureSupervisorMessage::ClientEvent(event))
            }),
        ));

        let client_config =
            TCPClientConfig::from_env(&*args.config).map_err(ActorProcessingErr::from)?;
        let tcp_client = args
            .tcp_spawner
            .spawn(
                &args.service_name,
                myself.clone(),
                lifecycle_handler,
                router.clone(),
                client_config,
            )
            .await?;

        let crypto = if args.wants_crypto {
            let (crypto_actor, _) = polar_crypto::CryptoActor::spawn_linked(
                None,
                polar_crypto::CryptoActor,
                polar_crypto::CryptoActorArgs {
                    tcp_client: tcp_client.as_actor_ref(),
                    router: router.clone(),
                },
                myself_cell,
            )
            .await
            .map_err(ActorProcessingErr::from)?;
            Some(CryptoHandle::new(crypto_actor))
        } else {
            None
        };

        if args.listen_for_signals {
            shutdown::spawn_signal_listener(myself.clone());
        }

        Ok(InfrastructureSupervisorState {
            config: args.config,
            events_output,
            infrastructure_events,
            router,
            tcp_client,
            crypto,
            healthcheck: HealthHandle::new(healthcheck),
            service_name: args.service_name,
            subscribed_topics,
            wants_crypto: args.wants_crypto,
            shutdown: ShutdownMachine::new(),
            shutdown_budget: args.shutdown_budget,
            process_exit: args.process_exit,
            tcp_spawner: args.tcp_spawner,
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
                if let ClientEvent::Registered { .. } = &event {
                    if let Some(crypto) = &state.crypto {
                        if let Err(e) = crypto
                            .as_actor_ref()
                            .send_message(polar_crypto::CryptoMessage::ClientRegistered)
                        {
                            tracing::warn!("failed to tell CryptoActor the client registered: {e}");
                        }
                    }
                }
                state.events_output.send(event);
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
                let _ = reply.send(AgentResources {
                    router: state.router.clone(),
                    tcp_client: state.tcp_client.clone(),
                    healthcheck: state.healthcheck.clone(),
                    crypto: state.crypto.clone(),
                    events_output: state.events_output.clone(),
                    infrastructure_events: state.infrastructure_events.clone(),
                    infrastructure: myself.clone(),
                    config: state.config.clone(),
                });
            }
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
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                Self::child_ended(&myself, state, cell, Death::Terminated(reason)).await;
            }
            SupervisionEvent::ActorFailed(cell, error) => {
                Self::child_ended(&myself, state, cell, Death::Failed(error.to_string())).await;
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
            _message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
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
            _router: ActorRef<polar_router::RouterMessage>,
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
                InfrastructureEvent::Respawned { .. } => "respawned".to_string(),
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
            let dir = tempfile::tempdir().unwrap();
            let vars: HashMap<String, String> = [
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
            let startup =
                crate::startup::prepare(&vars, &[], &[crate::Capability::Cassini], None).unwrap();

            let exits = Arc::new(Mutex::new(Vec::new()));
            let recorded = exits.clone();
            let spawner = Arc::new(FakeSpawner::default());

            let (infra, done) = Actor::spawn(
                None,
                InfrastructureSupervisor,
                InfrastructureSupervisorArgs {
                    service_name: service.to_string(),
                    health_check_args: HealthCheckArgs::new(false, 300, vec![])
                        .with_config(startup.health),
                    config: startup.config,
                    subscriptions: vec![],
                    wants_crypto: false,
                    shutdown_budget: budget,
                    listen_for_signals,
                    process_exit: Arc::new(move |code| recorded.lock().unwrap().push(code)),
                    tcp_spawner: spawner.clone(),
                },
            )
            .await
            .unwrap();

            let resources: AgentResources = ractor::call!(infra, |reply| {
                InfrastructureSupervisorMessage::GetResources { reply }
            })
            .unwrap();

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
    async fn the_cassini_client_stopping_is_replaced_not_fatal() {
        let rig = Rig::standard("infra-tcp-replaced").await;
        assert_eq!(rig.spawner.spawned.load(Ordering::SeqCst), 1);

        rig.resources.tcp_client.as_actor_ref().kill();
        rig.settle().await;

        assert_eq!(
            rig.spawner.spawned.load(Ordering::SeqCst),
            2,
            "a new client"
        );
        assert_eq!(rig.events(), ["respawned"], "told, and not shut down");
        assert!(rig.infra.get_status() == ActorStatus::Running);
    }

    #[tokio::test(start_paused = true)]
    async fn the_client_is_started_and_restarted_with_the_snapshots_coordinates() {
        // The rig's process has no BROKER_ADDR; the snapshot is built from a map that does.
        let rig = Rig::standard("infra-tcp-config").await;
        rig.resources.tcp_client.as_actor_ref().kill();
        rig.settle().await;

        let configs = rig.spawner.configs.lock().unwrap().clone();
        assert_eq!(configs.len(), 2, "the first client and its replacement");
        for config in configs {
            assert_eq!(config.broker_endpoint, "cassini:8080");
            assert_eq!(config.server_name, "cassini.local");
            assert_eq!(config.client_certificate_path, "/c/cert.pem");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_cassini_client_failing_starts_an_orderly_shutdown() {
        let rig = Rig::standard("infra-tcp-fails").await;
        rig.spawner
            .clients_fail_on_message
            .store(true, Ordering::SeqCst);
        // Respawn one that will fail, then make it fail.
        rig.resources.tcp_client.as_actor_ref().kill();
        rig.settle().await;
        assert_eq!(rig.events(), ["respawned"]);

        let client = ractor::registry::where_is("infra-tcp-fails.tcp".to_string())
            .expect("the replacement is registered");
        let client: ActorRef<TcpClientMessage> = client.into();
        client.cast(TcpClientMessage::Register).unwrap();
        rig.settle().await;

        assert_eq!(rig.events(), ["respawned", "prepare_shutdown"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cassini_client_that_cannot_be_replaced_starts_an_orderly_shutdown() {
        let rig = Rig::standard("infra-tcp-no-replacement").await;
        rig.spawner.fail_next_spawn.store(true, Ordering::SeqCst);
        rig.resources.tcp_client.as_actor_ref().kill();
        rig.settle().await;
        assert_eq!(rig.events(), ["prepare_shutdown"], "and no respawned event");
    }

    #[tokio::test(start_paused = true)]
    async fn children_ending_during_the_drain_change_nothing() {
        let rig = Rig::standard("infra-drain-deaths").await;
        rig.begin("test");
        rig.settle().await;

        rig.resources.router.kill();
        rig.resources.tcp_client.as_actor_ref().kill();
        rig.settle().await;

        assert_eq!(
            rig.events(),
            ["prepare_shutdown"],
            "no second event, no respawn"
        );
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
}
