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

//! A watcher session (docs/architecture/key-custody.md, step 5): the custody daemon's, kept beside its identity's own
//! session so the daemon hears of its revocation whatever the workload does. Neither supersedes the other, nothing is
//! held for a watcher that goes, and a watcher may hear the credentials agent's authority and ask for it, nothing else.

#[path = "support/credentials.rs"]
mod credentials;
mod support;

use credentials::CredentialsAgent;
use credentials_agent_common::authority_update::{
    AUTHORITY_SNAPSHOT_REPLIES_TOPIC, AUTHORITY_SNAPSHOT_REQUESTS_TOPIC, AUTHORITY_UPDATES_TOPIC,
};
use std::time::Duration;
use support::{TestBroker, TestClient, pause, serial};

const POD: &str = "git-repo-observer.polar.serviceaccount.cluster.local";

#[tokio::test(flavor = "multi_thread")]
async fn a_watcher_and_its_identity_s_session_stand_side_by_side() {
    let _serial = serial().await;
    let broker = TestBroker::start().await;
    let watcher = TestClient::connect_watching(&broker, POD).await;
    let agent = TestClient::connect(&broker, POD).await;
    pause().await;
    assert!(
        !watcher.lost_its_connection(),
        "the identity's session does not close its watcher"
    );
    let second = TestClient::connect_watching(&broker, POD).await;
    pause().await;
    assert!(
        !agent.lost_its_connection(),
        "a watcher does not close its identity's session"
    );
    assert!(!watcher.lost_its_connection() && !second.lost_its_connection());

    // A new pod of the identity supersedes the old one's session, and no watcher.
    let _replacement = TestClient::connect(&broker, POD).await;
    agent
        .wait_for("the old session superseded", Duration::from_secs(10), |c| {
            c.lost_its_connection()
        })
        .await;
    pause().await;
    assert!(!watcher.lost_its_connection() && !second.lost_its_connection());
    broker.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_watcher_may_hear_authority_and_ask_for_it_and_nothing_else() {
    let _serial = serial().await;
    // The open policy permits everything: the watcher's limit is its own.
    let broker = TestBroker::start().await;
    let watcher = TestClient::connect_watching(&broker, POD).await;
    for topic in [AUTHORITY_UPDATES_TOPIC, AUTHORITY_SNAPSHOT_REPLIES_TOPIC] {
        watcher.subscribe(topic);
        watcher
            .wait_for(topic, Duration::from_secs(10), |c| c.subscribed(topic))
            .await;
    }
    watcher.publish(AUTHORITY_SNAPSHOT_REQUESTS_TOPIC, b"a request");
    watcher
        .wait_for("the request taken", Duration::from_secs(10), |c| {
            c.acknowledged(AUTHORITY_SNAPSHOT_REQUESTS_TOPIC) == 1
        })
        .await;

    watcher.subscribe("news");
    watcher
        .wait_for("a refusal", Duration::from_secs(10), |c| {
            !c.refusals("news").is_empty()
        })
        .await;
    assert!(watcher.refusals("news")[0].contains("watcher"));
    assert!(!watcher.subscribed("news"));
    for (topic, why) in [
        (AUTHORITY_UPDATES_TOPIC, "only the credentials agent's"),
        ("news", "nothing else"),
    ] {
        watcher.publish(topic, b"x");
        watcher
            .wait_for(why, Duration::from_secs(10), |c| {
                !c.refusals(topic).is_empty()
            })
            .await;
    }
    // Its identity's own session, under the same certificate, is not limited so.
    let agent = TestClient::connect(&broker, POD).await;
    agent.subscribe("news");
    agent
        .wait_for("the agent's subscription", Duration::from_secs(10), |c| {
            c.subscribed("news")
        })
        .await;
    broker.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_held_for_a_watcher_nor_claimed_by_one() {
    let _serial = serial().await;
    let broker = TestBroker::start_retaining(Some(60)).await;
    let credentials = CredentialsAgent::connect(&broker).await;
    // Subscribed throughout, so the topic itself keeps nothing for a first subscriber: what reaches a watcher below
    // was held for a session, or not at all.
    let bystander =
        TestClient::connect(&broker, "kube-observer.polar.serviceaccount.cluster.local").await;
    bystander.subscribe(AUTHORITY_SNAPSHOT_REPLIES_TOPIC);
    bystander
        .wait_for(
            "the bystander's subscription",
            Duration::from_secs(10),
            |c| c.subscribed(AUTHORITY_SNAPSHOT_REPLIES_TOPIC),
        )
        .await;

    // A watcher that goes is ended at once: nothing published meanwhile waits for the next.
    let watcher = TestClient::connect_watching(&broker, POD).await;
    watcher.subscribe(AUTHORITY_SNAPSHOT_REPLIES_TOPIC);
    watcher
        .wait_for("the subscription", Duration::from_secs(10), |c| {
            c.subscribed(AUTHORITY_SNAPSHOT_REPLIES_TOPIC)
        })
        .await;
    let gone = watcher.registration_id().unwrap();
    watcher.vanish().await;
    // Its session is gone with it, not kept for the retention window as an agent's is.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while broker.has_session(&gone).await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the watcher's session was kept"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    credentials.revoke(&[]);
    pause().await;
    let next = TestClient::connect_watching(&broker, POD).await;
    next.subscribe(AUTHORITY_SNAPSHOT_REPLIES_TOPIC);
    next.wait_for("the subscription", Duration::from_secs(10), |c| {
        c.subscribed(AUTHORITY_SNAPSHOT_REPLIES_TOPIC)
    })
    .await;
    assert_eq!(
        bystander
            .wait_for_messages(AUTHORITY_SNAPSHOT_REPLIES_TOPIC, 1)
            .await
            .len(),
        1
    );
    pause().await;
    assert!(
        next.received(AUTHORITY_SNAPSHOT_REPLIES_TOPIC).is_empty(),
        "nothing was held for the watcher that went"
    );

    // What a superseded session of its identity holds is not the watcher's to claim, and waits for the session that
    // took over.
    let agent = TestClient::connect(&broker, POD).await;
    agent.subscribe(AUTHORITY_SNAPSHOT_REPLIES_TOPIC);
    agent
        .wait_for("the subscription", Duration::from_secs(10), |c| {
            c.subscribed(AUTHORITY_SNAPSHOT_REPLIES_TOPIC)
        })
        .await;
    agent.vanish().await;
    pause().await;
    credentials.revoke(&[]);
    pause().await;
    let successor = TestClient::connect(&broker, POD).await;
    pause().await;
    let third = TestClient::connect_watching(&broker, POD).await;
    third.subscribe(AUTHORITY_SNAPSHOT_REPLIES_TOPIC);
    third
        .wait_for("the subscription", Duration::from_secs(10), |c| {
            c.subscribed(AUTHORITY_SNAPSHOT_REPLIES_TOPIC)
        })
        .await;
    pause().await;
    assert!(
        third.received(AUTHORITY_SNAPSHOT_REPLIES_TOPIC).is_empty(),
        "a watcher claims nothing held for its identity's session"
    );
    successor.subscribe(AUTHORITY_SNAPSHOT_REPLIES_TOPIC);
    assert_eq!(
        successor
            .wait_for_messages(AUTHORITY_SNAPSHOT_REPLIES_TOPIC, 1)
            .await
            .len(),
        1,
        "the session that took over claims what was held"
    );
    broker.stop().await;
}
