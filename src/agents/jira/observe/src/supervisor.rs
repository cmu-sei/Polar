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
use crate::issues::{AskCursor, CURSOR_LABEL, CURSOR_PROPERTY};
use crate::projects::JiraProjectObserver;
use crate::users::JiraUserObserver;
use crate::{
    BROKER_CLIENT_NAME, JIRA_GROUP_OBSERVER, JIRA_ISSUE_OBSERVER, JIRA_PROJECT_OBSERVER,
    JIRA_USER_OBSERVER, JiraAuth, JiraDeployment, JiraObserverArgs, JiraObserverMessage, RETRY,
};
use cassini_client::{OfflineBehavior, PublishRequest};
use cassini_types::{ClientEvent, WireTraceCtx};
use polar::cassini::{CassiniClient, TcpClient};
use polar::egress::Egress;
use polar_agent_builder::{
    AgentBuilder, Configured, HealthClient, HealthHandle, InfrastructureEvent,
    InfrastructureSupervisorMessage, Subscription,
};
use ractor::concurrency::Duration;
use ractor::{Actor, ActorId, ActorProcessingErr, ActorRef, SupervisionEvent, async_trait};
use reqwest::{Client, ClientBuilder};
use soft_state_common::{ReplyRouter, StateReply, StateRequest};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Where the state agent answers this observer (docs/architecture/soft-state.md): its own topic of the reply family,
/// `soft-state-replies.jira-observer`, which it alone subscribes to.
pub const STATE_REPLIES_TOPIC: &str = "soft-state-replies.jira-observer";

/// How long one question to the state agent is waited for; one that is not answered in time is asked again on the
/// observer's backoff, with a fresh correlation id. Under the observers' stop bound, so a stop is not held up by it.
pub const CURSOR_TIMEOUT: Duration = Duration::from_secs(8);

/// Whether a reply came from the state agent, as the broker names its publisher: replies from anyone else, or from
/// no one the broker vouched for, are dropped.
pub fn from_state_agent(published_by: Option<&str>) -> bool {
    published_by.and_then(cert_issuer_common::identity::workload_of)
        == Some(soft_state_common::OBSERVER_NAME)
}

/// The subscription to this observer's reply topic, handing each reply from the state agent to `router`.
pub fn state_replies(router: Arc<ReplyRouter<StateReply>>) -> Subscription {
    Subscription::new(STATE_REPLIES_TOPIC, move |incoming| {
        if !from_state_agent(incoming.published_by.as_deref()) {
            warn!(
                publisher = incoming.published_by.as_deref().unwrap_or("unverified"),
                "dropped a message on {STATE_REPLIES_TOPIC} that is not the state agent's"
            );
            return;
        }
        if let Err(why) = soft_state_common::deliver(&router, &incoming.payload) {
            warn!("dropped a message on {STATE_REPLIES_TOPIC}: {why}");
        }
    })
}

/// How the issue observer asks the state agent where its first pull starts: `Max { JiraIssue, updated_ms }` on
/// `soft-state-requests` through `client`, a fresh correlation id each time, the reply awaited on `router` for at
/// most [`CURSOR_TIMEOUT`].
pub fn state_asker(router: Arc<ReplyRouter<StateReply>>, client: TcpClient) -> AskCursor {
    Arc::new(move || {
        let router = router.clone();
        let client = client.clone();
        Box::pin(async move {
            soft_state_common::ask(
                &router,
                StateRequest::Max {
                    label: CURSOR_LABEL.to_string(),
                    property: CURSOR_PROPERTY.to_string(),
                },
                || uuid::Uuid::new_v4().to_string(),
                CURSOR_TIMEOUT,
                move |payload| {
                    client
                        .publish(PublishRequest {
                            topic: soft_state_common::REQUESTS_TOPIC.to_string(),
                            payload,
                            trace_ctx: WireTraceCtx::from_current_span(),
                            offline_behavior: OfflineBehavior::default(),
                            share_identity: true,
                        })
                        .map_err(|e| format!("{e:?}"))
                },
            )
            .await
        })
    })
}

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
    /// Where the state agent's replies are handed to the issue observer's question.
    state_replies: Arc<ReplyRouter<StateReply>>,
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
            cursor: Some(state_asker(
                self.state_replies.clone(),
                self.tcp_client.clone(),
            )),
            wall_clock: crate::issues::wall_clock_ms,
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
        // depend on Cassini, which is the default capability; the one
        // subscription is the state agent's replies to the issue observer.
        let state_replies = Arc::new(ReplyRouter::new());
        let resources = AgentBuilder::new(myself.clone(), BROKER_CLIENT_NAME)
            .with_subscription(self::state_replies(state_replies.clone()))
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
            state_replies,
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
                // own. A failure in talking to Jira, a rejected token among
                // them, never stops one (it is retried), so this is something
                // else gone wrong, and a visible pod restart is the signal.
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
    use cassini_client::TcpClientMessage;
    use polar::egress::testing::{TestCa, given, https_server, rerun};
    use soft_state_common::{Answer, Envelope, decode_request, encode_reply};
    use std::sync::Mutex;

    /// Each publish: its topic and its payload.
    type Sent = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

    /// Stands in for the broker client: keeps every publish, and answers a question on the state agent's behalf.
    struct StateAgent {
        sent: Sent,
        answer: Option<(Arc<ReplyRouter<StateReply>>, StateReply)>,
    }

    #[async_trait]
    impl Actor for StateAgent {
        type Msg = TcpClientMessage;
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
            if let TcpClientMessage::Publish { topic, payload, .. } = message {
                self.sent.lock().unwrap().push((topic, payload.clone()));
                if let Some((router, reply)) = &self.answer {
                    let asked = decode_request(&payload).unwrap();
                    let bytes = encode_reply(&Envelope {
                        correlation_id: asked.correlation_id,
                        payload: reply.clone(),
                    });
                    soft_state_common::deliver(router, &bytes).unwrap();
                }
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn the_question_is_the_largest_updated_ms_of_the_issues_with_a_fresh_id_each_time() {
        let router = Arc::new(ReplyRouter::new());
        let sent = Arc::new(Mutex::new(Vec::new()));
        let (client, _) = Actor::spawn(
            None,
            StateAgent {
                sent: sent.clone(),
                answer: Some((router.clone(), StateReply::Value(Some(42)))),
            },
            (),
        )
        .await
        .unwrap();
        let ask = state_asker(router, TcpClient::from_actor_ref(client.clone()));
        assert_eq!(ask().await, Answer::Replied(StateReply::Value(Some(42))));
        assert_eq!(ask().await, Answer::Replied(StateReply::Value(Some(42))));
        let sent = sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 2);
        let ids: Vec<String> = sent
            .iter()
            .map(|(topic, payload)| {
                assert_eq!(topic, soft_state_common::REQUESTS_TOPIC);
                let asked = decode_request(payload).unwrap();
                assert_eq!(
                    asked.payload,
                    StateRequest::Max {
                        label: "JiraIssue".into(),
                        property: "updated_ms".into()
                    }
                );
                asked.correlation_id
            })
            .collect();
        assert_ne!(ids[0], ids[1]);
        client.stop(None);
    }

    #[test]
    fn its_reply_topic_is_its_own_of_the_state_agent_s_family_and_only_the_state_agent_is_heard_on_it()
     {
        assert_eq!(
            STATE_REPLIES_TOPIC,
            soft_state_common::replies_topic(BROKER_CLIENT_NAME)
        );
        assert!(from_state_agent(Some(
            "soft-state-observer.polar.serviceaccount.cluster.local"
        )));
        assert!(!from_state_agent(Some(
            "jira-processor.polar.serviceaccount.cluster.local"
        )));
        assert!(!from_state_agent(None));
    }
    use std::time::Instant;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
