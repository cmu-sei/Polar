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
pub mod projects;
pub mod runners;
pub mod supervisor;
pub mod users;

use cynic::Operation;
use gitlab_queries::groups::*;
use gitlab_queries::runners::MultiRunnerQuery;
use gitlab_queries::runners::MultiRunnerQueryArguments;
use gitlab_queries::MultiProjectQuery;
use gitlab_queries::MultiProjectQueryArguments;
use gitlab_queries::MultiUserQuery;
use gitlab_queries::MultiUserQueryArguments;
use parse_link_header::parse_with_rel;
use ractor::ActorRef;
use reqwest::header::LINK;
use reqwest::Client;
use reqwest::Error;
use reqwest::Method;
use reqwest::Response;
use serde::Deserialize;
use tracing::{debug, error};

pub const GITLAB_USERS_OBSERVER: &str = "GITLAB_USERS_OBSERVER";
pub const BROKER_CLIENT_NAME: &str = "gitlab_web_client";
const PRIVATE_TOKEN_HEADER_STR: &str = "PRIVATE-TOKEN";

/// General state for all gitlab observers
pub struct GitlabObserverState {
    pub gitlab_endpoint: String, // Endpoint of GitLab instance
    pub token: Option<String>,   // Token for authentication
    pub web_client: Client,      // HTTP client
    pub registration_id: String, // ID of the agent's session with the broker
}

/// Arguments taken in by gitlab observers
#[derive(Clone)]
pub struct GitlabObserverArgs {
    pub gitlab_endpoint: String,
    pub token: Option<String>,
    pub registration_id: String,
    pub web_client: Client
}

/// Messages that observers send themselves to prompt the retrieval of resources

pub enum GitlabObserverMessage {
    GetUsers(Operation<MultiUserQuery, MultiUserQueryArguments>),
    GetProjects(Operation<MultiProjectQuery, MultiProjectQueryArguments>),
    GetGroups(Operation<AllGroupsQuery, MultiGroupQueryArguments>),
    GetGroupMembers(Operation<GroupMembersQuery, GroupPathVariable>),
    GetRunners(Operation<MultiRunnerQuery, MultiRunnerQueryArguments>),
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
