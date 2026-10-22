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
    snapshot_of_policy(
        signer,
        &format!(
            r#"forbid (principal == Identity::"{PRODUCER}", action == Action::"publish", resource == Topic::"news");"#
        ),
    )
}

/// The credentials agent's snapshot carrying `source` as the policy, signed by `signer`.
fn snapshot_of_policy(signer: &RsaPrivateKey, source: &str) -> Vec<u8> {
    let policy = AuthorityPayload {
        sequence: 1,
        version: 1,
        body: AuthorityBody::Policy {
            source: source.to_string(),
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

/// The broker with the credentials agent able to publish its snapshots, and the sink subscribed to `news`.
async fn sink_subscribed_to_news(broker: &TestBroker) -> (TestClient, TestClient, TestClient) {
    broker.pki.issue_client_carrying(
        CREDENTIALS,
        &[
            format!("p:pub:{AUTHORITY_SNAPSHOT_REPLIES_TOPIC}"),
            "g:cassini-client".to_string(),
        ],
    );
    let producer = TestClient::connect(broker, PRODUCER).await;
    let sink = TestClient::connect(broker, SINK).await;
    sink.subscribe("news");
    sink.wait_for("the subscription", Duration::from_secs(10), |c| {
        c.subscribed("news")
    })
    .await;
    let credentials = TestClient::connect(broker, CREDENTIALS).await;
    (producer, sink, credentials)
}

#[tokio::test]
async fn a_held_subscription_a_new_forbid_refuses_is_dropped_and_the_client_told() {
    let _serial = serial().await;
    let broker = broker_with_two_workloads().await;
    let (producer, sink, credentials) = sink_subscribed_to_news(&broker).await;
    let signer = RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap();
    credentials.publish(
        AUTHORITY_SNAPSHOT_REPLIES_TOPIC,
        &snapshot_of_policy(
            &signer,
            &format!(
                r#"forbid (principal == Identity::"{SINK}", action == Action::"subscribe", resource == Topic::"news");"#
            ),
        ),
    );
    // Told without asking: the subscription it held is gone.
    let why = refused(&sink, "news").await;
    assert!(why.contains("policy forbids"), "{why}");
    producer.publish("news", b"after the forbid");
    producer
        .wait_for("the publish to be taken", Duration::from_secs(10), |c| {
            c.acknowledged("news") >= 1
        })
        .await;
    support::pause().await;
    assert_eq!(
        sink.received_count("news"),
        0,
        "nothing reaches a dropped subscription"
    );
    broker.stop().await;
}

#[tokio::test]
async fn a_held_subscription_the_new_authority_still_admits_is_kept() {
    let _serial = serial().await;
    let broker = broker_with_two_workloads().await;
    let (producer, sink, credentials) = sink_subscribed_to_news(&broker).await;
    let signer = RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap();
    credentials.publish(
        AUTHORITY_SNAPSHOT_REPLIES_TOPIC,
        &snapshot_of_policy(
            &signer,
            &format!(
                r#"forbid (principal == Identity::"{SINK}", action == Action::"subscribe", resource == Topic::"weather");"#
            ),
        ),
    );
    // Once the change has applied (the producer's decisions are made again under it), news still flows.
    support::pause().await;
    producer.publish("news", b"still flowing");
    assert_eq!(sink.wait_for_messages("news", 1).await.len(), 1);
    assert!(sink.refusals("news").is_empty());
    broker.stop().await;
}

#[tokio::test]
async fn a_resumed_session_whose_renewed_certificate_carries_fewer_topics_loses_the_rest() {
    let _serial = serial().await;
    let broker = TestBroker::start_enforcing().await;
    broker.pki.issue_client_carrying(
        PRODUCER,
        &["p:pub:news".to_string(), "g:cassini-client".to_string()],
    );
    broker.pki.issue_client_carrying(
        SINK,
        &["p:sub:news".to_string(), "g:cassini-client".to_string()],
    );
    let producer = TestClient::connect(&broker, PRODUCER).await;
    let sink = TestClient::connect(&broker, SINK).await;
    let session = sink.registration_id().unwrap();
    sink.subscribe("news");
    sink.wait_for("the subscription", Duration::from_secs(10), |c| {
        c.subscribed("news")
    })
    .await;
    sink.vanish().await;
    support::pause().await;
    producer.publish("news", b"held while it was away");
    support::pause().await;

    // Its renewed certificate no longer carries the topic.
    broker
        .pki
        .issue_client_carrying(SINK, &["g:cassini-client".to_string()]);
    let back = TestClient::connect_resuming(&broker, SINK, Some(session.clone())).await;
    assert_eq!(back.registration_id().unwrap(), session, "the same session");
    let why = refused(&back, "news").await;
    assert!(
        why.contains("not among its declared or implied topics"),
        "{why}"
    );
    support::pause().await;
    assert_eq!(
        back.received_count("news"),
        0,
        "what was held is not delivered either"
    );
    broker.stop().await;
}

#[tokio::test]
async fn a_resumed_session_is_told_what_it_lost_even_with_nothing_held_for_it() {
    let _serial = serial().await;
    let broker = TestBroker::start_enforcing().await;
    broker.pki.issue_client_carrying(
        SINK,
        &["p:sub:news".to_string(), "g:cassini-client".to_string()],
    );
    let sink = TestClient::connect(&broker, SINK).await;
    let session = sink.registration_id().unwrap();
    sink.subscribe("news");
    sink.wait_for("the subscription", Duration::from_secs(10), |c| {
        c.subscribed("news")
    })
    .await;
    sink.vanish().await;
    support::pause().await;
    broker
        .pki
        .issue_client_carrying(SINK, &["g:cassini-client".to_string()]);
    let back = TestClient::connect_resuming(&broker, SINK, Some(session)).await;
    refused(&back, "news").await;
    broker.stop().await;
}

// -- the agent's side (stage 5b) --------------------------------------------------------------------------------------

use cassini_client::TcpClientMessage;
use cassini_client::facade::{RefusalNews, TopicUse};
use cassini_client::refused::StandingRefusal;
use credentials_agent_common::authority_update::AUTHORITY_UPDATES_TOPIC;
use polar_agent_builder::authority::AuthorityHandle;
use polar_agent_builder::topics::Facts;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// An agent's Cassini client, made as the agent builder makes one (polar-agent-builder's `infrastructure`): its gate
/// (its policy, then its topic set), its authority actor hearing the credentials agent, what it was refused judged
/// again on each change of its authority, and a refusal its gate did not predict asking for the authority.
struct Agent {
    /// The authority's actors and the client hang off these; stopping them ends the agent.
    parent: ractor::ActorRef<()>,
    registrar: ractor::ActorRef<cassini_types::ClientEvent>,
    router: ractor::ActorRef<polar_router::RouterMessage>,
    authority: AuthorityHandle,
    delivered: Arc<Mutex<Vec<(String, Vec<u8>)>>>,
    standing: Arc<Mutex<Vec<StandingRefusal>>>,
    unpredicted: Arc<AtomicUsize>,
}

/// Subscribes, each time the client registers, to the authority and to what the agent wants, as the builder's
/// lifecycle handler does.
struct Registrar {
    client: Arc<OnceLock<ractor::ActorRef<TcpClientMessage>>>,
    wants: Vec<String>,
    authority: AuthorityHandle,
}

#[ractor::async_trait]
impl ractor::Actor for Registrar {
    type Msg = cassini_types::ClientEvent;
    type State = ();
    type Arguments = ();
    async fn pre_start(
        &self,
        _: ractor::ActorRef<Self::Msg>,
        _: (),
    ) -> Result<(), ractor::ActorProcessingErr> {
        Ok(())
    }
    async fn handle(
        &self,
        _: ractor::ActorRef<Self::Msg>,
        event: Self::Msg,
        _: &mut (),
    ) -> Result<(), ractor::ActorProcessingErr> {
        if let cassini_types::ClientEvent::Registered { .. } = event
            && let Some(client) = self.client.get()
        {
            for topic in [AUTHORITY_UPDATES_TOPIC, AUTHORITY_SNAPSHOT_REPLIES_TOPIC]
                .into_iter()
                .map(str::to_string)
                .chain(self.wants.iter().cloned())
            {
                client.send_message(TcpClientMessage::Subscribe {
                    topic,
                    trace_ctx: None,
                })?;
            }
            self.authority.registered();
        }
        Ok(())
    }
}

struct Parent;

#[ractor::async_trait]
impl ractor::Actor for Parent {
    type Msg = ();
    type State = ();
    type Arguments = ();
    async fn pre_start(
        &self,
        _: ractor::ActorRef<()>,
        _: (),
    ) -> Result<(), ractor::ActorProcessingErr> {
        Ok(())
    }
}

/// The agent of the workload `name` with the certificate of `identity`, declaring `declared` and subscribing to
/// `wants`. Its actors are named for a service of its own, as tests run one after another in one process.
async fn agent(
    broker: &TestBroker,
    identity: &str,
    name: &str,
    declared: &[polar_agent_builder::TopicDecl],
    wants: &[&str],
) -> Agent {
    use ractor::Actor;
    let service = format!("{name}-{}", uuid::Uuid::new_v4());
    let (parent, _) = Actor::spawn(None, Parent, ()).await.unwrap();
    let authority = AuthorityHandle::spawn(&service, CREDENTIALS, parent.get_cell())
        .await
        .unwrap();
    let (router, _) = Actor::spawn(None, polar_router::RouterActor, ())
        .await
        .unwrap();
    let delivered: Arc<Mutex<Vec<(String, Vec<u8>)>>> = Arc::default();
    let mut subscriptions = polar_agent_builder::authority::subscriptions(&authority);
    for topic in wants {
        let delivered = delivered.clone();
        subscriptions.push(polar_agent_builder::Subscription::new(
            *topic,
            move |incoming| {
                delivered
                    .lock()
                    .unwrap()
                    .push((incoming.topic, incoming.payload))
            },
        ));
    }
    for subscription in subscriptions {
        router
            .clone()
            .send_message(polar_router::RouterMessage::Register {
                topic: subscription.topic,
                deliver: subscription.deliver,
            })
            .unwrap();
    }

    let (cert, key) = broker.pki.client(identity);
    let set = polar_agent_builder::TopicSet::compute(
        &Facts {
            name,
            cassini_client: true,
            self_test: false,
            encryption: false,
        },
        declared,
    );
    let gate = polar_agent_builder::topic_gate::gate(
        &service,
        identity.to_string(),
        Some(std::fs::read_to_string(&cert).unwrap()),
        Arc::new(set),
    );
    let standing: Arc<Mutex<Vec<StandingRefusal>>> = Arc::default();
    let unpredicted = Arc::new(AtomicUsize::new(0));
    let watch: cassini_client::facade::RefusalWatch = {
        let authority = authority.clone();
        let standing = standing.clone();
        let unpredicted = unpredicted.clone();
        Arc::new(move |news| match news {
            RefusalNews::Unpredicted { .. } => {
                unpredicted.fetch_add(1, Ordering::SeqCst);
                authority.behind();
            }
            RefusalNews::Standing(now) => *standing.lock().unwrap() = now,
        })
    };

    let client = Arc::new(OnceLock::new());
    let (registrar, _) = Actor::spawn(
        None,
        Registrar {
            client: client.clone(),
            wants: wants.iter().map(|t| t.to_string()).collect(),
            authority: authority.clone(),
        },
        (),
    )
    .await
    .unwrap();
    let tcp = polar::cassini::TcpClient::spawn_with_router_config(
        &service,
        registrar.clone(),
        Some,
        router.clone(),
        cassini_client::TCPClientConfig::from_values(
            broker.addr.clone(),
            "localhost".to_string(),
            broker.pki.ca().to_string_lossy().into_owned(),
            cert.to_string_lossy().into_owned(),
            key.to_string_lossy().into_owned(),
        ),
        Some(gate),
        Some(watch),
    )
    .await
    .unwrap();
    authority
        .changes()
        .subscribe(tcp.as_actor_ref(), |_| Some(TcpClientMessage::JudgeAgain));
    client.set(tcp.as_actor_ref()).unwrap();
    Agent {
        parent,
        registrar,
        router,
        authority,
        delivered,
        standing,
        unpredicted,
    }
}

impl Agent {
    async fn stop(self) {
        for cell in [
            self.registrar.get_cell(),
            self.router.get_cell(),
            self.parent.get_cell(),
        ] {
            cell.stop_and_wait(None, None).await.ok();
        }
    }

    fn standing(&self) -> Vec<(String, TopicUse, bool)> {
        self.standing
            .lock()
            .unwrap()
            .iter()
            .map(|s| (s.topic.clone(), s.use_, s.by_broker))
            .collect()
    }

    fn delivered(&self, topic: &str) -> usize {
        self.delivered
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _)| t == topic)
            .count()
    }
}

/// Waits for `ready`, doing `step` between looks, for up to ten seconds.
async fn until(what: &str, mut step: impl FnMut(), ready: impl Fn() -> bool) {
    for _ in 0..50 {
        if ready() {
            return;
        }
        step();
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("never: {what}");
}

#[tokio::test]
async fn an_agent_sends_a_refused_subscription_again_once_a_permit_arrives() {
    let _serial = serial().await;
    let broker = TestBroker::start_enforcing().await;
    broker
        .pki
        .issue_client_carrying(SINK, &["g:cassini-client".to_string()]);
    broker.pki.issue_client_carrying(
        PRODUCER,
        &["p:pub:weather".to_string(), "g:cassini-client".to_string()],
    );
    broker.pki.issue_client_carrying(
        CREDENTIALS,
        &[
            format!("p:pub:{AUTHORITY_SNAPSHOT_REPLIES_TOPIC}"),
            "g:cassini-client".to_string(),
        ],
    );
    let sink = agent(&broker, SINK, "harness-sink", &[], &["weather"]).await;
    // Not its topic, and no policy says otherwise: its own gate refuses it, and it stands refused.
    until(
        "the sink is refused weather",
        || {},
        || sink.standing() == [("weather".to_string(), TopicUse::Subscribe, false)],
    )
    .await;

    let producer = TestClient::connect(&broker, PRODUCER).await;
    let credentials = TestClient::connect(&broker, CREDENTIALS).await;
    let signer = RsaPrivateKey::new(&mut rand::rng(), 2048).unwrap();
    let permit = snapshot_of_policy(
        &signer,
        &format!(
            r#"permit (principal == Identity::"{SINK}", action == Action::"subscribe", resource == Topic::"weather");"#
        ),
    );
    // The credentials agent publishes the permit; the sink and the broker both hear it.
    let versions = Arc::new(Mutex::new(0usize));
    {
        let held = versions.clone();
        let authority = sink.authority.clone();
        tokio::spawn(async move {
            for _ in 0..100 {
                *held.lock().unwrap() = authority.versions().await.map(|v| v.len()).unwrap_or(0);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
    }
    until(
        "the sink holds the permit",
        || credentials.publish(AUTHORITY_SNAPSHOT_REPLIES_TOPIC, &permit),
        || *versions.lock().unwrap() > 0,
    )
    .await;
    // Judged again, admitted, and sent: it stands refused no more.
    until(
        "the sink's refusal is lifted",
        || {},
        || sink.standing().is_empty(),
    )
    .await;
    until(
        "weather reaches the sink",
        || producer.publish("weather", b"sunny"),
        || sink.delivered("weather") > 0,
    )
    .await;
    assert_eq!(
        sink.unpredicted.load(Ordering::SeqCst),
        0,
        "the gate and the broker agreed throughout"
    );
    sink.stop().await;
    broker.stop().await;
}

#[tokio::test]
async fn a_refusal_the_agent_s_gate_did_not_predict_stands_refused_and_asks_for_the_authority() {
    let _serial = serial().await;
    let broker = TestBroker::start_enforcing().await;
    // The agent declares news, but its certificate does not carry it: the broker refuses what its gate admits.
    broker
        .pki
        .issue_client_carrying(SINK, &["g:cassini-client".to_string()]);
    let sink = agent(
        &broker,
        SINK,
        "harness-sink",
        &[polar_agent_builder::TopicDecl::subscribe(
            "news",
            "what the test publishes",
        )],
        &["news"],
    )
    .await;
    until(
        "the broker refuses the sink news",
        || {},
        || sink.standing() == [("news".to_string(), TopicUse::Subscribe, true)],
    )
    .await;
    assert!(sink.unpredicted.load(Ordering::SeqCst) >= 1);
    sink.stop().await;
    broker.stop().await;
}
