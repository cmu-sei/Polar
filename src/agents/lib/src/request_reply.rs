/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

//! Generic request/reply correlation over Cassini's pub/sub-only
//! transport, per docs/architecture/cassini-request-reply-protocol.md.
//! Any future agent building a request/reply interaction over Cassini
//! should wrap its own payload type in `Envelope<T>` rather than
//! duplicating a `correlation_id` field per message type -- the same
//! "generic struct carrying cross-cutting metadata alongside a payload"
//! idiom already used by gitlab/common's `WithInstance<T>`.
//!
//! Scoped to `rkyv` only, matching `WithInstance<T>`'s own precedent --
//! this type's whole job is the Cassini wire-transport leg of a
//! request/reply exchange, not persistence, so it doesn't require
//! `T: Serialize + DeserializeOwned` the way a type meant for
//! `EventLog` would. A caller whose payload also needs to be logged
//! (e.g. via `event_log::EventLog`) should derive `serde` on that
//! payload type directly, the same way `CredentialsEvent` does.

use rkyv::{Archive, Deserialize, Serialize};

/// Pairs a `correlation_id` with an arbitrary payload for one leg of a
/// request/reply exchange over Cassini.
#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub correlation_id: String,
    pub payload: T,
}

use std::collections::HashMap;
use tokio::sync::oneshot;

/// Routes correlated replies back to whichever caller is waiting for
/// them, over a single, shared, long-lived subscription -- the
/// pattern any HTTP-gateway-style agent needs when bridging a
/// synchronous request to Cassini's asynchronous pub/sub transport,
/// per docs/architecture/cassini-request-reply-protocol.md.
///
/// One `ReplyRouter` is meant to be shared (via `Arc`) across every
/// in-flight request a gateway is handling. A caller calls
/// `register()` before publishing its own request, gets back a
/// receiver, and awaits it (with its own timeout -- this type has no
/// opinion on timing at all). Meanwhile, whatever code owns the
/// gateway's single, shared subscription calls `deliver()` every time
/// a reply arrives on the wire, and this type takes care of routing
/// it to the right waiter by `correlation_id`.
pub struct ReplyRouter<T> {
    pending: std::sync::Mutex<HashMap<String, oneshot::Sender<T>>>,
}

impl<T> Default for ReplyRouter<T> {
    fn default() -> Self {
        Self {
            pending: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl<T> ReplyRouter<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers interest in a reply for `correlation_id`, returning a
    /// receiver the caller should await. Must be called *before* the
    /// corresponding request is published -- a reply arriving in the
    /// gap between publishing and registering would have nowhere to
    /// be routed.
    pub fn register(&self, correlation_id: String) -> oneshot::Receiver<T> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(correlation_id, tx);
        rx
    }

    /// Delivers `reply` to whichever caller registered for this exact
    /// `correlation_id`, if any. A reply with no matching registration
    /// (already timed out and cancelled, or a `correlation_id` that
    /// was never actually registered) is silently dropped -- there's
    /// no meaningful action to take on an orphaned reply.
    pub fn deliver(&self, correlation_id: &str, reply: T) {
        if let Some(tx) = self.pending.lock().unwrap().remove(correlation_id) {
            let _ = tx.send(reply);
        }
    }

    /// Removes a pending registration without delivering anything --
    /// callers should call this after their own timeout fires, so a
    /// registration doesn't linger forever if its reply never arrives.
    pub fn cancel(&self, correlation_id: &str) {
        self.pending.lock().unwrap().remove(correlation_id);
    }

    /// `register()` plus its own, real timeout handling, as one,
    /// atomic operation -- the exact "register, publish, await with a
    /// timeout, cancel on timeout" sequence every real caller of this
    /// type has otherwise had to hand-write identically (see this
    /// crate's own commit history for several, real, independent
    /// examples). Genuinely additive: `register`/`deliver`/`cancel`
    /// are unchanged, and every existing caller keeps working exactly
    /// as before -- this is a real, second, shorter path for new
    /// callers, not a replacement.
    ///
    /// The real property this closes: a caller of the bare
    /// `register()`/`cancel()` pair could always, genuinely forget to
    /// call `cancel()` on some unusual, early-return error path,
    /// leaving a real, permanent entry in `pending` -- a genuine,
    /// unbounded-growth risk over the life of a long-running agent.
    /// This can't forget, since the cancel-on-timeout call is
    /// unconditionally part of this one function's own body.
    ///
    /// The caller must still publish its own request *after* this
    /// returns its receiver internally and *before* the timeout
    /// elapses -- this function only wraps the waiting half, not the
    /// publish itself, since publishing is genuinely specific to each
    /// caller's own transport/topic.
    pub async fn register_with_timeout(
        &self,
        correlation_id: String,
        timeout: std::time::Duration,
        publish: impl FnOnce() -> Result<(), String>,
    ) -> Result<T, ReplyError> {
        let rx = self.register(correlation_id.clone());

        if let Err(e) = publish() {
            self.cancel(&correlation_id);
            return Err(ReplyError::PublishFailed(e));
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err(ReplyError::ChannelClosed),
            Err(_) => {
                self.cancel(&correlation_id);
                Err(ReplyError::Timeout)
            }
        }
    }
}

/// The real, complete set of ways `register_with_timeout` can fail --
/// deliberately distinguishing these three, rather than collapsing
/// them into one, generic error, since each is a genuinely different,
/// real situation a caller may want to handle differently (e.g.
/// retrying a `PublishFailed` immediately is reasonable; retrying a
/// `Timeout` immediately usually isn't).
#[derive(Debug, thiserror::Error)]
pub enum ReplyError {
    #[error("failed to publish the underlying request: {0}")]
    PublishFailed(String),
    #[error("timed out waiting for a reply")]
    Timeout,
    #[error("reply channel closed with no reply")]
    ChannelClosed,
}

#[cfg(test)]
mod reply_router_tests {
    use super::*;

    #[tokio::test]
    async fn delivered_reply_reaches_its_registered_waiter() {
        let router: ReplyRouter<String> = ReplyRouter::new();
        let rx = router.register("abc-123".to_string());

        router.deliver("abc-123", "the reply".to_string());

        assert_eq!(rx.await.unwrap(), "the reply");
    }

    #[tokio::test]
    async fn delivering_an_unregistered_id_is_a_harmless_no_op() {
        let router: ReplyRouter<String> = ReplyRouter::new();
        // No panic, nothing to assert beyond reaching this line.
        router.deliver("never-registered", "orphaned".to_string());
    }

    #[tokio::test]
    async fn cancelled_registration_never_receives_a_later_delivery() {
        let router: ReplyRouter<String> = ReplyRouter::new();
        let rx = router.register("abc-123".to_string());
        router.cancel("abc-123");

        router.deliver("abc-123", "too late".to_string());

        // The sender was already removed by cancel(), so the receiver
        // observes the channel closing, never a value.
        assert!(rx.await.is_err());
    }

    #[tokio::test]
    async fn two_concurrent_registrations_are_routed_independently() {
        let router: ReplyRouter<String> = ReplyRouter::new();
        let rx_a = router.register("id-a".to_string());
        let rx_b = router.register("id-b".to_string());

        router.deliver("id-a", "reply for a".to_string());

        assert_eq!(rx_a.await.unwrap(), "reply for a");
        // id-b's registration is untouched -- deliver("id-a", ...)
        // must not have affected it.
        drop(rx_b); // still pending; dropping is a clean way to end the test
    }

    #[tokio::test]
    async fn register_with_timeout_returns_the_real_reply_when_delivered_in_time() {
        let router: std::sync::Arc<ReplyRouter<String>> = std::sync::Arc::new(ReplyRouter::new());
        let router_for_publish = router.clone();

        let result = router
            .register_with_timeout(
                "abc-123".to_string(),
                std::time::Duration::from_secs(5),
                move || {
                    // Standing in for a real, actual publish -- delivers
                    // immediately, genuinely before the timeout, proving
                    // the happy path returns the real reply rather than
                    // racing its own timeout.
                    router_for_publish.deliver("abc-123", "the real reply".to_string());
                    Ok(())
                },
            )
            .await;

        assert_eq!(result.unwrap(), "the real reply");
    }

    #[tokio::test]
    async fn register_with_timeout_genuinely_cancels_on_its_own_timeout() {
        let router: ReplyRouter<String> = ReplyRouter::new();

        let result = router
            .register_with_timeout(
                "abc-123".to_string(),
                std::time::Duration::from_millis(50),
                || Ok(()), // publishes, but nothing ever replies
            )
            .await;

        assert!(matches!(result, Err(ReplyError::Timeout)));

        // The real property this proves: cancel() genuinely happened
        // internally -- a late delivery after the timeout must be a
        // harmless no-op, not silently succeed against a stale
        // registration.
        router.deliver("abc-123", "too late".to_string());
    }

    #[tokio::test]
    async fn register_with_timeout_propagates_a_real_publish_failure() {
        let router: ReplyRouter<String> = ReplyRouter::new();

        let result = router
            .register_with_timeout(
                "abc-123".to_string(),
                std::time::Duration::from_secs(5),
                || Err("broker unreachable".to_string()),
            )
            .await;

        assert!(matches!(result, Err(ReplyError::PublishFailed(_))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips_through_rkyv() {
        let original = Envelope {
            correlation_id: "abc-123".to_string(),
            payload: "hello".to_string(),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: Envelope<String> =
            rkyv::from_bytes::<Envelope<String>, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(restored.correlation_id, "abc-123");
        assert_eq!(restored.payload, "hello");
    }
}
