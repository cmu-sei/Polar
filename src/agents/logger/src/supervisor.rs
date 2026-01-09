//! logging/supervisor.rs
//! Minimal Ractor supervisor for the LoggingConsumer actor.

use crate::consume::logs::{Durability, LoggingConsumer, LoggingConsumerArgs, LOG_CONSUMER_TOPIC};
use ractor::{Actor, ActorRef};
use std::path::PathBuf;

pub struct Supervisor;

impl Supervisor {
    /// Spawn the logging consumer actor under the supervisor.
    pub async fn spawn_logging_consumer(
        registration_id: String,
        db_path: PathBuf,
        durability: Durability,
    ) -> Result<ActorRef<crate::consume::logs::DispatcherMessage>, Box<dyn std::error::Error>> {
        let args = LoggingConsumerArgs {
            registration_id,
            db_path,
            durability,
        };

        // Spawn with registry name = topic string for dispatcher routing.
        let (actor, _handle) =
            Actor::spawn(Some(LOG_CONSUMER_TOPIC.to_string()), LoggingConsumer, args).await?;

        Ok(actor)
    }
}
