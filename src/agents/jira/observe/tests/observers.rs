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
use ractor::{Actor, ActorProcessingErr, ActorRef, ActorStatus, async_trait};
use serde_json::json;
use soft_state_common::{Answer, StateReply};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MYSELF: &str = "/rest/api/2/myself";
const PROJECTS: &str = "/rest/api/2/project";
const SEARCH: &str = "/rest/api/2/search";

/// Stands in for the broker client and keeps the topic of every publish.
struct Recorder {
    topics: Arc<Mutex<Vec<String>>>,
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
        if let TcpClientMessage::Publish { topic, .. } = message {
            self.topics.lock().unwrap().push(topic);
        }
        Ok(())
    }
}

async fn recorder() -> (ActorRef<TcpClientMessage>, Arc<Mutex<Vec<String>>>) {
    let topics = Arc::new(Mutex::new(Vec::new()));
    let (actor, _) = Actor::spawn(
        None,
        Recorder {
            topics: topics.clone(),
        },
        (),
    )
    .await
    .unwrap();
    (actor, topics)
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
        cursor: Some(answers(vec![], Answer::Replied(StateReply::Value(None))).0),
        wall_clock: || NOW_MS,
    }
}

/// The wall clock of these tests, in epoch milliseconds.
const NOW_MS: i64 = 1_704_164_645_000;

/// A stand-in state agent: it gives `script`'s answers in turn, then `then` for ever, and counts the questions.
fn answers(script: Vec<Answer>, then: Answer) -> (AskCursor, Arc<AtomicU64>) {
    let asked = Arc::new(AtomicU64::new(0));
    let counter = asked.clone();
    let script = Arc::new(Mutex::new(std::collections::VecDeque::from(script)));
    let ask: AskCursor = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        let answer = script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| then.clone());
        Box::pin(async move { answer })
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
        (
            "/rest/api/2/groups/picker",
            json!({ "groups": [], "total": 0 }),
        ),
        (
            "/rest/api/2/user/picker",
            json!({ "users": [], "total": 0 }),
        ),
        ("/rest/api/2/field", json!([])),
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
    let paths = paths(&server).await;
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
            Answer::Replied(StateReply::Unavailable(
                "the graph could not be read".into(),
            )),
            Answer::NoReply("the state agent did not answer within 8s".into()),
            Answer::Replied(StateReply::Refused(
                "jira-observer may not read JiraIssue".into(),
            )),
        ],
        Answer::Replied(StateReply::Value(Some(newest))),
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
        Answer::Replied(StateReply::Unavailable(
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
            "updated > 2024-01-01 ORDER BY updated ASC",
        ),
    ] {
        let server = MockServer::start().await;
        mount_site(&server).await;
        let (client, topics) = recorder().await;
        let (ask, asked) = answers(vec![], Answer::Replied(StateReply::Value(None)));
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
    let (ask, asked) = answers(vec![], Answer::NoReply("never".into()));
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
