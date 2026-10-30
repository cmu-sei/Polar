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
use crate::{
    Command, JIRA_ISSUE_OBSERVER, JiraDeployment, JiraError, JiraObserverArgs, JiraObserverMessage,
    JiraObserverState, publish_data,
};
use jira_common::JIRA_ISSUES_CONSUMER_TOPIC;
use jira_common::types::{JiraData, JiraField, JsonString};
use polar::cassini::TcpClient;
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use reqwest::Url;
use serde_json::Value;
use soft_state_common::{Answer, StateReply};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

const ISSUES_OP: &str = "/rest/api/2/search";
const PAGE_SIZE: usize = 50;

/// An incremental pull asks for everything updated since the last completed
/// pull started plus this much, so that clock skew between here and Jira
/// does not lose an update that landed between two windows. The processor
/// upserts, so an issue seen twice is harmless.
pub const INCREMENTAL_OVERLAP_SECS: u64 = 300;

/// What every issue pull is ordered by, so that a pull cut short leaves the graph's newest `updated` meaning that
/// everything older was written (docs/architecture/soft-state.md, "The Jira cursor").
pub const ORDER: &str = "ORDER BY updated ASC";

/// The label and property the cursor is the largest of: what jira-processor writes for each issue
/// (`jira/process/src/issues.rs`, `UPDATED_MS`).
pub const CURSOR_LABEL: &str = "JiraIssue";
pub const CURSOR_PROPERTY: &str = "updated_ms";

/// One question to the state agent, where issue pulls start: an [`Answer`] for each call, a fresh correlation id each
/// time (`supervisor.rs`, `state_asker`).
pub type AskCursor = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Answer> + Send>> + Send + Sync>;

/// Where the first issue pull starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// From the newest update of an issue the graph has, in epoch milliseconds.
    Since(i64),
    /// The graph has no issue: the first pull is full, or from `QUERY_DATE`.
    Scratch,
}

/// What the state agent's answer means for the first pull: where it starts, or why it cannot start yet. Only a value
/// says where; a refusal, an unavailable graph or no answer is retried, and never taken for an empty graph, since a
/// full pull is what the cursor is there to avoid.
pub fn start_from(answer: &Answer) -> Result<Start, String> {
    match answer {
        Answer::Replied(StateReply::Value(Some(newest_ms))) => Ok(Start::Since(*newest_ms)),
        Answer::Replied(StateReply::Value(None)) => Ok(Start::Scratch),
        Answer::Replied(StateReply::Refused(why)) => Err(format!(
            "the state agent refused to say where issue pulls start: {why}"
        )),
        Answer::Replied(StateReply::Unavailable(why)) => Err(format!(
            "the state agent could not say where issue pulls start: {why}"
        )),
        Answer::NoReply(why) => Err(format!(
            "no answer from the state agent on where issue pulls start: {why}"
        )),
    }
}

/// How many minutes back the first pull asks, from `now_ms` on this clock to the graph's newest update `newest_ms`:
/// the gap plus the overlap ([`INCREMENTAL_OVERLAP_SECS`], which covers the skew between the clocks), rounded up, and
/// at least one. A newest update ahead of this clock leaves the overlap alone.
pub fn cursor_minutes(now_ms: i64, newest_ms: i64) -> u64 {
    // In i128, so no difference of two i64s saturates.
    let gap = u128::try_from(i128::from(now_ms) - i128::from(newest_ms)).unwrap_or(0);
    let reach = gap + u128::from(INCREMENTAL_OVERLAP_SECS) * 1000;
    u64::try_from(reach.div_ceil(60_000))
        .unwrap_or(u64::MAX)
        .max(1)
}

/// This machine's time, in epoch milliseconds.
pub fn wall_clock_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

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
        // whole interval after every start. Each pull schedules the next
        // (`JiraObserverState::settle`): an interval away after a success,
        // sooner after a failure.
        myself.cast(Self::tick())?;
        Ok(())
    }

    /// Where the first pull starts, asked of the state agent once per attempt. An observer given no way to ask (a test
    /// of something else) starts as it did before there was one.
    async fn learn_start(state: &JiraObserverState) -> Result<Start, String> {
        match &state.cursor {
            Some(ask) => start_from(&ask().await),
            None => Ok(Start::Scratch),
        }
    }

    /// Pulls issues from Jira and publishes each one. A failure in talking
    /// to Jira is a JiraError, and the pull is tried again later
    /// (`JiraObserverState::settle`); any other failure ends the observer.
    async fn pull_issues(
        op: &str,
        state: &mut JiraObserverState,
    ) -> Result<(), ActorProcessingErr> {
        state.ensure_token().await?;
        let fields: Vec<JiraField> = state
            .get_json(&format!("{}/rest/api/2/field", state.jira_url))
            .await?;
        let names: HashMap<String, String> = fields
            .into_iter()
            .map(|f| (f.id.to_string(), f.name.to_string()))
            .collect();

        let filter = jql_filter(
            state.full_pull_done,
            incremental_window(
                state.last_pull_started,
                (state.clock)(),
                state.base_interval,
            ),
            state.start,
            (state.wall_clock)(),
            state.query_date.as_deref(),
        );
        info!(
            "Pulling issues ({})",
            match (state.full_pull_done, state.start) {
                (true, _) => "incremental",
                (false, Some(Start::Since(_))) => "from the newest issue the graph has",
                (false, _) => "full",
            }
        );
        let jql = issue_jql(filter.as_deref(), state.deployment);

        match state.deployment {
            JiraDeployment::ServerOrDataCenter => {
                let endpoint = format!("{}{}", state.jira_url, op);
                let mut start_at = 0usize;

                loop {
                    let params = vec![
                        ("startAt", start_at.to_string()),
                        ("maxResults", PAGE_SIZE.to_string()),
                        ("expand", "changelog".to_string()),
                        ("jql", jql.clone()),
                    ];
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
                // /rest/api/3/search/jql with cursor-based pagination; the
                // JQL is bounded even for a full pull (`issue_jql`).
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
        .with_query_date(args.query_date)
        .with_retry(args.retry, args.clock)
        .with_cursor(args.cursor, args.wall_clock))
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
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            JiraObserverMessage::Tick(command) => {
                if let Command::GetIssues(op) = command {
                    // Before the first pull, where it starts: the graph's newest issue, through the state agent. Until
                    // it says, nothing is pulled, and the question is asked again on the observer's backoff.
                    if !state.full_pull_done && state.start.is_none() {
                        match JiraIssueObserver::learn_start(state).await {
                            Ok(start) => {
                                match start {
                                    Start::Since(newest_ms) => info!(
                                        "the graph's newest issue was updated at {newest_ms} ms; the first pull \
                                         starts there"
                                    ),
                                    Start::Scratch => info!(
                                        "the graph has no issue; the first pull is full, or from QUERY_DATE"
                                    ),
                                }
                                state.start = Some(start);
                            }
                            Err(why) => {
                                return state.settle(
                                    &myself,
                                    JIRA_ISSUE_OBSERVER,
                                    Err(JiraError::no_cursor(why).into()),
                                    Self::tick,
                                );
                            }
                        }
                    }
                    let started = (state.clock)();
                    let pulled = JiraIssueObserver::pull_issues(&op, state).await;
                    if pulled.is_ok() {
                        state.full_pull_done = true;
                        state.last_pull_started = Some(started);
                    }
                    state.settle(&myself, JIRA_ISSUE_OBSERVER, pulled, Self::tick)?;
                }
            }
        }
        Ok(())
    }
}

/// How far back an incremental pull asks: from the start of the last pull
/// that completed, so that a run of failed pulls, or one that ran late,
/// leaves no gap; never less than one interval, which is what it asked for
/// before pulls were retried.
pub fn incremental_window(
    last_pull_started: Option<Instant>,
    now: Instant,
    base_interval: Duration,
) -> Duration {
    last_pull_started
        .map_or(base_interval, |started| {
            now.saturating_duration_since(started)
        })
        .max(base_interval)
}

/// The JQL restricting a pull, if any. Every pull after a completed one only asks for what changed within the window
/// (`incremental_window`) plus the overlap, in whole minutes rounded up. The first starts where the graph's newest
/// issue was updated (`start`, [`cursor_minutes`] back from `now_ms`), relative, so the Jira user's time zone, which
/// JQL applies to an absolute date, does not enter; with no issue in the graph it is full, unless a QUERY_DATE was
/// given to start from.
pub fn jql_filter(
    full_pull_done: bool,
    window: Duration,
    start: Option<Start>,
    now_ms: i64,
    query_date: Option<&str>,
) -> Option<String> {
    if full_pull_done {
        let reach = window + Duration::from_secs(INCREMENTAL_OVERLAP_SECS);
        let minutes = reach.as_millis().div_ceil(60_000);
        Some(format!("updated >= -{minutes}m"))
    } else if let Some(Start::Since(newest_ms)) = start {
        Some(format!(
            "updated >= -{}m",
            cursor_minutes(now_ms, newest_ms)
        ))
    } else {
        match query_date {
            Some(date) if !date.is_empty() => Some(format!("updated > {date}")),
            _ => None,
        }
    }
}

/// The whole JQL of an issue pull: its filter, if any, then [`ORDER`]. Jira Cloud rejects fully unbounded JQL, so a
/// full pull there is bounded by "project is not EMPTY", Atlassian's own recommended dummy bound.
pub fn issue_jql(filter: Option<&str>, deployment: JiraDeployment) -> String {
    match (filter, deployment) {
        (Some(filter), _) => format!("{filter} {ORDER}"),
        (None, JiraDeployment::Cloud) => format!("project is not EMPTY {ORDER}"),
        (None, JiraDeployment::ServerOrDataCenter) => ORDER.to_string(),
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
    let items = res["issues"]
        .as_array()
        .ok_or_else(|| JiraError::new("the search response has no issues array".to_string()))?;

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

    const NOW: i64 = 1_704_164_645_000;

    #[test]
    fn with_no_issue_in_the_graph_the_first_pull_is_full_unless_a_start_date_was_given() {
        let interval = Duration::from_secs(1800);
        for start in [None, Some(Start::Scratch)] {
            assert_eq!(jql_filter(false, interval, start, NOW, None), None);
            assert_eq!(jql_filter(false, interval, start, NOW, Some("")), None);
            assert_eq!(
                jql_filter(false, interval, start, NOW, Some("2024-01-01")),
                Some("updated > 2024-01-01".to_string())
            );
        }
    }

    #[test]
    fn the_first_pull_starts_at_the_graph_s_newest_issue_and_query_date_does_not_apply() {
        let interval = Duration::from_secs(1800);
        // Updated an hour ago: 60 minutes, and the overlap's five.
        assert_eq!(
            jql_filter(
                false,
                interval,
                Some(Start::Since(NOW - 3_600_000)),
                NOW,
                Some("2020-01-01")
            ),
            Some("updated >= -65m".to_string())
        );
        // A second over a minute rounds up.
        assert_eq!(
            jql_filter(false, interval, Some(Start::Since(NOW - 61_000)), NOW, None),
            Some("updated >= -7m".to_string())
        );
        // A newest update ahead of this clock still asks for the overlap.
        assert_eq!(
            jql_filter(
                false,
                interval,
                Some(Start::Since(NOW + 600_000)),
                NOW,
                None
            ),
            Some("updated >= -5m".to_string())
        );
    }

    #[test]
    fn later_pulls_cover_the_interval_plus_the_overlap() {
        let interval = Duration::from_secs(1800);
        for start in [None, Some(Start::Scratch), Some(Start::Since(0))] {
            assert_eq!(
                jql_filter(true, interval, start, NOW, Some("2024-01-01")),
                Some("updated >= -35m".to_string())
            );
        }
    }

    #[test]
    fn every_issue_pull_is_ordered_by_its_last_update_oldest_first_on_both_deployments() {
        assert_eq!(
            issue_jql(None, JiraDeployment::ServerOrDataCenter),
            "ORDER BY updated ASC"
        );
        assert_eq!(
            issue_jql(None, JiraDeployment::Cloud),
            "project is not EMPTY ORDER BY updated ASC"
        );
        for deployment in [JiraDeployment::ServerOrDataCenter, JiraDeployment::Cloud] {
            assert_eq!(
                issue_jql(Some("updated >= -35m"), deployment),
                "updated >= -35m ORDER BY updated ASC"
            );
        }
    }

    #[test]
    fn only_a_value_says_where_to_start_and_anything_else_is_tried_again() {
        assert_eq!(
            start_from(&Answer::Replied(StateReply::Value(Some(NOW)))),
            Ok(Start::Since(NOW))
        );
        assert_eq!(
            start_from(&Answer::Replied(StateReply::Value(None))),
            Ok(Start::Scratch)
        );
        for answer in [
            Answer::Replied(StateReply::Refused("not declared".into())),
            Answer::Replied(StateReply::Unavailable("graph down".into())),
            Answer::NoReply("timed out".into()),
        ] {
            assert!(start_from(&answer).is_err(), "{answer:?}");
        }
    }
}
