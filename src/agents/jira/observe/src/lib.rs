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
use polar::cassini::TcpClient;
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

/// The endpoints that describe the site rather than hold its data: a body from one of them that does not decode is
/// not a moment's trouble but a site (or something in front of it, such as a sign-in page) that is not the Jira this
/// observer reads.
const CONFIGURATION_ENDPOINTS: &[&str] = &[MYSELF_OP, SERVER_INFO_OP, FIELD_OP];

/// The endpoint that says what the site is: its version and its deployment type.
pub const SERVER_INFO_OP: &str = "/rest/api/2/serverInfo";

/// The endpoint that lists the site's fields, with each one's schema.
pub const FIELD_OP: &str = "/rest/api/2/field";

/// What a failure means for trying again (docs/architecture/jira.md, "The review before 3.0.0", 5). Every kind is
/// still retried on the backoff ([`RETRY`]); the kind is what the observer keeps ([`JiraObserverState::failure`]) and
/// what health is told once it can be (the hook is [`JiraObserverState::settle`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The site could not be reached, took too long, answered 5xx or 429, or the state agent did not say where to
    /// start: trying again may work.
    Transient,
    /// The site refuses the credential, or does not let it read what was asked (401 or 403 after the token was
    /// checked again; anonymously, at once): trying again cannot change that.
    Credentials,
    /// The address, or what answers there, is not a Jira this observer reads: 404, 410 or 400, or a body that does
    /// not decode from an endpoint that describes the site. Trying again cannot change that either.
    Configuration,
}

impl FailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FailureKind::Transient => "transient",
            FailureKind::Credentials => "credentials",
            FailureKind::Configuration => "configuration",
        }
    }

    /// The kind of failure an answer with `status` is.
    pub fn of_status(status: StatusCode) -> Self {
        match status.as_u16() {
            401 | 403 => FailureKind::Credentials,
            400 | 404 | 410 => FailureKind::Configuration,
            _ => FailureKind::Transient,
        }
    }
}

/// An observer's last failure, by kind, with what it said (the request by its path; never the site's address or
/// the credential). Kept until a pull succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

/// A pull that failed in talking to Jira: the site refused or could not be reached, answered
/// with an error status, took too long, sent a body that does not decode, or did not accept
/// the token. The observer logs it and tries again later; it does not fail for it. The message
/// names the request by its path, never the site's address.
#[derive(Debug)]
pub struct JiraError {
    message: String,
    kind: FailureKind,
    /// How long the site asked to be left alone (`Retry-After` on a 429 or a 503).
    retry_after: Option<Duration>,
    token_rejected: bool,
    no_cursor: bool,
}

impl JiraError {
    /// A failure that may pass: [`FailureKind::Transient`].
    pub fn new(message: String) -> Self {
        Self::of_kind(message, FailureKind::Transient)
    }

    pub fn of_kind(message: String, kind: FailureKind) -> Self {
        Self {
            message,
            kind,
            retry_after: None,
            token_rejected: false,
            no_cursor: false,
        }
    }

    /// A request to `path` answered with `status`, and the `Retry-After` it gave, if any.
    pub fn status(path: &str, status: StatusCode, retry_after: Option<Duration>) -> Self {
        Self {
            retry_after,
            ..Self::of_kind(
                format!("{path} returned {status}"),
                FailureKind::of_status(status),
            )
        }
    }

    fn rejected_token(message: String) -> Self {
        Self {
            token_rejected: true,
            ..Self::of_kind(message, FailureKind::Credentials)
        }
    }

    /// The issue observer could not learn where its first pull starts (the state agent did not say): retried as a
    /// failed pull is, without anything having been asked of Jira.
    pub fn no_cursor(message: String) -> Self {
        Self {
            no_cursor: true,
            ..Self::new(message)
        }
    }

    /// True if Jira did not accept the token, as opposed to any other failure.
    pub fn token_rejected(&self) -> bool {
        self.token_rejected
    }

    /// What the failure is, as the request found it: a refused credential is [`FailureKind::Credentials`] here even
    /// before the token has been checked again ([`JiraObserverState::settle`] decides).
    pub fn kind(&self) -> FailureKind {
        self.kind
    }

    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }
}

/// The wait a `Retry-After` header asks for, given the time now: a number of seconds, or an HTTP date (a date
/// already past is no wait). `None` for anything else.
pub fn retry_after(header: &str, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    let header = header.trim();
    if !header.is_empty() && header.bytes().all(|b| b.is_ascii_digit()) {
        return header.parse().ok().map(Duration::from_secs);
    }
    let date = chrono::DateTime::parse_from_rfc2822(header).ok()?;
    Some(
        (date.with_timezone(&chrono::Utc) - now)
            .to_std()
            .unwrap_or(Duration::ZERO),
    )
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JiraDeployment {
    Cloud,
    ServerOrDataCenter,
}

impl JiraDeployment {
    /// The deployment type until the site says (`serverInfo`, [`JiraObserverState::ensure_site`]), and when it does
    /// not: JIRA_DEPLOYMENT ([`settings::parse_deployment`]), else Cloud for an Atlassian-hosted address
    /// (*.atlassian.net), else Server or Data Center.
    pub fn resolve(configured: Option<JiraDeployment>, jira_url: &str) -> Self {
        configured.unwrap_or(if jira_url.contains(".atlassian.net") {
            JiraDeployment::Cloud
        } else {
            JiraDeployment::ServerOrDataCenter
        })
    }

    /// The deployment type `serverInfo`'s `deploymentType` names: `Cloud` is Cloud, anything else (`Server`, as all
    /// four public Data Centers answer, or `DataCenter`) is Server or Data Center. `None` when it names nothing.
    pub fn from_server_info(deployment_type: &str) -> Option<Self> {
        match deployment_type.trim() {
            "" => None,
            t if t.eq_ignore_ascii_case("cloud") => Some(JiraDeployment::Cloud),
            _ => Some(JiraDeployment::ServerOrDataCenter),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            JiraDeployment::Cloud => "Cloud",
            JiraDeployment::ServerOrDataCenter => "Server or Data Center",
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
    /// Whether the token was checked in the pull under way: a credential refused after that is the credential's
    /// ([`FailureKind::Credentials`]), one refused before it may be a token revoked since, checked again next.
    pub token_fresh: bool,
    /// Whether the site has been asked what it is (`serverInfo`), so that `deployment` is what it said, or what the
    /// setting says where it did not say.
    pub deployment_learned: bool,
    /// The last failure, by kind, until a pull succeeds.
    pub failure: Option<Failure>,
    /// The issue observer's pull under way, kept when a page fails so the next attempt goes on from it.
    pub issue_walk: Option<issues::IssueWalk>,
    /// The users observer's walk under way, kept likewise.
    pub user_walk: Option<users::UserWalk>,
    /// The groups observer's walk under way, kept likewise.
    pub group_walk: Option<groups::GroupWalk>,
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
            token_fresh: false,
            deployment_learned: false,
            failure: None,
            issue_walk: None,
            user_walk: None,
            group_walk: None,
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
            return Err(failed_status(&request_path(url), &response).into());
        }

        response.json::<T>().await.map_err(|e| {
            JiraError::of_kind(
                format!(
                    "could not decode the response from {}: {}",
                    request_path(url),
                    error_chain(&e.without_url())
                ),
                decode_failure_kind(url),
            )
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
            return Err(failed_status(MYSELF_OP, &response));
        }

        let myself: Value = response.json().await.map_err(|e| {
            JiraError::of_kind(
                format!(
                    "could not decode the response from {MYSELF_OP}: {}",
                    error_chain(&e.without_url())
                ),
                FailureKind::Configuration,
            )
        })?;
        account_kind(&myself).ok_or_else(|| rejected("returned no user".to_string()))
    }

    /// Checks the token before a pull, unless Jira accepted it and nothing has failed since.
    pub async fn ensure_token(&mut self) -> Result<(), ActorProcessingErr> {
        if !self.token_checked {
            let account = self.check_token().await?;
            info!("Jira accepted the token ({account})");
            self.token_checked = true;
            self.token_fresh = true;
        }
        Ok(())
    }

    /// Asks the site what it is, once: `serverInfo`'s `deploymentType` (`Server` on all four public Data Centers)
    /// decides how it is read, over JIRA_DEPLOYMENT and the address, which are kept where it does not say (an error
    /// status, a body that does not decode, or no `deploymentType`). A site that cannot be reached fails the pull, and
    /// the next asks again.
    pub async fn ensure_site(&mut self) -> Result<(), ActorProcessingErr> {
        if self.deployment_learned {
            return Ok(());
        }
        let response = self
            .send(&format!("{}{SERVER_INFO_OP}", self.jira_url))
            .await?;
        let status = response.status();
        let said = if status.is_success() {
            response
                .json::<Value>()
                .await
                .ok()
                .and_then(|info| info["deploymentType"].as_str().map(str::to_string))
        } else {
            None
        };
        match said.as_deref().and_then(JiraDeployment::from_server_info) {
            Some(deployment) => {
                if deployment != self.deployment {
                    warn!(
                        "the site says it is {} (serverInfo deploymentType {:?}), where JIRA_DEPLOYMENT or the \
                         address said {}: reading it as the site says",
                        deployment.as_str(),
                        said.as_deref().unwrap_or_default(),
                        self.deployment.as_str()
                    );
                } else {
                    info!(
                        "the site says it is {} (serverInfo deploymentType {:?})",
                        deployment.as_str(),
                        said.as_deref().unwrap_or_default()
                    );
                }
                self.deployment = deployment;
            }
            None => warn!(
                "{SERVER_INFO_OP} did not say the deployment type ({status}): reading the site as {}, as \
                 JIRA_DEPLOYMENT or the address says",
                self.deployment.as_str()
            ),
        }
        self.deployment_learned = true;
        Ok(())
    }

    /// The observer's last failure, by kind, until a pull succeeds.
    pub fn failure(&self) -> Option<&Failure> {
        self.failure.as_ref()
    }

    /// What `failure` is for this observer: a credential refused on an endpoint before the token was checked again
    /// in this pull may be a token revoked since its last check, which the next pull checks first, so it is
    /// [`FailureKind::Transient`] until then; after a check it is the credential's.
    fn classify(&self, failure: &JiraError) -> FailureKind {
        match failure.kind {
            FailureKind::Credentials if !failure.token_rejected && !self.token_fresh => {
                FailureKind::Transient
            }
            kind => kind,
        }
    }

    /// Settles a pull and schedules the next, and returns the wait. After a success the next pull is a base interval
    /// away and the last failure is cleared. A failure in talking to Jira ([`JiraError`]) is kept by kind
    /// ([`JiraObserverState::failure`]) and logged (a kind a retry cannot fix at error, with what to check), the
    /// token is checked again before the next pull, and that pull is the retry's next wait away, or what the site's
    /// `Retry-After` asked if that is longer (never more than the base interval): Jira being unreachable is not the
    /// agent being unhealthy, so the observer stays up. Any other error is returned and fails the observer, as before.
    ///
    /// HEALTH HOOK (pulled health, the soft-state agents' workstream): this is where an observer's failure is decided
    /// and cleared. Once the builder's health API takes a domain failure, the observer is given the agent's
    /// `HealthHandle` in [`JiraObserverArgs`] and reports `self.failure` here: set with its kind and its message on a
    /// failure, cleared on a success.
    pub fn settle(
        &mut self,
        myself: &ActorRef<JiraObserverMessage>,
        observer: &str,
        pulled: Result<(), ActorProcessingErr>,
        tick: fn() -> JiraObserverMessage,
    ) -> Result<Duration, ActorProcessingErr> {
        let wait = match pulled {
            Ok(()) => {
                if self.retry.failures() > 0 {
                    info!(
                        "{observer}: pulled from Jira after {} failed attempts",
                        self.retry.failures()
                    );
                }
                self.failure = None;
                self.retry.next(true, self.base_interval)
            }
            Err(e) => {
                let Some(failure) = e.downcast_ref::<JiraError>() else {
                    return Err(e);
                };
                let attempt = self.retry.failures() + 1;
                let backoff = self.retry.next(false, self.base_interval);
                let wait = match failure.retry_after {
                    Some(asked) => backoff.max(asked.min(self.base_interval)),
                    None => backoff,
                };
                let kind = self.classify(failure);
                self.failure = Some(Failure {
                    kind,
                    message: failure.to_string(),
                });
                if failure.no_cursor {
                    // Nothing was asked of Jira, so the token stands as it was checked.
                    warn!(
                        "{observer}: not pulling yet: {failure}. Asking again in {wait:?} (attempt {attempt})"
                    );
                    self.token_fresh = false;
                    myself.send_after(wait, tick);
                    return Ok(wait);
                }
                self.token_checked = false;
                match kind {
                    FailureKind::Transient => warn!(
                        "{observer}: pull from Jira failed: {failure}. Trying again in {wait:?} \
                         (attempt {attempt})"
                    ),
                    FailureKind::Credentials | FailureKind::Configuration => error!(
                        "{observer}: {failure}. A retry cannot fix this ({}): {}. Trying again in {wait:?} \
                         (attempt {attempt})",
                        kind.as_str(),
                        match kind {
                            FailureKind::Credentials =>
                                "the site refuses the credential, or does not let it read this; check the token, \
                                 the auth mode and the account's permissions",
                            _ =>
                                "check JIRA_URL, and that the site is a Jira Server, Data Center or Cloud this \
                                  observer can read",
                        }
                    ),
                }
                wait
            }
        };
        self.token_fresh = false;
        myself.send_after(wait, tick);
        Ok(wait)
    }
}

/// The failure an answer that is not a success is, with the `Retry-After` it gave on a 429 or a 503.
fn failed_status(path: &str, response: &reqwest::Response) -> JiraError {
    let status = response.status();
    let asked = (status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::SERVICE_UNAVAILABLE)
        .then(|| response.headers().get(reqwest::header::RETRY_AFTER))
        .flatten()
        .and_then(|v| v.to_str().ok())
        .and_then(|v| retry_after(v, chrono::Utc::now()));
    JiraError::status(path, status, asked)
}

/// What a body that does not decode means from `url`: the site's configuration from an endpoint that describes the
/// site, a moment's trouble from any other.
fn decode_failure_kind(url: &str) -> FailureKind {
    let path = Url::parse(url)
        .map(|u| u.path().to_string())
        .unwrap_or_default();
    if CONFIGURATION_ENDPOINTS.iter().any(|op| path.ends_with(op)) {
        FailureKind::Configuration
    } else {
        FailureKind::Transient
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
/// actor adds the broker framing and the registration itself. It waits
/// while the client's outbox is full, so a broker that falls behind slows
/// the pull instead of failing it (docs/architecture/cassini-backpressure.md).
pub async fn publish_data(
    tcp_client: &TcpClient,
    topic: &str,
    data: &JiraData,
) -> Result<(), ActorProcessingErr> {
    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(data)
        .map_err(|e| ActorProcessingErr::from(format!("failed to serialize {topic} payload: {e}")))?
        .into_vec();

    tcp_client
        .publish_when_ready(PublishRequest {
            topic: topic.to_string(),
            trace_ctx: WireTraceCtx::from_current_span(),
            payload,
            offline_behavior: OfflineBehavior::default(),
            share_identity: true,
        })
        .await
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
    /// The broker ended this agent's session while it was away (`ClientEvent::SessionEnded`): what it was owed is
    /// lost. The issue observer's next pull starts from the graph's cursor again, as its first did; the others pull
    /// everything each time, and need nothing.
    Resync,
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

    /// What the status of a failed request means for trying again.
    #[tokio::test]
    async fn a_failed_request_is_classified_by_what_a_retry_could_change() {
        use FailureKind::*;
        for (status, kind) in [
            (401, Credentials),
            (403, Credentials),
            (404, Configuration),
            (410, Configuration),
            (400, Configuration),
            (429, Transient),
            (500, Transient),
            (502, Transient),
            (503, Transient),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            let s = state(server.uri(), bearer()).await;
            let error = s
                .get_json::<Value>(&format!("{}/rest/api/2/project", server.uri()))
                .await
                .unwrap_err();
            let error = error.downcast_ref::<JiraError>().unwrap();
            assert_eq!(error.kind(), kind, "{status}");
        }
    }

    /// A body that does not decode is the site's configuration when it comes from an endpoint that describes the
    /// site (its fields, its version, the credential's user); from a search it may be a moment's trouble.
    #[tokio::test]
    async fn a_malformed_body_is_configuration_only_from_an_endpoint_that_describes_the_site() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>sign in</html>"))
            .mount(&server)
            .await;
        let s = state(server.uri(), bearer()).await;
        for (endpoint, kind) in [
            ("/rest/api/2/field", FailureKind::Configuration),
            ("/rest/api/2/serverInfo", FailureKind::Configuration),
            ("/rest/api/2/search", FailureKind::Transient),
        ] {
            let error = s
                .get_json::<Value>(&format!("{}{endpoint}", server.uri()))
                .await
                .unwrap_err();
            assert_eq!(
                error.downcast_ref::<JiraError>().unwrap().kind(),
                kind,
                "{endpoint}"
            );
        }
        let error = s.check_token().await.unwrap_err();
        assert_eq!(error.kind(), FailureKind::Configuration);
    }

    #[test]
    fn retry_after_is_read_as_seconds_or_as_a_date() {
        let now = chrono::DateTime::parse_from_rfc2822("Wed, 21 Oct 2015 07:28:00 GMT")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(retry_after("120", now), Some(Duration::from_secs(120)));
        assert_eq!(retry_after(" 0 ", now), Some(Duration::ZERO));
        assert_eq!(
            retry_after("Wed, 21 Oct 2015 07:30:30 GMT", now),
            Some(Duration::from_secs(150))
        );
        // A date already past is now.
        assert_eq!(
            retry_after("Wed, 21 Oct 2015 07:00:00 GMT", now),
            Some(Duration::ZERO)
        );
        for nonsense in ["", "soon", "-5", "1.5"] {
            assert_eq!(retry_after(nonsense, now), None, "{nonsense:?}");
        }
    }

    /// Takes the observer messages a settled pull schedules, and drops them.
    struct Ticks;

    #[async_trait]
    impl Actor for Ticks {
        type Msg = JiraObserverMessage;
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

    fn tick() -> JiraObserverMessage {
        JiraObserverMessage::Resync
    }

    /// The observer keeps what its last failure was, by kind, until a pull succeeds. A credential the site refuses on
    /// an endpoint is the credential's only once the token has been checked again: before that it may be a token
    /// revoked since its check, and the next pull checks it first.
    #[tokio::test]
    async fn the_observer_keeps_the_kind_of_its_last_failure_until_a_pull_succeeds() {
        let (ticks, _) = Actor::spawn(None, Ticks, ()).await.unwrap();
        let mut s = state("http://jira.invalid".into(), bearer()).await;
        let forbidden = || -> ActorProcessingErr {
            JiraError::status("/rest/api/2/project", StatusCode::FORBIDDEN, None).into()
        };

        // The token was checked in this very pull: the refusal is the credential's.
        s.token_checked = true;
        s.token_fresh = true;
        s.settle(&ticks, "test", Err(forbidden()), tick).unwrap();
        assert_eq!(s.failure().map(|f| f.kind), Some(FailureKind::Credentials));
        assert!(!s.token_checked, "checked again before the next pull");

        // A success clears it.
        s.settle(&ticks, "test", Ok(()), tick).unwrap();
        assert!(s.failure().is_none());

        // The token was last checked in an earlier pull: not yet the credential's, but a moment's.
        s.token_checked = true;
        s.token_fresh = false;
        s.settle(&ticks, "test", Err(forbidden()), tick).unwrap();
        assert_eq!(s.failure().map(|f| f.kind), Some(FailureKind::Transient));

        // A site that is down, and a 404, as they are.
        s.settle(
            &ticks,
            "test",
            Err(JiraError::status("/rest/api/2/project", StatusCode::BAD_GATEWAY, None).into()),
            tick,
        )
        .unwrap();
        assert_eq!(s.failure().map(|f| f.kind), Some(FailureKind::Transient));
        s.settle(
            &ticks,
            "test",
            Err(JiraError::status("/rest/api/2/project", StatusCode::NOT_FOUND, None).into()),
            tick,
        )
        .unwrap();
        let failure = s.failure().unwrap();
        assert_eq!(failure.kind, FailureKind::Configuration);
        assert!(
            failure.message.contains("/rest/api/2/project"),
            "{}",
            failure.message
        );
        ticks.stop(None);
    }

    /// The wait after a failure is the backoff's, or what the site's `Retry-After` said if that is longer, but never
    /// longer than the base interval.
    #[tokio::test]
    async fn retry_after_lengthens_the_wait_up_to_the_base_interval() {
        let (ticks, _) = Actor::spawn(None, Ticks, ()).await.unwrap();
        let mut s = state("http://jira.invalid".into(), bearer()).await;
        let limited = |secs: u64| -> ActorProcessingErr {
            JiraError::status(
                "/rest/api/2/search",
                StatusCode::TOO_MANY_REQUESTS,
                Some(Duration::from_secs(secs)),
            )
            .into()
        };
        assert_eq!(
            s.settle(&ticks, "test", Err(limited(120)), tick).unwrap(),
            Duration::from_secs(120)
        );
        // The backoff's second wait (60 s) is longer than one second.
        assert_eq!(
            s.settle(&ticks, "test", Err(limited(1)), tick).unwrap(),
            Duration::from_secs(60)
        );
        assert_eq!(
            s.settle(&ticks, "test", Err(limited(86_400)), tick)
                .unwrap(),
            Duration::from_secs(1800)
        );
        ticks.stop(None);
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
