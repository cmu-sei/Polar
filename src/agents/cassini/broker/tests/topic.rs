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
