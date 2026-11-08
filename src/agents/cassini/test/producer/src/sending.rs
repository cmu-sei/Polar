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

//! The producer's loop: the run's messages, paced (`harness_common::schedule`), and never more than the window sent
//! and not yet settled. A message is settled when the broker acknowledges it or refuses it; one the client would not
//! take is failed, and never in flight. When nothing settles for the stall time the producer gives up on the rest.

use harness_common::schedule::{due_at, due_by};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;

/// What the broker has said about the producer's messages, kept by the client's events and read by the loop.
#[derive(Default)]
pub struct Progress {
    acknowledged: AtomicU64,
    refused: AtomicU64,
    changed: Notify,
}

impl Progress {
    pub fn acknowledge(&self) {
        self.acknowledged.fetch_add(1, Ordering::Relaxed);
        self.changed.notify_one();
    }

    pub fn refuse(&self) {
        self.refused.fetch_add(1, Ordering::Relaxed);
        self.changed.notify_one();
    }

    pub fn acknowledged(&self) -> u64 {
        self.acknowledged.load(Ordering::Relaxed)
    }

    pub fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }

    fn settled(&self) -> u64 {
        self.acknowledged() + self.refused()
    }

    /// Waits until something settles, for at most `stall`; false if nothing did.
    async fn wait(&self, stall: Duration) -> bool {
        tokio::time::timeout(stall, self.changed.notified())
            .await
            .is_ok()
    }
}

/// What to send.
#[derive(Debug, Clone, Copy)]
pub struct Plan {
    pub count: u64,
    /// Messages a second; 0 is as fast as the window allows.
    pub rate: u64,
    /// The most messages in flight, at least 1.
    pub window: u64,
    pub stall: Duration,
}

/// What was sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sent {
    pub sent: u64,
    pub failed: u64,
    /// From the start to the last message handed to the client.
    pub sending: Duration,
    /// From the start to the last settled (or to giving up).
    pub total: Duration,
    pub gave_up: bool,
}

/// Sends the plan's messages through `publish`, which is given each message's sequence number and says whether the
/// client took it.
pub async fn send_all(
    plan: Plan,
    progress: &Progress,
    mut publish: impl FnMut(u64) -> bool,
) -> Sent {
    let window = plan.window.max(1);
    let start = Instant::now();
    let (mut sent, mut failed, mut gave_up) = (0u64, 0u64, false);
    while sent < plan.count {
        let in_flight = (sent - failed).saturating_sub(progress.settled());
        let room = window.saturating_sub(in_flight);
        if room == 0 {
            if !progress.wait(plan.stall).await {
                gave_up = true;
                break;
            }
            continue;
        }
        let due = due_by(start.elapsed(), plan.rate, plan.count);
        let now = due.saturating_sub(sent).min(room);
        if now == 0 {
            tokio::time::sleep_until(start + due_at(sent, plan.rate)).await;
            continue;
        }
        for _ in 0..now {
            if !publish(sent) {
                failed += 1;
            }
            sent += 1;
        }
    }
    let sending = start.elapsed();
    while !gave_up && progress.settled() + failed < sent {
        if !progress.wait(plan.stall).await {
            gave_up = true;
        }
    }
    Sent {
        sent,
        failed,
        sending,
        total: start.elapsed(),
        gave_up,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::Arc;

    const STALL: Duration = Duration::from_secs(5);

    fn plan(count: u64, rate: u64, window: u64) -> Plan {
        Plan {
            count,
            rate,
            window,
            stall: STALL,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn every_message_is_sent_once_in_order_at_the_rate() {
        let progress = Progress::default();
        let mut seen = Vec::new();
        let sent = send_all(plan(100, 1000, 10), &progress, |seqno| {
            seen.push(seqno);
            progress.acknowledge();
            true
        })
        .await;
        assert_eq!(seen, (0..100).collect::<Vec<_>>());
        assert_eq!((sent.sent, sent.failed, sent.gave_up), (100, 0, false));
        // The last of a hundred at a thousand a second is due 99 ms in.
        assert!(sent.sending >= Duration::from_millis(99), "{sent:?}");
        assert!(sent.sending < Duration::from_millis(110), "{sent:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn with_nothing_acknowledged_it_stops_at_the_window_and_gives_up_after_the_stall() {
        let progress = Progress::default();
        let sent = send_all(plan(100, 0, 8), &progress, |_| true).await;
        assert_eq!((sent.sent, sent.gave_up), (8, true));
        assert!(sent.total >= STALL, "{sent:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn refusals_settle_and_failures_are_never_in_flight() {
        let progress = Progress::default();
        let sent = send_all(plan(50, 0, 4), &progress, |seqno| {
            if seqno % 2 == 0 {
                progress.refuse();
                true
            } else {
                false
            }
        })
        .await;
        assert_eq!((sent.sent, sent.failed, sent.gave_up), (50, 25, false));
        assert_eq!(progress.refused(), 25);
    }

    #[tokio::test(start_paused = true)]
    async fn acknowledgements_that_arrive_later_open_the_window() {
        let progress = Arc::new(Progress::default());
        let acker = progress.clone();
        tokio::spawn(async move {
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(10)).await;
                acker.acknowledge();
            }
        });
        let sent = send_all(plan(20, 0, 5), &progress, |_| true).await;
        assert_eq!((sent.sent, sent.gave_up), (20, false));
        assert!(sent.total >= Duration::from_millis(200), "{sent:?}");
    }

    proptest! {
        /// However the broker acknowledges, the producer never has more than its window in flight, sends each
        /// sequence number once and in order, and never more than the run.
        #[test]
        fn the_window_is_never_exceeded(
            count in 0u64..300,
            window in 1u64..20,
            rate in prop::sample::select(vec![0u64, 100, 10_000]),
            every in 1u64..5,
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap();
            runtime.block_on(async {
                let progress = Progress::default();
                let (mut seen, mut most) = (Vec::new(), 0u64);
                let sent = send_all(plan(count, rate, window), &progress, |seqno| {
                    seen.push(seqno);
                    most = most.max(seen.len() as u64 - progress.settled());
                    // Acknowledge one message in every `every`.
                    if seqno % every == 0 {
                        progress.acknowledge();
                    }
                    true
                })
                .await;
                prop_assert!(most <= window, "{} in flight, window {}", most, window);
                prop_assert_eq!(seen, (0..sent.sent).collect::<Vec<_>>());
                prop_assert!(sent.sent <= count);
                Ok(())
            })?;
        }
    }
}
