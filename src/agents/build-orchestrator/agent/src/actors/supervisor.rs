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

use std::sync::Arc;

use crate::actors::build_job::{BuildJobActor, BuildJobArguments};
use crate::actors::build_registry::{BuildRegistryActor, RegistryMessage};
use crate::admission::{Admission, Admitted};
use crate::client::StorageClient;
use crate::config::RepoMapping;
use crate::credentials;
use crate::mappings;
use crate::observed::Observed;
use cassini_types::ClientEvent;
use git_agent_common::GitRepositoryMessage;
use orchestrator_core::{
    backend::BuildBackend,
    types::{BuildRequest, BuildState, SourceRef, SourceRefError, subjects::BUILD_REQUESTS_TOPIC},
};
use polar::RkyvError;
use polar::cassini::CassiniClient;
use polar::graph::nodes::git::RepoId;
use polar::topics::GIT_REPOSITORY_EVENTS;
use polar_agent_builder::{
    AgentBuilder, ConfigurationHandle, Configured, CryptoClient, HealthClient, HealthHandle,
    InfrastructureEvent, InfrastructureSupervisorMessage, Subscription,
};
use ractor::{Actor, ActorProcessingErr, ActorRef};
use rkyv::from_bytes;
use tracing::{error, info, warn};

use chrono::Utc;
use std::collections::HashMap;
use uuid::Uuid;

const REGISTRY_ACTOR_NAME: &str = "build-registry";

/// How many commits the supervisor remembers having taken for a build (see `Observed`).
const OBSERVED_COMMITS: usize = 1024;

/// What the supervisor is told: the broker's events, and the builder's.
#[derive(Debug)]
pub enum SupervisorMessage {
    ClientEvent {
        event: ClientEvent,
    },
    /// A message on a topic the orchestrator subscribed to, as the router delivers it.
    Incoming {
        topic: String,
        payload: Vec<u8>,
    },
    /// The builder has begun an orderly shutdown.
    PrepareShutdown,
    /// A runtime section was replaced.
    RuntimeChanged {
        section: String,
    },
    /// Time to remove the finished jobs that are past their retention.
    SweepJobs,
}

#[derive(Debug, thiserror::Error)]
pub enum EventConversionError {
    #[error(transparent)]
    InvalidSource(#[from] SourceRefError),
}

/// Arguments for supervisor initialization.
pub struct SupervisorArguments {
    pub backend: Arc<dyn BuildBackend>,
    pub config: Arc<crate::config::OrchestratorConfig>,
    pub configured: Configured,
}

pub struct OrchestratorSupervisor;

pub struct SupervisorState {
    backend: Arc<dyn BuildBackend>,
    registry: ActorRef<RegistryMessage>,
    config: Arc<crate::config::OrchestratorConfig>,
    tcp_client: Arc<dyn CassiniClient>,
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    configuration: ConfigurationHandle,
    healthcheck: HealthHandle,
    /// Opens the credentials a mapping names, for a build that is to clone a private repository.
    crypto: Arc<dyn CryptoClient>,
    /// The repositories built: the configuration file's list until the `mappings` section arrives, and then
    /// the section's, replaced whole each time.
    mappings: Vec<RepoMapping>,
    /// How many builds run at once, and who waits.
    admission: Admission,
    /// The commits the git observer has shown, so that one is not built twice for being reported twice.
    observed: Observed,
    draining: bool,
}

impl OrchestratorSupervisor {
    /// Convert a git observer `RefUpdated` into a BuildRequest, or None when
    /// the repo has no mapping in config.
    ///
    /// The observer identifies repos by `RepoId`, which cannot be turned back
    /// into a clone URL. The repo mappings hold the real URLs, and a git build
    /// needs a mapped pipeline image anyway, so a mapping is both the URL
    /// source and the filter: observed repos without one are not built.
    ///
    /// The git scheduler derives `RepoId` from un-normalized URLs, so a mapping
    /// matches whether its URL is written raw or normalized.
    pub fn build_request_from_ref_update(
        config: &crate::config::OrchestratorConfig,
        repo: &RepoId,
        ref_name: &str,
        new_commit: &str,
        observed_at: &str,
    ) -> Result<Option<BuildRequest>, EventConversionError> {
        let Some(mapping) = config.repo_mappings.iter().find(|m| {
            RepoId::from_url(&m.repo_url) == *repo
                || RepoId::from_url(&RepoId::normalize_repo_url(&m.repo_url)) == *repo
        }) else {
            return Ok(None);
        };

        // Validate the commit SHA up front. It is passed straight to git
        // checkout inside the init container, and a bad SHA failing the
        // clone rather than the submission is a confusing failure mode.
        let source = SourceRef::Git {
            url: mapping.repo_url.clone(),
            commit_sha: new_commit.to_string(),
        };
        source.validate()?;

        let metadata = HashMap::from([
            ("git_ref".to_string(), ref_name.to_string()),
            ("observed_at".to_string(), observed_at.to_string()),
        ]);

        Ok(Some(BuildRequest {
            build_id: Uuid::new_v4(),
            source,
            pipeline_image: None,
            requested_by: "polar.git.observer".to_string(),
            requested_at: Utc::now(),
            metadata,
            target_registry: "some.registry".into(), // todo: remove, vestigitial
            command: mapping
                .command
                .clone()
                .unwrap_or_else(|| config.backend.command.clone()),
        }))
    }

    /// Route an inbound message by topic.
    ///
    /// - GIT_REPOSITORY_EVENTS: rkyv `GitRepositoryMessage` from the git
    ///   observer; `RefUpdated` on a mapped repo becomes a git build.
    /// - BUILD_REQUESTS_TOPIC: a JSON `BuildRequest`, e.g. from the analysis
    ///   scheduler. Validated in handle_build_requested like any other.
    async fn deserialize_and_dispatch(
        &self,
        myself: ActorRef<SupervisorMessage>,
        state: &mut SupervisorState,
        topic: String,
        payload: Vec<u8>,
    ) -> Result<(), ActorProcessingErr> {
        let request = match topic.as_str() {
            GIT_REPOSITORY_EVENTS => {
                match from_bytes::<GitRepositoryMessage, RkyvError>(&payload)? {
                    GitRepositoryMessage::RefUpdated {
                        repo,
                        ref_name,
                        old,
                        new,
                        observed_at,
                    } => {
                        // The observer reports every ref each time it looks, moved or not. Nothing moved.
                        if old.as_deref() == Some(new.as_str()) {
                            tracing::debug!(ref_name = %ref_name, commit = %new, "ref reported unchanged; not building");
                            return Ok(());
                        }
                        match Self::build_request_from_ref_update(
                            &Self::current_config(state),
                            &repo,
                            &ref_name,
                            &new,
                            &observed_at,
                        ) {
                            Ok(Some(request)) => {
                                // A restarted observer reports each ref from nothing, so a commit already built
                                // arrives again with no `old`.
                                if let SourceRef::Git { url, commit_sha } = &request.source
                                    && !state.observed.first_sight(url, commit_sha)
                                {
                                    tracing::info!(
                                        ref_name = %ref_name,
                                        commit = %commit_sha,
                                        "this commit was already taken for a build; not building it again"
                                    );
                                    return Ok(());
                                }
                                request
                            }
                            Ok(None) => {
                                tracing::debug!(repo = ?repo, "ref update on unmapped repo — not building");
                                return Ok(());
                            }
                            Err(e) => {
                                warn!(error = %e, "ignored malformed RefUpdated");
                                return Ok(());
                            }
                        }
                    }
                    // Commits are built when a ref moves to them, not on discovery.
                    GitRepositoryMessage::CommitDiscovered { .. } => return Ok(()),
                }
            }
            BUILD_REQUESTS_TOPIC => serde_json::from_slice::<BuildRequest>(&payload)?,
            other => {
                warn!(topic = %other, "message on unexpected topic — ignoring");
                return Ok(());
            }
        };

        if let Err(e) = Self::handle_build_requested(self, myself, state, request).await {
            tracing::error!(error = %e, "failed to handle build request");
        }
        Ok(())
    }
}

#[ractor::async_trait]
impl Actor for OrchestratorSupervisor {
    type Msg = SupervisorMessage;
    type State = SupervisorState;
    type Arguments = SupervisorArguments;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("OrchestratorSupervisor starting");

        // The broker connection, health, certificates and shutdown are the builder's. The orchestrator holds
        // no graph connection and encrypts nothing: Cassini is its only dependency.
        // The router only delivers topics registered with the builder; a topic subscribed to by hand is dropped.
        let mut builder = AgentBuilder::new(myself.clone(), crate::agent::AGENT_NAME);
        for topic in [GIT_REPOSITORY_EVENTS, BUILD_REQUESTS_TOPIC] {
            let deliver_to = myself.clone();
            builder = builder.with_subscription(Subscription::new(topic, move |incoming| {
                if let Err(e) = deliver_to.cast(SupervisorMessage::Incoming {
                    topic: incoming.topic,
                    payload: incoming.payload,
                }) {
                    warn!("failed to hand a message to the supervisor: {e}");
                }
            }));
        }
        let resources = builder
            .with_lifecycle_handler(|event| Some(SupervisorMessage::ClientEvent { event }))
            .into_configured(args.configured)
            .build()
            .await?;

        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => Some(SupervisorMessage::PrepareShutdown),
            });
        resources
            .configuration
            .runtime_changes()
            .subscribe(myself.clone(), |changed| {
                Some(SupervisorMessage::RuntimeChanged {
                    section: changed.section,
                })
            });
        // A section already held (the replay may have been quicker than this subscription) is read first.
        for section in [mappings::SECTION_NAME, credentials::SECTION_NAME] {
            myself.cast(SupervisorMessage::RuntimeChanged {
                section: section.to_string(),
            })?;
        }
        // The agent declares that it needs encryption, so the builder gives it the CryptoActor.
        let crypto = resources
            .crypto
            .clone()
            .ok_or("the builder gave no CryptoActor, which this agent needs for its credentials")?;

        // Spawn the registry as a child of this supervisor.
        let (registry, _) = Actor::spawn_linked(
            Some(REGISTRY_ACTOR_NAME.to_string()),
            BuildRegistryActor,
            (),
            myself.get_cell(),
        )
        .await
        .map_err(|e| ActorProcessingErr::from(format!("failed to spawn registry actor: {e}")))?;

        tracing::info!("BuildRegistryActor spawned");

        let mappings = args.config.repo_mappings.clone();
        let admission = Admission::new(args.config.build_cap());
        tracing::info!(
            cap = admission.cap(),
            "builds run at most this many at once; the rest wait"
        );
        let retention = args.config.retention();
        let sweep = args.config.sweep_interval();
        tracing::info!(
            succeeded_secs = retention.succeeded.as_secs(),
            failed_secs = retention.failed.as_secs(),
            sweep_secs = sweep.as_secs(),
            "finished jobs are removed when they are this old, by how they ended"
        );
        myself.send_interval(sweep, || SupervisorMessage::SweepJobs);
        Ok(SupervisorState {
            backend: args.backend,
            registry,
            config: args.config,
            tcp_client: Arc::new(resources.tcp_client),
            infrastructure: resources.infrastructure,
            configuration: resources.configuration,
            healthcheck: resources.healthcheck,
            crypto: Arc::new(crypto),
            mappings,
            admission,
            observed: Observed::new(OBSERVED_COMMITS),
            draining: false,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            SupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    info!("Orchestrator successfully initialized");
                    let _ = state.healthcheck.cassini_connected();
                }
                // Non-fatal: in-flight builds keep polling the backend while the builder's client
                // reconnects. Whether an outage is long enough to restart is the builder's decision.
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => (),
            },
            SupervisorMessage::Incoming { topic, payload } => {
                if state.draining {
                    // Builds requested during the drain are dropped, not started.
                    warn!(
                        "draining -- logging message on topic '{topic}' \
                         ({} bytes) instead of dispatching; will be lost on exit",
                        payload.len()
                    );
                } else if let Err(e) =
                    Self::deserialize_and_dispatch(&self, myself, state, topic, payload).await
                {
                    error!("Failed to handle new message. {e}");
                }
            }
            SupervisorMessage::RuntimeChanged { section } => {
                if section == mappings::SECTION_NAME {
                    Self::load_mappings(state).await;
                } else if section == credentials::SECTION_NAME {
                    Self::apply_credentials(state).await;
                }
            }
            SupervisorMessage::SweepJobs => {
                if !state.draining {
                    // Off the supervisor's own turn: listing and deleting are the cluster's to answer.
                    let backend = Arc::clone(&state.backend);
                    let retention = state.config.retention();
                    tokio::spawn(async move {
                        match backend.reap(&retention).await {
                            Ok(removed) if removed.is_empty() => {
                                tracing::debug!("no finished job is past its retention")
                            }
                            Ok(removed) => info!(
                                removed = removed.len(),
                                "finished jobs past their retention were removed"
                            ),
                            Err(e) => warn!(
                                "could not remove the finished jobs past their retention: {e}"
                            ),
                        }
                    });
                }
            }
            SupervisorMessage::PrepareShutdown => {
                info!(
                    "PrepareShutdown received: no new builds are taken; jobs already submitted keep running"
                );
                state.draining = true;
                for request in state.admission.abandon_waiting() {
                    tracing::warn!(build_id = %request.build_id, "shutting down: a build that was waiting for a place is not started");
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
        myself: ActorRef<Self::Msg>,
        event: ractor::SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            ractor::SupervisionEvent::ActorTerminated(actor, _, reason) => {
                tracing::info!(
                    actor = ?actor.get_name(),
                    reason = ?reason,
                    "child actor terminated"
                );
                if let Some(name) = actor.get_name() {
                    Self::build_ended(&myself, state, &name).await?;
                }
                // The infrastructure is this supervisor's linked child from build(): its ending means the
                // shutdown handshake is done, or that it failed. Either way exit non-zero, so the operator
                // starts a new pod with fresh certificates.
                if actor.get_id() == state.infrastructure.get_id() {
                    info!("infrastructure is down, exiting");
                    std::process::exit(1);
                }
            }
            ractor::SupervisionEvent::ActorFailed(actor, error) => {
                let name = actor.get_name().unwrap_or_default();
                if name == REGISTRY_ACTOR_NAME || actor.get_id() == state.infrastructure.get_id() {
                    // Without these the orchestrator can't do its job: no build state, or no broker.
                    // Exit and let the operator replace it, rather than run deaf and mute.
                    tracing::error!(actor = %name, error = %error, "essential child actor failed -- exiting");
                    if !state.draining {
                        std::process::exit(1);
                    }
                } else if name.starts_with("build-job-") {
                    // A build job actor only fails on a bug or a backend error it didn't handle. Its
                    // registry record keeps its last state; nothing marks it failed.
                    tracing::error!(actor = %name, error = %error, "build job actor failed; its registry record keeps its last state");
                    Self::build_ended(&myself, state, &name).await?;
                } else {
                    tracing::error!(actor = %name, error = %error, "child actor failed");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl OrchestratorSupervisor {
    /// Reads the latest `mappings` section. A body that does not read keeps the set that was held: the
    /// builder judged it before accepting it, so this is a disagreement between the two, not an author's
    /// mistake.
    async fn load_mappings(state: &mut SupervisorState) {
        let value = match state.configuration.runtime(mappings::SECTION_NAME).await {
            Ok(Some(value)) => value,
            Ok(None) => return,
            Err(e) => {
                warn!("could not read the mappings section: {e}");
                return;
            }
        };
        match mappings::parse(&value.body) {
            Ok(list) => {
                info!(
                    version = value.version,
                    repositories = list.len(),
                    "mappings section loaded"
                );
                state.mappings = list;
            }
            Err(problems) => error!(
                version = value.version,
                "the mappings section does not read, so the earlier set stays: {}",
                problems.join("; ")
            ),
        }
    }

    /// Reads the latest `credentials` section and hands it to the CryptoActor, which opens each entry, keeps it
    /// with its binding and escrows it. A body that does not read is logged and left: the builder judged it
    /// before accepting it, so that is a disagreement between the two, not an author's mistake.
    async fn apply_credentials(state: &SupervisorState) {
        let value = match state.configuration.runtime(credentials::SECTION_NAME).await {
            Ok(Some(value)) => value,
            Ok(None) => return,
            Err(e) => {
                warn!("could not read the credentials section: {e}");
                return;
            }
        };
        let entries = match credentials::parse(&value.body) {
            Ok(entries) => entries,
            Err(problems) => {
                warn!(
                    version = value.version,
                    "the credentials section does not read: {}",
                    problems.join("; ")
                );
                return;
            }
        };
        if let Err(e) = credentials::apply(
            state.crypto.as_ref(),
            crate::agent::AGENT_NAME,
            value.version,
            &entries,
        )
        .await
        {
            error!(
                version = value.version,
                "the credentials section was not applied: {e}"
            );
        }
    }

    /// What a mapped repository's build runs is the mapping's, or the configuration file's, and never what the
    /// request says: a request can come from anyone who can publish on the broker. A repository with no mapping
    /// keeps the request's own command, as it has no other.
    pub fn command_from_mapping(
        mappings: &[RepoMapping],
        default: &[String],
        request: &mut BuildRequest,
    ) {
        let SourceRef::Git { url, .. } = &request.source else {
            return;
        };
        if let Some(mapping) = mappings.iter().find(|m| &m.repo_url == url) {
            request.command = mapping.command.clone().unwrap_or_else(|| default.to_vec());
        }
    }

    /// The configuration as it stands now: the file's, with the repositories the latest section named.
    fn current_config(state: &SupervisorState) -> Arc<crate::config::OrchestratorConfig> {
        Arc::new(crate::config::OrchestratorConfig {
            repo_mappings: state.mappings.clone(),
            ..(*state.config).clone()
        })
    }

    async fn handle_build_requested(
        &self,
        myself: ActorRef<SupervisorMessage>,
        state: &mut SupervisorState,
        request: BuildRequest,
    ) -> Result<(), ActorProcessingErr> {
        let build_id = request.build_id;
        tracing::info!(
            build_id = %build_id,
            source = %request.source.locator(),
            revision = %request.source.revision(),
            requested_by = %request.requested_by,
            "received build request"
        );

        // Requests may come from anywhere on the broker, not only from
        // build_request_from_event — validate at this boundary too.
        if let Err(e) = request.validate() {
            tracing::warn!(build_id = %build_id, error = %e, "rejected invalid build request");
            return Ok(());
        }

        // Dependency analysis is not ported: its events and graph nodes are not in the fork's library yet.
        if matches!(request.source, SourceRef::RegistryArchive { .. }) {
            tracing::warn!(build_id = %build_id, "rejected an archive build: analysis jobs are not supported on this build");
            return Ok(());
        }

        let mut request = request;
        Self::command_from_mapping(&state.mappings, &state.config.backend.command, &mut request);

        match state.admission.request(request.clone()) {
            Admitted::Duplicate => {
                tracing::warn!(build_id = %build_id, "ignored a build request for a build already running or waiting");
                Ok(())
            }
            Admitted::Waiting(place) => {
                // Known to the registry, as Pending, while it waits.
                state
                    .registry
                    .send_message(RegistryMessage::Insert(request))?;
                tracing::info!(
                    build_id = %build_id,
                    place,
                    running = state.admission.running(),
                    cap = state.admission.cap(),
                    "all places are taken; the build waits"
                );
                Ok(())
            }
            Admitted::Now(request) => {
                state
                    .registry
                    .send_message(RegistryMessage::Insert(request.clone()))?;
                Self::start_build(&myself, state, request).await
            }
        }
    }

    /// A build job actor ended, however it did. Its place goes to the next build waiting, if any.
    async fn build_ended(
        myself: &ActorRef<SupervisorMessage>,
        state: &mut SupervisorState,
        actor_name: &str,
    ) -> Result<(), ActorProcessingErr> {
        let Some(id) = actor_name
            .strip_prefix("build-job-")
            .and_then(|id| id.parse::<uuid::Uuid>().ok())
        else {
            return Ok(());
        };
        if state.draining {
            return Ok(());
        }
        // A commit whose build did not succeed is tried again when it is next reported.
        if let Ok(Some(record)) = ractor::call_t!(state.registry, RegistryMessage::Get, 5000, id)
            && record.state != BuildState::Succeeded
            && let SourceRef::Git { url, commit_sha } = &record.request.source
        {
            state.observed.forget(url, commit_sha);
        }
        if let Some(next) = state.admission.finished(id) {
            tracing::info!(build_id = %next.build_id, "a place is free; the next build starts");
            Self::start_build(myself, state, next).await?;
        }
        Ok(())
    }

    /// Spawn the actor for a build that has a place. Its registry record was made before this, so there is never
    /// a window where the actor exists but has no record.
    async fn start_build(
        myself: &ActorRef<SupervisorMessage>,
        state: &mut SupervisorState,
        request: BuildRequest,
    ) -> Result<(), ActorProcessingErr> {
        let build_id = request.build_id;
        let actor_name = format!("build-job-{}", build_id);

        let client = StorageClient::new(&state.config.storage);

        let args = BuildJobArguments {
            request,
            backend: Arc::clone(&state.backend),
            registry: state.registry.clone(),
            publisher: Arc::clone(&state.tcp_client),
            config: Self::current_config(state),
            storage: Arc::new(client),
            crypto: Some(Arc::clone(&state.crypto)),
        };

        if let Err(e) =
            Actor::spawn_linked(Some(actor_name), BuildJobActor, args, myself.get_cell()).await
        {
            // The place it was given is free again.
            let _ = state.admission.finished(build_id);
            return Err(ActorProcessingErr::from(format!(
                "failed to spawn BuildJobActor for {build_id}: {e}"
            )));
        }

        tracing::info!(build_id = %build_id, "BuildJobActor spawned");
        Ok(())
    }
}
