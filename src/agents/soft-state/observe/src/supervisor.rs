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

//! The state agent's supervisor, on the agent builder: health, the Cassini client and the shutdown handshake come from
//! `AgentBuilder`. It subscribes to the requests, starts a graph controller it only ever asks to read, and answers
//! each request on its asker's own reply topic. What it decides is [`crate::decision`]; what it reads is
//! [`crate::query`].
//!
//! Each request is answered in a task of its own, so a slow graph does not hold the next request up. A request with no
//! verified asker, or from an identity that names no workload, has no reply topic, and is dropped with a warning, as
//! the credentials agent's request gate drops one.

use crate::decision::{self, Judged};
use crate::floor::{self, Floor};
use crate::query;
use crate::settings;
use cassini_client::{OfflineBehavior, PublishRequest};
use cassini_types::{ClientEvent, WireTraceCtx};
use polar::cassini::{CassiniClient, TcpClient};
use polar::graph::controller::{GraphController, GraphSignal};
use polar_agent_builder::topic_gate::PolicySaid;
use polar_agent_builder::{
    AgentBuilder, ConfigurationHandle, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage, LookupError, PolicyLookup, Subscription,
    spawn_graph_controller, stop_graph_controller,
};
use ractor::concurrency::Duration;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use soft_state_common::{
    Envelope, OBSERVER_NAME, REQUESTS_TOPIC, StateReply, StateRequest, decode_request,
    encode_reply, replies_topic,
};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// What it holds is a read in flight at most, so this is the read's bound plus the handshake's margin.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(20);

/// How long the graph controller is given to stop.
const GRAPH_STOP: Duration = Duration::from_secs(10);

pub enum StateSupervisorMessage {
    ClientEvent {
        event: ClientEvent,
    },
    /// A request, with the identity the broker stamped on it (`None` if it stamped none).
    Request {
        payload: Vec<u8>,
        published_by: Option<String>,
    },
    Graph(GraphSignal),
    /// A runtime section was replaced.
    RuntimeChanged {
        section: String,
    },
    PrepareShutdown,
}

/// What policy says about a question [`decision::checked`] passed, as the topic gate reads it.
pub async fn ask_policy(
    lookup: &PolicyLookup,
    question: &decision::PolicyQuestion<'_>,
) -> PolicySaid {
    if lookup.actor().is_none() {
        return PolicySaid::NotRunning;
    }
    match lookup
        .decide(
            question.identity,
            None,
            decision::PolicyQuestion::ACTION,
            &question.resource(),
            polar_policy::Facts {
                topic: Some(polar_policy::TopicFacts {
                    declared: question.declared,
                    implied: false,
                }),
                ..Default::default()
            },
        )
        .await
    {
        Ok(decision) => PolicySaid::Decided(decision),
        Err(LookupError::NoPolicyLoaded) => PolicySaid::NoPolicyLoaded,
        Err(LookupError::Unavailable(why)) => {
            warn!("policy could not be consulted about a read: {why}");
            PolicySaid::Unavailable(why)
        }
    }
}

/// The answer to `request` from `published_by`: decided against the schema in force (`None` before a version has
/// arrived), then read from `graph` within `timeout` if admitted.
pub async fn answer(
    floor: Option<&Floor>,
    policy: &PolicyLookup,
    graph: &GraphController,
    timeout: Duration,
    published_by: Option<&str>,
    request: &StateRequest,
) -> StateReply {
    let said = match decision::checked(floor, published_by, request) {
        Ok(question) => ask_policy(policy, &question).await,
        // Refused before policy is asked; what it would say does not matter.
        Err(_) => PolicySaid::NotRunning,
    };
    match decision::decide(floor, published_by, request, &said) {
        Judged::Max { label, property } => query::read_max(graph, &label, &property, timeout).await,
        Judged::Refused(why) => StateReply::Refused(why),
        Judged::Unavailable(why) => StateReply::Unavailable(why),
    }
}

/// Where the answer to a request from `published_by` goes: the reply topic of the workload its identity names.
pub fn reply_topic_for(published_by: Option<&str>) -> Option<String> {
    published_by
        .and_then(cert_issuer_common::identity::workload_of)
        .map(replies_topic)
}

fn publish_reply(client: &TcpClient, topic: String, envelope: &Envelope<StateReply>) {
    if let Err(e) = client.publish(PublishRequest {
        topic: topic.clone(),
        payload: encode_reply(envelope),
        trace_ctx: WireTraceCtx::from_current_span(),
        offline_behavior: OfflineBehavior::default(),
        share_identity: true,
    }) {
        warn!("could not answer on {topic}: {e:?}");
    }
}

/// What the health actor is told about the graph: the state agent stays up, healthy, while the graph is down, and its
/// health file says the graph is not connected (the state agent does not expect the graph, so the actor never makes
/// it unhealthy for it). The closest the health actor has to `Degraded` with reason `Graph`, which health in the graph
/// will name (remaining-work.md, "HEALTH IN THE GRAPH, PULLED").
pub fn report_graph(health: &dyn HealthClient, signal: &GraphSignal) {
    let _ = match signal {
        GraphSignal::Available => health.graph_available(),
        GraphSignal::OpFailed(_) => health.graph_disconnected(),
    };
}

pub struct StateSupervisor;

pub struct StateSupervisorState {
    tcp_client: TcpClient,
    healthcheck: HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    graph: Option<GraphController>,
    graph_timeout: Duration,
    configuration: ConfigurationHandle,
    /// The schema in force; `None` until a version has arrived.
    floor: Option<Arc<Floor>>,
    policy: PolicyLookup,
    shutting_down: bool,
}

#[async_trait]
impl Actor for StateSupervisor {
    type Msg = StateSupervisorMessage;
    type State = StateSupervisorState;
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        // Judged with the rest of the configuration before the agent started, so it reads.
        let graph_timeout =
            settings::graph_timeout(&**configured.config()).map_err(ActorProcessingErr::from)?;

        let deliver_to = myself.clone();
        let requests = Subscription::new(REQUESTS_TOPIC, move |incoming| {
            if let Err(e) = deliver_to.cast(StateSupervisorMessage::Request {
                payload: incoming.payload,
                published_by: incoming.published_by,
            }) {
                warn!("failed to hand a request to the supervisor: {e}");
            }
        });
        let resources = AgentBuilder::new(myself.clone(), OBSERVER_NAME)
            .with_subscription(requests)
            .with_lifecycle_handler(|event| Some(StateSupervisorMessage::ClientEvent { event }))
            .into_configured(configured)
            .build()
            .await?;
        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => {
                    Some(StateSupervisorMessage::PrepareShutdown)
                }
            });
        resources
            .configuration
            .runtime_changes()
            .subscribe(myself.clone(), |changed| {
                Some(StateSupervisorMessage::RuntimeChanged {
                    section: changed.section,
                })
            });
        // A schema already held (the replay was quick) is read now.
        myself.cast(StateSupervisorMessage::RuntimeChanged {
            section: floor::SECTION_NAME.to_string(),
        })?;
        let graph = spawn_graph_controller(
            &myself,
            format!("{OBSERVER_NAME}.graph_controller"),
            &resources.config,
            StateSupervisorMessage::Graph,
        )
        .await?;

        Ok(StateSupervisorState {
            tcp_client: resources.tcp_client,
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            graph: Some(graph),
            graph_timeout,
            configuration: resources.configuration,
            floor: None,
            policy: PolicyLookup::for_service(OBSERVER_NAME),
            shutting_down: false,
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            StateSupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cassini_connected();
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => (),
            },
            // The graph does not end the agent: a read that fails is answered as unavailable.
            StateSupervisorMessage::Graph(signal) => {
                if let GraphSignal::OpFailed(reason) = &signal {
                    debug!("the graph did not answer: {reason}");
                }
                report_graph(&state.healthcheck, &signal);
            }
            StateSupervisorMessage::RuntimeChanged { section } => {
                if section == floor::SECTION_NAME {
                    match state.configuration.runtime(floor::SECTION_NAME).await {
                        Ok(Some(value)) => match Floor::from_body(&value.body) {
                            Ok(schema) => {
                                info!(
                                    version = value.version,
                                    workloads = schema.workloads.len(),
                                    readers = schema
                                        .workloads
                                        .iter()
                                        .filter(|e| !e.reads.is_empty())
                                        .count(),
                                    "the schema is loaded"
                                );
                                state.floor = Some(Arc::new(schema));
                            }
                            // The configuration actor judged it with the same function, so this is a disagreement
                            // between the two, not an author's mistake; the schema in force stays.
                            Err(problems) => error!(
                                version = value.version,
                                "the schema does not read, so the earlier one stays: {}",
                                problems.join("; ")
                            ),
                        },
                        Ok(None) => {}
                        Err(e) => warn!("could not read the schema: {e}"),
                    }
                }
            }
            StateSupervisorMessage::Request {
                payload,
                published_by,
            } => {
                if state.shutting_down {
                    return Ok(());
                }
                let Some(graph) = state.graph.clone() else {
                    return Ok(());
                };
                let envelope = match decode_request(&payload) {
                    Ok(envelope) => envelope,
                    Err(why) => {
                        warn!(
                            requester = published_by.as_deref().unwrap_or("unverified"),
                            "dropped a message on {REQUESTS_TOPIC}: {why}"
                        );
                        return Ok(());
                    }
                };
                let Some(topic) = reply_topic_for(published_by.as_deref()) else {
                    warn!(
                        requester = published_by.as_deref().unwrap_or("unverified"),
                        "a request with no workload to answer was dropped"
                    );
                    return Ok(());
                };
                let floor = state.floor.clone();
                let timeout = state.graph_timeout;
                let policy = state.policy.clone();
                let client = state.tcp_client.clone();
                tokio::spawn(async move {
                    let reply = answer(
                        floor.as_deref(),
                        &policy,
                        &graph,
                        timeout,
                        published_by.as_deref(),
                        &envelope.payload,
                    )
                    .await;
                    match &reply {
                        StateReply::Value(_) => {
                            debug!(requester = published_by.as_deref(), "answered a read")
                        }
                        StateReply::Refused(why) => {
                            warn!(requester = published_by.as_deref(), "refused a read: {why}")
                        }
                        StateReply::Unavailable(why) => {
                            info!(
                                requester = published_by.as_deref(),
                                "could not answer: {why}"
                            )
                        }
                    }
                    publish_reply(
                        &client,
                        topic,
                        &Envelope {
                            correlation_id: envelope.correlation_id,
                            payload: reply,
                        },
                    );
                });
            }
            StateSupervisorMessage::PrepareShutdown => {
                info!("PrepareShutdown received, stopping the graph controller");
                state.shutting_down = true;
                if let Some(graph) = state.graph.take() {
                    stop_graph_controller(graph, GRAPH_STOP).await;
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
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorStarted(actor) => debug!("{actor:?} started"),
            SupervisionEvent::ActorTerminated(actor, _, reason) => {
                // The infrastructure is this supervisor's linked child from build(); its ending is the end of the
                // shutdown handshake, or a failure. Either way exit non-zero, so the pod starts afresh.
                if actor.get_id() == state.infrastructure.get_id() {
                    info!("infrastructure is down, exiting");
                    std::process::exit(1);
                }
                if !state.shutting_down {
                    error!("{actor:?} stopped unexpectedly: {reason:?}");
                    std::process::exit(1);
                }
            }
            SupervisionEvent::ActorFailed(actor, reason) => {
                error!("{actor:?} failed: {reason:?}");
                if !state.shutting_down {
                    std::process::exit(1);
                }
            }
            _ => (),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::floor::Entry;
    use polar::graph::controller::GraphControllerMsg;

    /// A graph controller that cannot read: every query fails, as a graph that is down does.
    struct DownGraph;

    #[async_trait]
    impl Actor for DownGraph {
        type Msg = GraphControllerMsg;
        type State = ();
        type Arguments = ();
        async fn pre_start(&self, _: ActorRef<Self::Msg>, _: ()) -> Result<(), ActorProcessingErr> {
            Ok(())
        }
        async fn handle(
            &self,
            _: ActorRef<Self::Msg>,
            message: Self::Msg,
            _: &mut (),
        ) -> Result<(), ActorProcessingErr> {
            if let GraphControllerMsg::Query(_, reply) = message {
                let _ = reply.send(Err("connection refused".to_string()));
            }
            Ok(())
        }
    }

    fn id(workload: &str) -> String {
        format!("{workload}.polar.serviceaccount.cluster.local")
    }

    fn floor() -> Floor {
        Floor {
            workloads: vec![Entry {
                workload: "jira-observer".into(),
                identity: id("jira-observer"),
                owns: vec![],
                reads: vec!["JiraIssue".into()],
            }],
        }
    }

    const T: Duration = Duration::from_secs(5);

    fn max(label: &str) -> StateRequest {
        StateRequest::Max {
            label: label.into(),
            property: "updated_ms".into(),
        }
    }

    #[tokio::test]
    async fn a_refusal_never_reaches_the_graph_and_a_graph_that_is_down_is_unavailable() {
        let (graph, _) = Actor::spawn(None, DownGraph, ()).await.unwrap();
        // No policy actor is running in this test, so the floor decides.
        let policy = PolicyLookup::named("no-such-policy-actor");
        let asker = id("jira-observer");
        assert_eq!(
            answer(
                Some(&floor()),
                &policy,
                &graph,
                T,
                Some(&asker),
                &max("JiraIssue")
            )
            .await,
            StateReply::Unavailable("the graph could not be read".to_string())
        );
        assert!(matches!(
            answer(
                Some(&floor()),
                &policy,
                &graph,
                T,
                Some(&asker),
                &max("JiraUser")
            )
            .await,
            StateReply::Refused(_)
        ));
        assert!(matches!(
            answer(Some(&floor()), &policy, &graph, T, None, &max("JiraIssue")).await,
            StateReply::Refused(ref why) if why.contains("no verified asker")
        ));
        // Before a schema has arrived, nothing is decided: ask again.
        assert!(matches!(
            answer(None, &policy, &graph, T, Some(&asker), &max("JiraIssue")).await,
            StateReply::Unavailable(ref why) if why.contains("no schema yet")
        ));
        graph.stop(None);
    }

    /// Records what the health actor is told.
    #[derive(Default)]
    struct Told(std::sync::Mutex<Vec<&'static str>>);

    impl HealthClient for Told {
        fn cassini_connected(
            &self,
        ) -> Result<(), ractor::MessagingErr<polar_health::HealthCheckMessage>> {
            self.0.lock().unwrap().push("cassini_connected");
            Ok(())
        }
        fn cassini_disconnected(
            &self,
        ) -> Result<(), ractor::MessagingErr<polar_health::HealthCheckMessage>> {
            self.0.lock().unwrap().push("cassini_disconnected");
            Ok(())
        }
        fn graph_connected(
            &self,
        ) -> Result<(), ractor::MessagingErr<polar_health::HealthCheckMessage>> {
            self.0.lock().unwrap().push("graph_connected");
            Ok(())
        }
        fn graph_disconnected(
            &self,
        ) -> Result<(), ractor::MessagingErr<polar_health::HealthCheckMessage>> {
            self.0.lock().unwrap().push("graph_disconnected");
            Ok(())
        }
        fn graph_available(
            &self,
        ) -> Result<(), ractor::MessagingErr<polar_health::HealthCheckMessage>> {
            self.0.lock().unwrap().push("graph_available");
            Ok(())
        }
        fn graph_op_failed(
            &self,
            _reason: String,
        ) -> Result<(), ractor::MessagingErr<polar_health::HealthCheckMessage>> {
            self.0.lock().unwrap().push("graph_op_failed");
            Ok(())
        }
    }

    #[test]
    fn a_graph_that_is_down_is_reported_as_not_connected_never_as_a_failure() {
        let told = Told::default();
        report_graph(&told, &GraphSignal::OpFailed("refused".into()));
        report_graph(&told, &GraphSignal::Available);
        // Never graph_op_failed, which writes the health file unhealthy.
        assert_eq!(
            *told.0.lock().unwrap(),
            ["graph_disconnected", "graph_available"]
        );
    }

    #[test]
    fn each_answer_goes_to_its_asker_s_own_reply_topic() {
        assert_eq!(
            reply_topic_for(Some(&id("jira-observer"))).as_deref(),
            Some("soft-state-replies.jira-observer")
        );
        assert_eq!(reply_topic_for(None), None);
        // A person's identity names no workload, so there is nowhere to answer.
        assert_eq!(reply_topic_for(Some("2f0c6a1e-alice")), None);
    }
}
