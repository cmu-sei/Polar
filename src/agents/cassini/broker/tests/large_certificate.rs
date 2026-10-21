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

//! A certificate that carries a workload's floor (docs/architecture/policy-enforcement.md, stage 3 and open point C):
//! the broker reads its identity, and the client registers, subscribes and publishes, whatever the size of the topic
//! and group names it carries. Run with a few hundred names to confirm rustls' handshake limits rather than trust
//! them; the largest real set is about 34 names (the credentials agent's).

mod support;
use support::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_client_whose_certificate_carries_hundreds_of_topics_is_served_as_any_other() {
    with_broker(|broker| async move {
        let identity = "big.polar.serviceaccount.cluster.local";
        let uris: Vec<String> = (0..300)
            .map(|i| format!("p:pub:polar.example.a-rather-long-topic-name-{i:04}.events"))
            .chain(["g:cassini-client".to_string(), "g:self-test".to_string()])
            .collect();
        let size = broker.pki.issue_client_carrying(identity, &uris);
        assert!(
            size > 15_000,
            "the certificate is as large as meant: {size} bytes"
        );

        let big = TestClient::connect(&broker, identity).await;
        big.subscribe("news");
        pause().await;
        let other = TestClient::connect(&broker, "other.polar.serviceaccount.cluster.local").await;
        other.publish("news", b"to the big one");
        assert_eq!(
            big.wait_for_messages("news", 1).await,
            [b"to the big one".to_vec()]
        );
        other.subscribe("replies");
        pause().await;
        big.publish("replies", b"from the big one");
        assert_eq!(
            other.wait_for_messages("replies", 1).await,
            [b"from the big one".to_vec()]
        );
    })
    .await;
}
