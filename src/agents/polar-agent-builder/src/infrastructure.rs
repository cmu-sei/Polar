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
//! Also coordinates a real, two-way shutdown handshake with the
//! domain supervisor -- HealthCheckActor's own, real,
//! rejuvenation-driven termination doesn't, by itself, mean
//! infrastructure is safe to tear down yet: the domain supervisor's
//! own, separate children (a graph controller, watcher trees) may
//! still, genuinely need the router and TCP client alive while they
//! themselves shut down cleanly. See InfrastructureEvent::PrepareShutdown
//! and InfrastructureSupervisorMessage::ProceedWithShutdown for the
//! real, complete protocol.

use crate::crypto::CryptoHandle;
use crate::health::HealthHandle;
use crate::{AgentResources, Subscription, build_lifecycle_handler};
use cassini_types::ClientEvent;
use polar::cassini::TcpClient;
use polar_health::{HealthCheckActor, HealthCheckArgs};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, RpcReplyPort, SupervisionEvent, async_trait};
use std::sync::Arc;

/// The "infra → domain" direction -- a real agent subscribes to this
/// via AgentResources's own events_output/infrastructure_events
/// fields, using its own, real mapping closure, exactly mirroring
/// the established prepare_shutdown_port/graph_signal_port pattern.
#[derive(Clone)]
pub enum InfrastructureEvent {
    /// HealthCheckActor (this actor's own, direct child) has
    /// genuinely, cleanly terminated -- the domain supervisor should
    /// begin coordinating its own shutdown now (its own domain
    /// children first), then send
    /// InfrastructureSupervisorMessage::ProceedWithShutdown once
    /// that's genuinely, fully done.
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
}

pub struct InfrastructureSupervisorArgs {
    pub service_name: String,
    pub health_check_args: HealthCheckArgs,
    pub subscriptions: Vec<Subscription>,
    pub wants_crypto: bool,
}

pub struct InfrastructureSupervisorState {
    events_output: Arc<OutputPort<ClientEvent>>,
    infrastructure_events: Arc<OutputPort<InfrastructureEvent>>,
    router: ActorRef<polar_router::RouterMessage>,
    tcp_client: TcpClient,
    crypto: Option<CryptoHandle>,
    healthcheck: HealthHandle,
    service_name: String,
    subscribed_topics: Vec<String>,
    wants_crypto: bool,
    shutting_down: bool,
}

pub struct InfrastructureSupervisor;

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

        let (healthcheck, _) = HealthCheckActor::spawn_linked(
            None,
            HealthCheckActor,
            args.health_check_args,
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
        let lifecycle_handler = build_lifecycle_handler(
            args.service_name.clone(),
            subscribed_topics.clone(),
            Box::new(|event: ClientEvent| {
                Some(InfrastructureSupervisorMessage::ClientEvent(event))
            }),
        );

        let tcp_client = TcpClient::spawn_with_router(
            &args.service_name,
            myself.clone(),
            lifecycle_handler,
            router.clone(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

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

        Ok(InfrastructureSupervisorState {
            events_output,
            infrastructure_events,
            router,
            tcp_client,
            crypto,
            healthcheck: HealthHandle::new(healthcheck),
            service_name: args.service_name,
            subscribed_topics,
            wants_crypto: args.wants_crypto,
            shutting_down: false,
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
            InfrastructureSupervisorMessage::ProceedWithShutdown => {
                tracing::info!(
                    "InfrastructureSupervisor: proceeding with its own shutdown"
                );
                myself
                    .stop_children_and_wait(
                        Some("infrastructure shutdown".to_string()),
                        Some(std::time::Duration::from_secs(10)),
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
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                // HealthCheckActor is spawned unnamed (matching
                // AgentBuilder's own, already-established convention)
                // -- matched by real, genuine actor id, the same,
                // now-established fix every domain supervisor already
                // uses for the identical, real reason.
                if actor_cell.get_id() == state.healthcheck.as_actor_ref().get_id() {
                    tracing::info!(
                        "InfrastructureSupervisor: healthcheck terminated cleanly ({reason:?}), \
                         signalling the domain supervisor to begin its own shutdown"
                    );
                    state.shutting_down = true;
                    state
                        .infrastructure_events
                        .send(InfrastructureEvent::PrepareShutdown);
                    return Ok(());
                }

                // TcpClientActor is spawned under its own, real,
                // deterministic name -- see
                // TcpClient::spawn_with_router's own, established
                // convention.
                if actor_cell.get_name() == Some(format!("{}.tcp", state.service_name)) {
                    if state.shutting_down {
                        tracing::info!(
                            "InfrastructureSupervisor: TcpClientActor terminated during a \
                             genuine, real shutdown -- not respawning"
                        );
                        return Ok(());
                    }

                    tracing::warn!(
                        "InfrastructureSupervisor: TcpClientActor terminated unexpectedly \
                         ({reason:?}) -- respawning it, and CryptoActor if requested, together"
                    );

                    let myself_cell = myself.get_cell();
                    let lifecycle_handler = build_lifecycle_handler(
                        state.service_name.clone(),
                        state.subscribed_topics.clone(),
                        Box::new(|event: ClientEvent| {
                            Some(InfrastructureSupervisorMessage::ClientEvent(event))
                        }),
                    );

                    let new_tcp_client = TcpClient::spawn_with_router(
                        &state.service_name,
                        myself.clone(),
                        lifecycle_handler,
                        state.router.clone(),
                    )
                    .await
                    .map_err(ActorProcessingErr::from)?;

                    let new_crypto = if state.wants_crypto {
                        let (crypto_actor, _) = polar_crypto::CryptoActor::spawn_linked(
                            None,
                            polar_crypto::CryptoActor,
                            polar_crypto::CryptoActorArgs {
                                tcp_client: new_tcp_client.as_actor_ref(),
                                router: state.router.clone(),
                            },
                            myself_cell,
                        )
                        .await
                        .map_err(ActorProcessingErr::from)?;
                        Some(CryptoHandle::new(crypto_actor))
                    } else {
                        None
                    };

                    state.tcp_client = new_tcp_client.clone();
                    state.crypto = new_crypto.clone();

                    state.infrastructure_events.send(InfrastructureEvent::Respawned {
                        tcp_client: new_tcp_client,
                        crypto: new_crypto,
                    });
                    return Ok(());
                }

                // Any other real child (router, or crypto if this
                // termination wasn't caught by the two, real,
                // specific cases above) genuinely, unexpectedly dying
                // is treated as fatal, ordinary supervision -- this
                // actor stops, and the domain supervisor sees that
                // via its own, real ActorTerminated for this actor
                // itself.
                tracing::error!(
                    "InfrastructureSupervisor: child {0:?}:{1:?} terminated unexpectedly \
                     ({reason:?}) -- treating as fatal",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                myself.stop(Some("infrastructure child terminated unexpectedly".to_string()));
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                tracing::error!(
                    "InfrastructureSupervisor: child {0:?}:{1:?} failed: {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                myself.stop(Some("infrastructure child failed".to_string()));
            }
            _ => {}
        }
        Ok(())
    }
}
