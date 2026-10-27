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

//! The broker refuses and ends a revoked certificate's sessions (docs/architecture/key-custody.md, step 5). The
//! credentials agent publishes the whole set of revoked serials, signed; the broker holds it, as every agent does. A
//! revoked certificate may not register, except as a watcher, so the custody daemon still hears of its own
//! revocation; and on a change, every connection whose certificate is now revoked is closed, a watcher's apart.

#[path = "support/credentials.rs"]
mod credentials;
mod support;

use credentials::CredentialsAgent;
use credentials_agent_common::authority_update::AUTHORITY_SNAPSHOT_REPLIES_TOPIC;
use proptest::prelude::*;
use std::time::Duration;
use support::{TestBroker, TestClient, pause, serial};

const POD: &str = "git-repo-observer.polar.serviceaccount.cluster.local";
const OTHER: &str = "kube-observer.polar.serviceaccount.cluster.local";

async fn closed(client: &TestClient) {
    client
        .wait_for("the connection closed", Duration::from_secs(10), |c| {
            c.lost_its_connection()
        })
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_ends_when_its_certificate_is_revoked_and_another_s_does_not() {
    let _serial = serial().await;
    let broker = TestBroker::start().await;
    let credentials = CredentialsAgent::connect(&broker).await;
    let pod = TestClient::connect(&broker, POD).await;
    let other = TestClient::connect(&broker, OTHER).await;

    credentials.revoke(&[broker.pki.serial_of(POD)]);
    closed(&pod).await;
    pause().await;
    assert_eq!(
        pod.registration_events(),
        1,
        "its client connects again, and is not registered"
    );
    assert!(!other.lost_its_connection());
    other.subscribe("news");
    other
        .wait_for("another identity goes on", Duration::from_secs(10), |c| {
            c.subscribed("news")
        })
        .await;
    broker.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_certificate_cannot_register_but_may_watch() {
    let _serial = serial().await;
    let broker = TestBroker::start().await;
    let credentials = CredentialsAgent::connect(&broker).await;
    // Its revocation is believed once the broker has it: a connection of its own is closed.
    let first = TestClient::connect(&broker, POD).await;
    credentials.revoke(&[broker.pki.serial_of(POD)]);
    closed(&first).await;

    let again = TestClient::start_connecting(&broker, POD, None, false).await;
    closed(&again).await;
    pause().await;
    assert_eq!(again.registration_id(), None, "refused registration");

    let watcher = TestClient::connect_watching(&broker, POD).await;
    watcher.subscribe(AUTHORITY_SNAPSHOT_REPLIES_TOPIC);
    watcher
        .wait_for("the subscription", Duration::from_secs(10), |c| {
            c.subscribed(AUTHORITY_SNAPSHOT_REPLIES_TOPIC)
        })
        .await;
    broker.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_watcher_is_left_to_hear_its_own_revocation() {
    let _serial = serial().await;
    let broker = TestBroker::start().await;
    let credentials = CredentialsAgent::connect(&broker).await;
    let watcher = TestClient::connect_watching(&broker, POD).await;
    watcher.subscribe(AUTHORITY_SNAPSHOT_REPLIES_TOPIC);
    watcher
        .wait_for("the subscription", Duration::from_secs(10), |c| {
            c.subscribed(AUTHORITY_SNAPSHOT_REPLIES_TOPIC)
        })
        .await;
    credentials.revoke(&[broker.pki.serial_of(POD)]);
    assert_eq!(
        watcher
            .wait_for_messages(AUTHORITY_SNAPSHOT_REPLIES_TOPIC, 1)
            .await
            .len(),
        1
    );
    pause().await;
    assert!(!watcher.lost_its_connection());
    broker.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_identity_s_next_certificate_claims_what_the_revoked_one_s_session_held() {
    let _serial = serial().await;
    let broker = TestBroker::start_retaining(Some(60)).await;
    let credentials = CredentialsAgent::connect(&broker).await;
    let publisher = TestClient::connect(&broker, OTHER).await;
    let pod = TestClient::connect(&broker, POD).await;
    pod.subscribe("news");
    pod.wait_for("the subscription", Duration::from_secs(10), |c| {
        c.subscribed("news")
    })
    .await;
    credentials.revoke(&[broker.pki.serial_of(POD)]);
    closed(&pod).await;
    publisher.publish("news", b"while it was away");
    pause().await;
    pod.vanish().await;

    // The pod is issued again, under a new certificate: revocation is of a certificate, not of the identity.
    broker.pki.issue_client_carrying(POD, &[]);
    let next = TestClient::connect(&broker, POD).await;
    next.subscribe("news");
    assert_eq!(
        next.wait_for_messages("news", 1).await,
        vec![b"while it was away".to_vec()]
    );
    broker.stop().await;
}

/// Each of three identities: connected as an agent, a watcher, or both, and whether its certificate is revoked.
fn a_cluster() -> impl Strategy<Value = Vec<(bool, bool, bool)>> {
    proptest::collection::vec((any::<bool>(), any::<bool>(), any::<bool>()), 3)
}

const IDENTITIES: [&str; 3] = [
    "a.polar.serviceaccount.cluster.local",
    "b.polar.serviceaccount.cluster.local",
    "c.polar.serviceaccount.cluster.local",
];

proptest! {
    #![proptest_config(ProptestConfig { cases: 8, ..ProptestConfig::default() })]

    /// Whatever is connected and whatever is revoked: once the broker holds the set, no agent's connection under a
    /// revoked certificate stays open, every other stays, and of the certificates that come after only the
    /// unrevoked register as agents, while every one may watch.
    #[test]
    fn no_agent_s_connection_outlives_its_certificate_s_revocation(cluster in a_cluster()) {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let _serial = serial().await;
            // The late watchers below replace the first of each identity at once: the watchers' tenure is not what is
            // tested here.
            let broker = TestBroker::start_with_tenure(Duration::ZERO).await;
            let credentials = CredentialsAgent::connect(&broker).await;
            let mut agents = Vec::new();
            let mut watchers = Vec::new();
            for (identity, (agent, watching, _)) in IDENTITIES.iter().zip(&cluster) {
                if *agent {
                    agents.push((identity, TestClient::connect(&broker, identity).await));
                }
                if *watching {
                    watchers.push((identity, TestClient::connect_watching(&broker, identity).await));
                }
            }
            let revoked: Vec<&str> = IDENTITIES
                .iter()
                .zip(&cluster)
                .filter(|(_, (_, _, revoked))| *revoked)
                .map(|(identity, _)| *identity)
                .collect();
            credentials.revoke(&revoked.iter().map(|i| broker.pki.serial_of(i)).collect::<Vec<_>>());
            // Something to wait on whatever the case: a connection under a certificate revoked for the purpose.
            let marker = "marker.polar.serviceaccount.cluster.local";
            let mark = TestClient::connect(&broker, marker).await;
            credentials.revoke(
                &revoked.iter().map(|i| broker.pki.serial_of(i))
                    .chain([broker.pki.serial_of(marker)])
                    .collect::<Vec<_>>(),
            );
            closed(&mark).await;
            pause().await;
            for (identity, client) in &agents {
                assert_eq!(client.lost_its_connection(), revoked.contains(identity), "{identity}'s agent");
            }
            for (identity, client) in &watchers {
                assert!(!client.lost_its_connection(), "{identity}'s watcher");
            }
            for identity in IDENTITIES {
                let late = TestClient::start_connecting(&broker, identity, None, false).await;
                let watcher = TestClient::connect_watching(&broker, identity).await;
                if revoked.contains(&identity) {
                    closed(&late).await;
                    pause().await;
                    assert_eq!(late.registration_id(), None, "{identity} registered after its revocation");
                } else {
                    late.wait_for("a registration", Duration::from_secs(10), |c| c.registration_id().is_some()).await;
                }
                assert!(!watcher.lost_its_connection());
            }
            broker.stop().await;
        });
    }
}
