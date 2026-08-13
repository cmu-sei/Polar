use cassini_types::{BrokerMessage, ShutdownPhase};
use ractor::{Actor, ActorProcessingErr};
use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;

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
        ).await?;

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
        ).await?;

        // Send a publish request (this should create the topic if it doesn't exist)
        let msg = BrokerMessage::PublishRequest {
            registration_id: "test-client".to_string(),
            topic: "test-topic".to_string(),
            payload: Arc::new(vec![1u8, 2, 3, 4, 5]),
            reply_to: None,
            trace_ctx: None,
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
        ).await?;

        // Send initiate shutdown phase message
        let msg = BrokerMessage::InitiateShutdownPhase {
            phase: ShutdownPhase::FlushTopicQueues,
        };

        // This should not panic or error
        actor_ref.send_message(msg).unwrap();

        Ok(())
    }
}