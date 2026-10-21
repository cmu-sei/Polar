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

//! A session belongs to the identity that made it: resuming it with its registration id is allowed only for a
//! connection presenting the same identity, so a registration id is not a bearer token
//! (docs/architecture/policy-enforcement.md, stage 0).
mod support;
use cassini_broker::session::may_resume;
use proptest::prelude::*;
use support::{TestClient, pause, with_broker};

#[test]
fn only_the_same_identity_may_resume() {
    assert!(may_resume(Some("agent.test"), Some("agent.test")));
    assert!(!may_resume(Some("agent.test"), Some("intruder.test")));
    assert!(!may_resume(Some("agent.test"), None));
    assert!(!may_resume(None, Some("agent.test")));
    assert!(
        !may_resume(None, None),
        "no identity on either side is nothing to match"
    );
}

proptest! {
    /// For any two identities, present or not: resuming is allowed exactly when both are present and equal.
    #[test]
    fn resuming_is_allowed_exactly_for_a_present_equal_identity(
        session in proptest::option::of("[a-z][a-z0-9.-]{0,30}"),
        connection in proptest::option::of("[a-z][a-z0-9.-]{0,30}"),
    ) {
        let expected = session.is_some() && session == connection;
        prop_assert_eq!(may_resume(session.as_deref(), connection.as_deref()), expected);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn another_identity_with_a_sessions_id_gets_a_session_of_its_own_and_nothing_held_for_the_owner()
 {
    with_broker(|broker| async move {
        let owner = TestClient::connect(&broker, "owner.test").await;
        let session = owner.registration_id().unwrap();
        owner.subscribe("private");
        pause().await;
        owner.vanish().await;
        pause().await;

        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("private", b"for the owner only");
        pause().await;

        let intruder =
            TestClient::connect_resuming(&broker, "intruder.test", Some(session.clone())).await;
        assert_ne!(
            intruder.registration_id().unwrap(),
            session,
            "a session of its own, not the owner's"
        );
        pause().await;
        assert!(
            intruder.received("private").is_empty(),
            "what was held for the owner is not delivered to another identity"
        );

        let back = TestClient::connect_resuming(&broker, "owner.test", Some(session.clone())).await;
        assert_eq!(
            back.registration_id().unwrap(),
            session,
            "the owner still resumes it"
        );
        assert_eq!(
            back.wait_for_messages("private", 1).await,
            [b"for the owner only".to_vec()]
        );
    })
    .await;
}
