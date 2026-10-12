/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/
use crate::groups::JiraGroupObserver;
use crate::issues::JiraIssueObserver;
use crate::projects::JiraProjectObserver;
use crate::users::JiraUserObserver;
use crate::{
    BROKER_CLIENT_NAME, JIRA_GROUP_OBSERVER, JIRA_ISSUE_OBSERVER, JIRA_PROJECT_OBSERVER,
    JIRA_USER_OBSERVER, JiraAuth, JiraDeployment, JiraObserverArgs, JiraObserverMessage,
};
use cassini_types::ClientEvent;
use polar::async_get_file_as_byte_vec;
use polar::cassini::TcpClient;
use polar_agent_builder::{
    AgentBuilder, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage,
};
use ractor::concurrency::Duration;
use ractor::{Actor, ActorId, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use reqwest::{Certificate, Client, ClientBuilder};
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
}

pub struct ObserverSupervisor;

pub struct ObserverSupervisorState {
    jira_url: String,
    auth: JiraAuth,
    deployment: JiraDeployment,
    base_interval: u64,
    query_date: Option<String>,
    web_client: Client,
    tcp_client: TcpClient,
    healthcheck: HealthHandle,
    /// Sent ProceedWithShutdown once the observers have stopped.
    infrastructure: ActorRef<InfrastructureSupervisorMessage>,
    projects: Option<ActorRef<JiraObserverMessage>>,
    groups: Option<ActorRef<JiraObserverMessage>>,
    users: Option<ActorRef<JiraObserverMessage>>,
    issues: Option<ActorRef<JiraObserverMessage>>,
}

pub struct ObserverSupervisorArgs {
    /// The configuration `run` judged; the builder is made from it.
    pub configured: Configured,
    pub jira_url: String,
    pub auth: JiraAuth,
    pub deployment: JiraDeployment,
    pub proxy_ca_cert_file: Option<String>,
    pub query_date: Option<String>,
    pub base_interval: u64,
}

impl ObserverSupervisorState {
    fn observer_args(&self) -> JiraObserverArgs {
        JiraObserverArgs {
            jira_url: self.jira_url.clone(),
            auth: self.auth.clone(),
            deployment: self.deployment,
            tcp_client: self.tcp_client.clone(),
            web_client: self.web_client.clone(),
            base_interval: self.base_interval,
            query_date: self.query_date.clone(),
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
}

impl ObserverSupervisor {
    /// Build reqwest client, optionally with a proxy CA certificate.
    async fn get_client(proxy_ca_cert_path: Option<&str>) -> Result<Client, ActorProcessingErr> {
        let mut builder = ClientBuilder::new();

        if let Some(path) = proxy_ca_cert_path {
            let cert_data = async_get_file_as_byte_vec(path).await.map_err(|e| {
                ActorProcessingErr::from(format!(
                    "failed to read proxy CA certificate at {path}: {e}"
                ))
            })?;
            let root_cert = Certificate::from_pem(&cert_data).map_err(|e| {
                ActorProcessingErr::from(format!("{path} is not a PEM certificate: {e}"))
            })?;

            info!("Found PROXY_CA_CERT at: {path}, configuring web client...");
            builder = builder.add_root_certificate(root_cert).use_rustls_tls();
        }

        builder
            .build()
            .map_err(|e| ActorProcessingErr::from(format!("failed to build web client: {e}")))
    }

    /// Starts whichever observers are not running. Safe to call on every
    /// Registered: a reconnect finds the slots full and starts nothing.
    async fn spawn_observers(
        myself: &ActorRef<ObserverSupervisorMessage>,
        state: &mut ObserverSupervisorState,
    ) -> Result<(), ActorProcessingErr> {
        let args = state.observer_args();

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

        // Fail on a bad proxy CA before anything is started.
        let web_client = ObserverSupervisor::get_client(args.proxy_ca_cert_file.as_deref()).await?;

        // Health check, router and broker client in one call. Observers only
        // depend on Cassini, which is the default capability, so there is
        // nothing to declare beyond the lifecycle handler.
        let resources = AgentBuilder::new(myself.clone(), BROKER_CLIENT_NAME)
            .with_lifecycle_handler(|event| Some(ObserverSupervisorMessage::ClientEvent { event }))
            .into_configured(args.configured)
            .build()
            .await?;

        resources
            .infrastructure_events
            .subscribe(myself.clone(), |event| match event {
                InfrastructureEvent::PrepareShutdown => {
                    Some(ObserverSupervisorMessage::PrepareShutdown)
                }
            });

        Ok(ObserverSupervisorState {
            jira_url: args.jira_url,
            auth: args.auth,
            deployment: args.deployment,
            base_interval: args.base_interval,
            query_date: args.query_date,
            web_client,
            tcp_client: resources.tcp_client,
            healthcheck: resources.healthcheck,
            infrastructure: resources.infrastructure,
            projects: None,
            groups: None,
            users: None,
            issues: None,
        })
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
            ObserverSupervisorMessage::ClientEvent { event } => match event {
                ClientEvent::Registered { .. } => {
                    let _ = state.healthcheck.cassini_connected();
                    ObserverSupervisor::spawn_observers(&myself, state).await?;
                }
                ClientEvent::MessagePublished { .. } => {
                    warn!("OBSERVER_SUPERVISOR: unexpected MessagePublished event, ignoring");
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
                // own, typically after exhausting its backoff or hitting bad
                // credentials. That needs an admin, and a visible pod
                // restart is the signal.
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
