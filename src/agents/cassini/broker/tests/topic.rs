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

#[cfg(test)]
mod tests {
    use super::*;

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
                payload: Arc::new(payload),
                reply_to: None,
                trace_ctx: None,
                published_by: None,
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
            payload: Arc::new(vec![1u8, 2, 3, 4, 5]),
            reply_to: None,
            trace_ctx: None,
            published_by: None,
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
}
