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

use crate::types::JiraData;
use polar::DispatcherMessage;
use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait, registry::where_is};
use rkyv::rancor::Error;
use tracing::{info, warn};

/// Dispatcher Definition
/// The Dispatcher is a core component of each agent. Its function is to deserialize data from it's binary
/// format into strong rust datatypes, and forward it to any interested actors.
pub struct MessageDispatcher;

impl MessageDispatcher {
    /// Helper function - does as the name says.
    /// It's pretty safe to assume whatever message comes in contains data for our Jira agent.
    /// We can jsut discard any message that don't conform to our expectations as bad data.
    /// The incoming topic string will inform us of which actor is responsible for handling the message.
    ///  Other agents may want to implement DLQ for when consumers aren't present and may return.
    /// For now, we don't need to bother since we're getting new data all the time from jira.
    ///         
    pub fn deserialize_and_dispatch(message: Vec<u8>, topic: String) {
        match rkyv::from_bytes::<JiraData, Error>(&message) {
            Ok(message) => {
                if let Some(consumer) = where_is(topic.clone())
                    && let Err(e) = consumer.send_message(message)
                {
                    tracing::warn!("Error forwarding message. {e}");
                }
            }
            Err(err) => warn!("Failed to deserialize message: {:?}", err),
        }
    }
}

pub struct DispatcherState;

#[async_trait]
impl Actor for MessageDispatcher {
    type Msg = DispatcherMessage;
    type State = DispatcherState;
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(DispatcherState)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("{myself:?} started");
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        _: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            DispatcherMessage::Dispatch { message, topic } => {
                MessageDispatcher::deserialize_and_dispatch(message, topic);
            }
        }
        Ok(())
    }
}
