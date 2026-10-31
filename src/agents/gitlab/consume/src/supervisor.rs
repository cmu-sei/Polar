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

use crate::BROKER_CLIENT_NAME;
use crate::GitlabConsumer;
use crate::GitlabConsumerState;
use crate::groups::GitlabGroupConsumer;
use crate::meta::MetaConsumer;
use crate::pipelines::GitlabPipelineConsumer;
use crate::projects::GitlabProjectConsumer;
use crate::repositories::GitlabRepositoryConsumer;
use crate::runners::GitlabRunnerConsumer;
use crate::users::GitlabUserConsumer;
use cassini_types::ClientEvent;
use common::GROUPS_CONSUMER_TOPIC;
use common::METADATA_CONSUMER_TOPIC;
use common::PIPELINE_CONSUMER_TOPIC;
use common::PROJECTS_CONSUMER_TOPIC;
use common::REPOSITORY_CONSUMER_TOPIC;
use common::RUNNERS_CONSUMER_TOPIC;
use common::USER_CONSUMER_TOPIC;
use common::types::GitlabEnvelope;
use polar::Supervisor;
use polar::SupervisorMessage;
use polar::cassini::TcpClient;
use polar::graph::controller::{GraphControllerActor, GraphControllerMsg};
use polar::graph::environment::GraphSettings;
use polar::polar_health::{DepCertEndpoint, HealthCheckActor, HealthCheckArgs, HealthCheckMessage};
use ractor::Actor;
use ractor::ActorCell;
use ractor::ActorProcessingErr;
use ractor::ActorRef;
use ractor::SupervisionEvent;
use ractor::async_trait;
use ractor::concurrency::Duration;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;
use tracing::{instrument, trace};

pub struct ConsumerSupervisor;

/// The name the supervisor's graph controller is registered under.
pub const GRAPH_CONTROLLER_NAME: &str = "polar.gitlab.consumer.graph";

/// The reason the supervisor gives the children it stops itself, at a registration after the first.
const RE_REGISTERED: &str = "re-registered";

/// The consumers the supervisor starts at each registration, by the names they are registered under (each its topic).
pub const CHILDREN: [&str; 7] = [
    METADATA_CONSUMER_TOPIC,
    USER_CONSUMER_TOPIC,
    GROUPS_CONSUMER_TOPIC,
    RUNNERS_CONSUMER_TOPIC,
    PROJECTS_CONSUMER_TOPIC,
    PIPELINE_CONSUMER_TOPIC,
    REPOSITORY_CONSUMER_TOPIC,
];

/// What the supervisor is started with. The default is the agent's own: its broker client and health check spawned
/// from the process environment, and the graph read from it. A test hands in stand-ins.
#[derive(Default)]
pub struct ConsumerSupervisorArgs {
    pub parts: Option<(TcpClient, ActorRef<HealthCheckMessage>)>,
    pub graph: Option<GraphSettings>,
}

pub struct ConsumerSupervisorState {
    tcp_client: TcpClient,
    u_consumer: Option<GitlabConsumer>,
    graph_controller: Option<ActorRef<GraphControllerMsg>>,
    #[allow(dead_code)]
    healthcheck: ActorRef<HealthCheckMessage>,
    draining: bool,
    graph_settings: Option<GraphSettings>,
}

impl Supervisor for ConsumerSupervisor {
    #[instrument(level = "trace", fields(topic=topic), skip(payload))]
    fn deserialize_and_dispatch(topic: String, payload: Vec<u8>) {
        match rkyv::from_bytes::<GitlabEnvelope, rkyv::rancor::Error>(&payload) {
            Ok(message) => {
                if let Some(consumer) = ractor::registry::where_is(topic.clone()) {
                    trace!("Forwarding message to {topic}");
                    if let Err(e) = consumer.send_message(message) {
                        tracing::warn!("Error forwarding message. {e}");
                    }
                }
            }
            Err(err) => warn!("Failed to deserialize message: {:?}", err),
        }
    }
}

impl ConsumerSupervisor {
    /// Stops the consumers and then the graph controller, if they are running, and waits for each to end, so the
    /// names are free to spawn them again. A registration after the first (the client reconnected, or the broker ended
    /// the session) needs the consumers started afresh: each subscribes in its `pre_start`, and the new session has
    /// none of the old one's subscriptions, so keeping the running ones (`polar::spawn_linked_unless_running`) would
    /// leave them deaf. They end with `RE_REGISTERED`, which `handle_supervisor_evt` knows is the supervisor's own doing.
    async fn stop_children(state: &mut ConsumerSupervisorState) {
        state.u_consumer = None;
        state.graph_controller = None;
        for name in CHILDREN.into_iter().chain([GRAPH_CONTROLLER_NAME]) {
            let Some(child) = ractor::registry::where_is(name.to_string()) else {
                continue;
            };
            debug!("CONSUMER_SUPERVISOR: stopping {name} for the new registration");
            if let Err(e) = child
                .stop_and_wait(
                    Some(RE_REGISTERED.to_string()),
                    Some(Duration::from_secs(30)),
                )
                .await
            {
                warn!("CONSUMER_SUPERVISOR: {name} did not stop within 30s ({e:?}); killing it");
                let _ = child.kill_and_wait(Some(Duration::from_secs(5))).await;
            }
        }
    }

    pub async fn spawn_children(
        myself: ActorCell,
        state: &mut ConsumerSupervisorState,
        c_state: GitlabConsumerState,
    ) -> Result<(), ActorProcessingErr> {
        if let Err(e) = Actor::spawn_linked(
            Some(METADATA_CONSUMER_TOPIC.to_string()),
            MetaConsumer,
            c_state.clone(),
            myself.clone(),
        )
        .await
        {
            return Err(format!("failed to start meta consumer. {e}").into());
        }
        state.u_consumer = Some(
            Actor::spawn_linked(
                Some(USER_CONSUMER_TOPIC.to_string()),
                GitlabUserConsumer,
                c_state.clone(),
                myself.clone(),
            )
            .await?
            .0,
        );

        if let Err(e) = Actor::spawn_linked(
            Some(GROUPS_CONSUMER_TOPIC.to_string()),
            GitlabGroupConsumer,
            c_state.clone(),
            myself.clone(),
        )
        .await
        {
            return Err(format!("failed to start groups consumer. {e}").into());
        }
        if let Err(e) = Actor::spawn_linked(
            Some(RUNNERS_CONSUMER_TOPIC.to_string()),
            GitlabRunnerConsumer,
            c_state.clone(),
            myself.clone(),
        )
        .await
        {
            return Err(format!("failed to start runners consumer. {e}").into());
        }
        if let Err(e) = Actor::spawn_linked(
            Some(PROJECTS_CONSUMER_TOPIC.to_string()),
            GitlabProjectConsumer,
            c_state.clone(),
            myself.clone(),
        )
        .await
        {
            return Err(format!("failed to start projects consumer. {e}").into());
        }
        if let Err(e) = Actor::spawn_linked(
            Some(PIPELINE_CONSUMER_TOPIC.to_string()),
            GitlabPipelineConsumer,
            c_state.clone(),
            myself.clone(),
        )
        .await
        {
            return Err(format!("failed to start pipeline consumer. {e}").into());
        }
        if let Err(e) = Actor::spawn_linked(
            Some(REPOSITORY_CONSUMER_TOPIC.to_string()),
            GitlabRepositoryConsumer,
            c_state.clone(),
            myself.clone(),
        )
        .await
        {
            return Err(format!("failed to start repository consumer. {e}").into());
        }

        Ok(())
    }
}
#[async_trait]
impl Actor for ConsumerSupervisor {
    type Msg = SupervisorMessage;
    type State = ConsumerSupervisorState;
    type Arguments = ConsumerSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ConsumerSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        if let Some((tcp_client, healthcheck)) = args.parts {
            return Ok(ConsumerSupervisorState {
                tcp_client,
                u_consumer: None,
                graph_controller: None,
                healthcheck,
                draining: false,
                graph_settings: args.graph,
            });
        }

        let tcp_client = TcpClient::spawn(BROKER_CLIENT_NAME, myself.clone(), |event| {
            Some(SupervisorMessage::ClientEvent { event })
        })
        .await?;

        let (healthcheck, _) = Actor::spawn_linked(
            Some("polar.healthcheck".to_string()),
            HealthCheckActor,
            HealthCheckArgs::new(
                true,
                300,
                vec![
                    DepCertEndpoint {
                        host: "cassini-ip-svc.polar.svc.cluster.local".to_string(),
                        port: 8080,
                        min_ttl_secs: 300,
                    },
                    DepCertEndpoint {
                        host: "polar-db-svc.polar-graph.svc.cluster.local".to_string(),
                        port: 7687,
                        min_ttl_secs: 300,
                    },
                ],
            ),
            myself.clone().into(),
        )
        .await?;

        let state = ConsumerSupervisorState {
            tcp_client,
            u_consumer: None,
            graph_controller: None,
            healthcheck,
            draining: false,
            graph_settings: args.graph,
        };

        Ok(state)
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::Heartbeat => {}
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    info!("Initializing agent.");
                    let _ = state.healthcheck.cast(HealthCheckMessage::CassiniConnected);
                    // A registration after the first: the old consumers go before the new ones take their names.
                    Self::stop_children(state).await;

                    let graph_signal_port = std::sync::Arc::new(ractor::OutputPort::<
                        polar::graph::controller::GraphSignal,
                    >::default());
                    graph_signal_port.subscribe(myself.clone(), |signal| {
                        Some(SupervisorMessage::GraphSignal(signal))
                    });
                    let mut graph_args =
                        polar::graph::controller::GraphControllerArgs::new(graph_signal_port);
                    if let Some(settings) = state.graph_settings.clone() {
                        graph_args = graph_args.with_settings(settings);
                    }
                    match Actor::spawn_linked(
                        Some(GRAPH_CONTROLLER_NAME.to_string()),
                        GraphControllerActor,
                        graph_args,
                        myself.get_cell(),
                    )
                    .await
                    {
                        Ok((graph_controller, _)) => {
                            let _ = state.healthcheck.cast(HealthCheckMessage::GraphConnected);
                            state.graph_controller = Some(graph_controller.clone());
                            let c_state = GitlabConsumerState {
                                graph_controller,
                                tcp_client: state.tcp_client.clone(),
                            };
                            Self::spawn_children(myself.get_cell(), state, c_state).await?;
                        }
                        Err(e) => {
                            error!("{e}");
                            myself.stop(Some(e.to_string()));
                        }
                    }
                    info!("Finished initialization. Waiting for messages...");
                }
                ClientEvent::MessagePublished { topic, payload, .. } => {
                    if state.draining {
                        warn!(
                            "CONSUMER_SUPERVISOR: draining -- logging message on topic '{topic}' \
                             ({} bytes) instead of dispatching; will be lost on exit",
                            payload.len()
                        );
                        // TODO: publish to dead-letter queue once Cassini supports one.
                    } else {
                        ConsumerSupervisor::deserialize_and_dispatch(topic, payload);
                    }
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state
                        .healthcheck
                        .cast(HealthCheckMessage::CassiniDisconnected);
                }
                ClientEvent::ControlResponse { .. } => {}
                _ => (),
            },
            SupervisorMessage::GraphSignal(signal) => match signal {
                polar::graph::controller::GraphSignal::Available => {
                    let _ = state.healthcheck.cast(HealthCheckMessage::GraphAvailable);
                }
                polar::graph::controller::GraphSignal::OpFailed(reason) => {
                    let _ = state
                        .healthcheck
                        .cast(HealthCheckMessage::GraphOpFailed(reason));
                }
            },
            SupervisorMessage::ForceExit => {
                warn!("CONSUMER_SUPERVISOR: drain window elapsed, beginning graceful shutdown");

                if let Some(controller) = state.graph_controller.take() {
                    warn!(
                        "CONSUMER_SUPERVISOR: stopping graph controller and waiting for any \
                         in-flight write to complete before exiting"
                    );
                    match controller
                        .stop_and_wait(
                            Some("supervisor shutting down".into()),
                            Some(Duration::from_secs(30)),
                        )
                        .await
                    {
                        Ok(()) => info!("CONSUMER_SUPERVISOR: graph controller stopped cleanly"),
                        Err(e) => error!(
                            "CONSUMER_SUPERVISOR: graph controller did not stop cleanly within 30s, exiting anyway: {e:?}"
                        ),
                    }
                }

                warn!("CONSUMER_SUPERVISOR: exiting now");
                std::process::exit(1);
            }
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
            SupervisionEvent::ActorStarted(actor_cell) => {
                debug!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} started",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
            }
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                let actor_name = actor_cell.get_name().unwrap_or_default();
                // Stopped by stop_children for a new registration: not a failure, and its successor is running.
                if reason.as_deref() == Some(RE_REGISTERED) {
                    debug!("CONSUMER_SUPERVISOR: {actor_name:?} stopped for the new registration");
                    return Ok(());
                }
                warn!(
                    "CONSUMER_SUPERVISOR: {0:?}:{1:?} terminated. {reason:?}",
                    actor_name,
                    actor_cell.get_id()
                );
                // Clean termination of the healthcheck actor means the
                // rejuvenation sequence completed -- enter the bounded drain
                // window rather than running forever with no exit trigger.
                if actor_name == "polar.healthcheck" && !state.draining {
                    state.draining = true;
                    info!(
                        "CONSUMER_SUPERVISOR: healthcheck actor terminated cleanly, \
                         entering drain window before exiting for rejuvenation"
                    );
                    let _ = myself
                        .send_after(ractor::concurrency::Duration::from_secs(5), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                let actor_name = actor_cell.get_name();
                error!(
                    "Consumer_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_name,
                    actor_cell.get_id()
                );
                if !state.draining {
                    state.draining = true;
                    warn!(
                        "CONSUMER_SUPERVISOR: entering drain window before exit; \
                         any messages that arrive will be logged, not dispatched"
                    );
                    let _ = myself
                        .send_after(ractor::concurrency::Duration::from_secs(5), || {
                            SupervisorMessage::ForceExit
                        })
                        .await;
                }
            }
            SupervisionEvent::ProcessGroupChanged(..) => todo!(),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cassini_client::TcpClientMessage;
    use polar::graph::environment::GraphTls;
    use ractor::ActorStatus;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// A broker client that records what it is asked to subscribe to.
    struct FakeBroker;

    #[async_trait]
    impl Actor for FakeBroker {
        type Msg = TcpClientMessage;
        type State = Arc<Mutex<Vec<String>>>;
        type Arguments = Arc<Mutex<Vec<String>>>;

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            seen: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(seen)
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            seen: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let TcpClientMessage::Subscribe { topic, .. } = message {
                seen.lock().unwrap().push(topic);
            }
            Ok(())
        }
    }

    /// A health check that takes what it is told and does nothing with it.
    struct FakeHealth;

    #[async_trait]
    impl Actor for FakeHealth {
        type Msg = HealthCheckMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<(), ActorProcessingErr> {
            Ok(())
        }
    }

    fn registered(id: &str) -> SupervisorMessage {
        SupervisorMessage::ClientEvent {
            event: ClientEvent::Registered {
                registration_id: id.to_string(),
            },
        }
    }

    /// The ids of the consumers and the graph controller registered now, by name.
    fn children_now() -> HashMap<&'static str, ractor::ActorId> {
        CHILDREN
            .iter()
            .copied()
            .chain([GRAPH_CONTROLLER_NAME])
            .filter_map(|n| ractor::registry::where_is(n.to_string()).map(|c| (n, c.get_id())))
            .collect()
    }

    /// Waits (five seconds at most) until every child is registered and `fresh` holds of each of their ids.
    async fn children_when(
        fresh: impl Fn(&str, ractor::ActorId) -> bool,
    ) -> HashMap<&'static str, ractor::ActorId> {
        let mut now = children_now();
        for _ in 0..200 {
            if now.len() == CHILDREN.len() + 1 && now.iter().all(|(n, id)| fresh(n, *id)) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            now = children_now();
        }
        now
    }

    // A second registration (the client reconnected, or the broker ended the session and it registered again) starts
    // every consumer afresh, so each subscribes again on the new session. Before, it spawned them under names still
    // taken and the supervisor stopped itself. Stopping the old ones is the supervisor's own doing, so it does not
    // drain: a drain would end the process five seconds on (ForceExit), and this test with it.
    #[tokio::test]
    async fn a_second_registration_restarts_the_consumers_and_the_supervisor_carries_on() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (broker, _) = Actor::spawn(None, FakeBroker, seen.clone()).await.unwrap();
        let (health, _) = Actor::spawn(None, FakeHealth, ()).await.unwrap();
        let args = ConsumerSupervisorArgs {
            parts: Some((TcpClient::from_actor_ref(broker), health)),
            // Nothing listens there: the controller starts (it connects when it is used) and its first query fails,
            // which it reports and survives.
            graph: Some(GraphSettings {
                database: "neo4j".to_string(),
                user: "neo4j".to_string(),
                password: "unused".to_string(),
                endpoint: "bolt://127.0.0.1:1".to_string(),
                tls: GraphTls::None,
            }),
        };
        let (supervisor, _) = Actor::spawn(None, ConsumerSupervisor, args).await.unwrap();

        supervisor.cast(registered("first")).unwrap();
        let first = children_when(|_, _| true).await;
        assert_eq!(
            first.len(),
            CHILDREN.len() + 1,
            "the first registration started {first:?}"
        );

        supervisor.cast(registered("second")).unwrap();
        let second = children_when(|n, id| first[n] != id).await;
        assert_eq!(
            supervisor.get_status(),
            ActorStatus::Running,
            "the supervisor stopped at the second registration"
        );
        assert_eq!(
            second.len(),
            CHILDREN.len() + 1,
            "the second registration started {second:?}"
        );
        for (name, id) in &second {
            assert_ne!(first[name], *id, "{name} was not started again");
        }

        // Past the drain window: a supervisor that took the stops for failures would have ended the process by now.
        tokio::time::sleep(std::time::Duration::from_secs(6)).await;
        assert_eq!(supervisor.get_status(), ActorStatus::Running);
        assert_eq!(children_now(), second, "nothing restarted since");

        let subscribed = seen.lock().unwrap().clone();
        for topic in CHILDREN {
            assert_eq!(
                subscribed.iter().filter(|t| *t == topic).count(),
                2,
                "{topic} is subscribed once per registration: {subscribed:?}"
            );
        }

        supervisor.stop(None);
    }
}
