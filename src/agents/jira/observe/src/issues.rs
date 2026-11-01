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
    Command, FIELD_OP, JIRA_ISSUE_OBSERVER, JiraDeployment, JiraError, JiraObserverArgs,
    JiraObserverMessage, JiraObserverState, publish_data,
};
use jira_common::JIRA_ISSUES_CONSUMER_TOPIC;
use jira_common::types::{JiraData, JiraField, JsonString};
use polar::cassini::TcpClient;
use polar_agent_builder::{SoftValue, StateClient, StateError, StateQuery};
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use reqwest::Url;
use serde_json::Value;
use std::collections::HashMap;
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

/// How the issue observer reads where its first pull starts: the agent's state actor, or a test's stand-in.
pub type AskCursor = Arc<dyn StateClient>;

/// The question: the newest update of an issue the graph has.
pub fn cursor_query() -> StateQuery {
    StateQuery::Max {
        label: CURSOR_LABEL.to_string(),
        property: CURSOR_PROPERTY.to_string(),
    }
}

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
pub fn start_from(answer: &Result<SoftValue, StateError>) -> Result<Start, String> {
    match answer {
        Ok(value) => Ok(match value.largest() {
            Some(newest_ms) => Start::Since(newest_ms),
            None => Start::Scratch,
        }),
        Err(why) => Err(format!("cannot tell where issue pulls start: {why}")),
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
            Some(state) => start_from(&state.get(cursor_query()).await),
            None => Ok(Start::Scratch),
        }
    }

    /// Pulls issues from Jira and publishes each one. A failure in talking
    /// to Jira is a JiraError, and the pull is tried again later
    /// (`JiraObserverState::settle`); any other failure ends the observer.
    ///
    /// A pull is a walk ([`IssueWalk`]) kept in the observer's state while it is under way: a page that fails leaves
    /// it there, and the next attempt goes on from that page, asking what the first attempt asked. Once it completes,
    /// the next incremental pull reaches back to when it began.
    async fn pull_issues(
        op: &str,
        state: &mut JiraObserverState,
    ) -> Result<(), ActorProcessingErr> {
        state.ensure_site().await?;
        state.ensure_token().await?;
        let fields: Vec<JiraField> = state
            .get_json(&format!("{}{FIELD_OP}", state.jira_url))
            .await?;
        let site = SiteFields::new(&fields);

        let mut walk = match state.issue_walk.take() {
            Some(walk) => {
                info!(
                    "Going on with the issue pull from issue {} ({} published)",
                    walk.start_at, walk.published
                );
                walk
            }
            None => {
                info!(
                    "Pulling issues ({})",
                    match (state.full_pull_done, state.start) {
                        (true, _) => "incremental",
                        (false, Some(Start::Since(_))) => "from the newest issue the graph has",
                        (false, _) => "full",
                    }
                );
                IssueWalk::new(
                    walk_filter(
                        state.full_pull_done,
                        incremental_window(
                            state.last_pull_started,
                            (state.clock)(),
                            state.base_interval,
                        ),
                        state.start,
                        (state.wall_clock)(),
                        state.query_date.as_deref(),
                    ),
                    (state.clock)(),
                )
            }
        };

        let walked = match state.deployment {
            JiraDeployment::ServerOrDataCenter => {
                Self::walk_by_offset(op, state, &mut walk, &site).await
            }
            JiraDeployment::Cloud => Self::walk_by_token(state, &mut walk, &site).await,
        };
        match walked {
            Ok(()) => {
                state.full_pull_done = true;
                state.last_pull_started = Some(walk.started);
                Ok(())
            }
            Err(e) => {
                // Cloud's page tokens are not known to outlive a failure: the walk goes on from its first page, still
                // asking what it first asked.
                walk.next_page_token = None;
                state.issue_walk = Some(walk);
                Err(e)
            }
        }
    }

    /// Server and Data Center: `/rest/api/2/search`, paged by `startAt`.
    async fn walk_by_offset(
        op: &str,
        state: &JiraObserverState,
        walk: &mut IssueWalk,
        site: &SiteFields,
    ) -> Result<(), ActorProcessingErr> {
        let endpoint = format!("{}{}", state.jira_url, op);
        loop {
            let jql = walk.jql((state.clock)(), state.deployment);
            let params = vec![
                ("startAt", walk.start_at.to_string()),
                ("maxResults", PAGE_SIZE.to_string()),
                ("expand", "changelog".to_string()),
                ("jql", jql),
            ];
            let url = Url::parse_with_params(&endpoint, params)?;
            debug!("{url}");

            let res: Value = state.get_json(url.as_str()).await?;
            let fetched = publish_issues(&res, site, &state.tcp_client).await?;
            walk.published += fetched;
            let total = res["total"].as_u64().map(|t| t as usize);
            if total.is_none() && !walk.warned_no_total {
                warn!(
                    "{op} answered without a total: reading on to an empty page rather than stopping here"
                );
                walk.warned_no_total = true;
            }

            walk.start_at += fetched;
            debug!("Loaded {}/{total:?}...", walk.start_at);

            // An empty page ends the walk: with a non-zero total it would otherwise never advance. Without a
            // total, only an empty page does.
            if fetched == 0 || total.is_some_and(|total| walk.start_at >= total) {
                return Ok(());
            }
        }
    }

    /// Cloud removed /rest/api/2/search (CHANGE-2046): `/rest/api/3/search/jql`, paged by its next page token; the
    /// JQL is bounded even for a full pull (`issue_jql`).
    async fn walk_by_token(
        state: &JiraObserverState,
        walk: &mut IssueWalk,
        site: &SiteFields,
    ) -> Result<(), ActorProcessingErr> {
        let endpoint = format!("{}/rest/api/3/search/jql", state.jira_url);
        loop {
            // The enhanced search returns only issue ids unless the
            // fields are asked for.
            let mut params = vec![
                ("jql", walk.jql((state.clock)(), state.deployment)),
                ("maxResults", PAGE_SIZE.to_string()),
                ("expand", "changelog".to_string()),
                ("fields", "*all".to_string()),
            ];
            if let Some(token) = &walk.next_page_token {
                params.push(("nextPageToken", token.clone()));
            }
            let url = Url::parse_with_params(&endpoint, params)?;
            debug!("{url}");

            let res: Value = state.get_json(url.as_str()).await?;
            walk.published += publish_issues(&res, site, &state.tcp_client).await?;

            walk.next_page_token = res["nextPageToken"].as_str().map(str::to_string);
            match res["isLast"].as_bool() {
                Some(true) => return Ok(()),
                Some(false) => {}
                // Said, not taken for the end: the page token says whether there is more.
                None => warn!(
                    "the search answered without isLast: {}",
                    if walk.next_page_token.is_some() {
                        "reading on, since it gave a next page"
                    } else {
                        "stopping, since it gave no next page"
                    }
                ),
            }
            if walk.next_page_token.is_none() {
                return Ok(());
            }
            debug!("Fetching next page...");
        }
    }
}

/// An issue pull under way: what it asks, when it began, and how far it has read. Kept in the observer's state while
/// a page fails, so the next attempt goes on from that page rather than from the first.
#[derive(Debug, Clone)]
pub struct IssueWalk {
    /// What the walk asks, as it was at its start.
    filter: Filter,
    /// When the walk began, on the observer's clock: the next incremental pull reaches back to here, and a relative
    /// filter is reached back by the time since, so its oldest end never moves later while the walk goes on.
    started: Instant,
    /// Server and Data Center: the offset of the next page.
    start_at: usize,
    /// Cloud: the next page's token.
    next_page_token: Option<String>,
    published: usize,
    warned_no_total: bool,
}

impl IssueWalk {
    pub fn new(filter: Filter, started: Instant) -> Self {
        Self {
            filter,
            started,
            start_at: 0,
            next_page_token: None,
            published: 0,
            warned_no_total: false,
        }
    }

    /// The JQL of a page asked at `now`.
    fn jql(&self, now: Instant, deployment: JiraDeployment) -> String {
        let filter = self.filter.jql(now.saturating_duration_since(self.started));
        issue_jql(filter.as_deref(), deployment)
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
            // Its session was ended (what it was owed is lost): its next pull starts from the graph's cursor, asked
            // again, as its first did.
            JiraObserverMessage::Resync => {
                info!(
                    "the broker ended this agent's session: the next issue pull starts from the graph's cursor"
                );
                state.full_pull_done = false;
                state.start = None;
                state.last_pull_started = None;
                state.issue_walk = None;
            }
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
                                return state
                                    .settle(
                                        &myself,
                                        JIRA_ISSUE_OBSERVER,
                                        Err(JiraError::no_cursor(why).into()),
                                        Self::tick,
                                    )
                                    .map(|_| ());
                            }
                        }
                    }
                    let pulled = JiraIssueObserver::pull_issues(&op, state).await;
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
    walk_filter(full_pull_done, window, start, now_ms, query_date).jql(Duration::ZERO)
}

/// What an issue pull asks, as [`jql_filter`] says, kept as a value so that a walk can ask it again later.
pub fn walk_filter(
    full_pull_done: bool,
    window: Duration,
    start: Option<Start>,
    now_ms: i64,
    query_date: Option<&str>,
) -> Filter {
    if full_pull_done {
        let reach = window + Duration::from_secs(INCREMENTAL_OVERLAP_SECS);
        Filter::Within {
            minutes: u64::try_from(reach.as_millis().div_ceil(60_000)).unwrap_or(u64::MAX),
        }
    } else if let Some(Start::Since(newest_ms)) = start {
        Filter::Within {
            minutes: cursor_minutes(now_ms, newest_ms),
        }
    } else {
        match query_date {
            Some(date) if !date.is_empty() => Filter::After(date.to_string()),
            _ => Filter::Everything,
        }
    }
}

/// What an issue pull asks of Jira's `updated`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// Every issue.
    Everything,
    /// Updated within this many minutes of the walk's start: relative, so the Jira user's time zone does not enter.
    Within { minutes: u64 },
    /// Updated after a date (`QUERY_DATE`).
    After(String),
}

impl Filter {
    /// The JQL restricting a page asked `elapsed` after the walk began, if any. A relative filter reaches back by
    /// `elapsed` as well, rounded up to the minute, so that its oldest end stays where it was at the walk's start:
    /// Jira measures `-Nm` from when each page is asked, and an oldest end that moved later would drop issues from
    /// the front of a walk ordered oldest first and shift the rest under its offset, skipping some.
    pub fn jql(&self, elapsed: Duration) -> Option<String> {
        match self {
            Filter::Everything => None,
            Filter::Within { minutes } => {
                let reach = u128::from(*minutes) * 60_000 + elapsed.as_millis();
                Some(format!("updated >= -{}m", reach.div_ceil(60_000)))
            }
            // Quoted, as JQL needs a date with a time to be; the date was checked at start (`settings::parse_query_date`).
            Filter::After(date) => Some(format!("updated > \"{date}\"")),
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

/// The id of the field Jira's schema calls `system` (`updated`), whatever the site's language names it; `None` when
/// `/field` lists no such field.
pub fn system_field_id(fields: &[JiraField], system: &str) -> Option<String> {
    fields
        .iter()
        .find(|f| {
            f.schema
                .as_ref()
                .and_then(|s| s.system.as_deref())
                .is_some_and(|s| s == system)
        })
        .map(|f| f.id.clone())
}

/// What the observer knows of the site's fields (`/field`), for preparing each issue it publishes: every field's
/// display name, the id of the system field `updated`, and the id of each system field the processor makes edges of
/// ([`jira_common::SYSTEM_FIELDS`]), found by its schema or, where `/field` gives none, by the id that is its system
/// name.
pub struct SiteFields {
    names: HashMap<String, String>,
    updated_id: Option<String>,
    system: Vec<(&'static str, String)>,
}

impl SiteFields {
    pub fn new(fields: &[JiraField]) -> Self {
        Self {
            names: fields
                .iter()
                .map(|f| (f.id.clone(), f.name.clone()))
                .collect(),
            updated_id: system_field_id(fields, "updated"),
            system: jira_common::SYSTEM_FIELDS
                .iter()
                .map(|system| {
                    (
                        *system,
                        system_field_id(fields, system).unwrap_or_else(|| system.to_string()),
                    )
                })
                .collect(),
        }
    }

    /// Prepares an issue for the processor: its last update beside its fields ([`mark_updated`]), the system fields
    /// moved out of its fields to beside them by system id ([`jira_common::SYSTEM_FIELDS_KEY`]), and the rest renamed
    /// to their display names ([`rename_fields`]). False if the issue has no fields object.
    pub fn prepare(&self, issue: &mut Value) -> bool {
        mark_updated(issue, self.updated_id.as_deref());
        let Some(fields) = issue.get_mut("fields").and_then(Value::as_object_mut) else {
            return false;
        };
        let mut system = serde_json::Map::new();
        for (name, id) in &self.system {
            if let Some(value) = fields.remove(id) {
                system.insert(name.to_string(), value);
            }
        }
        if let Some(object) = issue.as_object_mut() {
            object.insert(
                jira_common::SYSTEM_FIELDS_KEY.to_string(),
                Value::Object(system),
            );
        }
        rename_fields(issue, &self.names)
    }
}

/// Puts the issue's last update, the value of the field `updated_id`, beside its fields under
/// [`jira_common::SYSTEM_UPDATED_KEY`], before the fields are renamed: the processor reads the cursor's time from there,
/// never from a display name. An issue without it, or a site whose `/field` names no such field, gets nothing.
pub fn mark_updated(issue: &mut Value, updated_id: Option<&str>) {
    let Some(id) = updated_id else {
        return;
    };
    let Some(updated) = issue["fields"][id].as_str().map(str::to_string) else {
        return;
    };
    if let Some(object) = issue.as_object_mut() {
        object.insert(
            jira_common::SYSTEM_UPDATED_KEY.to_string(),
            Value::String(updated),
        );
    }
}

/// Publishes every issue on a page, one message each. Returns how many
/// issues the page held, which is what pagination advances by. An issue
/// without a fields object is logged and skipped rather than failing the
/// whole pull.
async fn publish_issues(
    res: &Value,
    site: &SiteFields,
    tcp_client: &TcpClient,
) -> Result<usize, ActorProcessingErr> {
    let items = res["issues"]
        .as_array()
        .ok_or_else(|| JiraError::new("the search response has no issues array".to_string()))?;

    for issue in items {
        let mut issue = issue.clone();
        if !site.prepare(&mut issue) {
            warn!("Skipping issue {} with no fields", issue["key"]);
            continue;
        }

        publish_data(
            tcp_client,
            JIRA_ISSUES_CONSUMER_TOPIC,
            &JiraData::Issues(JsonString {
                json: issue.to_string(),
            }),
        )
        .await?;
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

    fn field(id: &str, name: &str, system: Option<&str>) -> JiraField {
        serde_json::from_value(json!({
            "id": id, "name": name, "custom": false, "navigable": true, "orderable": false, "searchable": true,
            "schema": system.map(|s| json!({ "type": "datetime", "system": s })),
        }))
        .unwrap()
    }

    #[test]
    fn the_last_update_is_found_by_its_system_field_whatever_the_site_s_language_names_it() {
        // A Jira in German: the system field `updated` is "Aktualisiert".
        let fields = [
            field("summary", "Zusammenfassung", Some("summary")),
            field("updated", "Aktualisiert", Some("updated")),
            field("customfield_10010", "Updated", None),
        ];
        let id = system_field_id(&fields, "updated");
        assert_eq!(id.as_deref(), Some("updated"));
        let names: HashMap<String, String> = fields
            .iter()
            .map(|f| (f.id.clone(), f.name.clone()))
            .collect();
        let mut issue = json!({
            "key": "PLAT-1",
            "fields": { "updated": "2024-01-02T03:04:05.000+0000", "customfield_10010": "not it" }
        });
        mark_updated(&mut issue, id.as_deref());
        assert!(rename_fields(&mut issue, &names));
        assert_eq!(issue["polar_updated"], "2024-01-02T03:04:05.000+0000");
        assert_eq!(
            issue["fields"]["Aktualisiert"],
            "2024-01-02T03:04:05.000+0000"
        );
        // A custom field that happens to be named "Updated" is not the cursor.
        assert_eq!(issue["fields"]["Updated"], "not it");
    }

    /// Assignee, reporter, creator, issue type and project travel beside the fields by their system field ids, found
    /// by `/field`'s schema, so the processor never needs their display names: here a German site, and a custom field
    /// named "Project" as Apache's Jira has one (customfield_12312823), which before took the system field's place
    /// whenever it sorted first.
    #[test]
    fn the_system_fields_travel_by_id_whatever_the_site_calls_them() {
        let fields = [
            field("assignee", "Bearbeiter", Some("assignee")),
            field("reporter", "Autor", Some("reporter")),
            field("creator", "Ersteller", Some("creator")),
            field("issuetype", "Vorgangstyp", Some("issuetype")),
            field("project", "Projekt", Some("project")),
            field("summary", "Zusammenfassung", Some("summary")),
            field("customfield_12312823", "Projekt", None),
        ];
        let site = SiteFields::new(&fields);
        let mut issue = json!({
            "key": "PLAT-1",
            "fields": {
                "assignee": { "key": "jdoe", "name": "jdoe" },
                "reporter": { "key": "asmith" },
                "creator": null,
                "issuetype": { "name": "Fehler" },
                "project": { "id": "10042", "key": "PLAT" },
                "summary": "hallo",
                "customfield_12312823": { "value": "custom" }
            }
        });
        assert!(site.prepare(&mut issue));
        assert_eq!(
            issue[jira_common::SYSTEM_FIELDS_KEY],
            json!({
                "assignee": { "key": "jdoe", "name": "jdoe" },
                "reporter": { "key": "asmith" },
                "creator": null,
                "issuetype": { "name": "Fehler" },
                "project": { "id": "10042", "key": "PLAT" }
            })
        );
        // The rest by display name, the custom "Projekt" among them; no system field is left there.
        assert_eq!(
            issue["fields"],
            json!({ "Zusammenfassung": "hallo", "Projekt": { "value": "custom" } })
        );
    }

    /// A `/field` that gives no schema (an old site, or a stand-in) still finds the system fields: their ids are the
    /// system names.
    #[test]
    fn without_a_schema_the_system_fields_are_found_by_their_ids() {
        let site = SiteFields::new(&[field("assignee", "Assignee", None)]);
        let mut issue = json!({ "key": "PLAT-1", "fields": { "assignee": { "key": "jdoe" }, "project": { "id": "1" } } });
        assert!(site.prepare(&mut issue));
        assert_eq!(
            issue[jira_common::SYSTEM_FIELDS_KEY]["assignee"]["key"],
            "jdoe"
        );
        assert_eq!(issue[jira_common::SYSTEM_FIELDS_KEY]["project"]["id"], "1");
        assert_eq!(issue["fields"], json!({}));
    }

    mod apache {
        include!("../../common/fixtures/apache.rs");
    }

    /// Real issues from Apache's Jira, prepared as the observer publishes them: exactly the pinned form
    /// jira-processor's tests write, each of which decodes as a `JiraIssue`, its authors without email included.
    #[test]
    fn apache_s_issues_are_published_in_the_pinned_form() {
        let fields: Vec<JiraField> = serde_json::from_str(apache::FIELDS).unwrap();
        let site = SiteFields::new(&fields);
        let search: Value = serde_json::from_str(apache::SEARCH).unwrap();
        let issues = search["issues"].as_array().unwrap();
        assert_eq!(issues.len(), apache::PUBLISHED.len());
        for (issue, pinned) in issues.iter().zip(apache::PUBLISHED) {
            let mut issue = issue.clone();
            assert!(site.prepare(&mut issue));
            let pinned: Value = serde_json::from_str(pinned).unwrap();
            assert_eq!(issue, pinned, "{}", issue["key"]);

            let decoded: jira_common::types::JiraIssue =
                serde_json::from_value(issue.clone()).unwrap();
            assert!(!decoded.changelog.histories.is_empty(), "{}", decoded.key);
            assert!(
                decoded.changelog.histories.iter().all(|h| h
                    .author
                    .as_ref()
                    .is_some_and(|a| a.email_address.is_none() && a.identity().is_some())),
                "{}",
                decoded.key
            );
            // The system field `project`, never Apache's custom field of the same display name.
            assert_eq!(issue["polar_system"]["project"]["key"], "COMDEV");
            assert!(issue["fields"]["Project"].is_null());
        }
    }

    #[test]
    fn apache_s_server_info_says_server() {
        let info: Value = serde_json::from_str(apache::SERVER_INFO).unwrap();
        assert_eq!(
            info["deploymentType"]
                .as_str()
                .and_then(JiraDeployment::from_server_info),
            Some(JiraDeployment::ServerOrDataCenter)
        );
    }

    #[test]
    fn without_the_system_field_nothing_is_marked() {
        assert_eq!(
            system_field_id(&[field("updated", "Updated", None)], "updated"),
            None
        );
        let mut issue =
            json!({ "key": "PLAT-1", "fields": { "updated": "2024-01-02T03:04:05.000+0000" } });
        mark_updated(&mut issue, None);
        assert!(issue.get("polar_updated").is_none());
        let mut issue = json!({ "key": "PLAT-1", "fields": {} });
        mark_updated(&mut issue, Some("updated"));
        assert!(issue.get("polar_updated").is_none());
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
                Some("updated > \"2024-01-01\"".to_string())
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
            start_from(&Ok(SoftValue::of(Some(NOW)))),
            Ok(Start::Since(NOW))
        );
        assert_eq!(start_from(&Ok(SoftValue::of(None))), Ok(Start::Scratch));
        for answer in [
            Err(StateError::Refused("not declared".into())),
            Err(StateError::Unavailable("graph down".into())),
            Err(StateError::NoReply("timed out".into())),
        ] {
            assert!(start_from(&answer).is_err(), "{answer:?}");
        }
    }
}
