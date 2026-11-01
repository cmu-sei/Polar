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

//! HealthHandle -- wraps a real, live polar_health::HealthCheckActor's
//! own ActorRef<HealthCheckMessage>, exposing its every, real, fire-
//! and-forget variant as a direct, ordinary method. Every real
//! HealthCheckMessage variant is genuinely fire-and-forget (a plain
//! cast, never a call) -- see that enum's own definition -- so, unlike
//! CryptoHandle, none of these methods need to be async at all.

use polar_health::{HealthCheckMessage, HealthReport, HoldReason, ReportReply};
use ractor::ActorRef;

pub trait HealthClient: Send + Sync {
    fn cassini_connected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>>;
    fn cassini_disconnected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>>;
    fn graph_connected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>>;
    fn graph_disconnected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>>;
    fn graph_available(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>>;
    fn graph_op_failed(
        &self,
        reason: String,
    ) -> Result<(), ractor::MessagingErr<HealthCheckMessage>>;
}

/// The real, live implementation of HealthClient -- a thin wrapper
/// around a real, already-spawned HealthCheckActor's own ActorRef.
#[derive(Clone)]
pub struct HealthHandle {
    actor: ActorRef<HealthCheckMessage>,
}

impl HealthHandle {
    pub fn new(actor: ActorRef<HealthCheckMessage>) -> Self {
        Self { actor }
    }

    /// The same, real, deliberate escape hatch every other handle in
    /// this crate offers -- see CryptoHandle::as_actor_ref's own doc
    /// comment for the full, shared reasoning.
    pub fn as_actor_ref(&self) -> ActorRef<HealthCheckMessage> {
        self.actor.clone()
    }

    /// The domain holds a standing problem, one a restart does not help: its configuration cannot work
    /// ([`HoldReason::Configuration`]) or its credentials are refused ([`HoldReason::Credentials`]). The agent stays
    /// up, answers health queries with the status `Unhealthy`, this reason and `message`, and is never ended for it;
    /// the domain stops retrying what cannot work until it calls [`release`](Self::release) (a new configuration, a new
    /// token). `message` is one line for a person (what to set); it is cut to a bound and cleaned of credentials and of
    /// the agent's secret values before it leaves the agent, and should never carry one. A later hold replaces it.
    /// This is the one way a domain makes a standing problem known (docs/architecture/soft-state.md, "Self-state and
    /// introspection"): nothing outside ever looks into the domain.
    pub fn hold(&self, reason: HoldReason, message: impl Into<String>) {
        if let Err(e) = self.actor.cast(HealthCheckMessage::Hold {
            reason,
            message: message.into(),
        }) {
            tracing::warn!("the health actor was not told of a standing problem: {e}");
        }
    }

    /// The standing problem of this kind is resolved: the agent reports as its connections say again.
    pub fn release(&self, reason: HoldReason) {
        if let Err(e) = self.actor.cast(HealthCheckMessage::Release(reason)) {
            tracing::warn!("the health actor was not told a standing problem is resolved: {e}");
        }
    }

    /// The agent began to end for a reason of the supervisor's own: its report says it is draining.
    pub(crate) fn draining(&self) {
        let _ = self.actor.cast(HealthCheckMessage::Draining);
    }

    /// The health actor's report, as the agent answers a health query with it.
    pub async fn report(&self) -> Result<HealthReport, String> {
        ractor::call!(self.actor, |reply| HealthCheckMessage::Report(
            ReportReply::new(reply)
        ))
        .map_err(|e| format!("the health actor did not answer: {e}"))
    }

    /// The topics the agent wants and is refused now, for its health file. They never make it unhealthy.
    pub fn topics_refused(&self, topics: Vec<polar_health::RefusedTopic>) {
        if let Err(e) = self.actor.cast(HealthCheckMessage::TopicsRefused(topics)) {
            tracing::warn!("the health actor was not told the topics refused: {e}");
        }
    }
}

impl HealthClient for HealthHandle {
    fn cassini_connected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>> {
        self.actor.cast(HealthCheckMessage::CassiniConnected)
    }

    fn cassini_disconnected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>> {
        self.actor.cast(HealthCheckMessage::CassiniDisconnected)
    }

    fn graph_connected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>> {
        self.actor.cast(HealthCheckMessage::GraphConnected)
    }

    fn graph_disconnected(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>> {
        self.actor.cast(HealthCheckMessage::GraphDisconnected)
    }

    fn graph_available(&self) -> Result<(), ractor::MessagingErr<HealthCheckMessage>> {
        self.actor.cast(HealthCheckMessage::GraphAvailable)
    }

    fn graph_op_failed(
        &self,
        reason: String,
    ) -> Result<(), ractor::MessagingErr<HealthCheckMessage>> {
        self.actor.cast(HealthCheckMessage::GraphOpFailed(reason))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ractor::{Actor, ActorProcessingErr, async_trait};
    use std::sync::{Arc, Mutex};

    /// A minimal, real stand-in for HealthCheckActor -- genuinely
    /// receives HealthCheckMessage, just records what it got, since
    /// this test's own, real purpose is proving HealthHandle's own
    /// methods genuinely, correctly send the right, real variant --
    /// not re-testing HealthCheckActor's own, separate, real,
    /// internal logic.
    struct FakeHealthCheckActor {
        received: Arc<Mutex<Vec<HealthCheckMessage>>>,
    }

    #[async_trait]
    impl Actor for FakeHealthCheckActor {
        type Msg = HealthCheckMessage;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            self.received.lock().unwrap().push(message);
            Ok(())
        }
    }

    #[tokio::test]
    async fn every_real_method_sends_its_own_matching_real_variant() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (actor, _) = Actor::spawn(
            None,
            FakeHealthCheckActor {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();

        let handle = HealthHandle::new(actor);

        handle.cassini_connected().unwrap();
        handle.cassini_disconnected().unwrap();
        handle.graph_connected().unwrap();
        handle.graph_disconnected().unwrap();
        handle.graph_available().unwrap();
        handle
            .graph_op_failed("a real, genuine failure reason".to_string())
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let messages = received.lock().unwrap();
        assert_eq!(messages.len(), 6);
        assert!(matches!(messages[0], HealthCheckMessage::CassiniConnected));
        assert!(matches!(
            messages[1],
            HealthCheckMessage::CassiniDisconnected
        ));
        assert!(matches!(messages[2], HealthCheckMessage::GraphConnected));
        assert!(matches!(messages[3], HealthCheckMessage::GraphDisconnected));
        assert!(matches!(messages[4], HealthCheckMessage::GraphAvailable));
        match &messages[5] {
            HealthCheckMessage::GraphOpFailed(reason) => {
                assert_eq!(reason, "a real, genuine failure reason");
            }
            other => panic!("expected GraphOpFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_hold_and_a_release_are_told_to_the_health_actor_as_they_were_given() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (actor, _) = Actor::spawn(
            None,
            FakeHealthCheckActor {
                received: received.clone(),
            },
            (),
        )
        .await
        .unwrap();
        let handle = HealthHandle::new(actor);
        handle.hold(HoldReason::Configuration, "JIRA_URL is a placeholder");
        handle.release(HoldReason::Credentials);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let messages = received.lock().unwrap();
        assert_eq!(messages.len(), 2);
        assert!(matches!(
            &messages[0],
            HealthCheckMessage::Hold { reason: HoldReason::Configuration, message }
                if message == "JIRA_URL is a placeholder"
        ));
        assert!(matches!(
            messages[1],
            HealthCheckMessage::Release(HoldReason::Credentials)
        ));
    }
}
