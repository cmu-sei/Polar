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

//! Many pods of one multi-instance workload, each an instance with its own certificate and session
//! (docs/architecture/authority-model.md, "Multi-instance workloads"): sessions supersede each other by identity and
//! instance, so two pods are two sessions, and a pod restarted in place (the same UID, a new key) still supersedes its
//! own. A single-instance workload's sessions are as they were (supersede.rs).
mod support;
use cassini_broker::session::{may_resume_as, supersedes};
use proptest::prelude::*;
use std::time::Duration;
use support::{TestClient, pause, with_broker};

const WORKLOAD: &str = "build-job.polar.serviceaccount.cluster.local";

#[tokio::test(flavor = "multi_thread")]
async fn two_pods_of_a_multi_instance_workload_are_two_sessions_and_neither_closes_the_other() {
    with_broker(|broker| async move {
        let a = TestClient::connect_as_instance(&broker, WORKLOAD, "uid-a", None).await;
        let b = TestClient::connect_as_instance(&broker, WORKLOAD, "uid-b", None).await;
        assert_ne!(a.registration_id(), b.registration_id());
        a.subscribe("news");
        b.subscribe("news");
        pause().await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"for both");
        assert_eq!(a.wait_for_messages("news", 1).await, [b"for both".to_vec()]);
        assert_eq!(b.wait_for_messages("news", 1).await, [b"for both".to_vec()]);
        pause().await;
        assert!(
            !a.lost_its_connection(),
            "the second pod did not supersede the first"
        );
        assert!(!b.lost_its_connection());
        assert!(broker.has_session(&a.registration_id().unwrap()).await);
        assert!(broker.has_session(&b.registration_id().unwrap()).await);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pod_restarted_in_place_supersedes_its_own_session_and_takes_what_it_held() {
    with_broker(|broker| async move {
        let before = TestClient::connect_as_instance(&broker, WORKLOAD, "uid-a", None).await;
        let old = before.registration_id().unwrap();
        before.subscribe("news");
        let other = TestClient::connect_as_instance(&broker, WORKLOAD, "uid-b", None).await;
        other.subscribe("news");
        pause().await;
        before.vanish().await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"while you were gone");
        pause().await;

        // The same pod, a new key: it supersedes its own session and inherits what was held for it.
        let after = TestClient::connect_as_instance(&broker, WORKLOAD, "uid-a", None).await;
        after.subscribe("news");
        assert_eq!(
            after.wait_for_messages("news", 1).await,
            [b"while you were gone".to_vec()]
        );
        let late =
            TestClient::connect_as_instance(&broker, WORKLOAD, "uid-a", Some(old.clone())).await;
        assert_ne!(
            late.registration_id().unwrap(),
            old,
            "superseded: never resumed"
        );
        // The other pod was untouched by either.
        pause().await;
        assert!(!other.lost_its_connection());
        assert_eq!(other.received("news").len(), 1);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn one_pod_neither_resumes_nor_inherits_another_pod_s_session() {
    with_broker(|broker| async move {
        let a = TestClient::connect_as_instance(&broker, WORKLOAD, "uid-a", None).await;
        let session = a.registration_id().unwrap();
        a.subscribe("news");
        pause().await;
        a.vanish().await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"for pod a");
        pause().await;

        // Pod b with a's registration id: a session of its own, and nothing that was held for a.
        let b = TestClient::connect_as_instance(&broker, WORKLOAD, "uid-b", Some(session.clone()))
            .await;
        assert_ne!(b.registration_id().unwrap(), session);
        b.subscribe("news");
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(b.received("news").is_empty(), "not its to take");

        // Pod a comes back and resumes its own.
        let back =
            TestClient::connect_as_instance(&broker, WORKLOAD, "uid-a", Some(session.clone()))
                .await;
        assert_eq!(
            back.registration_id().unwrap(),
            session,
            "still a's to resume"
        );
        assert_eq!(
            back.wait_for_messages("news", 1).await,
            [b"for pod a".to_vec()]
        );
    })
    .await;
}

/// The broker stamps the publisher's instance beside `published_by` on every delivery, from the verified certificate
/// as it takes the identity: a message names the workload and the pod it came from. A single-instance publisher's
/// deliveries carry none, and an anonymous publish carries neither.
#[tokio::test(flavor = "multi_thread")]
async fn every_delivery_names_the_pod_it_came_from_beside_the_workload() {
    with_broker(|broker| async move {
        let sink = TestClient::connect(&broker, "sink.test").await;
        sink.subscribe("news");
        pause().await;
        let a = TestClient::connect_as_instance(&broker, WORKLOAD, "uid-a", None).await;
        let b = TestClient::connect_as_instance(&broker, WORKLOAD, "uid-b", None).await;
        let one = TestClient::connect(&broker, "publisher.test").await;
        a.publish("news", b"from a");
        sink.wait_for_messages("news", 1).await;
        b.publish("news", b"from b");
        sink.wait_for_messages("news", 2).await;
        one.publish("news", b"from one");
        sink.wait_for_messages("news", 3).await;
        a.transport
            .cast(cassini_client::TcpClientMessage::publish_anonymous(
                "news".to_string(),
                b"from no one".to_vec(),
                None,
            ))
            .unwrap();
        sink.wait_for_messages("news", 4).await;
        let pod = |uid: &str| {
            Some(cassini_types::Instance {
                uid: uid.to_string(),
                pod: Some(format!("{uid}-pod")),
            })
        };
        assert_eq!(
            sink.publishers_of("news"),
            [
                (Some(WORKLOAD.to_string()), pod("uid-a")),
                (Some(WORKLOAD.to_string()), pod("uid-b")),
                (Some("publisher.test".to_string()), None),
                (None, None),
            ]
        );
    })
    .await;
}

#[test]
fn supersession_and_resumption_are_by_identity_and_instance() {
    let w = WORKLOAD;
    // One instance: as before.
    assert!(supersedes((w, None), (Some(w), None)));
    assert!(!supersedes((w, None), (Some("other"), None)));
    assert!(!supersedes((w, None), (None, None)));
    // Many: by the pod.
    assert!(supersedes((w, Some("a")), (Some(w), Some("a"))));
    assert!(!supersedes((w, Some("a")), (Some(w), Some("b"))));
    assert!(may_resume_as((Some(w), Some("a")), (Some(w), Some("a"))));
    assert!(!may_resume_as((Some(w), Some("a")), (Some(w), Some("b"))));
    assert!(!may_resume_as(
        (Some(w), Some("a")),
        (Some("other"), Some("a"))
    ));
}

/// A registration, as the session manager sees it: an identity (one of three, or none) and a pod (one of three, or
/// none).
fn registrations() -> impl Strategy<Value = Vec<(Option<u8>, Option<u8>)>> {
    prop::collection::vec(
        (proptest::option::of(0u8..3), proptest::option::of(0u8..3)),
        0..40,
    )
}

proptest! {
    /// For any sequence of registrations, each superseding the live sessions [`supersedes`] says it does: two live
    /// sessions of one identity are always of two named, different instances; a single-instance registration leaves
    /// only itself live of its identity; and a registration never supersedes a session of another pod or another
    /// identity.
    #[test]
    fn after_any_registrations_live_sessions_of_one_identity_are_distinct_instances(
        registrations in registrations(),
    ) {
        let name = |n: u8| format!("w{n}");
        let pod = |n: u8| format!("p{n}");
        // (identity, instance, live)
        let mut sessions: Vec<(Option<String>, Option<String>, bool)> = Vec::new();
        for (identity, instance) in registrations {
            let identity = identity.map(name);
            let instance = instance.map(pod);
            if let Some(newer) = &identity {
                for older in sessions.iter_mut().filter(|s| s.2) {
                    let superseded = supersedes(
                        (newer.as_str(), instance.as_deref()),
                        (older.0.as_deref(), older.1.as_deref()),
                    );
                    if superseded {
                        prop_assert_eq!(older.0.as_deref(), Some(newer.as_str()), "only its own identity's");
                        if let (Some(a), Some(b)) = (&instance, &older.1) {
                            prop_assert_eq!(a, b, "only its own pod's");
                        }
                        older.2 = false;
                    }
                }
            }
            sessions.push((identity, instance, true));
            let live: Vec<_> = sessions.iter().filter(|s| s.2 && s.0.is_some()).collect();
            for (i, a) in live.iter().enumerate() {
                for b in &live[i + 1..] {
                    if a.0 == b.0 {
                        prop_assert!(
                            a.1.is_some() && b.1.is_some() && a.1 != b.1,
                            "two live sessions of {:?}: {:?} and {:?}", a.0, a.1, b.1
                        );
                    }
                }
            }
        }
    }
}
