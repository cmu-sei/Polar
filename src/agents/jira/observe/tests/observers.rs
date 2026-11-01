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

//! The observers against a stand-in Jira: a pull that fails, or a token Jira does not accept,
//! is tried again and the observer stays up; an error that is not Jira's still ends it.

use cassini_client::TcpClientMessage;
use cassini_client::facade::Backoff;
use jira_common::types::JiraData;
use jira_common::{
    JIRA_GROUPS_CONSUMER_TOPIC, JIRA_ISSUES_CONSUMER_TOPIC, JIRA_PROJECTS_CONSUMER_TOPIC,
    JIRA_USERS_CONSUMER_TOPIC,
};
use jira_observer::groups::JiraGroupObserver;
use jira_observer::issues::AskCursor;
use jira_observer::issues::JiraIssueObserver;
use jira_observer::projects::JiraProjectObserver;
use jira_observer::users::JiraUserObserver;
use jira_observer::{JiraAuth, JiraDeployment, JiraObserverArgs, JiraObserverMessage};
use polar::cassini::TcpClient;
use polar_agent_builder::{SoftValue, StateClient, StateError, StateQuery};
use ractor::{Actor, ActorProcessingErr, ActorRef, ActorStatus, async_trait};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const MYSELF: &str = "/rest/api/2/myself";
const PROJECTS: &str = "/rest/api/2/project";
const SEARCH: &str = "/rest/api/2/search";
const GROUPS: &str = "/rest/api/2/groups/picker";
const USER_SEARCH: &str = "/rest/api/2/user/search";
const USER_PICKER: &str = "/rest/api/2/user/picker";
const SERVER_INFO: &str = "/rest/api/2/serverInfo";

/// Stands in for the broker client and keeps the topic of every publish, and its payload.
struct Recorder {
    topics: Arc<Mutex<Vec<String>>>,
    published: Published,
}

#[async_trait]
impl Actor for Recorder {
    type Msg = TcpClientMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(&self, _: ActorRef<Self::Msg>, _: ()) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _: ActorRef<Self::Msg>,
        message: Self::Msg,
        _: &mut (),
    ) -> Result<(), ActorProcessingErr> {
        if let TcpClientMessage::Publish { topic, payload, .. } = message {
            self.published
                .lock()
                .unwrap()
                .push((topic.clone(), payload));
            self.topics.lock().unwrap().push(topic);
        }
        Ok(())
    }
}

async fn recorder() -> (ActorRef<TcpClientMessage>, Arc<Mutex<Vec<String>>>) {
    let (actor, topics, _) = recording().await;
    (actor, topics)
}

/// Every publish: its topic and its payload.
type Published = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

async fn recording() -> (
    ActorRef<TcpClientMessage>,
    Arc<Mutex<Vec<String>>>,
    Published,
) {
    let topics = Arc::new(Mutex::new(Vec::new()));
    let published = Arc::new(Mutex::new(Vec::new()));
    let (actor, _) = Actor::spawn(
        None,
        Recorder {
            topics: topics.clone(),
            published: published.clone(),
        },
        (),
    )
    .await
    .unwrap();
    (actor, topics, published)
}

/// Every payload published on `topic`, decoded.
fn decoded(published: &Published, topic: &str) -> Vec<JiraData> {
    published
        .lock()
        .unwrap()
        .iter()
        .filter(|(t, _)| t == topic)
        .map(|(_, payload)| rkyv::from_bytes::<JiraData, rkyv::rancor::Error>(payload).unwrap())
        .collect()
}

/// The values of every `name` query parameter of `request`.
fn params(request: &Request, name: &str) -> Vec<String> {
    request
        .url
        .query_pairs()
        .filter(|(k, _)| k == name)
        .map(|(_, v)| v.to_string())
        .collect()
}

/// One query parameter of `request` as a number, or 0.
fn number(request: &Request, name: &str) -> usize {
    params(request, name)
        .first()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// The requests the site was asked on `endpoint`.
async fn asked(server: &MockServer, endpoint: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == endpoint)
        .collect()
}

fn args(server: &MockServer, client: ActorRef<TcpClientMessage>) -> JiraObserverArgs {
    JiraObserverArgs {
        jira_url: server.uri(),
        auth: JiraAuth::from_parts(None, "t0ken".into()),
        deployment: JiraDeployment::ServerOrDataCenter,
        tcp_client: TcpClient::from_actor_ref(client),
        web_client: reqwest::Client::new(),
        // Long enough that only a retry pulls again within a test.
        base_interval: 3600,
        query_date: None,
        retry: Backoff {
            first: Duration::from_millis(20),
            last: Duration::from_millis(80),
        },
        clock: Instant::now,
        // A graph with no issue in it, as a fresh cluster's.
        cursor: Some(answers(vec![], Ok(SoftValue::of(None))).0),
        wall_clock: || NOW_MS,
    }
}

/// The wall clock of these tests, in epoch milliseconds.
const NOW_MS: i64 = 1_704_164_645_000;

/// What a stand-in state agent answers.
type Answer = Result<SoftValue, StateError>;

/// A stand-in for the agent's state actor: it gives `script`'s answers in turn, then `then` for ever, and counts the
/// questions, each of which must be the cursor's.
struct Scripted {
    script: Mutex<std::collections::VecDeque<Answer>>,
    then: Answer,
    asked: Arc<AtomicU64>,
}

#[async_trait]
impl StateClient for Scripted {
    async fn get(&self, query: StateQuery) -> Answer {
        assert_eq!(query, jira_observer::issues::cursor_query());
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.then.clone())
    }
}

fn answers(script: Vec<Answer>, then: Answer) -> (AskCursor, Arc<AtomicU64>) {
    let asked = Arc::new(AtomicU64::new(0));
    let ask: AskCursor = Arc::new(Scripted {
        script: Mutex::new(script.into()),
        then,
        asked: asked.clone(),
    });
    (ask, asked)
}

/// The jql of every search the site was asked.
async fn searches(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == SEARCH)
        .map(|r| {
            r.url
                .query_pairs()
                .find(|(k, _)| k == "jql")
                .map(|(_, v)| v.to_string())
                .unwrap_or_default()
        })
        .collect()
}

/// A site that answers every endpoint the observers use, below anything a test mounts first.
async fn mount_site(server: &MockServer) {
    let issues =
        json!({ "issues": [{ "key": "PLAT-1", "fields": { "summary": "x" } }], "total": 1 });
    for (endpoint, body) in [
        (
            MYSELF,
            json!({ "name": "svc-polar", "key": "JIRAUSER10100" }),
        ),
        (PROJECTS, json!([])),
        (GROUPS, json!({ "groups": [], "total": 0 })),
        (USER_SEARCH, json!([])),
        ("/rest/api/2/field", json!([])),
        (
            SERVER_INFO,
            json!({ "version": "10.3.26", "deploymentType": "Server" }),
        ),
        (SEARCH, issues),
    ] {
        Mock::given(method("GET"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .with_priority(10)
            .mount(server)
            .await;
    }
}

/// Answers `endpoint` with `response` the first `times` it is asked, before the site's answer.
async fn fail_first(server: &MockServer, endpoint: &str, response: ResponseTemplate, times: u64) {
    Mock::given(method("GET"))
        .and(path(endpoint))
        .respond_with(response)
        .up_to_n_times(times)
        .with_priority(1)
        .mount(server)
        .await;
}

/// Waits up to five seconds for `topic` to have been published `count` times.
async fn published(topics: &Arc<Mutex<Vec<String>>>, topic: &str, count: usize) -> bool {
    for _ in 0..250 {
        if topics
            .lock()
            .unwrap()
            .iter()
            .filter(|t| *t == topic)
            .count()
            >= count
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

async fn paths(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.path().to_string())
        .collect()
}

#[tokio::test]
async fn a_pull_that_fails_is_tried_again_and_the_observer_stays_up() {
    let server = MockServer::start().await;
    fail_first(&server, PROJECTS, ResponseTemplate::new(503), 2).await;
    mount_site(&server).await;
    let (client, topics) = recorder().await;

    let (observer, _) = Actor::spawn(None, JiraProjectObserver, args(&server, client))
        .await
        .unwrap();

    assert!(
        published(&topics, JIRA_PROJECTS_CONSUMER_TOPIC, 1).await,
        "the projects were never published"
    );
    assert_eq!(observer.get_status(), ActorStatus::Running);

    // After the success the next pull is an interval away, not another retry.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let paths = paths(&server).await;
    let pulls = paths.iter().filter(|p| *p == PROJECTS).count();
    assert_eq!(pulls, 3, "two failures and one success: {paths:?}");
    // The token is checked before the first pull and again before each pull after a failure.
    let checks = paths.iter().filter(|p| *p == MYSELF).count();
    assert_eq!(checks, 3, "{paths:?}");
    observer.stop(None);
}

/// Runs `observer` against a site that treats the token as anonymous the first time it is
/// asked, and checks that it asked who the token is before pulling anything, asked again, and
/// then pulled and published on `topic`, still running.
async fn checks_the_token_first_and_again_until_accepted<A>(observer: A, topic: &str)
where
    A: Actor<Msg = JiraObserverMessage, Arguments = JiraObserverArgs>,
{
    let server = MockServer::start().await;
    fail_first(
        &server,
        MYSELF,
        ResponseTemplate::new(401).insert_header("x-ausername", "anonymous"),
        1,
    )
    .await;
    mount_site(&server).await;
    let (client, topics) = recorder().await;

    let (observer, _) = Actor::spawn(None, observer, args(&server, client))
        .await
        .unwrap();

    assert!(
        published(&topics, topic, 1).await,
        "{topic} was never published"
    );
    assert_eq!(observer.get_status(), ActorStatus::Running);
    // What the site is (serverInfo) is asked once, beside these.
    let paths: Vec<String> = paths(&server)
        .await
        .into_iter()
        .filter(|p| p != SERVER_INFO)
        .collect();
    assert_eq!(paths[..2], [MYSELF, MYSELF], "{paths:?}");
    assert!(paths[2..].iter().all(|p| p != MYSELF), "{paths:?}");
    observer.stop(None);
}

#[tokio::test]
async fn the_project_observer_checks_the_token_first_and_again_until_accepted() {
    checks_the_token_first_and_again_until_accepted(
        JiraProjectObserver,
        JIRA_PROJECTS_CONSUMER_TOPIC,
    )
    .await;
}

#[tokio::test]
async fn the_group_observer_checks_the_token_first_and_again_until_accepted() {
    checks_the_token_first_and_again_until_accepted(JiraGroupObserver, JIRA_GROUPS_CONSUMER_TOPIC)
        .await;
}

#[tokio::test]
async fn the_user_observer_checks_the_token_first_and_again_until_accepted() {
    checks_the_token_first_and_again_until_accepted(JiraUserObserver, JIRA_USERS_CONSUMER_TOPIC)
        .await;
}

#[tokio::test]
async fn the_issue_observer_checks_the_token_first_and_again_until_accepted() {
    checks_the_token_first_and_again_until_accepted(JiraIssueObserver, JIRA_ISSUES_CONSUMER_TOPIC)
        .await;
}

#[tokio::test]
async fn an_error_that_is_not_jiras_still_ends_the_observer() {
    let server = MockServer::start().await;
    mount_site(&server).await;
    // A broker client that has stopped: the publish fails, which is not Jira's doing.
    let (client, _) = recorder().await;
    client.stop_and_wait(None, None).await.unwrap();

    let (observer, handle) = Actor::spawn(None, JiraProjectObserver, args(&server, client))
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("the observer did not end")
        .unwrap();
    assert_eq!(observer.get_status(), ActorStatus::Stopped);
}

/// How far the issue observer's clock is ahead of the real one.
static AHEAD_SECS: AtomicU64 = AtomicU64::new(0);

fn clock() -> Instant {
    Instant::now() + Duration::from_secs(AHEAD_SECS.load(Ordering::SeqCst))
}

#[tokio::test]
async fn after_an_outage_the_first_incremental_pull_reaches_back_to_the_last_one_that_completed() {
    const OUTAGE: Duration = Duration::from_secs(3 * 3600);

    let server = MockServer::start().await;
    let issue =
        json!({ "issues": [{ "key": "PLAT-1", "fields": { "summary": "x" } }], "total": 1 });
    // The full pull completes; the next two fail; the one after completes.
    Mock::given(method("GET"))
        .and(path(SEARCH))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(SEARCH))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .with_priority(2)
        .mount(&server)
        .await;
    mount_site(&server).await;
    let (client, topics) = recorder().await;
    let mut args = args(&server, client);
    args.base_interval = 1;
    args.clock = clock;

    let (observer, _) = Actor::spawn(None, JiraIssueObserver, args).await.unwrap();
    assert!(published(&topics, JIRA_ISSUES_CONSUMER_TOPIC, 1).await);
    // The outage: the next pulls happen three hours after the full pull started.
    AHEAD_SECS.store(OUTAGE.as_secs(), Ordering::SeqCst);
    assert!(published(&topics, JIRA_ISSUES_CONSUMER_TOPIC, 2).await);
    observer.stop(None);

    let searches: Vec<Option<String>> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == SEARCH)
        .map(|r| {
            r.url
                .query_pairs()
                .find(|(k, _)| k == "jql")
                .map(|(_, v)| v.to_string())
        })
        .collect();
    assert_eq!(
        searches[0].as_deref(),
        Some("ORDER BY updated ASC"),
        "the first pull is full: {searches:?}"
    );
    let jql = searches[3].clone().expect("an incremental pull has a jql");
    let minutes: u64 = jql
        .strip_prefix("updated >= -")
        .and_then(|m| m.strip_suffix("m ORDER BY updated ASC"))
        .and_then(|m| m.parse().ok())
        .unwrap_or_else(|| panic!("unexpected jql {jql:?}"));
    assert!(
        minutes * 60 >= OUTAGE.as_secs() + 300,
        "{jql} does not reach back over the outage"
    );
}

/// The first issue pull waits for the state agent: whatever it answers that is not a value (a refusal, a graph that is
/// down, no answer in time) is asked again on the backoff, and nothing is pulled until it says; then the pull starts
/// at the graph's newest issue, ordered oldest first, and the next is an ordinary incremental one.
#[tokio::test]
async fn the_first_issue_pull_waits_for_the_state_agent_and_starts_at_the_graph_s_newest_issue() {
    let server = MockServer::start().await;
    mount_site(&server).await;
    let (client, topics) = recorder().await;
    let newest = NOW_MS - 2 * 3_600_000 - 30_000;
    let (ask, asked) = answers(
        vec![
            Err(StateError::Unavailable(
                "the graph could not be read".into(),
            )),
            Err(StateError::NoReply(
                "the state agent did not answer within 8s".into(),
            )),
            Err(StateError::Refused(
                "jira-observer may not read JiraIssue".into(),
            )),
        ],
        Ok(SoftValue::of(Some(newest))),
    );
    let mut args = args(&server, client);
    args.cursor = Some(ask);
    args.query_date = Some("2020-01-01".into());
    let (observer, _) = Actor::spawn(None, JiraIssueObserver, args).await.unwrap();

    assert!(published(&topics, JIRA_ISSUES_CONSUMER_TOPIC, 1).await);
    assert_eq!(observer.get_status(), ActorStatus::Running);
    assert_eq!(
        asked.load(Ordering::SeqCst),
        4,
        "three answers that were not values, then one"
    );
    // Two hours and thirty seconds back, and the overlap's five minutes: 126 minutes. QUERY_DATE does not apply.
    assert_eq!(
        searches(&server).await,
        ["updated >= -126m ORDER BY updated ASC"]
    );
    // Nothing was asked of Jira's search, nor of the token, while the state agent had not said.
    let paths = paths(&server).await;
    assert_eq!(
        paths.iter().filter(|p| *p == MYSELF).count(),
        1,
        "{paths:?}"
    );
    observer.stop(None);
}

/// A state agent that never says where to start: the observer keeps asking and never pulls everything.
#[tokio::test]
async fn without_an_answer_the_issue_observer_never_pulls_everything() {
    let server = MockServer::start().await;
    mount_site(&server).await;
    let (client, topics) = recorder().await;
    let (ask, asked) = answers(
        vec![],
        Err(StateError::Unavailable(
            "the graph could not be read".into(),
        )),
    );
    let mut args = args(&server, client);
    args.cursor = Some(ask);
    let (observer, _) = Actor::spawn(None, JiraIssueObserver, args).await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        asked.load(Ordering::SeqCst) >= 3,
        "it asks again on its backoff"
    );
    assert!(searches(&server).await.is_empty());
    assert!(
        !topics
            .lock()
            .unwrap()
            .iter()
            .any(|t| t == JIRA_ISSUES_CONSUMER_TOPIC)
    );
    assert_eq!(observer.get_status(), ActorStatus::Running);
    observer.stop(None);
}

/// A graph with no issue: the first pull is full, or from QUERY_DATE, ordered oldest first; asked once.
#[tokio::test]
async fn with_no_issue_in_the_graph_the_first_pull_is_from_query_date_or_full() {
    for (query_date, expected) in [
        (None, "ORDER BY updated ASC"),
        (
            Some("2024-01-01"),
            "updated > \"2024-01-01\" ORDER BY updated ASC",
        ),
    ] {
        let server = MockServer::start().await;
        mount_site(&server).await;
        let (client, topics) = recorder().await;
        let (ask, asked) = answers(vec![], Ok(SoftValue::of(None)));
        let mut args = args(&server, client);
        args.cursor = Some(ask);
        args.query_date = query_date.map(str::to_string);
        let (observer, _) = Actor::spawn(None, JiraIssueObserver, args).await.unwrap();
        assert!(published(&topics, JIRA_ISSUES_CONSUMER_TOPIC, 1).await);
        assert_eq!(searches(&server).await, [expected]);
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        observer.stop(None);
    }
}

/// The projects, groups and users observers do not ask the state agent anything.
#[tokio::test]
async fn the_other_observers_do_not_ask_the_state_agent() {
    let server = MockServer::start().await;
    mount_site(&server).await;
    let (client, topics) = recorder().await;
    let (ask, asked) = answers(vec![], Err(StateError::NoReply("never".into())));
    let mut args = args(&server, client);
    args.cursor = Some(ask);
    let (projects, _) = Actor::spawn(None, JiraProjectObserver, args.clone())
        .await
        .unwrap();
    let (groups, _) = Actor::spawn(None, JiraGroupObserver, args.clone())
        .await
        .unwrap();
    let (users, _) = Actor::spawn(None, JiraUserObserver, args).await.unwrap();
    assert!(published(&topics, JIRA_PROJECTS_CONSUMER_TOPIC, 1).await);
    assert!(published(&topics, JIRA_GROUPS_CONSUMER_TOPIC, 1).await);
    assert!(published(&topics, JIRA_USERS_CONSUMER_TOPIC, 1).await);
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    for observer in [projects, groups, users] {
        observer.stop(None);
    }
}

/// After the broker ended its session (what it was owed is lost), the issue observer's next pull starts from the
/// graph's cursor again, asking the state agent where, as its first did, rather than from its last pull
/// (cassini-backpressure.md, the decisions reviewed with the maintainer, 14).
#[tokio::test]
async fn after_its_session_ended_the_issue_observer_pulls_from_the_graph_s_cursor_again() {
    let server = MockServer::start().await;
    mount_site(&server).await;
    let (client, topics) = recorder().await;
    let (ask, asked) = answers(vec![], Ok(SoftValue::of(None)));
    let mut args = args(&server, client);
    args.cursor = Some(ask);
    let (observer, _) = Actor::spawn(None, JiraIssueObserver, args).await.unwrap();
    assert!(published(&topics, JIRA_ISSUES_CONSUMER_TOPIC, 1).await);
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    observer.cast(JiraObserverMessage::Resync).unwrap();
    observer
        .cast(JiraObserverMessage::Tick(
            jira_observer::Command::GetIssues("/rest/api/2/search".to_string()),
        ))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while searches(&server).await.len() < 2 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        asked.load(Ordering::SeqCst),
        2,
        "it asked where to start again"
    );
    assert_eq!(
        searches(&server).await,
        ["ORDER BY updated ASC", "ORDER BY updated ASC"],
        "from the graph's cursor (none: everything), not from its last pull"
    );
    observer.stop(None);
}

/// A Data Center's group picker: it takes no `startAt` (it ignores one), answers at most three groups (its
/// autocomplete limit) that are not in `exclude`, and says how many match.
fn group_picker(names: Vec<String>) -> impl Fn(&Request) -> ResponseTemplate + Send + Sync {
    move |request: &Request| {
        let excluded = params(request, "exclude");
        let groups: Vec<_> = names
            .iter()
            .filter(|n| !excluded.contains(n))
            .take(3)
            .map(|n| json!({ "name": n, "html": n, "labels": [] }))
            .collect();
        ResponseTemplate::new(200).set_body_json(json!({
            "header": format!("Showing {} of {} matching groups", groups.len(), names.len()),
            "total": names.len(),
            "groups": groups,
        }))
    }
}

/// The group picker is walked to its end: each request leaves out the groups already read, since a Data Center
/// pages the picker by `exclude`, not by `startAt`; every group is published once, together.
#[tokio::test]
async fn the_group_observer_walks_every_page_of_the_picker() {
    let server = MockServer::start().await;
    let names: Vec<String> = (1..=7).map(|i| format!("group-{i:02}")).collect();
    Mock::given(method("GET"))
        .and(path(GROUPS))
        .respond_with(group_picker(names.clone()))
        .with_priority(1)
        .mount(&server)
        .await;
    mount_site(&server).await;
    let (client, topics, published) = recording().await;
    let (observer, _) = Actor::spawn(None, JiraGroupObserver, args(&server, client))
        .await
        .unwrap();

    assert!(published_on(&topics, JIRA_GROUPS_CONSUMER_TOPIC, 1).await);
    let [JiraData::Groups(groups)] = &decoded(&published, JIRA_GROUPS_CONSUMER_TOPIC)[..] else {
        panic!("one message of groups");
    };
    let mut seen: Vec<String> = groups.iter().map(|g| g.name.clone()).collect();
    seen.sort();
    assert_eq!(seen, names);
    // Three pages: three, three and one.
    assert_eq!(asked(&server, GROUPS).await.len(), 3);
    observer.stop(None);
}

/// A Data Center's user search: `username=.` matches everyone, paged by `startAt` and `maxResults`, a plain list.
fn user_search(count: usize) -> impl Fn(&Request) -> ResponseTemplate + Send + Sync {
    move |request: &Request| {
        if params(request, "username") != ["."] {
            return ResponseTemplate::new(400);
        }
        let start = number(request, "startAt");
        let max = number(request, "maxResults");
        let users: Vec<_> = (start..count.min(start + max))
            .map(|i| {
                json!({ "name": format!("user{i}"), "key": format!("JIRAUSER{i}"), "displayName": format!("User {i}") })
            })
            .collect();
        ResponseTemplate::new(200).set_body_json(json!(users))
    }
}

/// The users are walked page by page of the user search, which a Data Center accepts, to an empty page; every user
/// is published once, together.
#[tokio::test]
async fn the_user_observer_walks_every_page_of_the_user_search() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(USER_SEARCH))
        .respond_with(user_search(107))
        .with_priority(1)
        .mount(&server)
        .await;
    mount_site(&server).await;
    let (client, topics, published) = recording().await;
    let (observer, _) = Actor::spawn(None, JiraUserObserver, args(&server, client))
        .await
        .unwrap();

    assert!(published_on(&topics, JIRA_USERS_CONSUMER_TOPIC, 1).await);
    let [JiraData::Users(users)] = &decoded(&published, JIRA_USERS_CONSUMER_TOPIC)[..] else {
        panic!("one message of users");
    };
    assert_eq!(users.len(), 107);
    let mut keys: Vec<&str> = users.iter().filter_map(|u| u.identity()).collect();
    keys.dedup();
    assert_eq!(keys.len(), 107);
    let starts: Vec<usize> = asked(&server, USER_SEARCH)
        .await
        .iter()
        .map(|r| number(r, "startAt"))
        .collect();
    assert_eq!(starts, [0, 50, 100, 107]);
    observer.stop(None);
}

/// A Data Center's issue search over `count` issues, paged by `startAt`, saying the total when `total` is set.
fn issue_search(count: usize, total: bool) -> impl Fn(&Request) -> ResponseTemplate + Send + Sync {
    move |request: &Request| {
        let start = number(request, "startAt");
        let max = number(request, "maxResults");
        let issues: Vec<_> = (start..count.min(start + max))
            .map(|i| json!({ "key": format!("PLAT-{i}"), "fields": { "summary": "x" } }))
            .collect();
        let mut body = json!({ "startAt": start, "maxResults": max, "issues": issues });
        if total {
            body["total"] = json!(count);
        }
        ResponseTemplate::new(200).set_body_json(body)
    }
}

/// Every page of a search is pulled and each issue published; a search that says its total ends there, and one
/// that does not (logged) is walked to an empty page rather than ended after its first.
#[tokio::test]
async fn the_issue_observer_walks_every_page_with_a_total_or_without_one() {
    for (total, searches) in [(true, vec![0, 50, 100]), (false, vec![0, 50, 100, 120])] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(SEARCH))
            .respond_with(issue_search(120, total))
            .with_priority(1)
            .mount(&server)
            .await;
        mount_site(&server).await;
        let (client, topics) = recorder().await;
        let (observer, _) = Actor::spawn(None, JiraIssueObserver, args(&server, client))
            .await
            .unwrap();

        assert!(
            published_on(&topics, JIRA_ISSUES_CONSUMER_TOPIC, 120).await,
            "total {total}"
        );
        let starts: Vec<usize> = asked(&server, SEARCH)
            .await
            .iter()
            .map(|r| number(r, "startAt"))
            .collect();
        assert_eq!(starts, searches, "total {total}");
        observer.stop(None);
    }
}

/// Waits up to five seconds for `topic` to have been published at least `count` times, then a little longer, so
/// that a walk that would go on is seen to.
async fn published_on(topics: &Arc<Mutex<Vec<String>>>, topic: &str, count: usize) -> bool {
    let reached = published(topics, topic, count).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    reached
}

/// A site that is rate limiting (429), or down for a while (503), says when to come back in `Retry-After`: the
/// observer waits that long, not its own shorter backoff, and then pulls.
#[tokio::test]
async fn retry_after_is_honoured_on_429_and_503() {
    for status in [429, 503] {
        let server = MockServer::start().await;
        fail_first(
            &server,
            PROJECTS,
            ResponseTemplate::new(status).insert_header("Retry-After", "1"),
            1,
        )
        .await;
        mount_site(&server).await;
        let (client, topics) = recorder().await;
        let started = Instant::now();
        let (observer, _) = Actor::spawn(None, JiraProjectObserver, args(&server, client))
            .await
            .unwrap();

        assert!(published(&topics, JIRA_PROJECTS_CONSUMER_TOPIC, 1).await);
        let waited = started.elapsed();
        // The test's backoff is 20 ms; Retry-After says a second.
        assert!(
            waited >= Duration::from_secs(1),
            "{status}: pulled again after {waited:?}"
        );
        assert_eq!(asked(&server, PROJECTS).await.len(), 2, "{status}");
        observer.stop(None);
    }
}

/// Fails the request to `endpoint` whose `startAt` is `start_at`, the first `times` it is asked.
async fn fail_page(server: &MockServer, endpoint: &str, start_at: usize, times: u64) {
    Mock::given(method("GET"))
        .and(path(endpoint))
        .and(wiremock::matchers::query_param(
            "startAt",
            start_at.to_string(),
        ))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(times)
        .with_priority(1)
        .mount(server)
        .await;
}

/// A page that fails is asked again, and the walk goes on from it: the pages before it are neither asked nor
/// published twice.
#[tokio::test]
async fn an_issue_pull_that_fails_on_a_page_resumes_at_that_page() {
    let server = MockServer::start().await;
    fail_page(&server, SEARCH, 50, 1).await;
    Mock::given(method("GET"))
        .and(path(SEARCH))
        .respond_with(issue_search(120, true))
        .with_priority(2)
        .mount(&server)
        .await;
    mount_site(&server).await;
    let (client, topics) = recorder().await;
    let (observer, _) = Actor::spawn(None, JiraIssueObserver, args(&server, client))
        .await
        .unwrap();

    assert!(published_on(&topics, JIRA_ISSUES_CONSUMER_TOPIC, 120).await);
    let starts: Vec<usize> = asked(&server, SEARCH)
        .await
        .iter()
        .map(|r| number(r, "startAt"))
        .collect();
    assert_eq!(starts, [0, 50, 50, 100]);
    let issues = topics
        .lock()
        .unwrap()
        .iter()
        .filter(|t| *t == JIRA_ISSUES_CONSUMER_TOPIC)
        .count();
    assert_eq!(issues, 120, "no page published twice");
    // The same question each time: the resumed walk asks what the first attempt asked.
    let jql: Vec<String> = searches(&server).await;
    assert!(jql.iter().all(|q| q == &jql[0]), "{jql:?}");
    observer.stop(None);
}

#[tokio::test]
async fn a_user_pull_that_fails_on_a_page_resumes_at_that_page() {
    let server = MockServer::start().await;
    fail_page(&server, USER_SEARCH, 50, 1).await;
    Mock::given(method("GET"))
        .and(path(USER_SEARCH))
        .respond_with(user_search(107))
        .with_priority(2)
        .mount(&server)
        .await;
    mount_site(&server).await;
    let (client, topics, published) = recording().await;
    let (observer, _) = Actor::spawn(None, JiraUserObserver, args(&server, client))
        .await
        .unwrap();

    assert!(published_on(&topics, JIRA_USERS_CONSUMER_TOPIC, 1).await);
    let [JiraData::Users(users)] = &decoded(&published, JIRA_USERS_CONSUMER_TOPIC)[..] else {
        panic!("one message of users");
    };
    assert_eq!(users.len(), 107);
    let starts: Vec<usize> = asked(&server, USER_SEARCH)
        .await
        .iter()
        .map(|r| number(r, "startAt"))
        .collect();
    assert_eq!(starts, [0, 50, 50, 100, 107]);
    observer.stop(None);
}

#[tokio::test]
async fn a_group_pull_that_fails_on_a_page_resumes_at_that_page() {
    let server = MockServer::start().await;
    let names: Vec<String> = (1..=7).map(|i| format!("group-{i:02}")).collect();
    // The second page (three groups left out) fails once.
    let picker = group_picker(names.clone());
    let failed = Arc::new(AtomicU64::new(0));
    let failed_in = failed.clone();
    Mock::given(method("GET"))
        .and(path(GROUPS))
        .respond_with(move |request: &Request| {
            if params(request, "exclude").len() == 3
                && failed_in.fetch_add(1, Ordering::SeqCst) == 0
            {
                return ResponseTemplate::new(503);
            }
            picker(request)
        })
        .with_priority(1)
        .mount(&server)
        .await;
    mount_site(&server).await;
    let (client, topics, published) = recording().await;
    let (observer, _) = Actor::spawn(None, JiraGroupObserver, args(&server, client))
        .await
        .unwrap();

    assert!(published_on(&topics, JIRA_GROUPS_CONSUMER_TOPIC, 1).await);
    let [JiraData::Groups(groups)] = &decoded(&published, JIRA_GROUPS_CONSUMER_TOPIC)[..] else {
        panic!("one message of groups");
    };
    assert_eq!(groups.len(), 7);
    let excluded: Vec<usize> = asked(&server, GROUPS)
        .await
        .iter()
        .map(|r| params(r, "exclude").len())
        .collect();
    assert_eq!(
        excluded,
        [0, 3, 3, 6],
        "resumed with the three already read left out"
    );
    observer.stop(None);
}

/// What the site says it is (`serverInfo`'s `deploymentType`) decides how it is read, over the setting; a site that
/// does not say leaves the setting. Here the users observer, whose endpoint differs: the user search on Server and
/// Data Center, the picker on Cloud.
#[tokio::test]
async fn the_deployment_is_what_the_site_says_it_is_and_otherwise_the_setting() {
    for (says, configured, endpoint) in [
        (
            json!({ "deploymentType": "Server" }),
            JiraDeployment::Cloud,
            USER_SEARCH,
        ),
        (
            json!({ "deploymentType": "Cloud" }),
            JiraDeployment::ServerOrDataCenter,
            USER_PICKER,
        ),
        (
            json!({ "version": "10.3.26" }),
            JiraDeployment::Cloud,
            USER_PICKER,
        ),
        (json!({}), JiraDeployment::ServerOrDataCenter, USER_SEARCH),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(SERVER_INFO))
            .respond_with(ResponseTemplate::new(200).set_body_json(says.clone()))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(USER_PICKER))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "users": [], "total": 0 })),
            )
            .with_priority(1)
            .mount(&server)
            .await;
        mount_site(&server).await;
        let (client, topics) = recorder().await;
        let mut args = args(&server, client);
        args.deployment = configured;
        let (observer, _) = Actor::spawn(None, JiraUserObserver, args).await.unwrap();

        assert!(
            published(&topics, JIRA_USERS_CONSUMER_TOPIC, 1).await,
            "{says}"
        );
        assert_eq!(asked(&server, endpoint).await.len(), 1, "{says}");
        assert_eq!(asked(&server, SERVER_INFO).await.len(), 1, "{says}");
        observer.stop(None);
    }
}

mod apache {
    include!("../../common/fixtures/apache.rs");
}

/// A site that answers as Apache's Jira did (its serverInfo, its fields, a page of its issues with their changelogs,
/// total 300): the issue observer, reading anonymously as it would there, publishes each issue in the pinned form.
#[tokio::test]
async fn apache_s_answers_are_published_as_the_processor_expects() {
    let server = MockServer::start().await;
    for (endpoint, body) in [
        (SERVER_INFO, apache::SERVER_INFO),
        ("/rest/api/2/field", apache::FIELDS),
    ] {
        Mock::given(method("GET"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .with_priority(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(SEARCH))
        .and(wiremock::matchers::query_param("startAt", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(apache::SEARCH, "application/json"))
        .with_priority(1)
        .mount(&server)
        .await;
    // The rest of the 300 are not here: the next page is empty, which ends the walk.
    Mock::given(method("GET"))
        .and(path(SEARCH))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({ "startAt": 3, "maxResults": 50, "total": 300, "issues": [] }),
            ),
        )
        .with_priority(2)
        .mount(&server)
        .await;
    mount_site(&server).await;
    let (client, topics, published) = recording().await;
    let mut args = args(&server, client);
    args.auth = JiraAuth::Anonymous;
    let (observer, _) = Actor::spawn(None, JiraIssueObserver, args).await.unwrap();

    assert!(published_on(&topics, JIRA_ISSUES_CONSUMER_TOPIC, 3).await);
    let sent: Vec<serde_json::Value> = decoded(&published, JIRA_ISSUES_CONSUMER_TOPIC)
        .into_iter()
        .map(|data| match data {
            JiraData::Issues(issue) => serde_json::from_str(&issue.json).unwrap(),
            _ => panic!("not an issue"),
        })
        .collect();
    let pinned: Vec<serde_json::Value> = apache::PUBLISHED
        .iter()
        .map(|p| serde_json::from_str(p).unwrap())
        .collect();
    assert_eq!(sent, pinned);
    observer.stop(None);
}

/// Reading anonymously (JIRA_ANONYMOUS): no request carries a credential, and the token is never checked, since
/// there is none.
#[tokio::test]
async fn an_anonymous_observer_sends_no_credential_and_checks_no_token() {
    let server = MockServer::start().await;
    mount_site(&server).await;
    let (client, topics) = recorder().await;
    let mut args = args(&server, client);
    args.auth = JiraAuth::Anonymous;
    let (projects, _) = Actor::spawn(None, JiraProjectObserver, args.clone())
        .await
        .unwrap();
    let (issues, _) = Actor::spawn(None, JiraIssueObserver, args).await.unwrap();

    assert!(published(&topics, JIRA_PROJECTS_CONSUMER_TOPIC, 1).await);
    assert!(published(&topics, JIRA_ISSUES_CONSUMER_TOPIC, 1).await);
    let requests = server.received_requests().await.unwrap();
    assert!(!requests.is_empty());
    assert!(
        requests
            .iter()
            .all(|r| !r.headers.contains_key("authorization")),
        "a credential was sent"
    );
    assert!(
        requests.iter().all(|r| r.url.path() != MYSELF),
        "the token was checked"
    );
    projects.stop(None);
    issues.stop(None);
}
