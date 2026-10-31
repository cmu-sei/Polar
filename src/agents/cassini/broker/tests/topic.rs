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

use cassini_types::{BrokerMessage, ShutdownPhase};
use ractor::{Actor, ActorProcessingErr};
use std::sync::Arc;

/// Keeps the payload of every message delivered to it.
struct Collector;

#[ractor::async_trait]
impl Actor for Collector {
    type Msg = BrokerMessage;
    type State = Arc<std::sync::Mutex<Vec<Vec<u8>>>>;
    type Arguments = Arc<std::sync::Mutex<Vec<Vec<u8>>>>;

    async fn pre_start(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        seen: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(seen)
    }

    async fn handle(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        message: Self::Msg,
        seen: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if let BrokerMessage::PublishResponse { payload, .. } = message {
            seen.lock().unwrap().push(payload.to_vec());
        }
        Ok(())
    }
}

/// What a session is told about its subscriptions: (topic, result), in order.
type Acks = Arc<std::sync::Mutex<Vec<(String, Result<(), String>)>>>;

/// Stands in for a session: it is registered under its registration id, as a session is, and keeps every
/// SubscribeAcknowledgment it is sent.
struct AckRecorder;

#[ractor::async_trait]
impl Actor for AckRecorder {
    type Msg = BrokerMessage;
    type State = Acks;
    type Arguments = Acks;

    async fn pre_start(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        acks: Acks,
    ) -> Result<Acks, ActorProcessingErr> {
        Ok(acks)
    }

    async fn handle(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        message: Self::Msg,
        acks: &mut Acks,
    ) -> Result<(), ActorProcessingErr> {
        if let BrokerMessage::SubscribeAcknowledgment { topic, result, .. } = message {
            acks.lock().unwrap().push((topic, result));
        }
        Ok(())
    }
}

/// A subscriber manager that refuses every subscription (`refuse`), or never answers.
struct StubSubscriberManager;

#[ractor::async_trait]
impl Actor for StubSubscriberManager {
    type Msg = BrokerMessage;
    type State = bool;
    type Arguments = bool;

    async fn pre_start(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        refuse: bool,
    ) -> Result<bool, ActorProcessingErr> {
        Ok(refuse)
    }

    async fn handle(
        &self,
        _myself: ractor::ActorRef<Self::Msg>,
        message: Self::Msg,
        refuse: &mut bool,
    ) -> Result<(), ActorProcessingErr> {
        if let BrokerMessage::CreateSubscriber { reply, .. } = message {
            if *refuse {
                let _ = reply.send(Err(ActorProcessingErr::from("Session not found")));
            } else {
                // Never answers: the reply port is kept, so the caller waits, and is dropped only when this stops.
                std::mem::forget(reply);
            }
        }
        Ok(())
    }
}

/// Subscribes `registration_id` to `topic` through a topic manager whose subscriber manager refuses (or never answers),
/// and returns what the session was told, once it has been told something or `within` has passed.
async fn subscribe_through(
    refuse: bool,
    registration_id: &str,
    topic: &str,
    within: std::time::Duration,
) -> Vec<(String, Result<(), String>)> {
    let acks: Acks = Default::default();
    let (_session, _) = Actor::spawn(Some(registration_id.to_string()), AckRecorder, acks.clone())
        .await
        .unwrap();
    let (subscriber_mgr, _) = Actor::spawn(None, StubSubscriberManager, refuse)
        .await
        .unwrap();
    let (manager, _) = Actor::spawn(
        None,
        cassini_broker::topic::TopicManager,
        cassini_broker::topic::TopicManagerArgs {
            topics: None,
            subscriber_mgr: Some(subscriber_mgr),
            bounds: cassini_broker::budget::Bounds::fallback(),
        },
    )
    .await
    .unwrap();
    manager
        .send_message(BrokerMessage::SubscribeRequest {
            registration_id: registration_id.to_string(),
            topic: topic.to_string(),
            trace_ctx: None,
        })
        .unwrap();
    let deadline = tokio::time::Instant::now() + within;
    while acks.lock().unwrap().is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    acks.lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A subscription the subscriber manager refuses is refused to the client, with the reason: a client never told is
    /// left waiting for an acknowledgement (docs/architecture/policy-enforcement.md, stage 0).
    #[tokio::test]
    async fn a_refused_subscription_is_refused_to_the_client() {
        let told = subscribe_through(
            true,
            "reg-refused",
            "t-refused",
            std::time::Duration::from_secs(5),
        )
        .await;
        assert_eq!(told.len(), 1, "{told:?}");
        assert_eq!(told[0].0, "t-refused");
        let said = told[0].1.clone().unwrap_err();
        assert!(said.contains("Session not found"), "{said}");
    }

    /// A subscriber manager that does not answer is waited for, and then the client is told: it used to be given up on
    /// after 200 ms, inside the topic manager (holding up every publish meanwhile), and the client never heard
    /// (stage 5a: a sink that started with three others had four of its six subscriptions go unanswered).
    #[tokio::test(start_paused = true)]
    async fn a_subscriber_manager_that_does_not_answer_is_given_up_on_and_the_client_is_told() {
        let told = subscribe_through(
            false,
            "reg-silent",
            "t-silent",
            std::time::Duration::from_secs(120),
        )
        .await;
        assert_eq!(told.len(), 1, "{told:?}");
        let said = told[0].1.clone().unwrap_err();
        assert!(said.contains("did not answer"), "{said}");
    }

    /// While a subscription waits on the subscriber manager, the topic manager goes on routing publishes.
    #[tokio::test]
    async fn a_waiting_subscription_does_not_hold_up_a_publish() {
        let acks: Acks = Default::default();
        let (_session, _) = Actor::spawn(Some("reg-wait".to_string()), AckRecorder, acks)
            .await
            .unwrap();
        let (subscriber_mgr, _) = Actor::spawn(None, StubSubscriberManager, false)
            .await
            .unwrap();
        let (manager, _) = Actor::spawn(
            None,
            cassini_broker::topic::TopicManager,
            cassini_broker::topic::TopicManagerArgs {
                topics: Some(vec!["t-busy".to_string()]),
                subscriber_mgr: Some(subscriber_mgr),
                bounds: cassini_broker::budget::Bounds::fallback(),
            },
        )
        .await
        .unwrap();
        let topic_actor: ractor::ActorRef<BrokerMessage> =
            ractor::registry::where_is("t-busy".to_string())
                .unwrap()
                .into();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (live, _) = Actor::spawn(None, Collector, seen.clone()).await.unwrap();
        topic_actor
            .send_message(BrokerMessage::AddSubscriber {
                registration_id: "live".to_string(),
                subscriber_ref: live,
                trace_ctx: None,
            })
            .unwrap();
        // Several subscriptions the subscriber manager will never answer, then a publish.
        for i in 0..5 {
            manager
                .send_message(BrokerMessage::SubscribeRequest {
                    registration_id: "reg-wait".to_string(),
                    topic: format!("t-other-{i}"),
                    trace_ctx: None,
                })
                .unwrap();
        }
        manager
            .send_message(BrokerMessage::PublishRequest {
                registration_id: "producer".to_string(),
                topic: "t-busy".to_string(),
                payload: Arc::new(b"now".to_vec()).into(),
                reply_to: None,
                trace_ctx: None,
                published_by: None,
                id: cassini_types::MessageId { epoch: 1, seq: 1 },
                window: cassini_types::Hold::none(),
            })
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(
            *seen.lock().unwrap(),
            vec![b"now".to_vec()],
            "the publish waited on the subscriptions"
        );
    }

    /// A topic whose subscriber has ended drops it at the next message, and keeps what it could not deliver for the
    /// next subscriber, as it does with no subscriber at all. It used to keep the ended subscriber for good and send it
    /// every message: each was lost and logged ("the actor is likely terminated"), once per ended subscriber, so every
    /// session that ended made each later message cost more (docs/architecture/policy-enforcement.md, stage 5a).
    #[tokio::test]
    async fn a_topic_drops_a_subscriber_that_has_ended_and_keeps_its_messages_for_the_next()
    -> Result<(), ActorProcessingErr> {
        let topic = "test-topic-ended-subscriber";
        let (_manager, _handle) = Actor::spawn(
            None,
            cassini_broker::topic::TopicManager,
            cassini_broker::topic::TopicManagerArgs {
                topics: Some(vec![topic.to_string()]),
                subscriber_mgr: None,
                bounds: cassini_broker::budget::Bounds::fallback(),
            },
        )
        .await?;
        let topic_actor: ractor::ActorRef<BrokerMessage> =
            ractor::registry::where_is(topic.to_string())
                .expect("the topic actor is named for its topic")
                .into();

        let (ended, ended_handle) = Actor::spawn(None, Collector, Default::default()).await?;
        topic_actor.send_message(BrokerMessage::AddSubscriber {
            registration_id: "ended".to_string(),
            subscriber_ref: ended.clone(),
            trace_ctx: None,
        })?;
        ended.stop(None);
        ended_handle.await.unwrap();

        for payload in [b"a".to_vec(), b"b".to_vec()] {
            topic_actor.send_message(BrokerMessage::PublishRequest {
                registration_id: "producer".to_string(),
                topic: topic.to_string(),
                payload: Arc::new(payload).into(),
                reply_to: None,
                trace_ctx: None,
                published_by: None,
                id: cassini_types::MessageId { epoch: 1, seq: 1 },
                window: cassini_types::Hold::none(),
            })?;
        }
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (live, _) = Actor::spawn(None, Collector, seen.clone()).await?;
        topic_actor.send_message(BrokerMessage::AddSubscriber {
            registration_id: "live".to_string(),
            subscriber_ref: live,
            trace_ctx: None,
        })?;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(*seen.lock().unwrap(), vec![b"a".to_vec(), b"b".to_vec()]);
        Ok(())
    }

    #[tokio::test]
    async fn test_topic_manager_spawn() -> Result<(), ActorProcessingErr> {
        let args = cassini_broker::topic::TopicManagerArgs {
            topics: None,
            subscriber_mgr: None,
            bounds: cassini_broker::budget::Bounds::fallback(),
        };

        // Just make sure we can spawn the TopicManager
        let _actor_ref = Actor::spawn(
            Some("test-topic-manager".to_string()),
            cassini_broker::topic::TopicManager,
            args,
        )
        .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_topic_manager_publish_request() -> Result<(), ActorProcessingErr> {
        let args = cassini_broker::topic::TopicManagerArgs {
            topics: None,
            subscriber_mgr: None,
            bounds: cassini_broker::budget::Bounds::fallback(),
        };

        // Spawn TopicManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-topic-manager-2".to_string()),
            cassini_broker::topic::TopicManager,
            args,
        )
        .await?;

        // Send a publish request (this should create the topic if it doesn't exist)
        let msg = BrokerMessage::PublishRequest {
            registration_id: "test-client".to_string(),
            topic: "test-topic".to_string(),
            payload: Arc::new(vec![1u8, 2, 3, 4, 5]).into(),
            reply_to: None,
            trace_ctx: None,
            published_by: None,
            id: cassini_types::MessageId { epoch: 1, seq: 1 },
            window: cassini_types::Hold::none(),
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    #[tokio::test]
    async fn test_topic_manager_shutdown_phase() -> Result<(), ActorProcessingErr> {
        let args = cassini_broker::topic::TopicManagerArgs {
            topics: None,
            subscriber_mgr: None,
            bounds: cassini_broker::budget::Bounds::fallback(),
        };

        // Spawn TopicManager
        let (actor_ref, _handle) = Actor::spawn(
            Some("test-topic-manager-3".to_string()),
            cassini_broker::topic::TopicManager,
            args,
        )
        .await?;

        // Send initiate shutdown phase message
        let msg = BrokerMessage::InitiateShutdownPhase {
            phase: ShutdownPhase::FlushTopicQueues,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }

    /// What a session is told about its publishes: an acknowledgement or a refusal, each with its number.
    type Answers = Arc<std::sync::Mutex<Vec<String>>>;

    struct AnswerRecorder;

    #[ractor::async_trait]
    impl Actor for AnswerRecorder {
        type Msg = BrokerMessage;
        type State = Answers;
        type Arguments = Answers;

        async fn pre_start(
            &self,
            _myself: ractor::ActorRef<Self::Msg>,
            answers: Answers,
        ) -> Result<Answers, ActorProcessingErr> {
            Ok(answers)
        }

        async fn handle(
            &self,
            _myself: ractor::ActorRef<Self::Msg>,
            message: Self::Msg,
            answers: &mut Answers,
        ) -> Result<(), ActorProcessingErr> {
            match message {
                BrokerMessage::PublishRequestAck { id, .. } => {
                    answers.lock().unwrap().push(format!("ack {}", id.seq))
                }
                BrokerMessage::PublishResponse {
                    result: Err(_),
                    origin: Some(id),
                    refusal,
                    ..
                } => answers
                    .lock()
                    .unwrap()
                    .push(format!("refused {} {refusal:?}", id.seq)),
                _ => {}
            }
            Ok(())
        }
    }

    /// A message for a topic with no subscriber, while the broker's budget is exhausted, is refused for now, not
    /// acknowledged and dropped: the client keeps it and sends it again (docs/architecture/cassini-backpressure.md,
    /// review finding 4).
    #[tokio::test]
    async fn a_message_no_one_can_hold_while_the_budget_is_exhausted_is_refused_for_now() {
        let bounds = cassini_broker::budget::Bounds::fallback();
        let (manager, _) = Actor::spawn(
            None,
            cassini_broker::topic::TopicManager,
            cassini_broker::topic::TopicManagerArgs {
                topics: None,
                subscriber_mgr: None,
                bounds: bounds.clone(),
            },
        )
        .await
        .unwrap();
        let answers: Answers = Default::default();
        let (session, _) = Actor::spawn(None, AnswerRecorder, answers.clone())
            .await
            .unwrap();
        let publish = |seq| BrokerMessage::PublishRequest {
            registration_id: "producer".to_string(),
            topic: "t-nobody".to_string(),
            payload: Arc::new(b"x".to_vec()).into(),
            reply_to: Some(session.clone()),
            trace_ctx: None,
            published_by: None,
            id: cassini_types::MessageId { epoch: 1, seq },
            window: cassini_types::Hold::none(),
        };
        manager.send_message(publish(1)).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // Exhaust the budget: everything taken, and one more asked for.
        let _held = bounds
            .budget
            .try_charge(bounds.budget.limit() - bounds.budget.used());
        assert!(bounds.budget.try_charge(1).is_none());
        manager.send_message(publish(2)).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            *answers.lock().unwrap(),
            ["ack 1", "refused 2 Some(Unavailable)"]
        );
        assert_eq!(bounds.budget.stats().refused, 1);
    }
}
