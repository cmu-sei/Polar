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
use crate::agent::Prepared;
use crate::credentials;
use crate::groups::JiraGroupObserver;
use crate::issues::JiraIssueObserver;
use crate::projects::JiraProjectObserver;
use crate::users::JiraUserObserver;
use crate::{
    BROKER_CLIENT_NAME, JIRA_GROUP_OBSERVER, JIRA_ISSUE_OBSERVER, JIRA_PROJECT_OBSERVER,
    JIRA_USER_OBSERVER, JiraAuth, JiraObserverArgs, JiraObserverMessage, RETRY,
};
use cassini_types::ClientEvent;
use polar::cassini::TcpClient;
use polar::egress::Egress;
use polar_agent_builder::{
    AgentBuilder, AgentResources, ConfigurationHandle, Configured, CryptoClient, HealthClient,
    HealthHandle, HoldReason, InfrastructureEvent, InfrastructureSupervisorMessage, StateClient,
};
use ractor::concurrency::Duration;
use ractor::{Actor, ActorId, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use reqwest::{Client, ClientBuilder};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// How long a child observer gets to stop before the supervisor moves on.
/// An observer in the middle of a Jira pull only sees the stop between
/// messages, so this has to be bounded.
const OBSERVER_STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the infrastructure supervisor gives this agent to drain: each observer
/// is stopped in turn, bounded at `OBSERVER_STOP_TIMEOUT`, so this allows for
/// several, plus a margin. If it is not done by then the process exits 1
/// regardless.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(OBSERVER_STOP_TIMEOUT.as_secs() * 3 + 10);

/// How long opening a connection to Jira may take, the TLS handshake included. A site that
/// has not answered by then is as good as unreachable, and the pull is tried again later.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long one request to Jira may take, from sending it to the end of its body. The largest
/// is a page of 50 issues with their changelogs; without a bound, a site that stops answering
/// in the middle of one leaves the observer waiting for ever.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// This supervisor's own message type. polar::SupervisorMessage cannot carry
/// the infrastructure signals without polar-agent-builder depending back on
/// lib, so the builder is generic over the message (see kube-observer).
pub enum ObserverSupervisorMessage {
    ClientEvent {
        event: ClientEvent,
    },
    /// The HealthCheckActor, a child of InfrastructureSupervisor, has
    /// terminated for rejuvenation. Stop the observers, then send
    /// InfrastructureSupervisorMessage::ProceedWithShutdown.
    PrepareShutdown,
    /// A runtime section was replaced: the `credentials` section brings a new credential, or withdraws it.
    RuntimeChanged {
        section: String,
    },
    /// A signed update changed the variables named: one the observers are started with makes the supervisor read the
    /// settings again and start them again.
    ConfigChanged {
        changed: Vec<String>,
    },
}

pub struct ObserverSupervisor;

pub struct ObserverSupervisorState {
    /// The settings the observers are started with.
    prepared: Prepared,
    base_interval: u64,
    web_client: Client,
    tcp_client: TcpClient,
    /// The agent's soft state, which the issue observer's cursor is read from (its state actor).
    state: Option<Arc<dyn StateClient>>,
    healthcheck: HealthHandle,
    /// The CryptoActor, which holds the credential and gives it for the site's host only.
    crypto: Option<Arc<dyn CryptoClient>>,
    /// The live configuration: the `credentials` runtime section is read from it.
    configuration: Option<ConfigurationHandle>,
    /// The name of the credential registered from the latest `credentials` section; none until one arrives, or
    /// once it is withdrawn.
    credential: Option<String>,
    /// Why the latest `credentials` section gave no credential, when it was refused.
    credential_problem: Option<String>,
    /// Why the settings in force cannot work, when a signed update made them so: no observer runs until another
    /// update mends them.
    settings_problem: Option<String>,
    /// Cassini has registered this agent: nothing calls Jira before.
    registered: bool,
    /// The standing problem the supervisor itself holds (no credential), apart from the observers' own.
    held: Option<HoldReason>,
    /// Sent ProceedWithShutdown once the observers have stopped.
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    projects: Option<ActorRef<JiraObserverMessage>>,
    groups: Option<ActorRef<JiraObserverMessage>>,
    users: Option<ActorRef<JiraObserverMessage>>,
    issues: Option<ActorRef<JiraObserverMessage>>,
}

/// Where the supervisor's infrastructure comes from.
pub enum Infrastructure {
    /// The configuration `run` judged: the supervisor builds the agent from it.
    Build(Configured),
    /// Built already, by the held supervisor of an agent that refused its configuration at start and was mended by a
    /// signed update (`polar_agent_builder::held`); `registered` is whether Cassini has registered it already.
    Built {
        resources: AgentResources,
        registered: bool,
    },
}

pub struct ObserverSupervisorArgs {
    /// The agent's infrastructure, to build or already built.
    pub infrastructure: Infrastructure,
    /// The settings `prepare` made of it.
    pub prepared: Prepared,
    pub base_interval: u64,
}

/// What the supervisor holds when it has no credential to start the observers with.
const NO_CREDENTIAL: &str = "jira-observer: no credential: sign a `credentials` runtime section for jira-observer \
     (polar-sign), or set JIRA_ANONYMOUS to true";

impl ObserverSupervisorState {
    /// How the observers authenticate: anonymously, or with the registered credential for the site's host. None while
    /// a credential is needed and none is registered.
    fn auth(&self) -> Option<JiraAuth> {
        if self.prepared.anonymous {
            return Some(JiraAuth::Anonymous);
        }
        Some(JiraAuth::Credential {
            name: self.credential.clone()?,
            host: self.prepared.credential_host.clone()?,
            crypto: self.crypto.clone()?,
        })
    }

    fn observer_args(&self, auth: JiraAuth) -> JiraObserverArgs {
        JiraObserverArgs {
            jira_url: self.prepared.jira_url.clone(),
            auth,
            deployment: self.prepared.deployment,
            tcp_client: self.tcp_client.clone(),
            web_client: self.web_client.clone(),
            base_interval: self.base_interval,
            query_date: self.prepared.query_date.clone(),
            retry: RETRY,
            clock: std::time::Instant::now,
            cursor: self.state.clone(),
            wall_clock: crate::issues::wall_clock_ms,
            health: self.healthcheck.clone(),
        }
    }

    /// True if this is one of the observers the supervisor currently expects
    /// to be running. Observers it stopped on purpose have already been
    /// taken out of their slot, so their termination is not an error.
    fn tracks(&self, id: ActorId) -> bool {
        [&self.projects, &self.groups, &self.users, &self.issues]
            .into_iter()
            .any(|slot| slot.as_ref().is_some_and(|o| o.get_id() == id))
    }

    fn take_observers(&mut self) -> Vec<ActorRef<JiraObserverMessage>> {
        [
            self.projects.take(),
            self.groups.take(),
            self.users.take(),
            self.issues.take(),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// Stops every running observer and waits for each, bounded. A timeout
    /// is logged rather than fatal: the process is about to exit or the
    /// observers are about to be rebuilt either way.
    async fn stop_observers(&mut self, reason: &str) {
        for observer in self.take_observers() {
            if let Err(e) = observer
                .stop_and_wait(Some(reason.to_string()), Some(OBSERVER_STOP_TIMEOUT))
                .await
            {
                warn!("OBSERVER_SUPERVISOR: observer did not stop cleanly: {e:?}");
            }
        }
    }

    /// Holds `reason` with `message` for the supervisor itself.
    fn hold(&mut self, reason: HoldReason, message: String) {
        error!("{message}");
        self.healthcheck.hold(reason, message);
        self.held = Some(reason);
    }
}

impl ObserverSupervisor {
    /// Starts the observers once Cassini has registered the agent, with the credential if one is needed: without
    /// one, nothing is pulled and the supervisor holds `Credentials` until a `credentials` section brings one.
    async fn start_observers(
        myself: &ActorRef<ObserverSupervisorMessage>,
        state: &mut ObserverSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        if !state.registered {
            return Ok(());
        }
        if let Some(why) = &state.settings_problem {
            let message = format!("jira-observer: {why}");
            state.hold(HoldReason::Configuration, message);
            return Ok(());
        }
        match state.auth() {
            None => {
                let message = match &state.credential_problem {
                    Some(why) => format!("jira-observer: {why}"),
                    None => NO_CREDENTIAL.to_string(),
                };
                state.hold(HoldReason::Credentials, message);
                Ok(())
            }
            Some(auth) => {
                if let Some(reason) = state.held.take() {
                    state.healthcheck.release(reason);
                }
                ObserverSupervisor::spawn_observers(myself, state, auth).await
            }
        }
    }

    /// Stops the observers and starts them again with what is in force now. A standing problem an observer held goes
    /// with it, so every hold is released first; an observer that meets it again holds it again.
    async fn restart_observers(
        myself: &ActorRef<ObserverSupervisorMessage>,
        state: &mut ObserverSupervisorState,
        why: &str,
    ) -> Result<(), ActorProcessingErr> {
        info!("OBSERVER_SUPERVISOR: starting the observers again: {why}");
        state.stop_observers(why).await;
        state.healthcheck.release(HoldReason::Credentials);
        state.healthcheck.release(HoldReason::Configuration);
        state.held = None;
        ObserverSupervisor::start_observers(myself, state).await
    }

    /// A signed update changed a setting: the settings are made again from the configuration in force
    /// (`agent::prepare` on `current()`), and the observers start again with them, which releases every hold. Settings
    /// that cannot work stop the observers, and the supervisor holds `Configuration` with why, until an update mends
    /// them.
    async fn settings_changed(
        myself: &ActorRef<ObserverSupervisorMessage>,
        state: &mut ObserverSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        let Some(configuration) = &state.configuration else {
            return Ok(());
        };
        let current = match configuration.current().await {
            Ok(current) => current,
            Err(e) => {
                warn!("could not read the configuration in force: {e}");
                return Ok(());
            }
        };
        match crate::agent::prepare(&current) {
            Ok(prepared) => {
                state.prepared = prepared;
                state.settings_problem = None;
            }
            Err(why) => {
                error!("the settings a signed update made cannot work: {why}");
                state.settings_problem = Some(why);
            }
        }
        ObserverSupervisor::restart_observers(myself, state, "a signed update changed the settings")
            .await
    }

    /// Reads the latest `credentials` section and hands it to the CryptoActor; the name registered is the credential
    /// the observers use. A body that does not read, or an entry the CryptoActor refuses, leaves no credential, and
    /// says why.
    async fn apply_credentials(state: &mut ObserverSupervisorState) {
        let Some(configuration) = &state.configuration else {
            return;
        };
        let value = match configuration.runtime(credentials::SECTION_NAME).await {
            Ok(Some(value)) => value,
            Ok(None) => return,
            Err(e) => {
                warn!("could not read the credentials section: {e}");
                return;
            }
        };
        ObserverSupervisor::use_credentials(state, value.version, &value.body).await;
    }

    /// Version `version` of the `credentials` section, `body`: its entry registered with the CryptoActor is the
    /// credential the observers use, or there is none, and why.
    async fn use_credentials(
        state: &mut ObserverSupervisorState,
        version: u64,
        body: &serde_json::Value,
    ) {
        let Some(crypto) = state.crypto.clone() else {
            return;
        };
        let applied = match credentials::parse(body) {
            Ok(entry) => credentials::apply(crypto.as_ref(), version, entry.as_ref()).await,
            Err(problems) => Err(format!(
                "the credentials section version {version} does not read: {}",
                problems.join("; ")
            )),
        };
        match applied {
            Ok(name) => {
                state.credential = name;
                state.credential_problem = None;
            }
            Err(why) => {
                error!(version, "{why}");
                state.credential = None;
                state.credential_problem = Some(why);
            }
        }
    }

    /// The reqwest client: the way out of the cluster the egress settings give (polar::egress), with
    /// a bound on opening a connection and on each request.
    async fn get_client(
        egress: &Egress,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Client, ActorProcessingErr> {
        let bundle = egress.ca_bundle().await?;
        if !bundle.is_empty() {
            info!(
                "trusting {} egress CA certificate(s) beside the system's",
                bundle.len()
            );
        }
        let builder = ClientBuilder::new()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout);
        egress
            .configure(builder, &bundle)?
            .build()
            .map_err(|e| ActorProcessingErr::from(format!("failed to build web client: {e}")))
    }

    /// Starts whichever observers are not running. Safe to call on every
    /// Registered: a reconnect finds the slots full and starts nothing.
    async fn spawn_observers(
        myself: &ActorRef<ObserverSupervisorMessage>,
        state: &mut ObserverSupervisorState,
        auth: JiraAuth,
    ) -> Result<(), ActorProcessingErr> {
        let args = state.observer_args(auth);

        if state.projects.is_none() {
            let (observer, _) = Actor::spawn_linked(
                Some(JIRA_PROJECT_OBSERVER.to_string()),
                JiraProjectObserver,
                args.clone(),
                myself.clone().into(),
            )
            .await?;
            state.projects = Some(observer);
        }
        if state.groups.is_none() {
            let (observer, _) = Actor::spawn_linked(
                Some(JIRA_GROUP_OBSERVER.to_string()),
                JiraGroupObserver,
                args.clone(),
                myself.clone().into(),
            )
            .await?;
            state.groups = Some(observer);
        }
        if state.users.is_none() {
            let (observer, _) = Actor::spawn_linked(
                Some(JIRA_USER_OBSERVER.to_string()),
                JiraUserObserver,
                args.clone(),
                myself.clone().into(),
            )
            .await?;
            state.users = Some(observer);
        }
        if state.issues.is_none() {
            let (observer, _) = Actor::spawn_linked(
                Some(JIRA_ISSUE_OBSERVER.to_string()),
                JiraIssueObserver,
                args,
                myself.clone().into(),
            )
            .await?;
            state.issues = Some(observer);
        }
        Ok(())
    }
}

#[async_trait]
impl Actor for ObserverSupervisor {
    type Msg = ObserverSupervisorMessage;
    type State = ObserverSupervisorState;
    type Arguments = ObserverSupervisorArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ObserverSupervisorArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting");

        // Fail on an egress CA that cannot be used before anything is started.
        let web_client =
            ObserverSupervisor::get_client(&args.prepared.egress, CONNECT_TIMEOUT, REQUEST_TIMEOUT)
                .await?;

        // Health check, router, broker client and CryptoActor in one call. Observers only
        // depend on Cassini, which is the default capability; the issue
        // observer's soft state comes with the state actor the builder starts.
        let (resources, registered) = match args.infrastructure {
            Infrastructure::Build(configured) => (
                AgentBuilder::new(myself.clone(), BROKER_CLIENT_NAME)
                    .with_lifecycle_handler(|event| {
                        Some(ObserverSupervisorMessage::ClientEvent { event })
                    })
                    .into_configured(configured)
                    .build()
                    .await?,
                false,
            ),
            // Its lifecycle events go to the held supervisor that built it, and to whoever subscribes.
            Infrastructure::Built {
                resources,
                registered,
            } => {
                resources.events_output.subscribe(myself.clone(), |event| {
                    Some(ObserverSupervisorMessage::ClientEvent { event })
                });
                (resources, registered)
            }
        };

        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => {
                    Some(ObserverSupervisorMessage::PrepareShutdown)
                }
            });
        resources
            .configuration
            .changes()
            .subscribe(myself.clone(), |changed| {
                Some(ObserverSupervisorMessage::ConfigChanged {
                    changed: changed.changed,
                })
            });
        resources
            .configuration
            .runtime_changes()
            .subscribe(myself.clone(), |changed| {
                Some(ObserverSupervisorMessage::RuntimeChanged {
                    section: changed.section,
                })
            });

        Ok(ObserverSupervisorState {
            prepared: args.prepared,
            base_interval: args.base_interval,
            web_client,
            tcp_client: resources.tcp_client,
            state: resources
                .state
                .map(|handle| Arc::new(handle) as Arc<dyn StateClient>),
            healthcheck: resources.healthcheck,
            crypto: resources
                .crypto
                .map(|handle| Arc::new(handle) as Arc<dyn CryptoClient>),
            configuration: Some(resources.configuration),
            credential: None,
            credential_problem: None,
            settings_problem: None,
            registered,
            held: None,
            infrastructure: resources.infrastructure,
            projects: None,
            groups: None,
            users: None,
            issues: None,
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // A section that arrived before the subscription.
        ObserverSupervisor::apply_credentials(state).await;
        // Built by a held supervisor after Cassini registered the agent: no `Registered` comes again.
        ObserverSupervisor::start_observers(&myself, state).await
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ObserverSupervisorMessage::PrepareShutdown => {
                info!(
                    "OBSERVER_SUPERVISOR: PrepareShutdown received, stopping observers \
                     before infrastructure"
                );
                state.stop_observers("rejuvenation").await;

                let _ = state
                    .infrastructure
                    .cast(InfrastructureSupervisorMessage::ProceedWithShutdown);
            }
            ObserverSupervisorMessage::ConfigChanged { changed } => {
                if crate::settings::touches_the_observers(&changed) {
                    ObserverSupervisor::settings_changed(&myself, state).await?;
                }
            }
            ObserverSupervisorMessage::RuntimeChanged { section } => {
                if section == credentials::SECTION_NAME {
                    ObserverSupervisor::apply_credentials(state).await;
                    ObserverSupervisor::restart_observers(
                        &myself,
                        state,
                        "a new version of the credentials section",
                    )
                    .await?;
                }
            }
            ObserverSupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cassini_connected();
                    state.registered = true;
                    ObserverSupervisor::start_observers(&myself, state).await?;
                }
                ClientEvent::MessagePublished { .. } => {
                    warn!("OBSERVER_SUPERVISOR: unexpected MessagePublished event, ignoring");
                }
                // The broker ended its session while it was away: the issue observer pulls from the graph's cursor
                // next (what it was owed is lost).
                ClientEvent::SessionEnded {
                    registration_id,
                    reason,
                } => {
                    warn!(%registration_id, "the broker ended this agent's session: {reason}");
                    if let Some(issues) = &state.issues {
                        let _ = issues.cast(JiraObserverMessage::Resync);
                    }
                }
                ClientEvent::TransportError { reason } => {
                    warn!("Transport error occurred (non-fatal, awaiting reconnect): {reason}");
                    let _ = state.healthcheck.cassini_disconnected();
                }
                _ => (),
            },
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisionEvent::ActorStarted(_) => (),
            SupervisionEvent::ActorTerminated(actor_cell, _, reason) => {
                let actor_name = actor_cell.get_name().unwrap_or_default();

                // InfrastructureSupervisor finishing is the end of the
                // shutdown handshake. Exit non-zero so the Job controller
                // starts a new pod with fresh certs.
                if actor_cell.get_id() == state.infrastructure.get_id() {
                    info!(
                        "OBSERVER_SUPERVISOR: infrastructure shutdown complete, exiting for rejuvenation"
                    );
                    std::process::exit(1);
                }

                // An observer we still expect to be running stopped on its
                // own. A failure in talking to Jira, a rejected token among
                // them, never stops one (it is retried, or held through the
                // agent's health), so this is something else gone wrong, and
                // a visible pod restart is the signal.
                if state.tracks(actor_cell.get_id()) {
                    error!(
                        "OBSERVER_SUPERVISOR: observer {actor_name:?} stopped unexpectedly: {reason:?}"
                    );
                    std::process::exit(1);
                }

                debug!("OBSERVER_SUPERVISOR: {actor_name:?} stopped as requested: {reason:?}");
            }
            SupervisionEvent::ActorFailed(actor_cell, e) => {
                error!(
                    "OBSERVER_SUPERVISOR: {0:?}:{1:?} failed! {e:?}",
                    actor_cell.get_name(),
                    actor_cell.get_id()
                );
                std::process::exit(1);
            }
            SupervisionEvent::ProcessGroupChanged(..) => (),
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_crypto::{FakeCrypto, Gives};
    use crate::test_health::{Told, health_recorder, told};
    use polar::egress::testing::{TestCa, given, https_server, rerun};
    use polar_agent_builder::HoldReason;
    use serde_json::json;

    use std::time::Instant;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Takes any message and drops it: the supervisor's own reference, its infrastructure and its broker client, in a
    /// test of what it starts.
    struct Sink<M>(std::marker::PhantomData<fn() -> M>);

    #[async_trait]
    impl<M: ractor::Message> Actor for Sink<M> {
        type Msg = M;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<M>,
            _args: (),
        ) -> Result<(), ActorProcessingErr> {
            Ok(())
        }
    }

    async fn sink<M: ractor::Message>() -> ActorRef<M> {
        Actor::spawn(None, Sink::<M>(std::marker::PhantomData), ())
            .await
            .unwrap()
            .0
    }

    /// The supervisor's state as `pre_start` makes it, for a site at `url`, read anonymously or with the credential
    /// `credential` that `crypto` gives, its health `health`, Cassini registered.
    async fn supervisor_state(
        url: String,
        anonymous: bool,
        credential: Option<&str>,
        crypto: FakeCrypto,
        health: ActorRef<polar_health::HealthCheckMessage>,
    ) -> ObserverSupervisorState {
        ObserverSupervisorState {
            prepared: Prepared {
                jira_url: url,
                anonymous,
                credential_host: (!anonymous).then(|| "jira.example.org".to_string()),
                deployment: crate::JiraDeployment::ServerOrDataCenter,
                egress: Egress::default(),
                query_date: None,
            },
            base_interval: 3600,
            web_client: Client::new(),
            tcp_client: TcpClient::from_actor_ref(sink().await),
            state: None,
            healthcheck: HealthHandle::new(health),
            crypto: Some(Arc::new(crypto)),
            configuration: None,
            credential: credential.map(str::to_string),
            credential_problem: None,
            settings_problem: None,
            registered: true,
            held: None,
            infrastructure: sink().await,
            projects: None,
            groups: None,
            users: None,
            issues: None,
        }
    }

    /// The observers are registered by name, so the tests that start them take turns.
    static NAMED: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn running(state: &ObserverSupervisorState) -> usize {
        [&state.projects, &state.groups, &state.users, &state.issues]
            .iter()
            .filter(|slot| slot.is_some())
            .count()
    }

    /// Without a credential, and not reading anonymously, nothing is asked of Jira: the supervisor starts no observer
    /// and holds `Credentials`, saying to sign a credentials section. One arriving releases it, and the observers
    /// start with it, asking the CryptoActor for the site's host.
    #[tokio::test]
    async fn without_a_credential_it_holds_and_a_credential_arriving_starts_the_observers() {
        let _turn = NAMED.lock().await;
        let server = site_without_data().await;
        let (health, recorded) = health_recorder().await;
        let crypto = FakeCrypto::giving(Gives::Bearer("t0ken"));
        let mut state =
            supervisor_state(server.uri(), false, None, crypto.clone(), health.clone()).await;
        let myself = sink::<ObserverSupervisorMessage>().await;

        ObserverSupervisor::start_observers(&myself, &mut state)
            .await
            .unwrap();
        assert_eq!(running(&state), 0);
        assert_eq!(
            told(&health, &recorded).await,
            [Told::Hold(
                HoldReason::Credentials,
                NO_CREDENTIAL.to_string()
            )]
        );
        assert!(server.received_requests().await.unwrap().is_empty());

        state.credential = Some("jira".to_string());
        ObserverSupervisor::restart_observers(&myself, &mut state, "a new credential")
            .await
            .unwrap();
        assert_eq!(running(&state), 4);
        let after = told_at_least(&health, &recorded, 3).await;
        assert_eq!(after[1], Told::Release(HoldReason::Credentials));
        assert!(
            crypto
                .asked
                .lock()
                .unwrap()
                .iter()
                .all(|(name, host)| name == "jira" && host == "jira.example.org")
        );
        assert!(!crypto.asked.lock().unwrap().is_empty());
        state.stop_observers("the test is over").await;
    }

    /// Starting again (a new credential, new settings) stops every observer first, releases every hold, since the
    /// observers that held are gone, and starts new ones, which hold again what they meet again.
    #[tokio::test]
    async fn starting_again_releases_every_hold_and_starts_new_observers() {
        let _turn = NAMED.lock().await;
        let server = site_without_data().await;
        let (health, recorded) = health_recorder().await;
        let mut state = supervisor_state(
            server.uri(),
            false,
            Some("jira"),
            FakeCrypto::giving(Gives::Bearer("t0ken")),
            health.clone(),
        )
        .await;
        let myself = sink::<ObserverSupervisorMessage>().await;
        ObserverSupervisor::start_observers(&myself, &mut state)
            .await
            .unwrap();
        let before: Vec<ActorId> = [&state.projects, &state.issues]
            .iter()
            .map(|o| o.as_ref().unwrap().get_id())
            .collect();
        // Each of the four is told 404 on what it pulls, and holds it.
        told_at_least(&health, &recorded, 4).await;

        ObserverSupervisor::restart_observers(&myself, &mut state, "new settings")
            .await
            .unwrap();
        let after: Vec<ActorId> = [&state.projects, &state.issues]
            .iter()
            .map(|o| o.as_ref().unwrap().get_id())
            .collect();
        assert!(before.iter().all(|id| !after.contains(id)), "new observers");
        let told = told_at_least(&health, &recorded, 10).await;
        assert_eq!(
            told[4..6],
            [
                Told::Release(HoldReason::Credentials),
                Told::Release(HoldReason::Configuration)
            ]
        );
        assert!(
            told[6..]
                .iter()
                .all(|t| matches!(t, Told::Hold(HoldReason::Configuration, _))),
            "{told:?}"
        );
        state.stop_observers("the test is over").await;
    }

    /// The live configuration, as the builder gives it, holding `pairs`: what `current()` answers.
    async fn configuration_of(pairs: &[(&str, &str)]) -> ConfigurationHandle {
        let env: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let config = polar_env::Config::builder(&env)
            .register(&crate::settings::SECTION)
            .register(&polar::egress::SECTION)
            .build();
        let parent = sink::<()>().await;
        ConfigurationHandle::spawn(None, BROKER_CLIENT_NAME, config, None, parent.get_cell())
            .await
            .unwrap()
    }

    /// A signed update that makes the settings unusable stops the observers and holds `Configuration` with why; one
    /// that mends them releases it and starts them again, with the settings now in force.
    #[tokio::test]
    async fn a_signed_update_to_the_settings_starts_the_observers_again_or_holds_why_not() {
        let _turn = NAMED.lock().await;
        let server = site_without_data().await;
        let (health, recorded) = health_recorder().await;
        let mut state = supervisor_state(
            "http://jira.invalid".into(),
            true,
            None,
            FakeCrypto::giving(Gives::NotRegistered),
            health.clone(),
        )
        .await;
        let myself = sink::<ObserverSupervisorMessage>().await;
        ObserverSupervisor::start_observers(&myself, &mut state)
            .await
            .unwrap();
        assert_eq!(running(&state), 4);

        // The placeholder address: nothing can be read.
        state.configuration = Some(
            configuration_of(&[
                ("JIRA_URL", "https://example.atlassian.net"),
                ("JIRA_ANONYMOUS", "true"),
            ])
            .await,
        );
        ObserverSupervisor::settings_changed(&myself, &mut state)
            .await
            .unwrap();
        assert_eq!(running(&state), 0);
        let told_now = told(&health, &recorded).await;
        let Some(Told::Hold(HoldReason::Configuration, message)) = told_now.last() else {
            panic!("a Configuration hold: {told_now:?}");
        };
        assert!(message.contains("placeholder"), "{message}");

        // An update that mends it: the observers start again at the address now in force.
        state.configuration = Some(
            configuration_of(&[("JIRA_URL", &server.uri()), ("JIRA_ANONYMOUS", "true")]).await,
        );
        ObserverSupervisor::settings_changed(&myself, &mut state)
            .await
            .unwrap();
        assert_eq!(running(&state), 4);
        assert_eq!(state.prepared.jira_url, server.uri());
        let after = told(&health, &recorded).await;
        assert!(
            after[told_now.len()..].contains(&Told::Release(HoldReason::Configuration)),
            "{after:?}"
        );
        // And they ask the new site.
        for _ in 0..100 {
            if !server.received_requests().await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!server.received_requests().await.unwrap().is_empty());
        state.stop_observers("the test is over").await;
    }

    /// The credentials section a signed version brings is registered and becomes the credential the observers use;
    /// one that does not read leaves none, and the supervisor says why when it holds.
    #[tokio::test]
    async fn the_credentials_section_gives_the_credential_or_says_why_not() {
        let (health, recorded) = health_recorder().await;
        let crypto = FakeCrypto::giving(Gives::Bearer("t0ken"));
        let mut state = supervisor_state(
            "http://jira.invalid".into(),
            false,
            None,
            crypto.clone(),
            health.clone(),
        )
        .await;
        let body = json!({"credentials": [{
            "name": "jira-dc", "host_pattern": "jira.example.org", "kind": "token",
            "secret": crate::credentials::tests_support::ciphertext(b"pat",
                &crate::credentials::context(3, "jira-dc").unwrap())}]});
        ObserverSupervisor::use_credentials(&mut state, 3, &body).await;
        assert_eq!(state.credential.as_deref(), Some("jira-dc"));
        assert_eq!(crypto.registered.lock().unwrap().len(), 1);

        ObserverSupervisor::use_credentials(&mut state, 4, &json!({"credentials": "no"})).await;
        assert_eq!(state.credential, None);
        let myself = sink::<ObserverSupervisorMessage>().await;
        ObserverSupervisor::start_observers(&myself, &mut state)
            .await
            .unwrap();
        let [Told::Hold(HoldReason::Credentials, message)] = &told(&health, &recorded).await[..]
        else {
            panic!("one hold");
        };
        assert!(message.contains("version 4 does not read"), "{message}");
    }

    /// A site that answers who the token is and what it is, and 404 to everything else.
    async fn site_without_data() -> MockServer {
        let server = MockServer::start().await;
        for (endpoint, body) in [
            (
                "/rest/api/2/myself",
                json!({ "name": "svc-polar", "key": "JIRAUSER10100" }),
            ),
            (
                "/rest/api/2/serverInfo",
                json!({ "deploymentType": "Server" }),
            ),
        ] {
            Mock::given(method("GET"))
                .and(wiremock::matchers::path(endpoint))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        server
    }

    /// Waits up to five seconds for the health actor to have been told `count` things.
    async fn told_at_least(
        health: &ActorRef<polar_health::HealthCheckMessage>,
        recorded: &std::sync::Arc<std::sync::Mutex<Vec<Told>>>,
        count: usize,
    ) -> Vec<Told> {
        for _ in 0..250 {
            let now = told(health, recorded).await;
            if now.len() >= count {
                return now;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        told(health, recorded).await
    }

    /// The observers it starts hold what a retry cannot fix through the agent's own health: the supervisor's
    /// `resources.healthcheck`.
    #[tokio::test]
    async fn the_observers_hold_through_the_agent_s_health() {
        let _turn = NAMED.lock().await;
        let server = site_without_data().await;
        let (health, recorded) = health_recorder().await;
        let mut state = supervisor_state(
            server.uri(),
            false,
            Some("jira"),
            FakeCrypto::giving(Gives::Bearer("t0ken")),
            health.clone(),
        )
        .await;
        let myself = sink::<ObserverSupervisorMessage>().await;
        ObserverSupervisor::start_observers(&myself, &mut state)
            .await
            .unwrap();

        // Each of the four is told 404 on what it pulls.
        let told = told_at_least(&health, &recorded, 4).await;
        let mut held: Vec<&str> = told
            .iter()
            .map(|t| match t {
                Told::Hold(HoldReason::Configuration, message) => {
                    message.split(':').next().unwrap_or_default()
                }
                other => panic!("{other:?}"),
            })
            .collect();
        held.sort();
        assert_eq!(
            held,
            [
                JIRA_GROUP_OBSERVER,
                JIRA_ISSUE_OBSERVER,
                JIRA_PROJECT_OBSERVER,
                JIRA_USER_OBSERVER
            ]
        );
        state.stop_observers("the test is over").await;
    }

    #[tokio::test]
    async fn a_request_that_takes_too_long_is_given_up() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        let client = ObserverSupervisor::get_client(
            &Egress::default(),
            Duration::from_secs(5),
            Duration::from_millis(200),
        )
        .await
        .unwrap();

        let started = Instant::now();
        let error = client.get(server.uri()).send().await.unwrap_err();
        assert!(error.is_timeout(), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn a_connection_that_never_opens_is_given_up() {
        // A listener that never accepts: the kernel completes the TCP handshake, and the TLS
        // handshake then waits for an answer that never comes.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = ObserverSupervisor::get_client(
            &Egress::default(),
            Duration::from_millis(200),
            Duration::from_secs(60),
        )
        .await
        .unwrap();

        let started = Instant::now();
        let sent = tokio::time::timeout(
            Duration::from_secs(5),
            client.get(format!("https://{address}/")).send(),
        )
        .await;
        let error = sent
            .expect("the connect timeout did not end the request")
            .unwrap_err();
        assert!(error.is_connect() || error.is_timeout(), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
        drop(listener);
    }

    /// The image's roots (SSL_CERT_FILE, read from the process environment, so the client is made in a child
    /// process) and the egress CA are trusted together: a site behind a TLS-inspecting proxy and one that is not.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_egress_ca_is_trusted_beside_the_image_roots() {
        if let Some(urls) = given("JIRA_TEST_URLS") {
            let egress = Egress {
                ca_cert: given("POLAR_EGRESS_CA_CERT"),
                ..Egress::default()
            };
            let client = ObserverSupervisor::get_client(&egress, CONNECT_TIMEOUT, REQUEST_TIMEOUT)
                .await
                .unwrap();
            for url in urls.split(',') {
                let body = client.get(url).send().await.unwrap().text().await.unwrap();
                assert_eq!(body, "ok", "{url}");
            }
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let image = TestCa::new("image roots");
        let corporate = TestCa::new("corporate root");
        let image_file = image.write(dir.path(), "image.pem");
        let corporate_file = corporate.write(dir.path(), "corporate.pem");
        let urls = format!(
            "{},{}",
            https_server(&image).await.url,
            https_server(&corporate).await.url
        );
        let output = rerun(
            "supervisor::tests::the_egress_ca_is_trusted_beside_the_image_roots",
            &[
                ("SSL_CERT_FILE", image_file.to_str().unwrap()),
                ("POLAR_EGRESS_CA_CERT", corporate_file.to_str().unwrap()),
                ("JIRA_TEST_URLS", &urls),
            ],
        )
        .await;
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
