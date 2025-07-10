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

pub mod projects;
pub mod supervisor;

use parse_link_header::parse_with_rel;
use ractor::concurrency::Duration;
//use ractor::concurrency::JoinHandle;
//use ractor::ActorRef;
//use jira_common::types::IdString;
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
//use tracing::info;
//use tracing::warn;
use tracing::{debug, error};

const JIRA_PROJECT_OBSERVER: &str = "jira.observer.projects";
pub const BROKER_CLIENT_NAME: &str = "jira:observer:web_client";
const PRIVATE_TOKEN_HEADER_STR: &str = "PRIVATE-TOKEN";
pub const BACKOFF_RECEIVED_LOG: &str = "{myself:?} received backoff message...";
pub const TOKEN_EXPIRED_BACKOFF_LOG: &str = "{myself:?} stopping due to bad credentials";
pub const MESSAGE_FORWARDING_FAILED: &str = "Expected to forward a message to self.";

/// General state for all jira observers
pub struct JiraObserverState {
    /// Endpoint of Jira instance
    pub jira_url: String, 
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
    /// Times we failed to query jira for one reason or another
    failed_attempts: u64, 
    /// RNG used to calculate jitter
    rng: rand::rngs::SmallRng,
    /// minimum amount of time an observer will wait between queries
    base_interval: Duration,
    /// thread handle containing the observer loop 
    task_handle: Option<AbortHandle>
}

impl JiraObserverState {
    /// Create a new JiraObserverState
    pub fn new(
        jira_url: String,
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
        JiraObserverState {
            jira_url,
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
        self.failed_attempts +=1;
    }

    /// Reset backoff interval to base
    pub fn reset_backoff(&mut self) {
        self.backoff_interval = self.base_interval;
    }
}

/// Arguments taken in by jira observers
#[derive(Clone)]
pub struct JiraObserverArgs {
    pub jira_url: String,
    pub token: Option<String>,
    pub registration_id: String,
    pub web_client: Client,
    pub max_backoff: u64,
    pub base_interval: u64,
}

pub enum Command {
    GetProjects(String)
}

/// Messages that observers send themselves to prompt the retrieval of resources

pub enum BackoffReason {
    FatalError(String),
    JiraUnreachable(String),
    TokenInvalid(String),
}
pub enum JiraObserverMessage  {
    Tick(Command),
    Backoff(BackoffReason),
}

/// helper function for our observers to respond to backoff messages
/// either returns a new duration, or 
pub fn handle_backoff(state: &mut JiraObserverState, reason: BackoffReason) -> Result<Duration, String> {
    match reason {
        BackoffReason::JiraUnreachable(_) =>  {
            // If jira is unreachable, it *could* come back, but we should only hang around so much before giving up
            if state.failed_attempts < 5 {
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
        _ => todo!()
    }
}

/**
 * Makes a request for to a given url using provided credentials to retrieve elements from a single page.
 */
async fn get_elements(client: &Client, token: String, url: String) -> Result<Response, Error> {
    match client
        .request(Method::GET, url.clone())
        .header(PRIVATE_TOKEN_HEADER_STR, token)
        .send()
        .await
    {
        Ok(response) => Ok(response),
        Err(e) => {
            error!("could not make request to {}, {}", url, e);
            Err(e)
        }
    }
}
/**
 * Makes one or a series of requests to a given url using provided credentials to retrieve as many items as possible.
 * Uses LINK header to crawl pages and retrieve multiple items as JSON elements that are then converted into a given type.
 */
pub async fn get_all_elements<T: for<'a> Deserialize<'a>>(
    client: &Client,
    token: String,
    url: String,
) -> Option<Vec<T>> {
    let mut elements: Vec<T> = Vec::new();
    debug!("Getting all elements from {}", url);
    let resp = match get_elements(client, token.clone(), url.clone()).await {
        Ok(resp) => resp,
        Err(_) => return None,
    };

    if !resp.status().is_success() {
        //TODO: make message elaborate on what each code could mean, 401, 403, etc.
        error!(
            "Error code: {} received at {}",
            resp.status().as_str(),
            url.clone()
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
