#![deny(clippy::disallowed_methods)]

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

pub mod agent;
pub mod groups;
pub mod issues;
pub mod projects;
pub mod settings;
pub mod supervisor;
pub mod users;

use cassini_client::{OfflineBehavior, PublishRequest};
use cassini_types::WireTraceCtx;
use jira_common::types::JiraData;
use polar::cassini::{CassiniClient, TcpClient};
use ractor::ActorProcessingErr;
use ractor::concurrency::Duration;
use reqwest::{Client, RequestBuilder};
use serde::de::DeserializeOwned;

const JIRA_PROJECT_OBSERVER: &str = "jira.observer.projects";
const JIRA_GROUP_OBSERVER: &str = "jira.observer.groups";
const JIRA_USER_OBSERVER: &str = "jira.observer.users";
const JIRA_ISSUE_OBSERVER: &str = "jira.observer.issues";
pub const BROKER_CLIENT_NAME: &str = "jira:observer:web_client";

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
        }
    }

    /// The date the first full pull starts from.
    pub fn with_query_date(mut self, query_date: Option<String>) -> Self {
        self.query_date = query_date;
        self
    }

    /// GETs a Jira URL with this agent's credentials and decodes the JSON
    /// body. Anything other than a success status is an error: a revoked
    /// token or a rejected query otherwise comes back as a JSON error
    /// body that decodes fine and reads as an empty result.
    pub async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, ActorProcessingErr> {
        let response = self
            .auth
            .apply(self.web_client.get(url))
            .send()
            .await
            .map_err(|e| {
                ActorProcessingErr::from(format!("request to {url} failed: {}", error_chain(&e)))
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(ActorProcessingErr::from(format!(
                "{url} returned {status}"
            )));
        }

        response.json::<T>().await.map_err(|e| {
            ActorProcessingErr::from(format!(
                "could not decode the response from {url}: {}",
                error_chain(&e)
            ))
        })
    }
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
