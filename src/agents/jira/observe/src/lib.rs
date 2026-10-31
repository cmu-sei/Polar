#![deny(clippy::disallowed_methods)]

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

pub mod agent;
pub mod groups;
pub mod issues;
pub mod projects;
pub mod settings;
pub mod supervisor;
pub mod users;

use cassini_client::facade::Backoff;
use cassini_client::{OfflineBehavior, PublishRequest};
use cassini_types::WireTraceCtx;
use jira_common::types::JiraData;
use polar::cassini::{CassiniClient, TcpClient};
use ractor::concurrency::Duration;
use ractor::{ActorProcessingErr, ActorRef};
use reqwest::{Client, RequestBuilder, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::time::Instant;
use tracing::{debug, error, info, warn};

const JIRA_PROJECT_OBSERVER: &str = "jira.observer.projects";
const JIRA_GROUP_OBSERVER: &str = "jira.observer.groups";
const JIRA_USER_OBSERVER: &str = "jira.observer.users";
const JIRA_ISSUE_OBSERVER: &str = "jira.observer.issues";
pub const BROKER_CLIENT_NAME: &str = "jira-observer";

/// The wait before an observer pulls again after a pull that failed: 30 s, doubling with each
/// failure in a row up to 15 minutes.
pub const RETRY: Backoff = Backoff {
    first: Duration::from_secs(30),
    last: Duration::from_secs(15 * 60),
};

/// The endpoint that says who a credential is.
const MYSELF_OP: &str = "/rest/api/2/myself";

/// What is logged when Jira does not accept the token.
const TOKEN_REJECTED: &str = "Jira did not accept the token: the request was treated as \
    anonymous. Check that the token is the raw token (not base64-encoded) and that the auth mode \
    matches the site: Data Center/Server personal access token = no JIRA_EMAIL (Bearer); Jira \
    Cloud API token = JIRA_EMAIL set (Basic)";

/// A pull that failed in talking to Jira: the site refused or could not be reached, answered
/// with an error status, took too long, sent a body that does not decode, or did not accept
/// the token. The observer logs it and tries again later; it does not fail for it. The message
/// names the request by its path, never the site's address.
#[derive(Debug)]
pub struct JiraError {
    message: String,
    token_rejected: bool,
    no_cursor: bool,
}

impl JiraError {
    pub fn new(message: String) -> Self {
        Self {
            message,
            token_rejected: false,
            no_cursor: false,
        }
    }

    fn rejected_token(message: String) -> Self {
        Self {
            message,
            token_rejected: true,
            no_cursor: false,
        }
    }

    /// The issue observer could not learn where its first pull starts (the state agent did not say): retried as a
    /// failed pull is, without anything having been asked of Jira.
    pub fn no_cursor(message: String) -> Self {
        Self {
            message,
            token_rejected: false,
            no_cursor: true,
        }
    }

    /// True if Jira did not accept the token, as opposed to any other failure.
    pub fn token_rejected(&self) -> bool {
        self.token_rejected
    }
}

impl std::fmt::Display for JiraError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for JiraError {}

/// When an observer pulls next. After a pull that succeeded, a base interval later; after one
/// that failed, the backoff's wait for that many failures in a row, which doubles from its
/// first to its last and starts from its first again after a success.
#[derive(Debug, Clone)]
pub struct Retry {
    backoff: Backoff,
    failures: u32,
}

impl Retry {
    pub fn new(backoff: Backoff) -> Self {
        Self {
            backoff,
            failures: 0,
        }
    }

    /// How many pulls in a row have failed.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// The wait before the next pull, after one that `succeeded` or did not.
    pub fn next(&mut self, succeeded: bool, base_interval: Duration) -> Duration {
        if succeeded {
            self.failures = 0;
            base_interval
        } else {
            let wait = self.backoff.delay(self.failures);
            self.failures = self.failures.saturating_add(1);
            wait
        }
    }
}

/// Authentication mode for the Jira REST API.
///
/// - `Basic` is what Atlassian Cloud expects: HTTP Basic Auth with the
///   account email as username and an API token as password.
/// - `Bearer` covers on-prem / Server / Data Center deployments (and
///   Cloud OAuth 2.0 3LO access tokens), which use a plain
///   `Authorization: Bearer <token>` header. This preserves today's
///   behavior for existing on-prem deployments.
#[derive(Clone)]
pub enum JiraAuth {
    Basic { email: String, token: String },
    Bearer { token: String },
}

impl JiraAuth {
    /// The mode, as a log names it, with the setting that chose it.
    fn mode(&self) -> &'static str {
        match self {
            JiraAuth::Basic { .. } => "Basic, since JIRA_EMAIL is set",
            JiraAuth::Bearer { .. } => "Bearer, since JIRA_EMAIL is not set",
        }
    }

    /// Resolve auth mode from configuration. An email present => Basic (Cloud-style); absent
    /// or empty => Bearer (on-prem/Server/DC-style).
    pub fn from_parts(email: Option<String>, token: String) -> Self {
        match email {
            Some(email) if !email.is_empty() => JiraAuth::Basic { email, token },
            _ => JiraAuth::Bearer { token },
        }
    }

    /// Apply this auth mode to a reqwest RequestBuilder.
    pub fn apply(&self, req: RequestBuilder) -> RequestBuilder {
        match self {
            JiraAuth::Basic { email, token } => req.basic_auth(email, Some(token)),
            JiraAuth::Bearer { token } => req.bearer_auth(token),
        }
    }
}

/// Which Jira deployment type this observer is talking to. Determines
/// which search API and pagination model to use — Cloud removed the
/// classic /rest/api/2/search endpoint (see
/// https://developer.atlassian.com/changelog/#CHANGE-2046) in favor of
/// /rest/api/3/search/jql with cursor-based pagination, while
/// Server/Data Center still uses the v2 endpoint with startAt/total
/// offset pagination.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum JiraDeployment {
    Cloud,
    ServerOrDataCenter,
}

impl JiraDeployment {
    /// Resolve deployment type from configuration. Defaults to
    /// ServerOrDataCenter (today's existing behavior) unless explicitly
    /// overridden with JIRA_DEPLOYMENT=cloud, or the URL itself looks
    /// like an Atlassian-hosted Cloud site (*.atlassian.net).
    pub fn resolve(configured: Option<&str>, jira_url: &str) -> Self {
        match configured {
            Some(v) if v.eq_ignore_ascii_case("cloud") => JiraDeployment::Cloud,
            Some(v) if v.eq_ignore_ascii_case("server") || v.eq_ignore_ascii_case("datacenter") => {
                JiraDeployment::ServerOrDataCenter
            }
            _ => {
                if jira_url.contains(".atlassian.net") {
                    JiraDeployment::Cloud
                } else {
                    JiraDeployment::ServerOrDataCenter
                }
            }
        }
    }
}

/// General state for all jira observers
pub struct JiraObserverState {
    /// Endpoint of Jira instance
    pub jira_url: String,
    /// Auth mode + credentials for the Jira REST API
    pub auth: JiraAuth,
    pub deployment: JiraDeployment,
    /// HTTP client
    pub web_client: Client,
    /// Handle to the agent's broker client. The infrastructure supervisor
    /// owns the client; observers publish through this handle and nothing
    /// looks it up by name.
    pub tcp_client: TcpClient,
    /// Time between polls of Jira
    pub base_interval: Duration,
    /// Set by the issue observer once a full pull has completed, after
    /// which it only pulls what changed. The other observers ignore it.
    pub full_pull_done: bool,
    /// A date the first full pull of issues starts from (`QUERY_DATE`); only the issue
    /// observer reads it.
    pub query_date: Option<String>,
    /// Set by the issue observer when a pull completes, to when that pull started: the next
    /// incremental pull asks for everything updated since then, so a run of failed pulls
    /// leaves no gap. The other observers ignore it.
    pub last_pull_started: Option<Instant>,
    /// The time, for the issue window. `Instant::now` except in a test.
    pub clock: fn() -> Instant,
    /// When this observer pulls next.
    pub retry: Retry,
    /// How the issue observer asks the state agent where its first pull starts; the other observers ignore it.
    pub cursor: Option<issues::AskCursor>,
    /// Where the issue observer's first pull starts, once the state agent has said.
    pub start: Option<issues::Start>,
    /// The time in epoch milliseconds, for the issue cursor: the system clock except in a test.
    pub wall_clock: fn() -> i64,
    /// Whether Jira accepted the token at the last check. It is checked before the first pull
    /// and again before the pull after any failure.
    pub token_checked: bool,
}

/// Renders an error together with every cause beneath it. reqwest reports
/// only "error sending request" at the top; the reason (DNS, TLS, refused,
/// timed out) is in a source, and without it a network failure cannot be
/// told apart from any other.
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        source = cause.source();
    }
    out
}

impl JiraObserverState {
    /// Create a new JiraObserverState
    pub fn new(
        jira_url: String,
        auth: JiraAuth,
        deployment: JiraDeployment,
        web_client: Client,
        tcp_client: TcpClient,
        base_interval: Duration,
    ) -> Self {
        JiraObserverState {
            jira_url,
            auth,
            deployment,
            web_client,
            tcp_client,
            base_interval,
            full_pull_done: false,
            query_date: None,
            last_pull_started: None,
            clock: Instant::now,
            retry: Retry::new(RETRY),
            token_checked: false,
            cursor: None,
            start: None,
            wall_clock: issues::wall_clock_ms,
        }
    }

    /// How the issue observer asks where its first pull starts, and the wall clock it measures from.
    pub fn with_cursor(
        mut self,
        cursor: Option<issues::AskCursor>,
        wall_clock: fn() -> i64,
    ) -> Self {
        self.cursor = cursor;
        self.wall_clock = wall_clock;
        self
    }

    /// The date the first full pull starts from.
    pub fn with_query_date(mut self, query_date: Option<String>) -> Self {
        self.query_date = query_date;
        self
    }

    /// The backoff a failed pull is retried on, and the clock.
    pub fn with_retry(mut self, backoff: Backoff, clock: fn() -> Instant) -> Self {
        self.retry = Retry::new(backoff);
        self.clock = clock;
        self
    }

    /// Sends a GET with this agent's credentials. A failure to send is a [`JiraError`] naming
    /// the request by its path; the whole error, address included, is logged at debug.
    async fn send(&self, url: &str) -> Result<reqwest::Response, JiraError> {
        self.auth
            .apply(self.web_client.get(url))
            .send()
            .await
            .map_err(|e| {
                debug!("GET {url} failed: {}", error_chain(&e));
                JiraError::new(format!(
                    "request to {} failed: {}",
                    request_path(url),
                    error_chain(&e.without_url())
                ))
            })
    }

    /// GETs a Jira URL with this agent's credentials and decodes the JSON
    /// body. Anything other than a success status is an error: a revoked
    /// token or a rejected query otherwise comes back as a JSON error
    /// body that decodes fine and reads as an empty result. Every failure
    /// is a [`JiraError`].
    pub async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, ActorProcessingErr> {
        let response = self.send(url).await?;

        let status = response.status();
        if !status.is_success() {
            return Err(JiraError::new(format!("{} returned {status}", request_path(url))).into());
        }

        response.json::<T>().await.map_err(|e| {
            JiraError::new(format!(
                "could not decode the response from {}: {}",
                request_path(url),
                error_chain(&e.without_url())
            ))
            .into()
        })
    }

    /// Asks Jira who the token is (`/rest/api/2/myself`) and refuses an answer that is not a
    /// user. Data Center treats a token it does not accept as anonymous: `/myself` answers 401
    /// with `X-AUSERNAME: anonymous`, and every other endpoint answers as if to nobody (an
    /// empty list of projects), which reads as an empty site. Returns the kind of account, for
    /// the log; never who it is.
    pub async fn check_token(&self) -> Result<String, JiraError> {
        let url = format!("{}{MYSELF_OP}", self.jira_url);
        let response = self.send(&url).await?;

        let status = response.status();
        let anonymous = response
            .headers()
            .get("x-ausername")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("anonymous"));
        let rejected = |why: String| {
            JiraError::rejected_token(format!(
                "{TOKEN_REJECTED}. This observer sends it as {}; {MYSELF_OP} {why}",
                self.auth.mode()
            ))
        };

        if status == StatusCode::UNAUTHORIZED
            || status == StatusCode::FORBIDDEN
            || (status.is_success() && anonymous)
        {
            return Err(rejected(format!("returned {status}")));
        }
        if !status.is_success() {
            return Err(JiraError::new(format!("{MYSELF_OP} returned {status}")));
        }

        let myself: Value = response.json().await.map_err(|e| {
            JiraError::new(format!(
                "could not decode the response from {MYSELF_OP}: {}",
                error_chain(&e.without_url())
            ))
        })?;
        account_kind(&myself).ok_or_else(|| rejected("returned no user".to_string()))
    }

    /// Checks the token before a pull, unless Jira accepted it and nothing has failed since.
    pub async fn ensure_token(&mut self) -> Result<(), ActorProcessingErr> {
        if !self.token_checked {
            let account = self.check_token().await?;
            info!("Jira accepted the token ({account})");
            self.token_checked = true;
        }
        Ok(())
    }

    /// Settles a pull and schedules the next. After a success the next pull is a base interval
    /// away. A failure in talking to Jira ([`JiraError`]) is logged, the token is checked again
    /// before the next pull, and that pull is the retry's next wait away: Jira being
    /// unreachable is not the agent being unhealthy, so the observer stays up. Any other error
    /// is returned and fails the observer, as before.
    pub fn settle(
        &mut self,
        myself: &ActorRef<JiraObserverMessage>,
        observer: &str,
        pulled: Result<(), ActorProcessingErr>,
        tick: fn() -> JiraObserverMessage,
    ) -> Result<(), ActorProcessingErr> {
        let wait = match pulled {
            Ok(()) => {
                if self.retry.failures() > 0 {
                    info!(
                        "{observer}: pulled from Jira after {} failed attempts",
                        self.retry.failures()
                    );
                }
                self.retry.next(true, self.base_interval)
            }
            Err(e) => {
                let Some(failure) = e.downcast_ref::<JiraError>() else {
                    return Err(e);
                };
                let attempt = self.retry.failures() + 1;
                let wait = self.retry.next(false, self.base_interval);
                if failure.no_cursor {
                    // Nothing was asked of Jira, so the token stands as it was checked.
                    warn!(
                        "{observer}: not pulling yet: {failure}. Asking again in {wait:?} (attempt {attempt})"
                    );
                    myself.send_after(wait, tick);
                    return Ok(());
                }
                self.token_checked = false;
                if failure.token_rejected() {
                    error!("{observer}: {failure}. Trying again in {wait:?} (attempt {attempt})");
                } else {
                    warn!(
                        "{observer}: pull from Jira failed: {failure}. Trying again in {wait:?} \
                         (attempt {attempt})"
                    );
                }
                wait
            }
        };
        myself.send_after(wait, tick);
        Ok(())
    }
}

/// The part of a URL after the host, which is what an error names, so that the site's
/// address stays out of the log.
fn request_path(url: &str) -> String {
    Url::parse(url)
        .map(|u| u[url::Position::BeforePath..].to_string())
        .unwrap_or_else(|_| "the request".to_string())
}

/// The kind of account Jira says a credential is, for the log: Cloud names it
/// (`accountType`), Data Center and Server only return the user. None if the answer holds no
/// user.
fn account_kind(myself: &Value) -> Option<String> {
    if let Some(kind) = myself["accountType"].as_str() {
        return Some(format!("{kind} account"));
    }
    ["accountId", "key", "name"]
        .iter()
        .any(|field| myself[field].as_str().is_some_and(|v| !v.is_empty()))
        .then(|| "a user account".to_string())
}

/// Arguments taken in by jira observers
#[derive(Clone)]
pub struct JiraObserverArgs {
    pub jira_url: String,
    pub auth: JiraAuth,
    pub deployment: JiraDeployment,
    pub tcp_client: TcpClient,
    pub web_client: Client,
    pub base_interval: u64,
    pub query_date: Option<String>,
    /// The wait before a failed pull is tried again: [`RETRY`], or a short one in a test.
    pub retry: Backoff,
    /// The time: `Instant::now`, or a test's clock.
    pub clock: fn() -> Instant,
    /// How the issue observer reads where its first pull starts: the state actor the builder gives the agent.
    pub cursor: Option<issues::AskCursor>,
    /// The time in epoch milliseconds: the system clock, or a test's.
    pub wall_clock: fn() -> i64,
}

/// Publishes one JiraData value on a topic through the agent's client.
/// The payload is the serialized JiraData and nothing else: the client
/// actor adds the broker framing and the registration itself.
pub fn publish_data(
    tcp_client: &TcpClient,
    topic: &str,
    data: &JiraData,
) -> Result<(), ActorProcessingErr> {
    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(data)
        .map_err(|e| ActorProcessingErr::from(format!("failed to serialize {topic} payload: {e}")))?
        .into_vec();

    tcp_client
        .publish(PublishRequest {
            topic: topic.to_string(),
            trace_ctx: WireTraceCtx::from_current_span(),
            payload,
            offline_behavior: OfflineBehavior::default(),
            share_identity: true,
        })
        .map_err(|e| ActorProcessingErr::from(format!("failed to publish on {topic}: {e:?}")))
}

pub enum Command {
    GetProjects(String),
    GetGroups(String),
    GetUsers(String),
    GetIssues(String),
}

/// Messages that observers send themselves to prompt the retrieval of resources
pub enum JiraObserverMessage {
    Tick(Command),
}

#[cfg(test)]
mod tests {
    use super::*;
    use cassini_client::TcpClientMessage;
    use ractor::{Actor, async_trait};
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Takes the broker client's messages and drops them.
    struct Nowhere;

    #[async_trait]
    impl Actor for Nowhere {
        type Msg = TcpClientMessage;
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

    async fn state(url: String, auth: JiraAuth) -> JiraObserverState {
        let (nowhere, _) = Actor::spawn(None, Nowhere, ()).await.unwrap();
        JiraObserverState::new(
            url,
            auth,
            JiraDeployment::ServerOrDataCenter,
            Client::new(),
            TcpClient::from_actor_ref(nowhere),
            Duration::from_secs(1800),
        )
    }

    fn bearer() -> JiraAuth {
        JiraAuth::from_parts(None, "t0ken".into())
    }

    /// A site whose `/myself` answers with `response`.
    async fn myself_answers(response: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(MYSELF_OP))
            .respond_with(response)
            .mount(&server)
            .await;
        server
    }

    fn host_of(server: &MockServer) -> String {
        server.uri().trim_start_matches("http://").to_string()
    }

    #[tokio::test]
    async fn a_token_treated_as_anonymous_is_refused_with_what_to_check() {
        let server =
            myself_answers(ResponseTemplate::new(401).insert_header("x-ausername", "anonymous"))
                .await;
        let error = state(server.uri(), bearer())
            .await
            .check_token()
            .await
            .unwrap_err();

        assert!(error.token_rejected(), "{error}");
        let message = error.to_string();
        assert!(message.starts_with(TOKEN_REJECTED), "{message}");
        assert!(
            message.contains("Bearer, since JIRA_EMAIL is not set"),
            "{message}"
        );
        assert!(message.contains("401"), "{message}");
        assert!(!message.contains(&host_of(&server)), "{message}");
        assert!(!message.contains("t0ken"), "{message}");
    }

    #[tokio::test]
    async fn a_success_that_names_the_anonymous_user_is_refused() {
        let server = myself_answers(
            ResponseTemplate::new(200)
                .insert_header("X-AUSERNAME", "anonymous")
                .set_body_json(json!({ "name": "anonymous" })),
        )
        .await;
        let error = state(server.uri(), bearer())
            .await
            .check_token()
            .await
            .unwrap_err();
        assert!(error.token_rejected(), "{error}");
    }

    #[tokio::test]
    async fn a_forbidden_token_or_an_answer_with_no_user_is_refused() {
        for response in [
            ResponseTemplate::new(403),
            ResponseTemplate::new(200).set_body_json(json!({})),
        ] {
            let server = myself_answers(response).await;
            let error = state(server.uri(), bearer())
                .await
                .check_token()
                .await
                .unwrap_err();
            assert!(error.token_rejected(), "{error}");
        }
    }

    #[tokio::test]
    async fn an_accepted_token_is_described_by_its_kind_of_account_only() {
        let server = myself_answers(ResponseTemplate::new(200).set_body_json(json!({
            "name": "svc-polar", "key": "JIRAUSER10100", "emailAddress": "svc@example.org"
        })))
        .await;
        let account = state(server.uri(), bearer())
            .await
            .check_token()
            .await
            .unwrap();
        assert_eq!(account, "a user account");

        let server = myself_answers(ResponseTemplate::new(200).set_body_json(json!({
            "accountId": "5b10a2844c20165700ede21g", "accountType": "atlassian"
        })))
        .await;
        let account = state(server.uri(), bearer())
            .await
            .check_token()
            .await
            .unwrap();
        assert_eq!(account, "atlassian account");
    }

    #[tokio::test]
    async fn a_site_that_is_down_is_a_failure_but_not_a_refused_token() {
        let server = myself_answers(ResponseTemplate::new(503)).await;
        let error = state(server.uri(), bearer())
            .await
            .check_token()
            .await
            .unwrap_err();
        assert!(!error.token_rejected(), "{error}");
        assert!(error.to_string().contains("503"), "{error}");
    }

    #[tokio::test]
    async fn a_failed_request_names_its_path_and_not_the_site() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let s = state(server.uri(), bearer()).await;
        let url = format!("{}/rest/api/2/project", server.uri());
        let error = s.get_json::<Value>(&url).await.unwrap_err();
        let message = error.to_string();
        assert!(error.downcast_ref::<JiraError>().is_some(), "{message}");
        assert!(message.contains("/rest/api/2/project"), "{message}");
        assert!(!message.contains(&host_of(&server)), "{message}");

        // Nothing listens on a port that was bound and let go, so the connection is refused.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let gone = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let s = state(gone.clone(), bearer()).await;
        let error = s
            .get_json::<Value>(&format!("{gone}/rest/api/2/project"))
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(error.downcast_ref::<JiraError>().is_some(), "{message}");
        assert!(
            !message.contains(gone.trim_start_matches("http://")),
            "{message}"
        );

        let s = state(gone.clone(), bearer()).await;
        let error = s.check_token().await.unwrap_err();
        assert!(
            !error
                .to_string()
                .contains(gone.trim_start_matches("http://")),
            "{error}"
        );
    }

    #[test]
    fn a_failed_pull_waits_longer_each_time_up_to_the_last_and_a_success_resets_it() {
        let base = Duration::from_secs(1800);
        let mut retry = Retry::new(RETRY);
        let waits: Vec<u64> = (0..7).map(|_| retry.next(false, base).as_secs()).collect();
        assert_eq!(waits, [30, 60, 120, 240, 480, 900, 900]);
        assert_eq!(retry.failures(), 7);
        assert_eq!(retry.next(true, base), base);
        assert_eq!(retry.failures(), 0);
        assert_eq!(retry.next(false, base), Duration::from_secs(30));
    }
}
