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
    BROKER_CLIENT_NAME, Command, JiraObserverArgs, JiraObserverMessage, JiraObserverState,
    handle_backoff,
};
use chrono::{DateTime, Utc};
use std::sync::{Arc, Mutex, LazyLock};
use std::env;
use cassini_client::TcpClientMessage;
use cassini_types::ClientMessage;
use jira_common::JIRA_ISSUES_CONSUMER_TOPIC;
use jira_common::types::{JiraData, JiraField, JsonString};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait, registry::where_is};
use rkyv::rancor::Error;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use cassini_types::WireTraceCtx;
use serde_json::Value;
use std::collections::HashMap;

pub struct JiraIssueObserver;

static START_TIME: LazyLock<Instant> = LazyLock::new(Instant::now);

impl JiraIssueObserver {
    fn observe(
        myself: ActorRef<JiraObserverMessage>,
        state: &mut JiraObserverState,
        _duration: Duration,
    ) {
        info!(
            "Observing every {} seconds",
            state.backoff_interval.as_secs()
        );

        info!("Building handle");
        let handle = myself
            .send_interval(state.backoff_interval, || {
                //build query
                let op = "/rest/api/2/search?";
                // pass query in message
                JiraObserverMessage::Tick(Command::GetIssues(op.to_string()))
            })
            .abort_handle();

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
            Duration::from_secs(args.max_backoff),
        );
        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let _ = myself.send_message(
            JiraObserverMessage::Tick(
                Command::GetIssues("/rest/api/2/search?".to_string())
                ));
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
                if let Command::GetIssues(op) = command {
                    let mut start_at = 0;
                    let max_results = 50;
                    println!("Staring to query for issues...");

                    // Load up the fields
                    let field_url = format!("{}/rest/api/2/field", state.jira_url);
                    match state.web_client.get(&field_url).bearer_auth(state.token.clone().expect("TOKEN").to_string()).send().await {
                        Ok(f_result) => {
                            let field_result = f_result.json::<Vec<JiraField>>().await?;
                            let mut field_map: HashMap<String, JiraField> = HashMap::new();
                            for field in field_result {
                                field_map.insert(field.id.to_string(), field);
                            }
                            let mut timestr = String::new();
                            let mut query_date = String::new();
                            match env::var("QUERY_DATE") {
                                Ok(val) => query_date = val.clone(),
                                Err(env::VarError::NotPresent) => (),
                                Err(env::VarError::NotUnicode(_)) => ()
                            }
                            if (*START_TIME + state.base_interval) < Instant::now() {
                                info!("Updating to do partial pull");
                                timestr.push_str("&jql=updated>=-");
                                timestr.push_str(&state.base_interval.as_secs().to_string());
                                timestr.push_str("s");
                            } else if query_date != "" {
                                info!("Adding in date to grab from");
                                timestr.push_str("&jql=updated>");
                                timestr.push_str(&query_date.to_string());
                            }
                            loop {
                                let url = format!(
                                    "{}{}?startAt={}&maxResults={}&expand=changelog{}",
                                    state.jira_url, op, start_at, max_results, timestr
                                );
                                info!("{}", url.to_string());
                                match state.web_client.get(&url).bearer_auth(state.token.clone().expect("TOKEN").to_string()).send().await?.json::<serde_json::Value>().await {
                                    Ok(res) => {
                                        let json_data = res["issues"].to_string();
                                        let value: Value = serde_json::from_str(&json_data).unwrap();

                                        if let Some(items) = value.as_array() {
                                            for issue in items {
                                                let mut cloned_issue = issue.clone();

                                                // Replace the "customfield_*" with the name
                                                let fields = cloned_issue.get_mut("fields").expect("FIELDS");

                                                let mut replacements = vec![];
                                                for (key, value) in fields.as_object().unwrap() {
                                                    if let Some(found_field) = field_map.get(key.as_str()) {
                                                        let new_key = found_field.name.clone();
                                                        replacements.push((new_key.to_string(), value.clone()));
                                                    }
                                                }
                                                for (new_key, value) in replacements {
                                                    fields[new_key] = value;
                                                }
                                                for key in field_map.keys() {
                                                    fields.as_object_mut().unwrap().remove(key);
                                                }

                                                let tcp_client = where_is(BROKER_CLIENT_NAME.to_string())
                                                    .expect("Expected to find client");
                                                let wrap = JiraData::Issues(JsonString {
                                                    json: cloned_issue.to_string(),
                                                });
                                                let bytes = rkyv::to_bytes::<Error>(&wrap).unwrap();

                                                let msg = ClientMessage::PublishRequest {
                                                    topic: JIRA_ISSUES_CONSUMER_TOPIC.to_string(),
                                                    payload: bytes.to_vec().into(),
                                                    registration_id: state.registration_id.clone(),
                                                    trace_ctx: None,
                                                };

                                                // Serialize the inner client message before sending
                                                let payload = rkyv::to_bytes::<rkyv::rancor::Error>(&msg)
                                                    .expect("Failed to serialize ClientMessage::PublishRequest");

                                                tcp_client.send_message(TcpClientMessage::Publish {
                                                    topic: JIRA_ISSUES_CONSUMER_TOPIC.to_string(),
                                                    payload: payload.into_vec(),
                                                    trace_ctx: WireTraceCtx::from_current_span(),
                                                })?;
                                            }
                                        }
                                        let fetched = max_results;
                                        let total = res["total"].as_u64().unwrap_or(0);

                                        if (start_at + fetched) >= total as usize {
                                            break;
                                        }

                                        start_at += fetched;
                                        info!("Loaded {}/{}...", start_at, total);
                                    }, Err(e) => {
                                        warn!("Error pulling issues, going to retry: {}", e);
                                        std::thread::sleep(Duration::from_secs(2));
                                    }
                                }
                            }
                        }, Err(e) => {
                            warn!("Error pulling fields, going to retry: {}", e);
                            std::thread::sleep(Duration::from_secs(2));
                        }
                    }
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
                        Err(e) => myself.stop(Some(e.to_string())),
                    }
                }
            }
        }
        Ok(())
    }
}
