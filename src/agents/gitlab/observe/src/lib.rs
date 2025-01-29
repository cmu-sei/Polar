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

pub mod supervisor;
pub mod users;
pub mod projects;
pub mod runners;
pub mod groups;

use cassini::client::TcpClientMessage;
use cassini::ClientMessage;
use common::types::GitlabData;

use cynic::Operation;
use cynic::QueryFragment;
use cynic::QueryVariables;
use gitlab_queries::MultiUserQuery;
use gitlab_queries::MultiUserQueryArguments;
use gitlab_queries::MultiUserQueryArgumentsFields;
use gitlab_queries::UserCoreConnection;
use ractor::rpc::call;
use ractor::rpc::CallResult;
use ractor::RpcReplyPort;
use tokio::task::JoinError;
use tokio::task::JoinHandle;
use tokio::time;
use tokio::time::Interval;
use tracing::{debug, error};
use ractor::ActorRef;
use reqwest::Client;
use reqwest::Response;
use reqwest::Error;
use reqwest::Method;
use reqwest::header::LINK;
use serde::Deserialize;
use parse_link_header::parse_with_rel;
use serde_json::to_string;

pub const GITLAB_USERS_OBSERVER: &str = "GITLAB_USERS_OBSERVER";
pub const BROKER_CLIENT_NAME: &str = "gitlab_web_client";
const PRIVATE_TOKEN_HEADER_STR : &str = "PRIVATE-TOKEN";

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
}

/// Messages that observers send themselves to prompt the retrieval of resources

pub enum GitlabObserverMessage {
    GetUsers(Operation<MultiUserQuery, MultiUserQueryArguments>),
    // GetProjects(RpcReplyPort<Result<(), String>>),
    // GetGroups(RpcReplyPort<Result<(), String>>),
}

pub async fn get_all_runners(client: &Client, token: String, endpoint_prefix: String) -> Result<Response, Error> {
    let endpoint = format!("{}{}", endpoint_prefix, "/runners/all");
    let response = client
    .request(reqwest::Method::GET, endpoint)
    .header(PRIVATE_TOKEN_HEADER_STR, token)
    .send()
    .await?;

    Ok(response)
}

pub async fn get_runner_jobs(client: &Client, runner_id: u32, endpoint_prefix: String, token: String) -> Result<Response, Error> {
    let endpoint = format!("{}{}{}", endpoint_prefix, "/".to_owned() + &runner_id.to_string(), "/jobs");
    let response = client.get(endpoint).header(PRIVATE_TOKEN_HEADER_STR, token).send().await?;
    Ok(response)
}

/**
 * Makes a request for to a given endpoint using provided credentials to retrieve elements from a single page.
 */
async fn get_elements(client: &Client, token: String, endpoint: String) -> Result<Response, Error> {
    match client
    .request(Method::GET, endpoint.clone())
    .header(PRIVATE_TOKEN_HEADER_STR, token)
    .send().await {
        Ok(response) => Ok(response),
        Err(e) => {
            error!("could not make request to {}, {}" , endpoint, e);
            Err(e)
        }
    }
}
/**
 * Makes one or a series of requests to a given endpoint using provided credentials to retrieve as many items as possible.
 * Uses LINK header to crawl pages and retrieve multiple items as JSON elements that are then converted into a given type.
 */
pub async fn get_all_elements<T: for<'a> Deserialize<'a>>(client: &Client, token: String, endpoint: String) -> Option<Vec<T>> {

    let mut elements: Vec<T> = Vec::new();
    debug!("Getting all elements from {}", endpoint);
    let resp = match get_elements(client, token.clone(), endpoint.clone()).await {
        Ok(resp) => resp,
        Err(_) => {
            return None
        }
    };
        
    if  !resp.status().is_success() {
        //TODO: make message elaborate on what each code could mean, 401, 403, etc.
        error!("Error code: {} received at {}", resp.status().as_str(), endpoint.clone());
        return None
    }

    let mut headers = resp.headers().clone();
    //get data from first page, if any
    match resp.json::<Vec<T>>().await {
        Ok(mut vec) => {
            elements.append(&mut vec);
        },
        Err(e) => {
            error!("Could not deserialize elements from json, {}", e);
        },
    }
    
    let mut link_map = parse_with_rel(headers
        .get(LINK)
        .unwrap().to_str()
    .unwrap()).unwrap();
    
    //Crawl pages, appending all elements to the list
    while let Some(link) = link_map.get("next") {
        let resp = match get_elements(client, token.clone(), link.raw_uri.clone()).await {
            Ok(resp) => resp, 
            Err(_) => {
                return None
            }
        };

        headers = resp.headers().clone();
        match resp.json::<Vec<T>>().await {
            Ok(mut vec) => {
                elements.append(&mut vec);
            },
            Err(e) => {
                error!("Could not deserialize elements from json, {}", e);
                //TODO: we got bad data here, continue or break?                
            },
        }
        link_map = parse_with_rel(headers.get(LINK).unwrap().to_str().unwrap()).unwrap();
    }

    return Some(elements)
}

/// TODO: Write a generic function to retrieve a list of a given capnproto message from an endpoint. 
/// 
/// The main problem with this at the moment is that I haven't yet figured out how to append additional elements onto the list after they're
/// deserailized. SEE: get_all_elements() for the comparison using serde
/// 
// pub async fn fetch_all<'a, T>(
//     client: &Client,
//     token: String, 
//     endpoint: String,
// ) -> Result<TypedReader<OwnedSegments, T>, Box<dyn std::error::Error>>
// where
//     T: Owned + 'a
// {}


///TODO: Helper function to spawn a task that calls an actor in a loop over some given interval of time.
/// Ideally, we'd be able to call and wait for a reply on an iterval, this is made difficult because RpcReplyPorts can't be moved.
/// The motiviation here is to avoid usele
// pub async fn call_every(observer: ActorRef<GitlabObserverMessage>, resource_type: GitlabResourceType, duration: Duration, timeout: Option<Duration>) -> Result<JoinHandle<()>, JoinError> {
//    

//     Ok(handle)

// }

// fn build_user_query(para)

/// TODO: Helper function that retrieves a given resource type once
pub fn get_once(observer: ActorRef<GitlabObserverMessage> ,message: GitlabObserverMessage) {
    todo!()
}