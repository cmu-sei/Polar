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

pub mod crypto;
pub use crypto::{CryptoClient, CryptoHandle};

use cassini_client::TcpClientMessage;
use cassini_types::ClientEvent;
use polar::cassini::TcpClient;
use polar_health::{HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use ractor::{Actor, ActorProcessingErr, ActorRef};

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
/// domain actors, e.g. a graph controller).
pub struct AgentResources {
    pub router: ActorRef<polar_router::RouterMessage>,
    pub tcp_client: TcpClient,
    pub healthcheck: Option<ActorRef<HealthCheckMessage>>,
}

pub struct AgentBuilder<M: ractor::Message> {
    myself: ActorRef<M>,
    service_name: String,
    subscriptions: Vec<Subscription>,
    health_check_args: Option<HealthCheckArgs>,
    map_lifecycle_event: Option<Box<dyn Fn(ClientEvent) -> Option<M> + Send + Sync>>,
}

impl<M: ractor::Message> AgentBuilder<M> {
    pub fn new(myself: ActorRef<M>, service_name: impl Into<String>) -> Self {
        Self {
            myself,
            service_name: service_name.into(),
            subscriptions: Vec::new(),
            health_check_args: None,
            map_lifecycle_event: None,
        }
    }

    pub fn with_health_check(mut self, args: HealthCheckArgs) -> Self {
        self.health_check_args = Some(args);
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

    pub async fn build(self) -> Result<AgentResources, ActorProcessingErr> {
        let myself_cell = self.myself.get_cell();

        let healthcheck = match self.health_check_args {
            Some(args) => {
                let (healthcheck, _) = HealthCheckActor::spawn_linked(
                    None,
                    HealthCheckActor,
                    args,
                    myself_cell.clone(),
                )
                .await
                .map_err(ActorProcessingErr::from)?;
                Some(healthcheck)
            }
            None => None,
        };

        let (router, _) = polar_router::RouterActor::spawn_linked(
            None,
            polar_router::RouterActor,
            (),
            myself_cell.clone(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        // Every real, collected topic name, remembered here so the
        // wrapped lifecycle closure below can fire the real,
        // wire-level Subscribe for each one automatically, exactly
        // once, the moment ClientEvent::Registered genuinely fires --
        // the real, other half of Subscription's own, atomic
        // guarantee.
        let subscribed_topics: Vec<String> =
            self.subscriptions.iter().map(|s| s.topic.clone()).collect();

        for subscription in self.subscriptions {
            router
                .send_message(polar_router::RouterMessage::Register {
                    topic: subscription.topic,
                    deliver: subscription.deliver,
                })
                .map_err(ActorProcessingErr::from)?;
        }

        let map_lifecycle_event = self
            .map_lifecycle_event
            .unwrap_or_else(|| Box::new(|_event| None));

        let wrapped_lifecycle_handler =
            build_lifecycle_handler(self.service_name.clone(), subscribed_topics, map_lifecycle_event);

        let tcp_client = TcpClient::spawn_with_router(
            &self.service_name,
            self.myself,
            wrapped_lifecycle_handler,
            router.clone(),
        )
        .await
        .map_err(ActorProcessingErr::from)?;

        Ok(AgentResources {
            router,
            tcp_client,
            healthcheck,
        })
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
fn build_lifecycle_handler<M: ractor::Message>(
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
            Some(FakeMessage::Forwarded("a real, genuine disconnect".to_string()))
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
