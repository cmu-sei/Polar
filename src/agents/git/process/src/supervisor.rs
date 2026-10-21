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

//! The git processor's supervisor, on the agent builder: health check, router, the Cassini client
//! and the subscription come from `AgentBuilder`, the graph controller from the shared helper.
//!
//! Each event the observer publishes is turned into graph operations (`ops`) and handed to the
//! controller. A failure of the controller starts a drain window and then ends the process, and the
//! Job starts a fresh one. Shutdown follows the same order as every graph-writing agent: the drain
//! window, the controller stopped waiting for a write in flight, then `ProceedWithShutdown`.

use crate::SERVICE_NAME;
use crate::ops;
use cassini_types::ClientEvent;
use git_agent_common::GitRepositoryMessage;
use git_agent_common::proposal::{PROPOSAL_DECISIONS_TOPIC, ProposalDecision};
use git_agent_common::status::{GIT_REPO_OBSERVATION_STATUS, ObservationStatus};
use polar::graph::controller::{GraphControllerMsg, GraphSignal};
use polar::topics::GIT_REPOSITORY_EVENTS;
use polar_agent_builder::{
    AgentBuilder, Configured, Drain, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage, Subscription, spawn_graph_controller, stop_graph_controller,
};
use ractor::concurrency::Duration;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use tracing::{debug, error, info, warn};

const DRAIN_WINDOW_SECS: u64 = 5;
const GRAPH_STOP_SECS: u64 = 30;

/// How long the infrastructure supervisor gives this agent to drain: the drain window and the
/// graph controller's stop, plus a margin. Past it the process exits 1 regardless.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(DRAIN_WINDOW_SECS + GRAPH_STOP_SECS + 10);

fn drain_window() -> Duration {
    Duration::from_secs(DRAIN_WINDOW_SECS)
}

pub enum GitProcessorMessage {
    ClientEvent {
        event: ClientEvent,
    },
    /// An event the observer published, decoded.
    Event(GitRepositoryMessage),
    /// What the observer says became of its latest attempt at a repository, decoded.
    Status(ObservationStatus),
    /// What the git controller decided about a proposal, decoded.
    Decision(ProposalDecision),
    GraphSignal(GraphSignal),
    /// The health check ended the pod's life; the infrastructure waits for
    /// `ProceedWithShutdown`.
    PrepareShutdown,
    /// The drain window has elapsed: after a failure this exits, after `PrepareShutdown` it
    /// flushes the graph controller and completes the handshake.
    DrainElapsed,
}

/// What was published on the events topic. Anything that does not decode is logged and dropped.
pub fn decode(payload: &[u8]) -> Option<GitRepositoryMessage> {
    match rkyv::from_bytes::<GitRepositoryMessage, rkyv::rancor::Error>(payload) {
        Ok(event) => Some(event),
        Err(e) => {
            warn!("Failed to deserialize a git repository event: {e}");
            None
        }
    }
}

/// What was published on the status topic. Anything that does not decode is logged and dropped.
pub fn decode_status(payload: &[u8]) -> Option<ObservationStatus> {
    match rkyv::from_bytes::<ObservationStatus, rkyv::rancor::Error>(payload) {
        Ok(status) => Some(status),
        Err(e) => {
            warn!("Failed to deserialize an observation status: {e}");
            None
        }
    }
}

/// What was published on the decisions topic. Anything that does not decode is logged and dropped. The bytes are
/// copied to where rkyv wants them first.
pub fn decode_decision(payload: &[u8]) -> Option<ProposalDecision> {
    let mut aligned = rkyv::util::AlignedVec::<16>::new();
    aligned.extend_from_slice(payload);
    match rkyv::from_bytes::<ProposalDecision, rkyv::rancor::Error>(&aligned) {
        Ok(decision) => Some(decision),
        Err(e) => {
            warn!("Failed to deserialize a proposal decision: {e}");
            None
        }
    }
}

pub fn subscriptions(supervisor: &ActorRef<GitProcessorMessage>) -> Vec<Subscription> {
    let events = supervisor.clone();
    let statuses = supervisor.clone();
    let decisions = supervisor.clone();
    vec![
        Subscription::new(GIT_REPOSITORY_EVENTS, move |incoming| {
            if let Some(event) = decode(&incoming.payload)
                && let Err(e) = events.cast(GitProcessorMessage::Event(event))
            {
                error!("failed to hand an event to the supervisor: {e}");
            }
        }),
        Subscription::new(GIT_REPO_OBSERVATION_STATUS, move |incoming| {
            if let Some(status) = decode_status(&incoming.payload)
                && let Err(e) = statuses.cast(GitProcessorMessage::Status(status))
            {
                error!("failed to hand a status to the supervisor: {e}");
            }
        }),
        Subscription::new(PROPOSAL_DECISIONS_TOPIC, move |incoming| {
            if let Some(decision) = decode_decision(&incoming.payload)
                && let Err(e) = decisions.cast(GitProcessorMessage::Decision(decision))
            {
                error!("failed to hand a decision to the supervisor: {e}");
            }
        }),
    ]
}

pub struct GitProcessorSupervisor;

pub struct GitProcessorState {
    healthcheck: HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    graph_controller: Option<ActorRef<GraphControllerMsg>>,
    drain: Drain,
}

#[async_trait]
impl Actor for GitProcessorSupervisor {
    type Msg = GitProcessorMessage;
    type State = GitProcessorState;
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        let mut builder = AgentBuilder::new(myself.clone(), SERVICE_NAME)
            .with_lifecycle_handler(|event| Some(GitProcessorMessage::ClientEvent { event }));
        for subscription in subscriptions(&myself) {
            builder = builder.with_subscription(subscription);
        }
        let resources = builder.into_configured(configured).build().await?;

        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => Some(GitProcessorMessage::PrepareShutdown),
            });

        let graph_controller = spawn_graph_controller(
            &myself,
            format!("{SERVICE_NAME}.graph_controller"),
            &resources.config,
            GitProcessorMessage::GraphSignal,
        )
        .await?;

        Ok(GitProcessorState {
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            graph_controller: Some(graph_controller),
            drain: Drain::default(),
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            GitProcessorMessage::GraphSignal(signal) => match signal {
                GraphSignal::Available => {
                    let _ = state.healthcheck.graph_available();
                }
                GraphSignal::OpFailed(reason) => {
                    let _ = state.healthcheck.graph_op_failed(reason);
                }
            },
            GitProcessorMessage::ClientEvent { event } => match event {
                // Fires on every registration, a reconnect included, and the builder has already
                // subscribed to the topic, so it only reports health. The graph controller is
                // started once, in pre_start: starting it here made the second registration fail
                // on the name the first had taken.
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cassini_connected();
                    let _ = state.healthcheck.graph_connected();
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => {}
            },
            GitProcessorMessage::Event(event) => {
                if state.drain.is_draining() {
                    warn!("draining -- an event will be lost on exit");
                } else if let Some(controller) = &state.graph_controller {
                    for op in ops::for_event(event) {
                        controller.cast(GraphControllerMsg::Op(op))?;
                    }
                }
            }
            GitProcessorMessage::Status(status) => {
                if state.drain.is_draining() {
                    warn!("draining -- a status will be lost on exit");
                } else if let Some(controller) = &state.graph_controller {
                    for op in ops::for_status(status) {
                        controller.cast(GraphControllerMsg::Op(op))?;
                    }
                }
            }
            GitProcessorMessage::Decision(decision) => {
                if state.drain.is_draining() {
                    warn!("draining -- a decision will be lost on exit");
                } else if let Some(controller) = &state.graph_controller {
                    for op in ops::for_decision(decision) {
                        controller.cast(GraphControllerMsg::Op(op))?;
                    }
                }
            }
            GitProcessorMessage::PrepareShutdown => {
                if state.drain.begin(&myself, drain_window(), true, || {
                    GitProcessorMessage::DrainElapsed
                }) {
                    info!("PrepareShutdown received (rejuvenation); entering the drain window");
                } else {
                    warn!(
                        "PrepareShutdown while already draining; the pending exit ends the process"
                    );
                }
            }
            GitProcessorMessage::DrainElapsed => {
                warn!("drain window elapsed, beginning shutdown");
                if let Some(controller) = state.graph_controller.take() {
                    stop_graph_controller(controller, Duration::from_secs(GRAPH_STOP_SECS)).await;
                }
                state.drain.complete(&state.infrastructure);
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorTerminated(actor, _, reason) => {
                // The infrastructure supervisor ending is the end of the handshake, or
                // infrastructure dying on its own; either way the process exits so the Job
                // starts a pod with fresh certificates.
                if actor.get_id() == state.infrastructure.get_id() {
                    info!("infrastructure terminated ({reason:?}), exiting");
                    std::process::exit(1);
                }
                warn!("Actor {:?} terminated: {reason:?}", actor.get_name());
            }
            SupervisionEvent::ActorFailed(actor, err) => {
                error!("Actor {:?} failed: {err:?}", actor.get_name());
                if state.drain.begin(&myself, drain_window(), false, || {
                    GitProcessorMessage::DrainElapsed
                }) {
                    warn!("entering drain window before exit");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_agent_common::GitRepositoryMessage;
    use polar::graph::nodes::git::RepoId;

    #[test]
    fn an_event_the_observer_published_decodes() {
        let event = GitRepositoryMessage::RefUpdated {
            repo: RepoId::new("r".into()),
            ref_name: "refs/heads/main".into(),
            old: None,
            new: "b".into(),
            observed_at: "1".into(),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&event).unwrap();
        assert!(matches!(
            decode(&bytes),
            Some(GitRepositoryMessage::RefUpdated { .. })
        ));
    }

    #[test]
    fn bytes_that_are_not_an_event_are_dropped() {
        assert!(decode(b"not an event").is_none());
    }

    #[test]
    fn a_status_the_observer_published_decodes() {
        use git_agent_common::status::ObservationState;
        let status = ObservationStatus {
            repo_id: RepoId::new("r".into()),
            state: ObservationState::PendingCredential,
            reason: None,
            last_success: None,
            tips: vec![],
            consecutive_failures: 0,
            observed_at: 5,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&status).unwrap();
        assert_eq!(decode_status(&bytes), Some(status));
    }

    #[test]
    fn bytes_that_are_not_a_status_are_dropped() {
        assert!(decode_status(b"not a status").is_none());
    }

    /// An actor that takes the processor's messages and does nothing with them, to have a reference to hand to
    /// `subscriptions`.
    struct NullActor;

    #[async_trait]
    impl Actor for NullActor {
        type Msg = GitProcessorMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(&self, _: ActorRef<Self::Msg>, _: ()) -> Result<(), ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _: ActorRef<Self::Msg>,
            _: Self::Msg,
            _: &mut (),
        ) -> Result<(), ActorProcessingErr> {
            Ok(())
        }
    }

    fn a_decision() -> git_agent_common::proposal::ProposalDecision {
        use git_agent_common::proposal::{Evidence, ProposalDecision, ProposalState};
        ProposalDecision {
            repo_id: Some(RepoId::new("r".into())),
            url: "https://github.com/o/r".into(),
            proposer: Some("jira".into()),
            state: ProposalState::Accepted,
            rule: None,
            reason: "accepted".into(),
            evidence: vec![Evidence {
                source: "jira".into(),
                reference: None,
            }],
            first_seen: 1,
            last_seen: 2,
            announcements: 3,
            decided_at: 4,
            held: true,
        }
    }

    #[test]
    fn a_decision_the_controller_published_decodes() {
        let decision = a_decision();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&decision).unwrap();
        assert_eq!(decode_decision(&bytes), Some(decision.clone()));
        let mut shifted = vec![0u8];
        shifted.extend_from_slice(&bytes);
        assert_eq!(decode_decision(&shifted[1..]), Some(decision));
    }

    #[test]
    fn bytes_that_are_not_a_decision_are_dropped() {
        assert!(decode_decision(b"not a decision").is_none());
        assert!(decode_decision(&[]).is_none());
    }

    #[tokio::test]
    async fn the_processor_listens_for_events_and_for_statuses() {
        // Both topics, by name: a subscription that is missing is a feature that silently does nothing.
        let (supervisor, _) = Actor::spawn(None, NullActor, ()).await.unwrap();
        let topics: Vec<String> = subscriptions(&supervisor)
            .into_iter()
            .map(|s| s.topic)
            .collect();
        assert!(
            topics.contains(&GIT_REPOSITORY_EVENTS.to_string()),
            "{topics:?}"
        );
        assert!(
            topics.contains(&GIT_REPO_OBSERVATION_STATUS.to_string()),
            "{topics:?}"
        );
        assert!(
            topics.contains(&git_agent_common::proposal::PROPOSAL_DECISIONS_TOPIC.to_string()),
            "{topics:?}"
        );
        supervisor.stop(None);
    }

    #[tokio::test]
    async fn every_topic_it_subscribes_to_is_in_its_declared_set() {
        let (supervisor, _) = Actor::spawn(None, NullActor, ()).await.unwrap();
        let set = polar_agent_builder::agent::topic_set::<crate::agent::GitProcessor>();
        for subscription in subscriptions(&supervisor) {
            assert!(
                set.allows(&subscription.topic, polar_agent_builder::Access::Subscribe),
                "{} is not declared",
                subscription.topic
            );
        }
        supervisor.stop(None);
    }
}
