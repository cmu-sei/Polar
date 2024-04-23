// Polar
// Copyright 2023 Carnegie Mellon University.
// NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.
// [DISTRIBUTION STATEMENT D] Distribution authorized to the Department of Defense and U.S. DoD contractors only (materials contain software documentation) (determination date: 2022-05-20). Other requests shall be referred to Defense Threat Reduction Agency.
// Notice to DoD Subcontractors:  This document may contain Covered Defense Information (CDI).  Handling of this information is subject to the controls identified in DFARS 252.204-7012 – SAFEGUARDING COVERED DEFENSE INFORMATION AND CYBER INCIDENT REPORTING
// Carnegie Mellon® is registered in the U.S. Patent and Trademark Office by Carnegie Mellon University.
// This Software includes and/or makes use of Third-Party Software subject to its own license, see license.txt file for more information. 
// DM23-0821
//
use gitlab_types::Pipeline;
use reqwest::Client;
use reqwest::Response;
use reqwest::Error;
use reqwest::Method;
use reqwest::header::LINK;
use serde::Deserialize;
use parse_link_header::parse_with_rel;

const PRIVATE_TOKEN_HEADER_STR : &str = "PRIVATE-TOKEN";

pub async fn get_version(client: &Client, token: String, endpoint_prefix: String) -> Result<Response, Error> {
    let endpoint = format!("{}{}", endpoint_prefix, "/version");
    let response = client.get(endpoint).header(PRIVATE_TOKEN_HEADER_STR, token).send().await?;
    Ok(response)
}

pub async fn get_all_namespaces(client: &Client, token: String, endpoint_prefix: String) -> Result<Response, Error> {
    let endpoint = format!("{}{}", endpoint_prefix, "/namespaces");

    let response = client.get(endpoint).header(PRIVATE_TOKEN_HEADER_STR, token).send().await?;
    Ok(response)
}

//NOTE: will get all subgroups as well if caller is an administrator
pub async fn get_all_groups(client: &Client, token: String, endpoint_prefix: String) -> Result<Response, Error> {
    let endpoint = format!("{}{}", endpoint_prefix, "/groups");

    let response = client.get(endpoint).header(PRIVATE_TOKEN_HEADER_STR, token).send().await?;
    Ok(response)
}
pub async fn get_group_members(client: &Client, token: String, endpoint_prefix: String, group_id: u32) -> Result<Response, Error> {
    let endpoint = format!("{}{}{}", endpoint_prefix, "/groups/".to_string() + &group_id.to_string(), "/members");

    let response = client.get(endpoint).header(PRIVATE_TOKEN_HEADER_STR, token).send().await?;
    Ok(response)
}

pub async fn find_group(client: &Client, query: String, token: String, endpoint_prefix: String) -> Result<Response, Error> {
    let endpoint = format!("{}{}", endpoint_prefix, "/groups?search=".to_owned() + &query);
    let response = client.get(endpoint).header(PRIVATE_TOKEN_HEADER_STR, token).send().await?;

    Ok(response)
}

pub async fn get_group_projects(client: &Client, group_id: u32, token: String, endpoint_prefix: String) -> Result<Response, Error> {
    let endpoint = format!("{}{}{}", endpoint_prefix, "/".to_owned() + group_id.to_string().as_ref(), "/projects");
    let response = client.get(endpoint).header(PRIVATE_TOKEN_HEADER_STR, token).send().await?;
    Ok(response)
}

pub async fn get_projects(client: &Client, token: String, endpoint_prefix: String) -> Result<Response, Error>{
    let endpoint = format!("{}{}", endpoint_prefix, "/projects");

    let request = client.get(endpoint).header(PRIVATE_TOKEN_HEADER_STR, token)
    .query(&[("per_page", "20")]).build().unwrap();

    let response = client.execute(request).await?;
    Ok(response)
}

pub async fn get_project_runners(client: &Client, project_id: u32, token: String, endpoint_prefix: String) -> Result<Response, Error> {
    let endpoint = format!("{}{}{}", endpoint_prefix, "/".to_owned() + &project_id.to_string(), "/runners");

    let response = client.get(endpoint).header(PRIVATE_TOKEN_HEADER_STR, token).send().await?;
    Ok(response)
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

async fn get_elements(client: &Client, token: String, endpoint: String) -> Result<Response, Error> {
    let response = client
    .request(Method::GET, endpoint)
    .header(PRIVATE_TOKEN_HEADER_STR, token)
    .send().await?;

    Ok(response)
}

pub async fn get_all_elements<T: for<'a> Deserialize<'a>>(client: &Client, token: String, endpoint: String) -> Option<Vec<T>> {

    let mut elements: Vec<T> = Vec::new();
    println!("{}", endpoint);
    let resp = get_elements(client, token.clone(), endpoint.clone()).await.unwrap();
    
    if  !resp.status().is_success() {
        println!("Error code: {} recieved", resp.status().as_str());
        return Some(elements)
    }

    let mut headers = resp.headers().clone();
    elements.append(&mut resp.json::<Vec<T>>().await.unwrap());
    
    let mut link_map = parse_with_rel(headers.get(LINK).unwrap().to_str().unwrap()).unwrap();
    
    while let Some(link) = link_map.get("next") {
        let resp = get_elements(client, token.clone(), link.raw_uri.clone()).await.unwrap();
        headers = resp.headers().clone();
        elements.append(&mut resp.json::<Vec<T>>().await.unwrap());
        link_map = parse_with_rel(headers.get(LINK).unwrap().to_str().unwrap()).unwrap();
    }

    return Some(elements)
}

// pub fn get_type_of<T>(_: &T) -> &str {
//     std::intrinsics::type_name::<T>()
// }


pub async fn get_user(client: &Client, user_id: u32 ,token: String, endpoint_prefix: String) -> Result<Response, Error> {
    let endpoint = format!("{}{}{}", endpoint_prefix, "/users/", user_id.to_string());
    let response = client
    .get(endpoint)
    .header(PRIVATE_TOKEN_HEADER_STR, token)
    .send().await?;
    Ok(response)
}
pub async fn get_user_projects(client: &Client, user_id: u32 ,token: String, endpoint_prefix: String) -> Result<Response, Error> {
    let endpoint = format!("{}{}{}", endpoint_prefix, "/users/".to_owned() + &user_id.to_string(), "/projects" );
    let response = client.get(endpoint).header(PRIVATE_TOKEN_HEADER_STR, token).send().await?;
    Ok(response)
}

pub async fn get_project_releases(client: &Client, project_id: u32 ,token: String, endpoint_prefix: String) -> Result<Response, Error> {

    let endpoint = format!("{}{}{}", endpoint_prefix, 
    "/projects/".to_owned() + project_id.to_string().as_ref(), 
    "/releases");

    let response = client.get(endpoint).header(PRIVATE_TOKEN_HEADER_STR, token).send().await?;

    Ok(response)
}

pub async fn get_project_registries(client: &Client, project_id: u32 ,token: String, endpoint_prefix: String) -> Result<Response, Error> {
    let endpoint = format!("{}{}{}", endpoint_prefix, "/".to_owned() + &project_id.to_string(), "/registry/repositories");
    let response = client.get(endpoint).header(PRIVATE_TOKEN_HEADER_STR, token).send().await?;
    Ok(response)
}

pub async fn get_group_registries(client: &Client, group_id: u32 ,token: String, endpoint_prefix: String) -> Result<Response, Error> {
    let endpoint = format!("{}{}{}", endpoint_prefix, "/groups/".to_owned() + group_id.to_string().as_ref(), "/registry/repositories");
    let response = client
    .get(endpoint)
    .header(PRIVATE_TOKEN_HEADER_STR, token)
    .query(&["tags", "true"]).send().await?;
    Ok(response)
}

pub async fn get_project_pipelines(client: &Client, project_id: u32 ,token: String, endpoint_prefix: String) -> Result<Vec<Pipeline>, Error> {
    let endpoint = format!("{}{}{}" 
    ,endpoint_prefix,
     "/projects/".to_owned() + project_id.to_string().as_ref(), 
     "/pipelines");
    println!("{}", endpoint);
    let response = client.request(Method::GET, endpoint)
    .header(PRIVATE_TOKEN_HEADER_STR, token)
    .send().await?;

    //check response
    if response.status().is_success() {
        Ok(response.json::<Vec<Pipeline>>().await.unwrap())
    }else {
        println!("Could not find pipelines for project id: {}", project_id);
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod service_tests { 

    use common::{get_gitlab_endpoint, get_gitlab_token};
    use crate::{get_user};
    use reqwest::{Client};
    use gitlab_types::User;
    #[test]
    #[should_panic (expected = "received invalid private token from environment.")]
    fn test_reading_bad_token() {
        temp_env::with_var("GITLAB_TOKEN", Some("abcdefg"), || {
            get_gitlab_token();    
        });
            
    }
    
    #[tokio::test]
    async fn test_get_user_as_admin() {
        let client = Client::new();
        let user_id = 90;

        match get_user(&client, user_id, get_gitlab_token(), get_gitlab_endpoint()).await {
            Ok(response) => {
                let user = response.json::<User>().await.unwrap();
                assert_eq!(user.id, 90);
                assert_eq!(user.username, "vcaaron");
            }
            Err(e) => {
                println!("{}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_get_user_no_credentials() {

        let client = Client::new();
        let user_id = 90;
        match get_user(&client,user_id, "".to_string(), get_gitlab_endpoint()).await {
            Ok(response) => {
                assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
            }
            Err(e) => println!("error {}", e)
        }
    }
}
