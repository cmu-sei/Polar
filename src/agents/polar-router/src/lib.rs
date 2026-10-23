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

//! polar-router
//!
//! RouterActor -- the real, per-agent fix for a genuine, widespread
//! anti-pattern found in this codebase's own, existing supervisors:
//! every one of them subscribes directly to a shared
//! `Arc<OutputPort<ClientEvent>>`, meaning every single incoming
//! Cassini event is broadcast to every subscriber, and each one
//! independently re-derives "is this actually mine?" via its own,
//! large `match topic.as_str() { ... }` -- silently dropping
//! everything else. That's real, genuine cross-actor message leakage:
//! an actor's own `handle()` has to at least name, and usually parse,
//! every topic any other actor in the same agent cares about, even
//! when it does nothing with them at all.
//!
//! RouterActor replaces that with real, directed, per-topic dispatch:
//! any local actor registers real interest in a specific topic, once,
//! at its own construction time, handing over a closure that knows
//! how to turn a raw incoming message into that actor's own, real,
//! concrete message type and send it there directly. RouterActor
//! itself never learns or cares what that concrete type is -- its own
//! message surface (see `RouterMessage`) is permanently fixed,
//! regardless of how many real topics/actors an agent ends up with.
//!
//! Deliberately, genuinely scoped to *this one, real problem* --
//! inter-actor message routing -- and not a general request/reply
//! mechanism: an actor that needs per-correlation-id dispatch on top
//! of this (multiple, concurrent, in-flight requests on the exact
//! same topic) still owns its own `polar::request_reply::ReplyRouter`
//! internally, inside its own registered `deliver` closure, exactly
//! as today -- the two compose, neither replaces the other.

use ractor::{Actor, ActorProcessingErr, ActorRef, async_trait};
use std::collections::HashMap;
use tracing::{error, warn};

/// A real, incoming message RouterActor has decided belongs to one,
/// specific, registered topic -- deliberately mirroring
/// `ClientEvent::MessagePublished`'s own, real, existing fields (the
/// exact shape every current supervisor already destructures), so a
/// migrating actor's own, real deserialization logic doesn't need to
/// change at all, only where it's called from.
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub topic: String,
    pub payload: Vec<u8>,
    pub published_by: Option<String>,
    pub trace_ctx: Option<cassini_types::WireTraceCtx>,
}

/// RouterActor's own, complete, permanently-fixed message surface --
/// this is the real interface guarantee the design rests on: no
/// matter how many real topics or local actors an agent ends up
/// composing, this type itself never needs to grow or change.
pub enum RouterMessage {
    /// Sent once, by a local actor at its own construction time --
    /// registers real, genuine interest in `topic`. `deliver` is the
    /// registering actor's own, private knowledge of how to turn a
    /// raw `IncomingMessage` into its own, real message type and send
    /// it there -- RouterActor itself never sees or handles any
    /// agent-specific type at all.
    ///
    /// Registering a topic that already has a real, existing
    /// registration is treated as a genuine bug, not silently
    /// accepted -- see this crate's own top-level doc comment for
    /// why cross-wiring is exactly the failure mode this actor
    /// exists to prevent. The existing registration is kept; the new
    /// one is rejected, loudly, via a real `tracing::error!`.
    Register {
        topic: String,
        deliver: Box<dyn Fn(IncomingMessage) + Send + Sync>,
    },
    /// The one, sole message the paired TcpClientActor variant
    /// (cassini-client's own, real, separate actor type) ever sends.
    /// Deliberately, permanently topic-agnostic -- every real,
    /// per-topic decision lives in RouterActor's own, internal
    /// registration table, never in this message's own shape.
    Route(IncomingMessage),
}

pub struct RouterActor;

pub struct RouterActorState {
    registrations: HashMap<String, Box<dyn Fn(IncomingMessage) + Send + Sync>>,
}

#[async_trait]
impl Actor for RouterActor {
    type Msg = RouterMessage;
    type State = RouterActorState;
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: (),
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(RouterActorState {
            registrations: HashMap::new(),
        })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            RouterMessage::Register { topic, deliver } => match state.registrations.entry(topic) {
                std::collections::hash_map::Entry::Occupied(existing) => error!(
                    "RouterActor: refusing duplicate registration for topic '{}' -- \
                         a topic must have exactly one, real registered handler; the existing \
                         one is kept",
                    existing.key()
                ),
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(deliver);
                }
            },
            RouterMessage::Route(incoming) => match state.registrations.get(&incoming.topic) {
                Some(deliver) => deliver(incoming),
                None => {
                    warn!(
                        "RouterActor: no registered handler for topic '{}' ({} bytes), \
                             dropping",
                        incoming.topic,
                        incoming.payload.len()
                    );
                }
            },
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn test_message(topic: &str, payload: &[u8]) -> IncomingMessage {
        IncomingMessage {
            topic: topic.to_string(),
            payload: payload.to_vec(),
            published_by: None,
            trace_ctx: None,
        }
    }

    #[tokio::test]
    async fn a_registered_topic_genuinely_reaches_its_own_deliver_closure() {
        let (router, _) = Actor::spawn(None, RouterActor, ()).await.unwrap();
        let received: Arc<Mutex<Option<IncomingMessage>>> = Arc::new(Mutex::new(None));
        let received_for_closure = received.clone();

        router
            .send_message(RouterMessage::Register {
                topic: "my-topic".to_string(),
                deliver: Box::new(move |msg| {
                    *received_for_closure.lock().unwrap() = Some(msg);
                }),
            })
            .unwrap();

        router
            .send_message(RouterMessage::Route(test_message("my-topic", b"hello")))
            .unwrap();

        // RouterMessage carries no real, typed reply of its own, so
        // there's no call()-based way to deterministically wait for
        // this specific message to finish processing -- a short,
        // real sleep is an honest compromise, matching the same,
        // established pattern this codebase already uses elsewhere
        // for actors with no reply port on the message being tested.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let got = received.lock().unwrap().clone();
        assert!(
            got.is_some(),
            "the registered closure must have genuinely received the message"
        );
        assert_eq!(got.unwrap().payload, b"hello");
    }

    #[tokio::test]
    async fn an_unregistered_topic_is_a_harmless_no_op() {
        let (router, _) = Actor::spawn(None, RouterActor, ()).await.unwrap();

        // No registration at all -- must not panic, just log and
        // drop, matching this crate's own, documented behavior.
        router
            .send_message(RouterMessage::Route(test_message(
                "never-registered",
                b"orphaned",
            )))
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Reaching this line at all is the assertion -- no panic.
    }

    #[tokio::test]
    async fn a_duplicate_registration_is_genuinely_rejected_not_silently_overwritten() {
        let (router, _) = Actor::spawn(None, RouterActor, ()).await.unwrap();
        let first_received: Arc<Mutex<Option<IncomingMessage>>> = Arc::new(Mutex::new(None));
        let second_received: Arc<Mutex<Option<IncomingMessage>>> = Arc::new(Mutex::new(None));
        let first_for_closure = first_received.clone();
        let second_for_closure = second_received.clone();

        router
            .send_message(RouterMessage::Register {
                topic: "contested-topic".to_string(),
                deliver: Box::new(move |msg| {
                    *first_for_closure.lock().unwrap() = Some(msg);
                }),
            })
            .unwrap();

        // A second, real registration attempt on the exact same
        // topic -- the actual, specific failure mode this crate
        // exists to prevent (two local actors accidentally both
        // trying to own the same topic).
        router
            .send_message(RouterMessage::Register {
                topic: "contested-topic".to_string(),
                deliver: Box::new(move |msg| {
                    *second_for_closure.lock().unwrap() = Some(msg);
                }),
            })
            .unwrap();

        router
            .send_message(RouterMessage::Route(test_message(
                "contested-topic",
                b"who gets this?",
            )))
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(
            first_received.lock().unwrap().is_some(),
            "the original, first registration must still genuinely own this topic"
        );
        assert!(
            second_received.lock().unwrap().is_none(),
            "the second, rejected registration must never receive anything at all"
        );
    }
}
