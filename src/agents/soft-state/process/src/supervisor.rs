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

//! The writer's supervisor, on the agent builder: health, the Cassini client and the shutdown handshake come from
//! `AgentBuilder`. On its interval it opens a round: one question, with the round's correlation id, on the topic of each
//! expected workload in its deployed family. The answers it hears on `polar-health-replies` count for the round only
//! when they carry its id and are stamped with an expected identity. When the round's timeout passes it classifies
//! every expected workload ([`crate::classify`]), records the transitions ([`crate::history`]), and writes the round
//! in one graph operation ([`crate::graph`]).
//!
//! The histories are read back from the graph before the first round, so a restart keeps them; until they are, no round
//! is written over them.

use crate::classify::{self, Observation};
use crate::expected::{self, Entry, Expected};
use crate::graph;
use crate::history::History;
use crate::settings::Timing;
use cassini_client::{OfflineBehavior, PublishRequest};
use cassini_types::ClientEvent;
use polar::cassini::{CassiniClient, TcpClient};
use polar::graph::controller::{GraphController, GraphControllerMsg, GraphSignal};
use polar_agent_builder::{
    Access, AgentBuilder, ConfigurationHandle, Configured, HealthClient, HealthHandle,
    InfrastructureEvent, InfrastructureSupervisorMessage, Subscription, TopicSet,
    spawn_graph_controller, stop_graph_controller,
};
use polar_health::HealthReport;
use polar_health::report::{self, HEALTH_REPLIES_TOPIC, HealthAnswer, HealthQuery};
use ractor::concurrency::Duration;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use soft_state_common::PROCESSOR_NAME;
use std::collections::BTreeMap;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// What it holds is a round in flight at most, and one graph write.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(20);

/// How long the graph controller is given to stop.
const GRAPH_STOP: Duration = Duration::from_secs(10);

/// How long reading the histories back may take.
const READ_BACK_TIMEOUT: Duration = Duration::from_secs(10);

pub enum WriterMessage {
    ClientEvent {
        event: ClientEvent,
    },
    /// An answer, with the identity the broker stamped on it.
    Answer {
        payload: Vec<u8>,
        published_by: Option<String>,
    },
    /// Time for a round.
    Ask,
    /// The round numbered so has waited its timeout.
    Close {
        round: u64,
    },
    Graph(GraphSignal),
    RuntimeChanged {
        section: String,
    },
    /// The histories read back from the graph, or why they could not be.
    Loaded(Result<BTreeMap<String, History>, String>),
    PrepareShutdown,
}

/// A round in flight: its number and correlation id, and the answers heard for it, by the identity that gave each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Round {
    pub number: u64,
    pub id: String,
    pub answers: BTreeMap<String, HealthReport>,
}

impl Round {
    pub fn new(number: u64, id: String) -> Self {
        Self {
            number,
            id,
            answers: BTreeMap::new(),
        }
    }

    /// Takes `answer` into the round if it is the round's (its correlation id) and was stamped with an expected
    /// identity; anything else counts for nothing. Returns whether it was taken.
    pub fn accept(
        &mut self,
        expected: &Expected,
        answer: HealthAnswer,
        published_by: Option<&str>,
    ) -> bool {
        let Some(identity) = published_by else {
            return false;
        };
        if answer.correlation_id != self.id || expected.by_identity(identity).is_none() {
            return false;
        }
        self.answers.insert(identity.to_string(), answer.report);
        true
    }
}

/// Closes a round at `at_ms`: what it found for every expected workload, the histories brought up to date, and the
/// graph's write.
pub fn close(
    round: &Round,
    expected: &Expected,
    reachable: impl Fn(&str) -> bool,
    histories: &mut BTreeMap<String, History>,
    at_ms: i64,
) -> Vec<(Entry, Observation)> {
    let found = classify::classify(expected, reachable, &round.answers);
    for (entry, observation) in &found {
        histories.entry(entry.workload.clone()).or_default().record(
            at_ms,
            observation.state(),
            observation.reason(),
        );
    }
    found
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub struct WriterSupervisor;

pub struct WriterState {
    tcp_client: TcpClient,
    healthcheck: HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    graph: Option<GraphController>,
    configuration: ConfigurationHandle,
    /// The deployed family: a workload outside it cannot be asked.
    topics: Arc<TopicSet>,
    timing: Timing,
    /// The `agents` section in force; `None` until a version has arrived.
    expected: Option<Expected>,
    round: Option<Round>,
    next_round: u64,
    /// The histories, once read back; `None` until then.
    histories: Option<BTreeMap<String, History>>,
    loading: bool,
    shutting_down: bool,
}

impl WriterSupervisor {
    fn reachable(topics: &TopicSet) -> impl Fn(&str) -> bool + '_ {
        |workload| topics.allows(&report::health_topic(workload), Access::Publish)
    }

    fn load(myself: &ActorRef<WriterMessage>, state: &mut WriterState) {
        let Some(graph) = state.graph.clone() else {
            return;
        };
        state.loading = true;
        let me = myself.clone();
        tokio::spawn(async move {
            let result = match ractor::rpc::call(
                &graph,
                |reply| {
                    GraphControllerMsg::Query(neo4rs::Query::new(graph::READ_BACK.into()), reply)
                },
                Some(READ_BACK_TIMEOUT),
            )
            .await
            {
                Ok(ractor::rpc::CallResult::Success(Ok(rows))) => Ok(graph::histories_of(&rows)),
                Ok(ractor::rpc::CallResult::Success(Err(e))) => Err(e),
                Ok(ractor::rpc::CallResult::Timeout) => {
                    Err("the graph did not answer in time".to_string())
                }
                Ok(ractor::rpc::CallResult::SenderError) | Err(_) => {
                    Err("the graph controller is not running".to_string())
                }
            };
            let _ = me.cast(WriterMessage::Loaded(result));
        });
    }

    fn ask(myself: &ActorRef<WriterMessage>, state: &mut WriterState) {
        if state.histories.is_none() {
            if !state.loading {
                Self::load(myself, state);
            }
            debug!("the histories are not read back yet; no round");
            return;
        }
        let Some(expected) = state.expected.clone() else {
            debug!("no version of `agents` has arrived yet; no one is asked");
            return;
        };
        if let Some(open) = state.round.take() {
            Self::finish(state, &open);
        }
        state.next_round += 1;
        let number = state.next_round;
        let id = format!("{PROCESSOR_NAME}-{}-{number}", now_ms());
        let reachable = Self::reachable(&state.topics);
        let asked = classify::asked(&expected, &reachable);
        let payload = report::encode_query(&HealthQuery {
            correlation_id: id.clone(),
        });
        for entry in &asked {
            let topic = report::health_topic(&entry.workload);
            if let Err(e) = state.tcp_client.publish(PublishRequest {
                topic: topic.clone(),
                payload: payload.clone(),
                trace_ctx: None,
                // A question that cannot go now would be answered too late for its round.
                offline_behavior: OfflineBehavior::Drop,
                share_identity: true,
            }) {
                warn!("could not ask on {topic}: {e:?}");
            }
        }
        debug!(round = number, asked = asked.len(), "asked about health");
        state.round = Some(Round::new(number, id));
        drop(
            myself.send_after(state.timing.timeout, move || WriterMessage::Close {
                round: number,
            }),
        );
    }

    fn finish(state: &mut WriterState, round: &Round) {
        let (Some(expected), Some(histories), Some(graph)) =
            (&state.expected, state.histories.as_mut(), &state.graph)
        else {
            return;
        };
        let at_ms = now_ms();
        let found = close(
            round,
            expected,
            Self::reachable(&state.topics),
            histories,
            at_ms,
        );
        let mut items = Vec::new();
        for (entry, observation) in &found {
            let history = histories.get(&entry.workload).cloned().unwrap_or_default();
            match graph::node_of(entry, observation, &history, at_ms) {
                Ok(item) => items.push(item),
                Err(why) => warn!("{why}"),
            }
        }
        let count = |state: &str| found.iter().filter(|(_, o)| o.state() == state).count();
        let unreachable: Vec<&str> = found
            .iter()
            .filter(|(_, o)| matches!(o, Observation::Unreachable))
            .map(|(e, _)| e.workload.as_str())
            .collect();
        if !unreachable.is_empty() {
            warn!(
                workloads = unreachable.join(","),
                "named by `agents` but outside the family this writer was deployed with: they cannot be asked (drift)"
            );
        }
        info!(
            round = round.number,
            answering = found.len() - count("unresponsive") - count("unreachable"),
            unresponsive = count("unresponsive"),
            unreachable = unreachable.len(),
            "a health round closed"
        );
        if !items.is_empty()
            && let Err(e) = graph.cast(GraphControllerMsg::Op(graph::round_op(items)))
        {
            warn!("the round could not be handed to the graph controller: {e}");
        }
    }
}

#[async_trait]
impl Actor for WriterSupervisor {
    type Msg = WriterMessage;
    type State = WriterState;
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        // Judged with the rest of the configuration before the agent started, so it reads.
        let timing = Timing::from_env(&**configured.config()).map_err(ActorProcessingErr::from)?;
        let topics = configured.topics().clone();

        let deliver_to = myself.clone();
        let answers = Subscription::new(HEALTH_REPLIES_TOPIC, move |incoming| {
            if let Err(e) = deliver_to.cast(WriterMessage::Answer {
                payload: incoming.payload,
                published_by: incoming.published_by,
            }) {
                warn!("failed to hand an answer to the supervisor: {e}");
            }
        });
        let resources = AgentBuilder::new(myself.clone(), PROCESSOR_NAME)
            .with_subscription(answers)
            .with_lifecycle_handler(|event| Some(WriterMessage::ClientEvent { event }))
            .into_configured(configured)
            .build()
            .await?;
        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => Some(WriterMessage::PrepareShutdown),
            });
        resources
            .configuration
            .runtime_changes()
            .subscribe(myself.clone(), |changed| {
                Some(WriterMessage::RuntimeChanged {
                    section: changed.section,
                })
            });
        // A section already held (the replay was quick) is read now.
        myself.cast(WriterMessage::RuntimeChanged {
            section: expected::SECTION_NAME.to_string(),
        })?;
        let graph = spawn_graph_controller(
            &myself,
            format!("{PROCESSOR_NAME}.graph_controller"),
            &resources.config,
            WriterMessage::Graph,
        )
        .await?;
        // The one timer: the writer's rounds. No agent it asks keeps one.
        drop(myself.send_interval(timing.interval, || WriterMessage::Ask));

        let mut state = WriterState {
            tcp_client: resources.tcp_client,
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            graph: Some(graph),
            configuration: resources.configuration,
            topics,
            timing,
            expected: None,
            round: None,
            next_round: 0,
            histories: None,
            loading: false,
            shutting_down: false,
        };
        Self::load(&myself, &mut state);
        Ok(state)
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            WriterMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cassini_connected();
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => (),
            },
            WriterMessage::Graph(signal) => {
                let _ = match signal {
                    GraphSignal::Available => state.healthcheck.graph_available(),
                    GraphSignal::OpFailed(reason) => state.healthcheck.graph_op_failed(reason),
                };
            }
            WriterMessage::RuntimeChanged { section } => {
                if section == expected::SECTION_NAME {
                    match state.configuration.runtime(expected::SECTION_NAME).await {
                        Ok(Some(value)) => match Expected::from_body(&value.body) {
                            Ok(agents) => {
                                info!(
                                    version = value.version,
                                    workloads = agents.workloads.len(),
                                    "the agents to ask are loaded"
                                );
                                state.expected = Some(agents);
                            }
                            // The configuration actor judged it with the same function, so this is a disagreement
                            // between the two, not an author's mistake; the section in force stays.
                            Err(problems) => error!(
                                version = value.version,
                                "`agents` does not read, so the earlier one stays: {}",
                                problems.join("; ")
                            ),
                        },
                        Ok(None) => {}
                        Err(e) => warn!("could not read `agents`: {e}"),
                    }
                }
            }
            WriterMessage::Loaded(result) => {
                state.loading = false;
                match result {
                    Ok(histories) => {
                        info!(
                            workloads = histories.len(),
                            "the health histories are read back"
                        );
                        state.histories = Some(histories);
                    }
                    Err(why) => {
                        warn!(
                            "the health histories could not be read back, trying again at the next round: {why}"
                        )
                    }
                }
            }
            WriterMessage::Ask => {
                if !state.shutting_down {
                    Self::ask(&myself, state);
                }
            }
            WriterMessage::Answer {
                payload,
                published_by,
            } => {
                let answer = match report::decode_answer(&payload) {
                    Ok(answer) => answer,
                    Err(why) => {
                        warn!(
                            publisher = published_by.as_deref().unwrap_or("unverified"),
                            "dropped a message on {HEALTH_REPLIES_TOPIC}: {why}"
                        );
                        return Ok(());
                    }
                };
                if let (Some(round), Some(expected)) = (state.round.as_mut(), &state.expected)
                    && !round.accept(expected, answer, published_by.as_deref())
                {
                    debug!(
                        publisher = published_by.as_deref().unwrap_or("unverified"),
                        "an answer that is not this round's, or not an expected workload's"
                    );
                }
            }
            WriterMessage::Close { round } => {
                if state.round.as_ref().is_some_and(|r| r.number == round)
                    && let Some(open) = state.round.take()
                {
                    Self::finish(state, &open);
                }
            }
            WriterMessage::PrepareShutdown => {
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
    use polar_health::{HealthCounters, HealthPhase, ReportStatus};

    fn id(workload: &str) -> String {
        format!("{workload}.polar.serviceaccount.cluster.local")
    }

    fn expected(names: &[&str]) -> Expected {
        Expected {
            workloads: names
                .iter()
                .map(|n| Entry {
                    workload: n.to_string(),
                    identity: id(n),
                })
                .collect(),
        }
    }

    fn answer(round: &str, status: ReportStatus) -> HealthAnswer {
        HealthAnswer {
            correlation_id: round.to_string(),
            report: HealthReport {
                status,
                phase: HealthPhase::Normal,
                reason: None,
                message: None,
                cassini_connected: true,
                graph_connected: false,
                expects_graph: false,
                counters: HealthCounters::default(),
                uptime_secs: 1,
            },
        }
    }

    #[test]
    fn a_round_takes_only_its_own_answers_from_expected_identities() {
        let agents = expected(&["a", "b"]);
        let mut round = Round::new(1, "r-1".into());
        assert!(round.accept(
            &agents,
            answer("r-1", ReportStatus::Healthy),
            Some(&id("a"))
        ));
        // Another round's, an unexpected identity's, and one the broker vouched for no one.
        assert!(!round.accept(
            &agents,
            answer("r-0", ReportStatus::Healthy),
            Some(&id("b"))
        ));
        assert!(!round.accept(
            &agents,
            answer("r-1", ReportStatus::Healthy),
            Some(&id("c"))
        ));
        assert!(!round.accept(&agents, answer("r-1", ReportStatus::Healthy), None));
        assert_eq!(round.answers.len(), 1);
    }

    #[test]
    fn closing_a_round_records_a_transition_only_when_what_it_found_changed() {
        let agents = expected(&["a", "b", "gone"]);
        let reachable = |w: &str| w != "gone";
        let mut histories = BTreeMap::new();
        let mut round = Round::new(1, "r-1".into());
        round.accept(
            &agents,
            answer("r-1", ReportStatus::Healthy),
            Some(&id("a")),
        );
        let found = close(&round, &agents, reachable, &mut histories, 10);
        assert_eq!(
            found.iter().map(|(_, o)| o.state()).collect::<Vec<_>>(),
            ["healthy", "unresponsive", "unreachable"]
        );
        // The same again changes no history; a new answer from b does.
        close(&round, &agents, reachable, &mut histories, 20);
        let mut next = Round::new(2, "r-2".into());
        next.accept(
            &agents,
            answer("r-2", ReportStatus::Healthy),
            Some(&id("a")),
        );
        next.accept(
            &agents,
            answer("r-2", ReportStatus::Degraded),
            Some(&id("b")),
        );
        close(&next, &agents, reachable, &mut histories, 30);
        assert_eq!(histories["a"].len(), 1);
        let b: Vec<&str> = histories["b"]
            .transitions()
            .map(|t| t.state.as_str())
            .collect();
        assert_eq!(b, ["unresponsive", "degraded"]);
        assert_eq!(histories["gone"].len(), 1);
    }
}
