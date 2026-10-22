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

//! Who may publish and subscribe to what (docs/architecture/policy-enforcement.md, stage 5b). The broker cannot be
//! a client of itself, so it keeps the credentials agent's authority the way every agent does, with an
//! `AuthorityHandle` of its own, and hears the authority topics as they pass through it, with the publisher's
//! identity from its own Listener. Each publish and subscribe is decided by the rule every agent's own gate applies
//! (`polar_agent_builder::topic_gate::rule`): Cedar first, then the floor the client's certificate gives, and
//! refused by default.

use cassini_types::BrokerMessage;
use credentials_agent_common::authority_update::{
    AUTHORITY_SNAPSHOT_REPLIES_TOPIC, AUTHORITY_SNAPSHOT_REQUESTS_TOPIC, AUTHORITY_UPDATES_TOPIC,
    AuthoritySnapshot, AuthoritySnapshotRequest, AuthorityUpdate,
};
use polar_agent_builder::authority::AuthorityHandle;
use polar_agent_builder::topic_gate::{ask, rule};
use polar_agent_builder::{Access, Floor, PolicyLookup};
use ractor::{ActorCell, ActorProcessingErr, ActorRef};
use std::sync::Arc;
use tracing::warn;

/// The name the broker's policy and permissions actors are registered under (`cassini.policy`).
pub const SERVICE: &str = "cassini";

/// The registration id the broker publishes its own requests under: no client has it.
const OWN_REGISTRATION: &str = "cassini";

/// The broker's authority, shared by every Listener.
#[derive(Clone)]
pub struct BrokerAuthority {
    handle: AuthorityHandle,
    lookup: PolicyLookup,
}

impl BrokerAuthority {
    /// Starts the broker's authority under `supervisor`, believing only `publisher` (the credentials agent's
    /// identity). What it wants to ask for is published on the requests topic through `topic_mgr`.
    pub async fn spawn(
        publisher: String,
        supervisor: ActorCell,
        topic_mgr: ActorRef<BrokerMessage>,
    ) -> Result<Self, ActorProcessingErr> {
        let handle = AuthorityHandle::spawn(SERVICE, publisher, supervisor).await?;
        let request: Arc<Vec<u8>> = Arc::new(
            rkyv::to_bytes::<rkyv::rancor::Error>(&AuthoritySnapshotRequest)
                .map_err(|e| ActorProcessingErr::from(e.to_string()))?
                .to_vec(),
        );
        handle.asks().subscribe(topic_mgr, move |_| {
            Some(BrokerMessage::PublishRequest {
                registration_id: OWN_REGISTRATION.to_string(),
                topic: AUTHORITY_SNAPSHOT_REQUESTS_TOPIC.to_string(),
                payload: request.clone(),
                reply_to: None,
                trace_ctx: None,
                published_by: None,
            })
        });
        // Asked once at start: the broker is not a client, so no registration asks for it.
        handle.registered();
        Ok(Self {
            handle,
            lookup: PolicyLookup::for_service(SERVICE),
        })
    }

    /// Whether the client whose floor is `floor` may use `topic` this way; `Err` says why not. The agent's own gate
    /// asks the same question with the same rule, so a refusal here means one of them holds stale authority.
    pub async fn decide(&self, floor: &Floor, topic: &str, access: Access) -> Result<(), String> {
        let said = ask(
            &self.lookup,
            floor.identity(),
            Some(floor.certificate_pem().to_string()),
            topic,
            access,
            floor.facts(topic, access),
        )
        .await;
        rule(&said, floor.allows(topic, access), topic, access)
            .map_err(|why| format!("the broker refused: {why}"))
    }

    /// Hands a publish on an authority topic to the broker's authority, as published by `publisher`, the identity
    /// the broker verified. Only the credentials agent is believed, which the authority checks.
    pub fn hear(&self, topic: &str, payload: &[u8], publisher: &str) {
        let publisher = Some(publisher.to_string());
        match topic {
            AUTHORITY_UPDATES_TOPIC => {
                match rkyv::from_bytes::<AuthorityUpdate, rkyv::rancor::Error>(payload) {
                    Ok(update) => self.handle.update(update, publisher),
                    Err(e) => {
                        warn!("a message on the authority updates topic is not an update: {e}")
                    }
                }
            }
            AUTHORITY_SNAPSHOT_REPLIES_TOPIC => {
                match rkyv::from_bytes::<AuthoritySnapshot, rkyv::rancor::Error>(payload) {
                    Ok(snapshot) => self.handle.snapshot(snapshot, publisher),
                    Err(e) => {
                        warn!("a message on the authority snapshot topic is not a snapshot: {e}")
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether `topic` carries authority the broker keeps.
    pub fn carries_authority(topic: &str) -> bool {
        topic == AUTHORITY_UPDATES_TOPIC || topic == AUTHORITY_SNAPSHOT_REPLIES_TOPIC
    }

    pub fn handle(&self) -> &AuthorityHandle {
        &self.handle
    }
}
