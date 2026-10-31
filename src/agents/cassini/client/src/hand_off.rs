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

//! A slow hand-off, for the load harness's slow consumer only (feature `slow-hand-off`, which only `harness-sink`
//! enables; docs/architecture/broker-baseline.md, the fifth set).
//!
//! The client acknowledges a delivery when it hands it to the agent (`TcpClientActor::delivery_handed_off`), so a
//! consumer that is slow after the hand-off never slows its acknowledgements: what waits for it waits in its own
//! mailbox, and the broker never feels it. With a delay set here, the connection's reader spends it on each delivery
//! *before* handing it over, as a consumer that does its work before acknowledging would (the domain-deferred
//! acknowledgement, not built): the hand-offs, and with them the acknowledgements, come at most one a delay, and what
//! waits, waits in the broker's delivery outbox and the socket.
//!
//! Nothing here acts unless [`slow`] is called, which no agent but the harness's sink does: a client built with the
//! feature and never told to be slow reads and hands over exactly as one without it.

use cassini_types::ArchivedClientMessage;
use rkyv::rancor::Error;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

static DELAY: OnceLock<Duration> = OnceLock::new();

/// How late a hand-off may begin after the one before it ended and still count as the same busy spell: a timer wakes
/// up to about a millisecond late, and that lateness is not the consumer's.
pub const SLACK: Duration = Duration::from_millis(2);

/// Every connection this process opens from now on spends `delay` on each delivery before handing it over and
/// acknowledging it. Set once; a zero delay, or a second call, changes nothing.
pub fn slow(delay: Duration) {
    if !delay.is_zero() {
        let _ = DELAY.set(delay);
    }
}

/// The pace a new connection's reader keeps, if the process was told to be slow.
pub fn pace() -> Option<Pace> {
    DELAY.get().map(|delay| Pace::new(*delay))
}

/// A consumer that takes `delay` over each delivery, one at a time: the `n`th of a backlog is handed over `n` delays
/// after the first began, whatever the timer's lateness, and one that comes after an idle spell takes `delay` from when
/// it came.
#[derive(Debug, Clone)]
pub struct Pace {
    delay: Duration,
    /// When the last hand-off ends.
    due: Option<Instant>,
}

impl Pace {
    pub fn new(delay: Duration) -> Self {
        Self { delay, due: None }
    }

    /// When the hand-off of a delivery read at `now` ends: `delay` after the last one ended, if that was no more than
    /// [`SLACK`] before `now` (the consumer was busy), or `delay` after `now` (it was idle).
    pub fn next(&mut self, now: Instant) -> Instant {
        let start = match self.due {
            Some(due) if due + SLACK >= now => due,
            _ => now,
        };
        let due = start + self.delay;
        self.due = Some(due);
        due
    }
}

/// Whether a frame is a delivery the client will acknowledge: only those are slowed, so a registration or a
/// subscription's answer is read at once.
pub fn is_delivery(frame: &[u8]) -> bool {
    matches!(
        rkyv::access::<ArchivedClientMessage, Error>(frame),
        Ok(ArchivedClientMessage::PublishResponse { delivery_id, result, .. })
            if delivery_id.is_some() && result.is_ok()
    )
}

/// Spends the pace's delay on `frame` if it is a delivery, before the reader hands it over.
pub async fn before_hand_off(pace: &mut Option<Pace>, frame: &[u8]) {
    if let Some(pace) = pace.as_mut()
        && is_delivery(frame)
    {
        let due = pace.next(Instant::now());
        tokio::time::sleep_until(due.into()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cassini_types::ClientMessage;
    use proptest::prelude::*;

    fn delivery(delivery_id: Option<u64>, result: Result<(), String>) -> Vec<u8> {
        let message = ClientMessage::PublishResponse {
            topic: "t".to_string(),
            payload: b"x".to_vec().into(),
            result,
            trace_ctx: None,
            published_by: None,
            origin: None,
            delivery_id,
            refusal: None,
        };
        rkyv::to_bytes::<Error>(&message).unwrap().to_vec()
    }

    #[test]
    fn only_a_delivery_with_a_number_is_slowed() {
        assert!(is_delivery(&delivery(Some(1), Ok(()))));
        assert!(!is_delivery(&delivery(None, Ok(()))));
        assert!(!is_delivery(&delivery(Some(1), Err("no".to_string()))));
        let ack = ClientMessage::PublishResponseAck { delivery_id: 1 };
        assert!(!is_delivery(&rkyv::to_bytes::<Error>(&ack).unwrap()));
        assert!(!is_delivery(b"not a frame"));
    }

    #[test]
    fn a_backlog_is_handed_over_a_delay_apart_and_an_idle_spell_starts_again() {
        let t0 = Instant::now();
        let d = Duration::from_millis(10);
        let mut pace = Pace::new(d);
        assert_eq!(pace.next(t0), t0 + d);
        // Woken a millisecond late: still busy, so the schedule holds.
        assert_eq!(pace.next(t0 + d + Duration::from_millis(1)), t0 + 2 * d);
        // Read while still busy (a backlog): one delay after the last.
        assert_eq!(pace.next(t0 + d + Duration::from_millis(5)), t0 + 3 * d);
        // After an idle spell, a delay from when it came.
        let later = t0 + Duration::from_secs(1);
        assert_eq!(pace.next(later), later + d);
    }

    proptest! {
        /// For any reads, each hand-off ends at least a delay after the one before, never more than a delay after
        /// both that one and its read, and never sooner than a delay, less the slack, after its read.
        #[test]
        fn hand_offs_are_a_delay_apart_and_never_early(
            delay_us in 1u64..20_000,
            gaps_us in proptest::collection::vec(0u64..40_000, 1..60),
        ) {
            let delay = Duration::from_micros(delay_us);
            let mut pace = Pace::new(delay);
            let mut now = Instant::now();
            let mut last: Option<Instant> = None;
            for gap in gaps_us {
                now += Duration::from_micros(gap);
                let due = pace.next(now);
                prop_assert!(due + SLACK >= now + delay);
                prop_assert!(due <= last.map_or(now, |l| l.max(now)) + delay);
                if let Some(last) = last {
                    prop_assert!(due >= last + delay);
                }
                last = Some(due);
            }
        }
    }

    /// Stands in for the transport: the acknowledgements under test go through the writer's channel, not here.
    struct Ignore;

    #[ractor::async_trait]
    impl ractor::Actor for Ignore {
        type Msg = crate::TcpClientMessage;
        type State = ();
        type Arguments = ();
        async fn pre_start(
            &self,
            _myself: ractor::ActorRef<Self::Msg>,
            _args: (),
        ) -> Result<(), ractor::ActorProcessingErr> {
            Ok(())
        }
    }

    /// The reader's step, as it runs (the delay, then the frame handed over): with a delay, the `n`th delivery is
    /// acknowledged no sooner than `n` delays after the first was read; without one, at once.
    #[tokio::test]
    async fn acknowledgements_wait_for_the_delay_only_when_slowed() {
        use crate::{DeliveryMode, TcpClientActor};
        let d = Duration::from_millis(30);
        for slowed in [true, false] {
            let (myself, _) = ractor::Actor::spawn(None, Ignore, ()).await.unwrap();
            let mode = DeliveryMode::Broadcast(std::sync::Arc::new(ractor::OutputPort::default()));
            let (acks, mut written) = tokio::sync::mpsc::unbounded_channel();
            let mut pace = slowed.then(|| Pace::new(d));
            let start = Instant::now();
            let reader = tokio::spawn(async move {
                for id in 1..=4u64 {
                    let frame = delivery(Some(id), Ok(()));
                    before_hand_off(&mut pace, &frame).await;
                    TcpClientActor::on_frame(myself.clone(), &mode, None, Some(&acks), id, &frame);
                }
            });
            for n in 1..=4u32 {
                let ack = written.recv().await.unwrap();
                let at = start.elapsed();
                assert!(
                    matches!(ack, ClientMessage::PublishResponseAck { delivery_id } if delivery_id == n as u64),
                    "{ack:?}"
                );
                if slowed {
                    assert!(
                        at >= d * n,
                        "acknowledgement {n} came after {at:?}, before {:?}",
                        d * n
                    );
                } else {
                    assert!(at < d, "acknowledgement {n}, not slowed, came after {at:?}");
                }
            }
            reader.await.unwrap();
        }
    }
}
