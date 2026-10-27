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
    // Under a second certificate of the identity: under the same one, the newer would replace it (below).
    let (cert, key, _) = broker.pki.another_client("second", POD);
    let second = TestClient::start_watching_with(&broker, &cert, &key).await;
    second
        .wait_for("the second watcher", Duration::from_secs(10), |c| {
            c.registration_id().is_some()
        })
        .await;
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
    // The watcher before it goes first: under one certificate, one watcher at a time.
    next.vanish().await;
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

// One watcher per certificate: it holds its place for a tenure, then the newest replaces it (`cassini-broker.md`,
// "Capping watchers").

#[tokio::test(flavor = "multi_thread")]
async fn a_watcher_holds_its_place_for_its_tenure_and_a_newer_one_replaces_it_after() {
    let _serial = serial().await;
    let tenure = Duration::from_secs(2);
    let broker = TestBroker::start_with_tenure(tenure).await;
    let agent = TestClient::connect(&broker, POD).await;
    let first = TestClient::connect_watching(&broker, POD).await;
    let first_session = first.registration_id().unwrap();

    // Within the tenure a second is refused, and closed so that it comes again.
    let refused = TestClient::start_connecting(&broker, POD, None, true).await;
    refused
        .wait_for("the second closed", Duration::from_secs(10), |c| {
            c.lost_its_connection()
        })
        .await;
    assert_eq!(refused.registration_id(), None);
    refused.vanish().await;
    assert!(!first.lost_its_connection(), "the first keeps its place");

    // After it, the newest replaces the first: closed, its session gone.
    tokio::time::sleep(tenure).await;
    let third = TestClient::connect_watching(&broker, POD).await;
    first
        .wait_for("the first watcher closed", Duration::from_secs(10), |c| {
            c.lost_its_connection()
        })
        .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while broker.has_session(&first_session).await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the first watcher's session was kept"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    pause().await;
    assert!(!third.lost_its_connection(), "the newest stays");
    assert!(
        !agent.lost_its_connection(),
        "the identity's own session is untouched"
    );
    broker.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn watchers_under_two_certificates_of_one_identity_both_stay() {
    let _serial = serial().await;
    let broker = TestBroker::start_with_tenure(Duration::from_secs(600)).await;
    let old_pod = TestClient::connect_watching(&broker, POD).await;
    // A second pod of the workload, under its own certificate, as in a rollout.
    let (cert, key, _) = broker.pki.another_client("new-pod", POD);
    let new_pod = TestClient::start_watching_with(&broker, &cert, &key).await;
    new_pod
        .wait_for("the new pod's watcher", Duration::from_secs(10), |c| {
            c.registration_id().is_some()
        })
        .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!old_pod.lost_its_connection() && !new_pod.lost_its_connection());
    assert!(
        broker
            .has_session(&old_pod.registration_id().unwrap())
            .await
    );
    assert!(
        broker
            .has_session(&new_pod.registration_id().unwrap())
            .await
    );
    broker.stop().await;
}

mod capping {
    use super::*;
    use proptest::prelude::*;

    /// A step: a watcher registers under one of two certificates of one identity, or a live one goes.
    #[derive(Debug, Clone)]
    enum Step {
        Watch(usize),
        Go(usize),
    }

    fn steps() -> impl Strategy<Value = Vec<Step>> {
        proptest::collection::vec(
            prop_oneof![
                (0usize..2).prop_map(Step::Watch),
                (0usize..4).prop_map(Step::Go)
            ],
            1..6,
        )
    }

    /// Runs `steps` against a broker whose watchers hold their places for `tenure`, and says, for each certificate, the
    /// watchers made under it in order, with whether each registered, and which of them are live at the end.
    async fn run(steps: &[Step], tenure: Duration) -> Vec<(Vec<bool>, Vec<usize>)> {
        let _serial = serial().await;
        let broker = TestBroker::start_with_tenure(tenure).await;
        let certs = [
            broker.pki.another_client("a", POD),
            broker.pki.another_client("b", POD),
        ];
        // Every watcher made: its certificate, the client, whether it registered, whether the test ended it.
        let mut watchers: Vec<(usize, TestClient, bool, bool)> = Vec::new();
        for step in steps {
            match step {
                Step::Watch(c) => {
                    let w =
                        TestClient::start_watching_with(&broker, &certs[*c].0, &certs[*c].1).await;
                    w.wait_for("a watcher's answer", Duration::from_secs(10), |w| {
                        w.registration_id().is_some() || w.lost_its_connection()
                    })
                    .await;
                    let registered = w.registration_id().is_some();
                    if !registered {
                        w.vanish().await;
                    }
                    watchers.push((*c, w, registered, !registered));
                }
                Step::Go(n) => {
                    let live: Vec<usize> = (0..watchers.len())
                        .filter(|i| watchers[*i].2 && !watchers[*i].3)
                        .collect();
                    if !live.is_empty() {
                        let i = live[n % live.len()];
                        watchers[i].1.vanish().await;
                        watchers[i].3 = true;
                    }
                }
            }
            pause().await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut by_certificate = Vec::new();
        for c in 0..2 {
            let mine: Vec<&(usize, TestClient, bool, bool)> =
                watchers.iter().filter(|w| w.0 == c).collect();
            let mut live = Vec::new();
            for (i, w) in mine.iter().enumerate() {
                if w.2 && !w.3 && broker.has_session(&w.1.registration_id().unwrap()).await {
                    live.push(i);
                }
            }
            by_certificate.push((mine.iter().map(|w| w.2).collect(), live));
        }
        broker.stop().await;
        by_certificate
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 8, ..ProptestConfig::default() })]

        /// With no tenure, after any sequence of watchers registering and going: every one registers, and at most one
        /// is live for each certificate, the newest under it.
        #[test]
        fn with_no_tenure_the_newest_watcher_of_a_certificate_is_its_only_one(steps in steps()) {
            for (c, (registered, live)) in runtime().block_on(run(&steps, Duration::ZERO)).iter().enumerate() {
                prop_assert!(registered.iter().all(|r| *r), "certificate {}: one refused", c);
                prop_assert!(live.len() <= 1, "certificate {}: {} live", c, live.len());
                if let Some(&i) = live.first() {
                    prop_assert_eq!(i, registered.len() - 1, "certificate {}: the live one is not the newest", c);
                }
            }
        }

        /// With a tenure longer than the test, after any sequence: a watcher registers exactly when no other under its
        /// certificate is live, and the one live is the one that registered last.
        #[test]
        fn within_its_tenure_a_watcher_keeps_its_place_and_others_are_refused(steps in steps()) {
            // The model: under each certificate, whether a watcher holds the place, step by step.
            let mut expected: [Vec<bool>; 2] = [Vec::new(), Vec::new()];
            let mut holder: [Option<usize>; 2] = [None, None];
            let mut made: Vec<(usize, bool, bool)> = Vec::new();
            for step in &steps {
                match step {
                    Step::Watch(c) => {
                        let admitted = holder[*c].is_none();
                        expected[*c].push(admitted);
                        if admitted { holder[*c] = Some(made.len()); }
                        made.push((*c, admitted, !admitted));
                    }
                    Step::Go(n) => {
                        let live: Vec<usize> = (0..made.len()).filter(|i| made[*i].1 && !made[*i].2).collect();
                        if !live.is_empty() {
                            let i = live[n % live.len()];
                            made[i].2 = true;
                            holder[made[i].0] = None;
                        }
                    }
                }
            }
            for (c, (registered, live)) in runtime().block_on(run(&steps, Duration::from_secs(600))).iter().enumerate() {
                prop_assert_eq!(registered, &expected[c], "certificate {}", c);
                prop_assert!(live.len() <= 1, "certificate {}: {} live", c, live.len());
                if let Some(h) = holder[c] {
                    let index = made.iter().take(h).filter(|m| m.0 == c).count();
                    prop_assert_eq!(live, &vec![index], "certificate {}", c);
                } else {
                    prop_assert!(live.is_empty(), "certificate {}: one live, none expected", c);
                }
            }
        }
    }
}
