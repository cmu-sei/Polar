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
    Command, JiraDeployment, JiraObserverArgs, JiraObserverMessage, JiraObserverState, publish_data,
};
use jira_common::JIRA_ISSUES_CONSUMER_TOPIC;
use jira_common::types::{JiraData, JiraField, JsonString};
use polar::cassini::TcpClient;
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use reqwest::Url;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, warn};

const ISSUES_OP: &str = "/rest/api/2/search";
const PAGE_SIZE: usize = 50;

/// An incremental pull asks for everything updated within one interval plus
/// this much, so a poll that runs a little late does not lose an update that
/// landed between two windows. The processor upserts, so an issue seen twice
/// is harmless.
const INCREMENTAL_OVERLAP_SECS: u64 = 300;

pub struct JiraIssueObserver;

impl JiraIssueObserver {
    fn tick() -> JiraObserverMessage {
        JiraObserverMessage::Tick(Command::GetIssues(ISSUES_OP.to_string()))
    }

    fn observe(
        myself: &ActorRef<JiraObserverMessage>,
        state: &JiraObserverState,
    ) -> Result<(), ActorProcessingErr> {
        info!("Observing every {} seconds", state.base_interval.as_secs());

        // Poll straight away rather than leaving the graph empty for a
        // whole interval after every start. The interval task ends on its
        // own once this actor has stopped and the cast fails, so there is
        // no handle to keep.
        myself.cast(Self::tick())?;
        myself.send_interval(state.base_interval, Self::tick);
        Ok(())
    }

    /// Pulls issues from Jira and publishes each one. Any failure is an
    /// error: the actor fails, the supervisor exits non-zero and the Job
    /// restarts the pod.
    async fn pull_issues(op: &str, state: &JiraObserverState) -> Result<(), ActorProcessingErr> {
        let fields: Vec<JiraField> = state
            .get_json(&format!("{}/rest/api/2/field", state.jira_url))
            .await?;
        let names: HashMap<String, String> = fields
            .into_iter()
            .map(|f| (f.id.to_string(), f.name.to_string()))
            .collect();

        let filter = jql_filter(
            state.full_pull_done,
            state.base_interval,
            state.query_date.as_deref(),
        );
        info!(
            "Pulling issues ({})",
            if state.full_pull_done {
                "incremental"
            } else {
                "full"
            }
        );

        match state.deployment {
            JiraDeployment::ServerOrDataCenter => {
                let endpoint = format!("{}{}", state.jira_url, op);
                let mut start_at = 0usize;

                loop {
                    let mut params = vec![
                        ("startAt", start_at.to_string()),
                        ("maxResults", PAGE_SIZE.to_string()),
                        ("expand", "changelog".to_string()),
                    ];
                    if let Some(jql) = &filter {
                        params.push(("jql", jql.clone()));
                    }
                    let url = Url::parse_with_params(&endpoint, params)?;
                    debug!("{url}");

                    let res: Value = state.get_json(url.as_str()).await?;
                    let fetched = publish_issues(&res, &names, &state.tcp_client)?;
                    let total = res["total"].as_u64().unwrap_or(0) as usize;

                    start_at += fetched;
                    debug!("Loaded {start_at}/{total}...");

                    // An empty page ends the walk: with a non-zero total
                    // it would otherwise never advance.
                    if fetched == 0 || start_at >= total {
                        break;
                    }
                }
            }
            JiraDeployment::Cloud => {
                // Cloud removed /rest/api/2/search (CHANGE-2046). Use
                // /rest/api/3/search/jql with cursor-based pagination.
                // Cloud rejects fully unbounded JQL, so a full pull uses
                // "project is not EMPTY", Atlassian's own recommended
                // dummy bound; an incremental pull is already bounded by
                // its updated clause.
                let jql = match &filter {
                    Some(filter) => format!("{filter} order by updated DESC"),
                    None => "project is not EMPTY order by created DESC".to_string(),
                };
                let endpoint = format!("{}/rest/api/3/search/jql", state.jira_url);
                let mut next_page_token: Option<String> = None;

                loop {
                    // The enhanced search returns only issue ids unless the
                    // fields are asked for.
                    let mut params = vec![
                        ("jql", jql.clone()),
                        ("maxResults", PAGE_SIZE.to_string()),
                        ("expand", "changelog".to_string()),
                        ("fields", "*all".to_string()),
                    ];
                    if let Some(token) = &next_page_token {
                        params.push(("nextPageToken", token.clone()));
                    }
                    let url = Url::parse_with_params(&endpoint, params)?;
                    debug!("{url}");

                    let res: Value = state.get_json(url.as_str()).await?;
                    publish_issues(&res, &names, &state.tcp_client)?;

                    if res["isLast"].as_bool().unwrap_or(true) {
                        break;
                    }
                    next_page_token = res["nextPageToken"].as_str().map(str::to_string);
                    if next_page_token.is_none() {
                        break;
                    }
                    debug!("Fetching next page...");
                }
            }
        }

        Ok(())
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

        Ok(JiraObserverState::new(
            args.jira_url,
            args.auth,
            args.deployment,
            args.web_client,
            args.tcp_client,
            Duration::from_secs(args.base_interval),
        )
        .with_query_date(args.query_date))
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        JiraIssueObserver::observe(&myself, state)
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            JiraObserverMessage::Tick(command) => {
                if let Command::GetIssues(op) = command {
                    JiraIssueObserver::pull_issues(&op, state).await?;
                    state.full_pull_done = true;
                }
            }
        }
        Ok(())
    }
}

/// The JQL restricting a pull, if any. The first pull is full unless a
/// QUERY_DATE was given to start from; every pull after a completed one only
/// asks for what changed within the interval plus the overlap.
fn jql_filter(
    full_pull_done: bool,
    base_interval: Duration,
    query_date: Option<&str>,
) -> Option<String> {
    if full_pull_done {
        let minutes = (base_interval.as_secs() + INCREMENTAL_OVERLAP_SECS) / 60;
        Some(format!("updated >= -{minutes}m"))
    } else {
        match query_date {
            Some(date) if !date.is_empty() => Some(format!("updated > {date}")),
            _ => None,
        }
    }
}

/// Replaces the field ids in an issue with their human readable names and
/// drops the id keys. A name the issue already has a value for is not
/// overwritten. Returns false if the issue has no fields object.
fn rename_fields(issue: &mut Value, names: &HashMap<String, String>) -> bool {
    let Some(fields) = issue.get_mut("fields").and_then(Value::as_object_mut) else {
        return false;
    };

    let renamed: Vec<(String, Value)> = fields
        .iter()
        .filter_map(|(key, value)| names.get(key).map(|name| (name.clone(), value.clone())))
        .collect();

    for (name, value) in renamed {
        fields.entry(name).or_insert(value);
    }
    for id in names.keys() {
        fields.remove(id);
    }
    true
}

/// Publishes every issue on a page, one message each. Returns how many
/// issues the page held, which is what pagination advances by. An issue
/// without a fields object is logged and skipped rather than failing the
/// whole pull.
fn publish_issues(
    res: &Value,
    names: &HashMap<String, String>,
    tcp_client: &TcpClient,
) -> Result<usize, ActorProcessingErr> {
    let items = res["issues"].as_array().ok_or_else(|| {
        ActorProcessingErr::from("the search response has no issues array".to_string())
    })?;

    for issue in items {
        let mut issue = issue.clone();
        if !rename_fields(&mut issue, names) {
            warn!("Skipping issue {} with no fields", issue["key"]);
            continue;
        }

        publish_data(
            tcp_client,
            JIRA_ISSUES_CONSUMER_TOPIC,
            &JiraData::Issues(JsonString {
                json: issue.to_string(),
            }),
        )?;
    }
    Ok(items.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn names() -> HashMap<String, String> {
        HashMap::from([
            ("customfield_10010".to_string(), "Sprint".to_string()),
            ("summary".to_string(), "Summary".to_string()),
        ])
    }

    #[test]
    fn field_ids_are_replaced_by_their_names() {
        let mut issue = json!({
            "key": "PLAT-1",
            "fields": { "customfield_10010": "S1", "summary": "hello" }
        });

        assert!(rename_fields(&mut issue, &names()));
        assert_eq!(
            issue["fields"],
            json!({ "Sprint": "S1", "Summary": "hello" })
        );
    }

    #[test]
    fn a_name_the_issue_already_has_is_not_overwritten() {
        let mut issue = json!({
            "key": "PLAT-1",
            "fields": { "Sprint": "kept", "customfield_10010": "ignored" }
        });

        assert!(rename_fields(&mut issue, &names()));
        assert_eq!(issue["fields"], json!({ "Sprint": "kept" }));
    }

    #[test]
    fn an_issue_without_fields_is_reported_not_panicked_on() {
        let mut issue = json!({ "key": "PLAT-1" });
        assert!(!rename_fields(&mut issue, &names()));
    }

    #[test]
    fn the_first_pull_is_full_unless_a_start_date_was_given() {
        let interval = Duration::from_secs(1800);
        assert_eq!(jql_filter(false, interval, None), None);
        assert_eq!(jql_filter(false, interval, Some("")), None);
        assert_eq!(
            jql_filter(false, interval, Some("2024-01-01")),
            Some("updated > 2024-01-01".to_string())
        );
    }

    #[test]
    fn later_pulls_cover_the_interval_plus_the_overlap() {
        let interval = Duration::from_secs(1800);
        assert_eq!(
            jql_filter(true, interval, Some("2024-01-01")),
            Some("updated >= -35m".to_string())
        );
    }
}
