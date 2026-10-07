//! Supervisor for the credentials-agent observer.
//!
//! The observer is the Credentials Agent's HTTP front door: it turns
//! POST /issue, /identity, /account and /admin-action into requests on the
//! Cassini request topics and waits for the consumer's reply on the matching
//! reply topic. This supervisor owns that whole lifecycle, on the same agent
//! builder every other migrated agent uses: health check, router, the four
//! reply subscriptions and the Cassini client come from `AgentBuilder`, and
//! the HTTP server runs under this supervisor instead of beside it.
//!
//! What it does NOT build, on purpose: no `with_encryption()` and no
//! `with_graph_write()`.
//!
//! * Encryption. `CryptoActor` is the process-local gateway to the
//!   Credentials Agent, and its requests go to the very topics the consumer
//!   half of this agent serves. An observer holding one would be asking the
//!   service it fronts for its own keys. The observer never encrypts or
//!   decrypts anything (it only forwards requests and replies), so it has no
//!   use for one. The deployment must not provide the `encryption` capability
//!   to this workload either: that would also make polar-init run the escrow
//!   bootstrap against a Credentials Agent that may be restarting, which is
//!   the wait this agent exists to avoid.
//! * Graph. The observer never touches Neo4j.
//!
//! Shutdown, in order, when the health check ends the pod's life for
//! rejuvenation:
//!
//! 1. `InfrastructureEvent::PrepareShutdown` reaches this supervisor.
//! 2. The HTTP server is told to stop accepting and to finish in-flight
//!    requests, for at most `DRAIN_TIMEOUT`.
//! 3. `ProceedWithShutdown` goes to the infrastructure supervisor, which
//!    stops its children.
//! 4. The infrastructure supervisor's termination makes this supervisor exit
//!    non-zero. Exit is never zero: this agent runs as a Job, so a clean exit
//!    would count as completion and nothing would restart it.

use crate::handler::{ObserverState, account, admin_action, identity, issue};
use axum::Router;
use axum::routing::post;
use cassini_types::ClientEvent;
use credentials_agent_common::account_claim::AccountReply;
use credentials_agent_common::admin_action::AdminActionReply;
use credentials_agent_common::identity::IdentityNormalizationReply;
use credentials_agent_common::issuance::IssuanceReply;
use credentials_agent_common::{
    CREDENTIALS_ACCOUNT_REPLIES_TOPIC, CREDENTIALS_ADMIN_ACTION_REPLIES_TOPIC,
    CREDENTIALS_IDENTITY_NORMALIZATION_REPLIES_TOPIC, CREDENTIALS_ISSUANCE_REPLIES_TOPIC,
};
use polar::cassini::TcpClient;
use polar::request_reply::{Envelope, ReplyRouter};
use polar_agent_builder::{
    AgentBuilder, HealthClient, HealthHandle, InfrastructureEvent, InfrastructureSupervisorMessage,
    Subscription,
};
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{error, info, warn};

pub const SERVICE_NAME: &str = "credentials-agent-observer";

const PORT_ENV: &str = "CREDENTIALS_AGENT_OBSERVER_PORT";
const DEFAULT_PORT: &str = "8080";

/// How long an in-flight request may keep the process alive once shutdown
/// has begun. A request waits for the consumer's reply for up to a third of
/// the caller's certificate lifetime, so this bounds the common case without
/// holding the pod's replacement back for minutes.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the infrastructure supervisor gives this agent to drain: the HTTP
/// server's drain, which can use all of `DRAIN_TIMEOUT`, plus a margin. If it is
/// not done by then the process exits 1 regardless.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(DRAIN_TIMEOUT.as_secs() + 10);

/// The four reply routers: one per request/reply flow this observer fronts.
/// Shared between the subscriptions that fill them and the HTTP handlers
/// that wait on them.
#[derive(Clone, Default)]
pub struct ReplyRouters {
    pub issuance: Arc<ReplyRouter<IssuanceReply>>,
    pub identity: Arc<ReplyRouter<IdentityNormalizationReply>>,
    pub account: Arc<ReplyRouter<AccountReply>>,
    pub admin_action: Arc<ReplyRouter<AdminActionReply>>,
}

macro_rules! reply_subscription {
    ($topic:expr, $reply:ty, $router:expr, $what:expr) => {{
        let router = $router.clone();
        Subscription::new($topic, move |incoming| {
            match rkyv::from_bytes::<Envelope<$reply>, rkyv::rancor::Error>(&incoming.payload) {
                Ok(envelope) => router.deliver(&envelope.correlation_id, envelope.payload),
                Err(e) => warn!("failed to deserialize {} reply: {e}", $what),
            }
        })
    }};
}

/// One subscription per reply topic. A subscription registers the topic with
/// the router and subscribes to it on the wire together, so there is no way
/// to have one without the other.
pub fn reply_subscriptions(routers: &ReplyRouters) -> Vec<Subscription> {
    vec![
        reply_subscription!(
            CREDENTIALS_ISSUANCE_REPLIES_TOPIC,
            IssuanceReply,
            routers.issuance,
            "issuance"
        ),
        reply_subscription!(
            CREDENTIALS_IDENTITY_NORMALIZATION_REPLIES_TOPIC,
            IdentityNormalizationReply,
            routers.identity,
            "identity-normalization"
        ),
        reply_subscription!(
            CREDENTIALS_ACCOUNT_REPLIES_TOPIC,
            AccountReply,
            routers.account,
            "account"
        ),
        reply_subscription!(
            CREDENTIALS_ADMIN_ACTION_REPLIES_TOPIC,
            AdminActionReply,
            routers.admin_action,
            "admin action"
        ),
    ]
}

/// This supervisor's own message type. The shared `polar::SupervisorMessage`
/// cannot carry the infrastructure signals without `polar-agent-builder`
/// depending back on `polar`, so each builder-based supervisor defines the
/// few messages it needs.
pub enum ObserverSupervisorMessage {
    ClientEvent {
        event: ClientEvent,
    },
    /// The TCP client died and the infrastructure supervisor spawned a fresh
    /// one. This agent has no CryptoActor, so the event's crypto half is
    /// always empty and dropped.
    InfrastructureRespawned {
        tcp_client: TcpClient,
    },
    /// The health check has ended for rejuvenation. Drain, then let the
    /// infrastructure go.
    PrepareShutdown,
}

pub struct ObserverSupervisor;

/// The running HTTP server, and the two switches that control it.
struct HttpServer {
    /// Tells the server to stop accepting and finish what is in flight.
    shutdown: Arc<Notify>,
    /// Set before `shutdown` is signalled, so the server task can tell an
    /// ordered stop from an unexpected one.
    stopping: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

pub struct ObserverSupervisorState {
    healthcheck: HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    observer: Arc<ObserverState>,
    http: Option<HttpServer>,
}

async fn start_http(state: Arc<ObserverState>) -> Result<HttpServer, ActorProcessingErr> {
    let port = std::env::var(PORT_ENV).unwrap_or_else(|_| DEFAULT_PORT.to_string());
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .map_err(|e| {
            ActorProcessingErr::from(format!(
                "cannot bind the observer's HTTP listener on port {port}: {e}"
            ))
        })?;
    info!("{SERVICE_NAME} listening on 0.0.0.0:{port}");

    let app = Router::new()
        .route("/issue", post(issue))
        .route("/identity", post(identity))
        .route("/account", post(account))
        .route("/admin-action", post(admin_action))
        .with_state(state);

    let shutdown = Arc::new(Notify::new());
    let stopping = Arc::new(AtomicBool::new(false));

    let task = {
        let shutdown = shutdown.clone();
        let stopping = stopping.clone();
        tokio::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(async move { shutdown.notified().await })
                .await;
            if stopping.load(Ordering::SeqCst) {
                info!("{SERVICE_NAME}: HTTP server drained ({result:?})");
            } else {
                // Nothing asked it to stop. A pod that cannot serve is not
                // worth keeping: exit so the Job starts a fresh one.
                error!("{SERVICE_NAME}: HTTP server stopped unexpectedly ({result:?}); exiting");
                std::process::exit(1);
            }
        })
    };

    Ok(HttpServer {
        shutdown,
        stopping,
        task,
    })
}

#[async_trait]
impl Actor for ObserverSupervisor {
    type Msg = ObserverSupervisorMessage;
    type State = ObserverSupervisorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        info!("{myself:?} starting");

        let routers = ReplyRouters::default();

        let mut builder = AgentBuilder::new(myself.clone(), SERVICE_NAME)
            .with_shutdown_budget(SHUTDOWN_BUDGET)
            .with_lifecycle_handler(|event| Some(ObserverSupervisorMessage::ClientEvent { event }));
        for subscription in reply_subscriptions(&routers) {
            builder = builder.with_subscription(subscription);
        }
        let resources = builder.build().await?;

        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::Respawned { tcp_client, .. } => {
                    Some(ObserverSupervisorMessage::InfrastructureRespawned { tcp_client })
                }
                InfrastructureEvent::PrepareShutdown => {
                    Some(ObserverSupervisorMessage::PrepareShutdown)
                }
            });

        let observer = Arc::new(ObserverState {
            broker_client: resources.tcp_client.as_actor_ref().into(),
            issuance_reply_router: routers.issuance.clone(),
            identity_reply_router: routers.identity.clone(),
            account_reply_router: routers.account.clone(),
            admin_action_reply_router: routers.admin_action.clone(),
        });

        let http = start_http(observer.clone()).await?;

        Ok(ObserverSupervisorState {
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            observer,
            http: Some(http),
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ObserverSupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    // The builder has already re-subscribed to the reply
                    // topics; this is only the health signal.
                    let _ = state.healthcheck.cassini_connected();
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => {}
            },
            ObserverSupervisorMessage::InfrastructureRespawned { tcp_client } => {
                warn!(
                    "TCP client was respawned by the infrastructure supervisor; \
                     pointing the request handlers at the new one"
                );
                state
                    .observer
                    .broker_client
                    .replace(tcp_client.as_actor_ref());
            }
            ObserverSupervisorMessage::PrepareShutdown => {
                info!("PrepareShutdown received: draining the HTTP server before infrastructure");
                if let Some(http) = state.http.take() {
                    http.stopping.store(true, Ordering::SeqCst);
                    http.shutdown.notify_one();
                    match tokio::time::timeout(DRAIN_TIMEOUT, http.task).await {
                        Ok(_) => info!("HTTP server drained"),
                        Err(_) => warn!(
                            "in-flight requests did not finish within {DRAIN_TIMEOUT:?}; \
                             continuing shutdown"
                        ),
                    }
                }
                let _ = state
                    .infrastructure
                    .cast(InfrastructureSupervisorMessage::ProceedWithShutdown);
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
            SupervisionEvent::ActorTerminated(actor, _, reason) => {
                // The infrastructure supervisor is this supervisor's one
                // direct child from the builder. Its termination is the end of
                // the shutdown sequence above (it is spawned unnamed, so match
                // on id). Exit so the Job starts a pod with fresh certificates.
                if actor.get_id() == state.infrastructure.get_id() {
                    info!(
                        "infrastructure shutdown complete ({reason:?}), exiting for rejuvenation"
                    );
                    std::process::exit(1);
                }
            }
            SupervisionEvent::ActorFailed(actor, err) => {
                error!("Child actor {actor:?} failed: {err}; exiting");
                std::process::exit(1);
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes `reply` the way the consumer publishes it and hands it to
    /// the subscription for `topic`, as the router would.
    macro_rules! deliver {
        ($subs:expr, $topic:expr, $correlation_id:expr, $reply:expr) => {{
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&Envelope {
                correlation_id: $correlation_id.to_string(),
                payload: $reply,
            })
            .expect("serializes");
            let sub = $subs
                .iter()
                .find(|s| s.topic == $topic)
                .expect("a subscription for the topic");
            (sub.deliver)(polar_router::IncomingMessage {
                topic: $topic.to_string(),
                payload: bytes.to_vec(),
                published_by: None,
                trace_ctx: None,
            });
        }};
    }

    #[test]
    fn subscribes_to_exactly_the_four_reply_topics() {
        let subs = reply_subscriptions(&ReplyRouters::default());
        let mut topics: Vec<&str> = subs.iter().map(|s| s.topic.as_str()).collect();
        topics.sort_unstable();
        let mut expected = vec![
            CREDENTIALS_ISSUANCE_REPLIES_TOPIC,
            CREDENTIALS_IDENTITY_NORMALIZATION_REPLIES_TOPIC,
            CREDENTIALS_ACCOUNT_REPLIES_TOPIC,
            CREDENTIALS_ADMIN_ACTION_REPLIES_TOPIC,
        ];
        expected.sort_unstable();
        assert_eq!(topics, expected);
    }

    #[test]
    fn each_reply_topic_delivers_to_its_own_router_and_no_other() {
        let routers = ReplyRouters::default();
        let subs = reply_subscriptions(&routers);

        let mut issuance = routers.issuance.register("c-issuance".to_string());
        let mut identity = routers.identity.register("c-identity".to_string());
        let mut account = routers.account.register("c-account".to_string());
        let mut admin = routers.admin_action.register("c-admin".to_string());

        deliver!(
            subs,
            CREDENTIALS_ISSUANCE_REPLIES_TOPIC,
            "c-issuance",
            IssuanceReply::Issued {
                certificate_pem: "cert".to_string(),
                ca_chain_pem: "chain".to_string(),
                escrow_public_key_pem: "key".to_string(),
            }
        );
        assert!(matches!(
            issuance.try_recv(),
            Ok(IssuanceReply::Issued { .. })
        ));
        assert!(identity.try_recv().is_err());
        assert!(account.try_recv().is_err());
        assert!(admin.try_recv().is_err());

        deliver!(
            subs,
            CREDENTIALS_IDENTITY_NORMALIZATION_REPLIES_TOPIC,
            "c-identity",
            IdentityNormalizationReply::Normalized {
                identity: "someone".to_string()
            }
        );
        assert!(matches!(
            identity.try_recv(),
            Ok(IdentityNormalizationReply::Normalized { .. })
        ));

        deliver!(
            subs,
            CREDENTIALS_ACCOUNT_REPLIES_TOPIC,
            "c-account",
            AccountReply::Offered {
                account_id: "acct".to_string()
            }
        );
        assert!(matches!(
            account.try_recv(),
            Ok(AccountReply::Offered { .. })
        ));

        deliver!(
            subs,
            CREDENTIALS_ADMIN_ACTION_REPLIES_TOPIC,
            "c-admin",
            AdminActionReply::Recorded
        );
        assert!(matches!(admin.try_recv(), Ok(AdminActionReply::Recorded)));
    }

    #[test]
    fn a_reply_for_a_different_correlation_id_reaches_nobody() {
        let routers = ReplyRouters::default();
        let subs = reply_subscriptions(&routers);
        let mut waiting = routers.admin_action.register("mine".to_string());

        deliver!(
            subs,
            CREDENTIALS_ADMIN_ACTION_REPLIES_TOPIC,
            "someone-elses",
            AdminActionReply::Recorded
        );

        assert!(waiting.try_recv().is_err());
    }

    #[test]
    fn a_malformed_payload_is_dropped_without_panicking_or_delivering() {
        let routers = ReplyRouters::default();
        let subs = reply_subscriptions(&routers);
        let mut waiting = routers.issuance.register("c".to_string());

        let sub = subs
            .iter()
            .find(|s| s.topic == CREDENTIALS_ISSUANCE_REPLIES_TOPIC)
            .expect("a subscription for the topic");
        (sub.deliver)(polar_router::IncomingMessage {
            topic: CREDENTIALS_ISSUANCE_REPLIES_TOPIC.to_string(),
            payload: vec![1, 2, 3],
            published_by: None,
            trace_ctx: None,
        });

        assert!(waiting.try_recv().is_err());
    }
}
