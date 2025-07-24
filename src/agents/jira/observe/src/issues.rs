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
use crate::{
    handle_backoff, BROKER_CLIENT_NAME,
    Command,
    JiraObserverArgs,
    JiraObserverMessage, JiraObserverState,
    JIRA_ISSUE_OBSERVER
    };
use cassini::{client::TcpClientMessage, ClientMessage};
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use jira_common::JIRA_ISSUES_CONSUMER_TOPIC;
use rkyv::rancor::Error;
use std::time::Duration;
use tracing::{info, debug};
use jira_common::types::{
    JiraIssue, JiraData, JsonString
    };

use serde_path_to_error;
use serde_json::Value;
use std::io::Cursor;


pub struct JiraIssueObserver;

impl JiraIssueObserver {
    fn observe(myself: ActorRef<JiraObserverMessage>,
               state: &mut JiraObserverState,
               _duration: Duration) {
        info!("Observing every {} seconds", state.backoff_interval.as_secs());

        let handle = myself.send_interval(state.backoff_interval, || {
            //build query
            let op = "/rest/api/2/search?";
            // pass query in message
            JiraObserverMessage::Tick(Command::GetIssues(op.to_string()))
        }).abort_handle();

        state.task_handle = Some(handle);
    }
    
}
#[async_trait]
impl Actor for JiraIssueObserver {
    type Msg = JiraObserverMessage;
    type State = JiraObserverState;
    type Arguments = JiraObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: JiraObserverArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, connecting to instance");

        let state = JiraObserverState::new(
            args.jira_url, 
            args.token, 
            args.web_client, 
            args.registration_id, 
            Duration::from_secs(args.base_interval), 
            Duration::from_secs(args.max_backoff));
        Ok(state)
    }
    
    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {

        JiraIssueObserver::observe(myself, state, state.base_interval);
        Ok(())
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            JiraObserverMessage::Tick(command) => {  
                match command {
                    Command::GetIssues(op) => {
                        let mut start_at = 0;
                        let max_results = 50;
                        let query_string = "''";
                        debug!("Staring to query for issues...");
                        // Load up the fields

                        loop {
                            //let mut all_issues = Vec::new();
                            let url = format!(
                                "{}{}?query={}&startAt={}&maxResults={}&expand=changelog",
                                state.jira_url, op, query_string, start_at, max_results
                            );
                            debug!("{}", url.to_string());
                            let res = state.web_client
                                .get(&url)
                                .bearer_auth(format!("{}", state.token.clone().expect("TOKEN")))
                                .send()
                                .await?
                                .json::<serde_json::Value>()
                                .await?;

                            let json_data = res["issues"].to_string();
                            let value: Value = serde_json::from_str(&json_data).unwrap();
                            //let value: serde_json::Value = serde_json::from_str(&json_data.to_string()).unwrap();
                            //let generic = GenericJson::from(value);
                            //println!("{:#?}", generic);

                            if let Some(items) = value.as_array() {
                                for issue in items {
                                    let tcp_client =
                                        where_is(BROKER_CLIENT_NAME.to_string())
                                            .expect("Expected to find client");
                                    let wrap = JiraData::Issues(JsonString { json: issue.to_string() });
                                    let bytes = rkyv::to_bytes::<Error>(&wrap).unwrap();
                                    println!("{:#?}", wrap);
                                    let msg = ClientMessage::PublishRequest {
                                        topic: JIRA_ISSUES_CONSUMER_TOPIC.to_string(),
                                        payload: bytes.to_vec(),
                                        registration_id: Some(
                                            state.registration_id.clone(),
                                        ),
                                    };
                                    tcp_client
                                        .send_message(TcpClientMessage::Send(msg))
                                        .expect("Expected to send message");
                                    /*
                                    let cursor = Cursor::new(issue.to_string());
                                    let mut deserializer = serde_json::Deserializer::from_reader(cursor);
                                    let result: Result<JiraIssue, _> = serde_path_to_error::deserialize(&mut deserializer);

                                    match result {
                                        Ok(issue) => println!("Success"),
                                        Err(err) => {
                                            println!("{:#?}", issue);
                                            println!("Deserialization error: {}", err);
                                            println!("Error path: {}", err.path());
                                        }
                                    }
                                    */
                                }
                            }

                            let fetched = max_results;
                            let total = res["total"].as_u64().unwrap_or(0);

                            //let issues: Vec<JiraIssue> = serde_json::from_value(res["issues"].clone())?;
                            //all_issues.extend(issues);

                            if (start_at as usize + fetched) >= total as usize {
                                break;
                            }

                            start_at += fetched;
                            debug!("Loaded {}...", start_at);



                            //let data = JiraData::Issues(all_issues.clone());
                            //println!("Data - ({})", json_data);
                            //let data = JiraData::Issues(json_data.clone());

                        }
                    }
                    _ => (),
                }
            }
            JiraObserverMessage::Backoff(reason) => {
                // cancel old event loop and start a new one with updated state, if observer hasn't stopped
                if let Some(handle) = &state.task_handle {
                    handle.abort();
                    // start new loop
                    match handle_backoff(state, reason) {
                        Ok(duration) => {
                            JiraIssueObserver::observe(myself, state, duration);
                        }
                        Err(e) => myself.stop(Some(e.to_string()))
                    }   
                }
            }
        }
        Ok(())
    }
}
