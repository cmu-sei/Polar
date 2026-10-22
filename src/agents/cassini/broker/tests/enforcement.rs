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

//! The broker decides who may publish and subscribe to what (docs/architecture/policy-enforcement.md, stage 5b):
//! Cedar first, then the floor the client's certificate gives (its `p:` names, and the topics its workload and `g:`
//! groups imply), refused by default. A refusal reaches the client as `PublishRefused` or `SubscribeRefused`.

mod support;

use credentials_agent_common::authority_update::{
    AUTHORITY_SNAPSHOT_REPLIES_TOPIC, AuthorityBody, AuthorityPayload, AuthoritySnapshot, sign,
};
use rsa::RsaPrivateKey;
use rsa::pkcs8::EncodePublicKey;
use std::time::Duration;
use support::{TestBroker, TestClient, serial};

const PRODUCER: &str = "harness-producer.polar.serviceaccount.cluster.local";
const SINK: &str = "harness-sink.polar.serviceaccount.cluster.local";
const CREDENTIALS: &str = "credentials-agent.polar.serviceaccount.cluster.local";

/// A broker with no policy yet, as on a fresh cluster: the floor decides. The producer may publish `news` and the
/// sink subscribe to it; both are Cassini clients.
async fn broker_with_two_workloads() -> TestBroker {
    let broker = TestBroker::start_enforcing().await;
    broker.pki.issue_client_carrying(
        PRODUCER,
        &["p:pub:news".to_string(), "g:cassini-client".to_string()],
    );
    broker.pki.issue_client_carrying(
        SINK,
        &["p:sub:news".to_string(), "g:cassini-client".to_string()],
    );
    broker
}

async fn refused(client: &TestClient, topic: &str) -> String {
    client
        .wait_for(
            &format!("a refusal on {topic}"),
            Duration::from_secs(10),
            |c| !c.refusals(topic).is_empty(),
        )
        .await;
    client.refusals(topic).remove(0)
}

#[tokio::test]
async fn with_no_policy_a_declared_topic_is_admitted_and_an_undeclared_one_refused() {
    let _serial = serial().await;
    let broker = broker_with_two_workloads().await;
    let producer = TestClient::connect(&broker, PRODUCER).await;
    let sink = TestClient::connect(&broker, SINK).await;

    sink.subscribe("news");
    sink.wait_for("the subscription", Duration::from_secs(10), |c| {
        c.subscribed("news")
    })
    .await;
    producer.publish("news", b"declared");
    assert_eq!(
        sink.wait_for_messages("news", 1).await,
        vec![b"declared".to_vec()]
    );

    producer.publish("weather", b"undeclared");
    let why = refused(&producer, "weather").await;
    assert!(why.contains("the broker refused"), "{why}");
    assert!(
        why.contains("not among its declared or implied topics"),
        "{why}"
    );
    // Declared to publish is not declared to subscribe.
    producer.subscribe("news");
    refused(&producer, "news").await;
    assert!(!producer.subscribed("news"));
    broker.stop().await;
}

#[tokio::test]
async fn a_workload_s_implied_topics_need_no_declaration_and_another_s_are_refused() {
    let _serial = serial().await;
    let broker = broker_with_two_workloads().await;
    let producer = TestClient::connect(&broker, PRODUCER).await;

    producer.subscribe("agent.harness-producer.configuration");
    producer
        .wait_for("its own configuration", Duration::from_secs(10), |c| {
            c.subscribed("agent.harness-producer.configuration")
        })
        .await;
    producer.subscribe("agent.harness-sink.configuration");
    refused(&producer, "agent.harness-sink.configuration").await;
    broker.stop().await;
}

#[tokio::test]
async fn a_certificate_that_names_no_topics_and_no_workload_is_refused_everything() {
    let _serial = serial().await;
    let broker = broker_with_two_workloads().await;
    let person = TestClient::connect(&broker, "alice").await;
    person.subscribe("news");
    let why = refused(&person, "news").await;
    // The client re-registers on this phrase, so a refusal never carries it.
    assert!(!why.contains("session mismatch"), "{why}");
    person.publish("news", b"x");
    assert_eq!(person.refusals("news").len() >= 1, true);
    broker.stop().await;
}

#[tokio::test]
async fn cedar_forbids_a_declared_topic_and_permits_an_undeclared_one() {
    let _serial = serial().await;
    let broker = broker_with_two_workloads().await;
    broker
        .set_policy(&format!(
            r#"forbid (principal == Identity::"{PRODUCER}", action == Action::"publish", resource == Topic::"news");
               permit (principal == Identity::"{PRODUCER}", action == Action::"publish", resource == Topic::"weather");
               permit (principal == Identity::"{SINK}", action == Action::"subscribe", resource == Topic::"weather");"#
        ))
        .await;
    let producer = TestClient::connect(&broker, PRODUCER).await;
    let sink = TestClient::connect(&broker, SINK).await;

    producer.publish("news", b"forbidden");
    let why = refused(&producer, "news").await;
    assert!(why.contains("policy forbids"), "{why}");

    sink.subscribe("weather");
    sink.wait_for("the permitted subscription", Duration::from_secs(10), |c| {
        c.subscribed("weather")
    })
    .await;
    producer.publish("weather", b"permitted");
    assert_eq!(
        sink.wait_for_messages("weather", 1).await,
        vec![b"permitted".to_vec()]
    );
    broker.stop().await;
}

fn snapshot_forbidding_the_producer_news(signer: &RsaPrivateKey) -> Vec<u8> {
    let policy = AuthorityPayload {
        sequence: 1,
        version: 1,
        body: AuthorityBody::Policy {
            source: format!(
                r#"forbid (principal == Identity::"{PRODUCER}", action == Action::"publish", resource == Topic::"news");"#
            ),
        },
        authored_by: None,
    };
    let snapshot = AuthoritySnapshot {
        public_key_pem: signer
            .to_public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap(),
        updates: vec![sign(&policy, signer).unwrap()],
    };
    rkyv::to_bytes::<rkyv::rancor::Error>(&snapshot)
        .unwrap()
        .to_vec()
}

#[tokio::test]
async fn the_broker_keeps_the_credentials_agent_s_authority_and_no_one_else_s() {
    let _serial = serial().await;
    let broker = broker_with_two_workloads().await;
    let replies = format!("p:pub:{AUTHORITY_SNAPSHOT_REPLIES_TOPIC}");
    broker.pki.issue_client_carrying(
        CREDENTIALS,
        &[replies.clone(), "g:cassini-client".to_string()],
    );
    // Allowed to publish on the topic, and still not the credentials agent.
    let impostor = "impostor.polar.serviceaccount.cluster.local";
    broker
        .pki
        .issue_client_carrying(impostor, &[replies, "g:cassini-client".to_string()]);
    let signer = RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap();
    let snapshot = snapshot_forbidding_the_producer_news(&signer);
    let producer = TestClient::connect(&broker, PRODUCER).await;
    let sink = TestClient::connect(&broker, SINK).await;
    sink.subscribe("news");
    sink.wait_for("the subscription", Duration::from_secs(10), |c| {
        c.subscribed("news")
    })
    .await;

    let impostor = TestClient::connect(&broker, impostor).await;
    impostor.publish(AUTHORITY_SNAPSHOT_REPLIES_TOPIC, &snapshot);
    tokio::time::sleep(Duration::from_millis(500)).await;
    producer.publish("news", b"still admitted");
    assert_eq!(sink.wait_for_messages("news", 1).await.len(), 1);
    assert!(producer.refusals("news").is_empty());

    let credentials = TestClient::connect(&broker, CREDENTIALS).await;
    credentials.publish(AUTHORITY_SNAPSHOT_REPLIES_TOPIC, &snapshot);
    // The policy applies once the broker has verified the snapshot.
    producer
        .wait_for("the forbid to apply", Duration::from_secs(10), |c| {
            if !c.refusals("news").is_empty() {
                return true;
            }
            c.publish("news", b"until forbidden");
            false
        })
        .await;
    assert!(producer.refusals("news")[0].contains("policy forbids"));
    broker.stop().await;
}

/// The producer has published `news` once and it was delivered, so the broker has decided it.
async fn decided_once(broker: &TestBroker) -> (TestClient, TestClient) {
    let producer = TestClient::connect(broker, PRODUCER).await;
    let sink = TestClient::connect(broker, SINK).await;
    sink.subscribe("news");
    sink.wait_for("the subscription", Duration::from_secs(10), |c| {
        c.subscribed("news")
    })
    .await;
    producer.publish("news", b"first");
    sink.wait_for_messages("news", 1).await;
    (producer, sink)
}

const FORBID_THE_PRODUCER_NEWS: &str = r#"forbid (principal == Identity::"harness-producer.polar.serviceaccount.cluster.local", action == Action::"publish", resource == Topic::"news");"#;

#[tokio::test]
async fn without_a_cache_a_new_policy_applies_to_the_very_next_publish() {
    let _serial = serial().await;
    let broker = broker_caching(Duration::ZERO).await;
    let (producer, _sink) = decided_once(&broker).await;
    broker.set_policy(FORBID_THE_PRODUCER_NEWS).await;
    producer.publish("news", b"second");
    assert!(refused(&producer, "news").await.contains("policy forbids"));
    broker.stop().await;
}

#[tokio::test]
async fn a_kept_decision_lasts_its_time_when_nothing_says_the_authority_changed() {
    let _serial = serial().await;
    let broker = broker_caching(Duration::from_secs(1)).await;
    let (producer, sink) = decided_once(&broker).await;
    // Given straight to the policy actor, as no verified update would be: nothing counts it as a change.
    broker.set_policy(FORBID_THE_PRODUCER_NEWS).await;
    producer.publish("news", b"kept");
    assert_eq!(sink.wait_for_messages("news", 2).await.len(), 2);
    assert!(producer.refusals("news").is_empty());
    tokio::time::sleep(Duration::from_millis(1200)).await;
    producer.publish("news", b"expired");
    assert!(refused(&producer, "news").await.contains("policy forbids"));
    broker.stop().await;
}

#[tokio::test]
async fn a_change_of_authority_ends_every_kept_decision_at_once() {
    let _serial = serial().await;
    // Kept far longer than the test runs, so only the change can end it.
    let broker = broker_caching(Duration::from_secs(600)).await;
    broker.pki.issue_client_carrying(
        CREDENTIALS,
        &[
            format!("p:pub:{AUTHORITY_SNAPSHOT_REPLIES_TOPIC}"),
            "g:cassini-client".to_string(),
        ],
    );
    let (producer, _sink) = decided_once(&broker).await;
    let signer = RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap();
    let credentials = TestClient::connect(&broker, CREDENTIALS).await;
    credentials.publish(
        AUTHORITY_SNAPSHOT_REPLIES_TOPIC,
        &snapshot_forbidding_the_producer_news(&signer),
    );
    producer
        .wait_for("the forbid to apply", Duration::from_secs(10), |c| {
            if !c.refusals("news").is_empty() {
                return true;
            }
            c.publish("news", b"until forbidden");
            false
        })
        .await;
    broker.stop().await;
}

async fn broker_caching(cache: Duration) -> TestBroker {
    let broker = TestBroker::start_enforcing_caching(cache).await;
    broker.pki.issue_client_carrying(
        PRODUCER,
        &["p:pub:news".to_string(), "g:cassini-client".to_string()],
    );
    broker.pki.issue_client_carrying(
        SINK,
        &["p:sub:news".to_string(), "g:cassini-client".to_string()],
    );
    broker
}
