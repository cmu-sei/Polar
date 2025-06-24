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
pub mod groups;
pub mod meta;
pub mod pipelines;
pub mod projects;
pub mod repositories;
pub mod runners;
pub mod supervisor;
pub mod users;

use cynic::{GraphQlError, Operation};
use gitlab_queries::groups::*;
use gitlab_queries::projects::{MultiProjectQuery, MultiProjectQueryArguments};
use gitlab_queries::runners::MultiRunnerQuery;
use gitlab_queries::runners::MultiRunnerQueryArguments;
use gitlab_queries::users::{MultiUserQuery, MultiUserQueryArguments};
use gitlab_schema::IdString;
use parse_link_header::parse_with_rel;
use ractor::concurrency::Duration;
use ractor::ActorRef;
use rand::rngs::SmallRng;
use rand::Rng;
use rand::SeedableRng;
use reqwest::header::LINK;
use reqwest::Client;
use reqwest::Error;
use reqwest::Method;
use reqwest::Response;
use serde::Deserialize;
use tokio::task::AbortHandle;
use tracing::{debug, error};

pub const GITLAB_USERS_OBSERVER: &str = "gitlab:observer:users";
pub const GITLAB_PROJECT_OBSERVER: &str = "gitlab.observer.projects";
pub const BROKER_CLIENT_NAME: &str = "gitlab:observer:web_client";
pub const GITLAB_PIPELINE_OBSERVER: &str = "gitlab:observer:pipelines";
pub const GITLAB_JOBS_OBSERVER: &str = "gitlab:observer:jobs";
pub const GITLAB_REPOSITORY_OBSERVER: &str = "gitlab:observer:repositories";
pub const GITLAB_GROUPS_OBSERVER: &str = "gitlab:observer:groups";
pub const GITLAB_RUNNER_OBSERVER: &str = "gitlab:observer:runners";
pub const BACKOFF_RECEIVED_LOG: &str = "{myself:?} received backoff message...";
pub const TOKEN_EXPIRED_BACKOFF_LOG: &str = "{myself:?} stopping due to bad credentials";
pub const MESSAGE_FORWARDING_FAILED: &str = "Expected to forward a message to self.";
const PRIVATE_TOKEN_HEADER_STR: &str = "PRIVATE-TOKEN";
/// General state for all gitlab observers
pub struct GitlabObserverState {
    /// Endpoint of GitLab instance
    pub gitlab_endpoint: String,
    /// Token for authentication
    pub token: Option<String>,
    /// HTTP client
    pub web_client: Client,
    /// ID of the agent's session with the broker
    pub registration_id: String,
    /// amount of time in seconds the observer will wait before next tick, updated internally by backoff
    backoff_interval: Duration,
    /// Max amount of time the observer will wait before ticking
    max_backoff: Duration,
    /// Times we failed to query gitlab for one reason or another
    failed_attempts: u64,
    /// RNG used to calculate jitter
    rng: rand::rngs::SmallRng,
    /// minimum amount of time an observer will wait between queries
    base_interval: Duration,
    /// thread handle containing the observer loop
    task_handle: Option<AbortHandle>,
}

impl GitlabObserverState {
    /// Create a new GitlabObserverState
    pub fn new(
        gitlab_endpoint: String,
        token: Option<String>,
        web_client: Client,
        registration_id: String,
        base_interval: Duration,
        max_backoff: Duration,
    ) -> Self {
        //init rng

        let mut rng = rand::rng();

        let small = SmallRng::from_rng(&mut rng);

        // state
        GitlabObserverState {
            gitlab_endpoint,
            token,
            web_client,
            registration_id,
            max_backoff,
            base_interval,
            backoff_interval: base_interval, // start with the base interval
            failed_attempts: 0,
            rng: small,
            task_handle: None,
        }
    }

    /// Get the current backoff interval
    pub fn backoff_interval(&self) -> Duration {
        self.backoff_interval
    }

    /// Increase backoff interval with exponential backoff and jitter
    pub fn apply_backoff(&mut self) {
        let jitter = Duration::from_secs(self.rng.random_range(0..30));
        let new_interval = self.backoff_interval * 2 + jitter;
        self.backoff_interval = std::cmp::min(new_interval, self.max_backoff);
        self.failed_attempts += 1;
    }

    /// Reset backoff interval to base
    pub fn reset_backoff(&mut self) {
        self.backoff_interval = self.base_interval;
    }
}

/// Arguments taken in by gitlab observers
#[derive(Clone)]
pub struct GitlabObserverArgs {
    pub gitlab_endpoint: String,
    pub token: Option<String>,
    pub registration_id: String,
    pub web_client: Client,
    pub max_backoff: u64,
    pub base_interval: u64,
}

/// Messages that observers send themselves to prompt the retrieval of resources

/// Queries observers can send
pub enum Command {
    GetUsers(Operation<MultiUserQuery, MultiUserQueryArguments>),
    GetProjects(Operation<MultiProjectQuery, MultiProjectQueryArguments>),
    GetGroups(Operation<AllGroupsQuery, MultiGroupQueryArguments>),
    GetGroupMembers(Operation<GroupMembersQuery, GroupPathVariable>),
    GetRunners(Operation<MultiRunnerQuery, MultiRunnerQueryArguments>),
    GetProjectPipelines(IdString),
    GetPipelineJobs(IdString),
    GetProjectContainerRepositories(IdString),
    GetProjectPackages(IdString),
    GetGroupContainerRepositories(IdString),
    GetGroupPackageRepositories(IdString),
    GetMetadata,
}
pub enum BackoffReason {
    FatalError(String),
    GraphqlError(String),
    GitlabUnreachable(String),
    TokenInvalid(String),
}
pub enum GitlabObserverMessage {
    Tick(Command),
    Backoff(BackoffReason),
}

pub fn handle_graphql_errors(
    errors: Vec<GraphQlError>,
    actor_ref: ActorRef<GitlabObserverMessage>,
) {
    let errors = errors
        .iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>()
        .join("\n");

    error!("Failed to query instance! {errors}");

    if let Err(e) = actor_ref.send_message(GitlabObserverMessage::Backoff(
        BackoffReason::GraphqlError(errors),
    )) {
        error!("{e}");
        actor_ref.stop(Some(e.to_string()))
    }
}
/// helper function for our observers to respond to backoff messages
/// either returns a new duration, or an error containing the reason it shouldn't
pub fn handle_backoff(
    state: &mut GitlabObserverState,
    reason: BackoffReason,
) -> Result<Duration, String> {
    match reason {
        BackoffReason::GitlabUnreachable(_) => {
            // If gitlab is unreachable, it *could* come back, but we should only hang around so much before giving up
            if state.failed_attempts < 5 {
                state.apply_backoff();
                Ok(state.backoff_interval())
            } else {
                let error = "Backoff limit reached! Stopping".to_string();
                error!(error);
                Err(error)
            }
        }
        BackoffReason::GraphqlError(_error) => {
            // Error could've been a timeout, authentication problem, or some malformed graphql query that's too complex/invalid.
            // unfortunately, there's really no way to know w/o some string parsing, graphql errors aren't well formed.
            // We'll choose to only try 3 just in case it's just a timeout.
            if state.failed_attempts < 3 {
                state.apply_backoff();
                Ok(state.backoff_interval())
            } else {
                let error = "Backoff limit reached! Stopping".to_string();
                error!(error);
                Err(error)
            }
        }
        BackoffReason::FatalError(error) => {
            error!("Encountered a fatal error message! {error}");
            Err(error)
        }
        _ => todo!(),
    }
}

pub async fn get_all_runners(
    client: &Client,
    token: String,
    endpoint_prefix: String,
) -> Result<Response, Error> {
    let endpoint = format!("{}{}", endpoint_prefix, "/runners/all");
    let response = client
        .request(reqwest::Method::GET, endpoint)
        .header(PRIVATE_TOKEN_HEADER_STR, token)
        .send()
        .await?;

    Ok(response)
}

pub async fn get_runner_jobs(
    client: &Client,
    runner_id: u32,
    endpoint_prefix: String,
    token: String,
) -> Result<Response, Error> {
    let endpoint = format!(
        "{}{}{}",
        endpoint_prefix,
        "/".to_owned() + &runner_id.to_string(),
        "/jobs"
    );
    let response = client
        .get(endpoint)
        .header(PRIVATE_TOKEN_HEADER_STR, token)
        .send()
        .await?;
    Ok(response)
}

/**
 * Makes a request for to a given endpoint using provided credentials to retrieve elements from a single page.
 */
async fn get_elements(client: &Client, token: String, endpoint: String) -> Result<Response, Error> {
    match client
        .request(Method::GET, endpoint.clone())
        .header(PRIVATE_TOKEN_HEADER_STR, token)
        .send()
        .await
    {
        Ok(response) => Ok(response),
        Err(e) => {
            error!("could not make request to {}, {}", endpoint, e);
            Err(e)
        }
    }
}
/**
 * Makes one or a series of requests to a given endpoint using provided credentials to retrieve as many items as possible.
 * Uses LINK header to crawl pages and retrieve multiple items as JSON elements that are then converted into a given type.
 */
pub async fn get_all_elements<T: for<'a> Deserialize<'a>>(
    client: &Client,
    token: String,
    endpoint: String,
) -> Option<Vec<T>> {
    let mut elements: Vec<T> = Vec::new();
    debug!("Getting all elements from {}", endpoint);
    let resp = match get_elements(client, token.clone(), endpoint.clone()).await {
        Ok(resp) => resp,
        Err(_) => return None,
    };

    if !resp.status().is_success() {
        //TODO: make message elaborate on what each code could mean, 401, 403, etc.
        error!(
            "Error code: {} received at {}",
            resp.status().as_str(),
            endpoint.clone()
        );
        return None;
    }

    let mut headers = resp.headers().clone();
    //get data from first page, if any
    match resp.json::<Vec<T>>().await {
        Ok(mut vec) => {
            elements.append(&mut vec);
        }
        Err(e) => {
            error!("Could not deserialize elements from json, {}", e);
        }
    }

    let mut link_map = parse_with_rel(headers.get(LINK).unwrap().to_str().unwrap()).unwrap();

    //Crawl pages, appending all elements to the list
    while let Some(link) = link_map.get("next") {
        let resp = match get_elements(client, token.clone(), link.raw_uri.clone()).await {
            Ok(resp) => resp,
            Err(_) => return None,
        };

        headers = resp.headers().clone();
        match resp.json::<Vec<T>>().await {
            Ok(mut vec) => {
                elements.append(&mut vec);
            }
            Err(e) => {
                error!("Could not deserialize elements from json, {}", e);
                //TODO: we got bad data here, continue or break?
            }
        }
        link_map = parse_with_rel(headers.get(LINK).unwrap().to_str().unwrap()).unwrap();
    }

    return Some(elements);
}

pub fn graphql_endpoint(gitlab_endpoint: &str) -> String {
    format!("{gitlab_endpoint}/graphql")
}
pub fn v4_api_endpoint(gitlab_endpoint: &str) -> String {
    format!("{gitlab_endpoint}/v4")
}
