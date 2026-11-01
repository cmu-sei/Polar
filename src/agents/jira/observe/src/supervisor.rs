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
use crate::groups::JiraGroupObserver;
use crate::issues::JiraIssueObserver;
use crate::projects::JiraProjectObserver;
use crate::users::JiraUserObserver;
use crate::{
    BROKER_CLIENT_NAME, JIRA_GROUP_OBSERVER, JIRA_ISSUE_OBSERVER, JIRA_PROJECT_OBSERVER,
    JIRA_USER_OBSERVER, JiraAuth, JiraDeployment, JiraObserverArgs, JiraObserverMessage, RETRY,
};
use cassini_types::ClientEvent;
use polar::cassini::TcpClient;
use polar::egress::Egress;
use polar_agent_builder::{
    AgentBuilder, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage, StateClient,
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
    /// The agent's soft state, which the issue observer's cursor is read from (its state actor).
    state: Option<Arc<dyn StateClient>>,
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
    /// How the site is reached: the proxy, the hosts that bypass it, and the CA trusted beside the system's.
    pub egress: Egress,
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
}

impl ObserverSupervisor {
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

        // Fail on an egress CA that cannot be used before anything is started.
        let web_client =
            ObserverSupervisor::get_client(&args.egress, CONNECT_TIMEOUT, REQUEST_TIMEOUT).await?;

        // Health check, router and broker client in one call. Observers only
        // depend on Cassini, which is the default capability; the issue
        // observer's soft state comes with the state actor the builder starts.
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
            state: resources
                .state
                .map(|handle| Arc::new(handle) as Arc<dyn StateClient>),
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

    /// The supervisor's state as `pre_start` makes it, for a site at `url` read with `auth`, its health `health`.
    async fn supervisor_state(
        url: String,
        auth: JiraAuth,
        health: ActorRef<polar_health::HealthCheckMessage>,
    ) -> ObserverSupervisorState {
        ObserverSupervisorState {
            jira_url: url,
            auth,
            deployment: JiraDeployment::ServerOrDataCenter,
            base_interval: 3600,
            query_date: None,
            web_client: Client::new(),
            tcp_client: TcpClient::from_actor_ref(sink().await),
            state: None,
            healthcheck: HealthHandle::new(health),
            infrastructure: sink().await,
            projects: None,
            groups: None,
            users: None,
            issues: None,
        }
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
        let server = site_without_data().await;
        let (health, recorded) = health_recorder().await;
        let mut state = supervisor_state(
            server.uri(),
            JiraAuth::from_parts(None, "t0ken".into()),
            health.clone(),
        )
        .await;
        let myself = sink::<ObserverSupervisorMessage>().await;
        ObserverSupervisor::spawn_observers(&myself, &mut state)
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
