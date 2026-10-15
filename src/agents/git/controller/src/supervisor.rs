//! The git controller's supervisor, on the agent builder: health, the Cassini client and the shutdown
//! handshake come from `AgentBuilder`. It reads two runtime sections the builder's configuration actor verifies
//! and holds (`repositories` and `admission`), and subscribes to the proposals other agents publish. What it
//! decides is in [`Control`]; this does the input and output around it.
//!
//! It announces the whole set on `git.repo.config.events` when it starts, when anything that changes the set
//! changes, and on a timer. Cassini keeps nothing for a subscriber that is not there yet, so an observer that
//! starts later would otherwise never hear; the observer ignores an announcement it has already acted on.

use crate::admission::{self, Decision, Resolution, Rules};
use crate::control::Control;
use crate::desired;
use crate::proposals::Key;
use crate::settings;
use cassini_types::ClientEvent;
use git_agent_common::proposal::{
    PROPOSAL_DECISIONS_TOPIC, PROPOSAL_OBSERVE_REPO_TOPIC, Proposal, ProposalDecision,
};
use git_agent_common::{
    GIT_REPO_CONFIG_EVENTS, RepoObservationConfig, RepositorySet, publish_event,
};
use polar::cassini::TcpClient;
use polar_agent_builder::{
    AgentBuilder, ConfigurationHandle, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage, Subscription,
};
use ractor::concurrency::Duration;
use ractor::{Actor, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use tracing::{debug, error, info, warn};

/// Nothing is held that needs draining, so this is the margin for the handshake alone.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(10);

/// The longest the judging of every claim, name lookups included, is waited for. A judging that takes longer is
/// abandoned and the next tick tries again.
const JUDGE_TIMEOUT: Duration = Duration::from_secs(60);

pub enum RootSupervisorMessage {
    /// Lifecycle events from the Cassini client.
    ClientEvent { event: ClientEvent },
    /// A runtime section was replaced.
    RuntimeChanged { section: String },
    /// An agent proposed a repository. `proposer` is what the broker said, `None` if it said nothing.
    Proposed {
        proposer: Option<String>,
        proposal: Proposal,
    },
    /// Judge every claim now (unless a judging is already under way, which then runs again when it is done).
    Judge,
    /// A judging finished; `None` if it failed or ran out of time.
    Judged(Option<Vec<(Key, Decision)>>),
    /// Time to expire what is stale, judge again and announce the set again.
    Republish,
    /// The infrastructure has decided the agent should end.
    PrepareShutdown,
}

/// Announces the whole set in one message: all of it or none of it, so that an observer which gets it knows what
/// is no longer wanted. `complete` says whether the observer may stop the workers the set does not name.
/// Returns whether it was published; one that cannot be is logged, and the next announcement tries again.
pub fn announce(client: &TcpClient, configs: &[RepoObservationConfig], complete: bool) -> bool {
    let set = RepositorySet {
        repositories: configs.to_vec(),
        complete,
    };
    match publish_event(client, GIT_REPO_CONFIG_EVENTS, &set) {
        Ok(()) => true,
        Err(e) => {
            warn!(
                "could not announce the set of {} repositories: {e}",
                configs.len()
            );
            false
        }
    }
}

/// What a proposal published on the proposals topic was, or `None` for bytes that are not one. Never fatal.
pub fn decode_proposal(payload: &[u8]) -> Option<Proposal> {
    // The bytes may not sit where rkyv wants them to, so they are copied to where it does.
    let mut aligned = rkyv::util::AlignedVec::<16>::new();
    aligned.extend_from_slice(payload);
    match rkyv::from_bytes::<Proposal, rkyv::rancor::Error>(&aligned) {
        Ok(proposal) => Some(proposal),
        Err(e) => {
            warn!("Failed to deserialize a proposal: {e}");
            None
        }
    }
}

/// Says what was decided about a proposal. One that cannot be published is logged and not retried: the decision
/// is made again when the proposal is announced again.
pub fn publish_decision(client: &TcpClient, decision: &ProposalDecision) {
    info!(
        state = decision.state.as_str(),
        rule = decision.rule.as_deref().unwrap_or("-"),
        proposer = decision.proposer.as_deref().unwrap_or("-"),
        "proposal of {}: {}",
        decision.url,
        decision.reason
    );
    if let Err(e) = publish_event(client, PROPOSAL_DECISIONS_TOPIC, decision) {
        warn!("could not publish a decision about {}: {e}", decision.url);
    }
}

/// What a host name resolves to, asked of the system resolver. Blocks, so it is only called from a blocking thread.
pub fn resolve_system(host: &str) -> Resolution {
    use std::net::ToSocketAddrs;
    (host, 443)
        .to_socket_addrs()
        .map(|addresses| addresses.map(|a| a.ip()).collect())
        .map_err(|e| e.to_string())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

pub struct RootSupervisor;

pub struct RootSupervisorState {
    tcp_client: TcpClient,
    healthcheck: HealthHandle,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    configuration: ConfigurationHandle,
    control: Control,
    /// A judging is under way.
    judging: bool,
    /// Something changed while it was, so it runs again when it is done.
    judge_again: bool,
    /// The last set announced, so that a change is announced at once and not at the next tick.
    last_announced: Option<(Vec<RepoObservationConfig>, bool)>,
    shutting_down: bool,
}

impl RootSupervisor {
    /// Reads the latest `repositories` section. A body that does not read keeps the set that was held: the
    /// builder judged it before accepting it, so this is a disagreement between the two, not an author's
    /// mistake.
    async fn load_repositories(state: &mut RootSupervisorState) {
        let value = match state.configuration.runtime(desired::SECTION_NAME).await {
            Ok(Some(value)) => value,
            Ok(None) => return,
            Err(e) => {
                warn!("could not read the repositories section: {e}");
                return;
            }
        };
        match desired::parse(&value.body) {
            Ok(configs) => {
                info!(
                    version = value.version,
                    repositories = configs.len(),
                    "repositories section loaded"
                );
                state.control.set_explicit(configs);
            }
            Err(problems) => error!(
                version = value.version,
                "the repositories section does not read, so the earlier set stays: {}",
                problems.join("; ")
            ),
        }
    }

    /// Reads the latest `admission` section, and says whether it was taken.
    async fn load_admission(state: &mut RootSupervisorState) -> bool {
        let value = match state.configuration.runtime(admission::SECTION_NAME).await {
            Ok(Some(value)) => value,
            Ok(None) => return false,
            Err(e) => {
                warn!("could not read the admission section: {e}");
                return false;
            }
        };
        match Rules::parse(&value.body) {
            Ok(rules) => {
                info!(version = value.version, "admission section loaded");
                state.control.set_rules(rules);
                true
            }
            Err(problems) => {
                error!(
                    version = value.version,
                    "the admission section does not read, so the earlier rules stay: {}",
                    problems.join("; ")
                );
                false
            }
        }
    }

    /// The warm-up in force, in milliseconds.
    async fn warmup_ms(state: &RootSupervisorState) -> i64 {
        match state.configuration.current().await {
            Ok(config) => settings::read_warmup(&config)
                .unwrap_or(Duration::from_secs(600))
                .as_millis() as i64,
            Err(e) => {
                warn!("could not read the warm-up, using ten minutes: {e}");
                600_000
            }
        }
    }

    /// Announces the set if there is anything the controller may say. With `only_if_changed`, only when it is
    /// not what was last announced.
    async fn announce_set(state: &mut RootSupervisorState, only_if_changed: bool) {
        if state.shutting_down {
            return;
        }
        let warmup = Self::warmup_ms(state).await;
        let Some(set) = state.control.to_announce(now_ms(), warmup) else {
            return;
        };
        if only_if_changed && state.last_announced.as_ref() == Some(&set) {
            return;
        }
        if announce(&state.tcp_client, &set.0, set.1) {
            state.last_announced = Some(set);
        }
    }
}

#[async_trait]
impl Actor for RootSupervisor {
    type Msg = RootSupervisorMessage;
    type State = RootSupervisorState;
    type Arguments = Configured;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        configured: Configured,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");
        let deliver_to = myself.clone();
        let proposals = Subscription::new(PROPOSAL_OBSERVE_REPO_TOPIC, move |incoming| {
            if let Some(proposal) = decode_proposal(&incoming.payload)
                && let Err(e) = deliver_to.cast(RootSupervisorMessage::Proposed {
                    proposer: incoming.published_by,
                    proposal,
                })
            {
                warn!("failed to hand a proposal to the supervisor: {e}");
            }
        });
        let resources = AgentBuilder::new(myself.clone(), crate::agent::AGENT_NAME)
            .with_subscription(proposals)
            .with_lifecycle_handler(|event| Some(RootSupervisorMessage::ClientEvent { event }))
            .into_configured(configured)
            .build()
            .await?;

        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => {
                    Some(RootSupervisorMessage::PrepareShutdown)
                }
            });
        resources
            .configuration
            .runtime_changes()
            .subscribe(myself.clone(), |changed| {
                Some(RootSupervisorMessage::RuntimeChanged {
                    section: changed.section,
                })
            });

        // A section already held (the agent started again in the same process, or the replay was quick) is
        // read first; the timer's first announcement follows it.
        for section in [desired::SECTION_NAME, admission::SECTION_NAME] {
            myself.cast(RootSupervisorMessage::RuntimeChanged {
                section: section.to_string(),
            })?;
        }
        myself.cast(RootSupervisorMessage::Republish)?;

        Ok(RootSupervisorState {
            tcp_client: resources.tcp_client,
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            configuration: resources.configuration,
            control: Control::new(now_ms()),
            judging: false,
            judge_again: false,
            last_announced: None,
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
            RootSupervisorMessage::ClientEvent { event } => match event {
                // Registered fires on every registration, a resume after a dropped connection included.
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cassini_connected();
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => (),
            },
            RootSupervisorMessage::RuntimeChanged { section } => {
                if section == desired::SECTION_NAME {
                    Self::load_repositories(state).await;
                    Self::announce_set(state, true).await;
                } else if section == admission::SECTION_NAME && Self::load_admission(state).await {
                    myself.cast(RootSupervisorMessage::Judge)?;
                }
            }
            RootSupervisorMessage::Proposed { proposer, proposal } => {
                if state.shutting_down {
                    return Ok(());
                }
                match state.control.receive(
                    proposer.as_deref(),
                    &proposal.url,
                    &proposal.evidence,
                    now_ms(),
                ) {
                    Some(refused) => publish_decision(&state.tcp_client, &refused),
                    None => myself.cast(RootSupervisorMessage::Judge)?,
                }
            }
            RootSupervisorMessage::Judge => {
                if state.shutting_down {
                    return Ok(());
                }
                if state.judging {
                    state.judge_again = true;
                    return Ok(());
                }
                state.judging = true;
                let (table, rules) = state.control.to_judge();
                let reply = myself.clone();
                // The names are looked up on a blocking thread, and a judging that never finishes is
                // abandoned, so a slow resolver cannot stop the controller from receiving or announcing.
                tokio::spawn(async move {
                    let now = now_ms();
                    let judging = tokio::task::spawn_blocking(move || {
                        table.evaluate(rules.as_ref(), &resolve_system, now)
                    });
                    let results = match tokio::time::timeout(JUDGE_TIMEOUT, judging).await {
                        Ok(Ok(results)) => Some(results),
                        Ok(Err(e)) => {
                            warn!("the judging of the proposals failed: {e}");
                            None
                        }
                        Err(_) => {
                            warn!("the judging of the proposals took too long and was abandoned");
                            None
                        }
                    };
                    let _ = reply.cast(RootSupervisorMessage::Judged(results));
                });
            }
            RootSupervisorMessage::Judged(results) => {
                state.judging = false;
                if let Some(results) = results {
                    for decision in state.control.judged(results, now_ms()) {
                        publish_decision(&state.tcp_client, &decision);
                    }
                    Self::announce_set(state, true).await;
                }
                if state.judge_again {
                    state.judge_again = false;
                    myself.cast(RootSupervisorMessage::Judge)?;
                }
            }
            RootSupervisorMessage::Republish => {
                if state.shutting_down {
                    return Ok(());
                }
                let config = state.configuration.current().await;
                let ttl = match &config {
                    Ok(config) => settings::read_proposal_ttl(config)
                        .unwrap_or(settings::proposal_ttl(86_400)),
                    Err(e) => {
                        warn!("could not read the proposal time limit, using a day: {e}");
                        settings::proposal_ttl(86_400)
                    }
                };
                for gone in state.control.expire(now_ms(), ttl.as_millis() as i64) {
                    publish_decision(&state.tcp_client, &gone);
                }
                // Names are looked up again, and a flood limit's hour moves on.
                myself.cast(RootSupervisorMessage::Judge)?;
                Self::announce_set(state, false).await;
                let wait = match config {
                    Ok(config) => {
                        settings::read_interval(&config).unwrap_or(settings::republish_interval(60))
                    }
                    Err(_) => settings::republish_interval(60),
                };
                drop(myself.send_after(wait, || RootSupervisorMessage::Republish));
            }
            RootSupervisorMessage::PrepareShutdown => {
                info!("PrepareShutdown received -- nothing to drain");
                state.shutting_down = true;
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
            SupervisionEvent::ActorStarted(actor) => debug!("{actor:?} started!"),
            SupervisionEvent::ActorTerminated(actor, _, reason) => {
                info!("{actor:?} terminated. {reason:?}");
                // The infrastructure is this supervisor's linked child from build(); its ending means the
                // shutdown handshake is done, or that it failed. Either way exit non-zero so the Job creates
                // a new pod with fresh certificates.
                if actor.get_id() == state.infrastructure.get_id() {
                    info!("infrastructure is down, exiting");
                    std::process::exit(1);
                }
            }
            SupervisionEvent::ActorFailed(actor, reason) => {
                error!("{actor:?} failed! {reason:?}");
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
    use cassini_client::TcpClientMessage;
    use polar::graph::nodes::git::RepoId;
    use std::sync::{Arc, Mutex};

    struct Sink {
        received: Arc<Mutex<Vec<TcpClientMessage>>>,
    }

    #[async_trait]
    impl Actor for Sink {
        type Msg = TcpClientMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _: ActorRef<Self::Msg>,
            _: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _: ActorRef<Self::Msg>,
            message: Self::Msg,
            _: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            self.received.lock().unwrap().push(message);
            Ok(())
        }
    }

    fn config(url: &str) -> RepoObservationConfig {
        RepoObservationConfig::new(
            RepoId::from_url(url),
            url.to_string(),
            vec!["origin".into()],
            None,
            Vec::new(),
        )
        .with_credential("gh")
    }

    async fn sink() -> (TcpClient, Arc<Mutex<Vec<TcpClientMessage>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (sink, _) = Actor::spawn(
            None,
            Sink {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();
        (TcpClient::from_actor_ref(sink), received)
    }

    fn sets(received: &Mutex<Vec<TcpClientMessage>>) -> Vec<RepositorySet> {
        received
            .lock()
            .unwrap()
            .iter()
            .map(|message| {
                let TcpClientMessage::Publish { topic, payload, .. } = message else {
                    panic!("not a publish: {message:?}");
                };
                assert_eq!(topic, GIT_REPO_CONFIG_EVENTS);
                let mut aligned = rkyv::util::AlignedVec::<16>::new();
                aligned.extend_from_slice(payload);
                rkyv::from_bytes::<RepositorySet, rkyv::rancor::Error>(&aligned)
                    .expect("a set the observer can decode")
            })
            .collect()
    }

    async fn until_published(received: &Mutex<Vec<TcpClientMessage>>, n: usize) {
        for _ in 0..200 {
            if received.lock().unwrap().len() >= n {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn the_whole_set_is_one_message_on_the_configuration_topic_in_order() {
        let (client, received) = sink().await;
        let configs = [
            config("https://github.com/o/a"),
            config("https://github.com/o/b"),
        ];
        assert!(announce(&client, &configs, true));
        until_published(&received, 1).await;
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let sets = sets(&received);
        assert_eq!(sets.len(), 1, "one message, not one per repository");
        assert!(sets[0].complete);
        let urls: Vec<&str> = sets[0]
            .repositories
            .iter()
            .map(|c| c.repo_url.as_str())
            .collect();
        assert_eq!(urls, ["https://github.com/o/a", "https://github.com/o/b"]);
        assert_eq!(sets[0].repositories[0].credential.as_deref(), Some("gh"));
    }

    #[tokio::test]
    async fn whether_the_set_is_complete_is_what_the_caller_said() {
        let (client, received) = sink().await;
        assert!(announce(
            &client,
            &[config("https://github.com/o/a")],
            false
        ));
        assert!(announce(&client, &[config("https://github.com/o/a")], true));
        until_published(&received, 2).await;
        let flags: Vec<bool> = sets(&received).iter().map(|s| s.complete).collect();
        assert_eq!(flags, [false, true]);
    }

    #[tokio::test]
    async fn an_empty_set_is_announced_as_given_and_it_is_the_supervisors_to_decide_whether_to() {
        let (client, received) = sink().await;
        assert!(announce(&client, &[], true));
        until_published(&received, 1).await;
        let sets = sets(&received);
        assert_eq!(sets.len(), 1);
        assert!(sets[0].repositories.is_empty() && sets[0].complete);
    }

    #[test]
    fn a_proposal_decodes_and_bytes_that_are_not_one_are_dropped() {
        use git_agent_common::proposal::Evidence;
        let sent = Proposal {
            url: "https://github.com/o/r".into(),
            evidence: Evidence {
                source: "jira".into(),
                reference: Some("ABC-1".into()),
            },
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&sent).unwrap();
        // from a slice that does not start where rkyv would like it to
        let mut shifted = vec![0u8];
        shifted.extend_from_slice(&bytes);
        assert_eq!(decode_proposal(&bytes), Some(sent.clone()));
        assert_eq!(decode_proposal(&shifted[1..]), Some(sent));
        assert!(decode_proposal(b"not a proposal").is_none());
        assert!(decode_proposal(&[]).is_none());
    }

    #[test]
    fn the_system_resolver_answers_for_a_name_every_machine_knows() {
        let addresses = resolve_system("localhost").expect("localhost resolves");
        assert!(addresses.iter().any(|a| a.is_loopback()), "{addresses:?}");
        // and what it answers is exactly what the guard refuses
        assert!(
            addresses
                .iter()
                .all(|a| admission::address_problem(*a).is_some())
        );
    }
}
