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

//! The broker's bounds under a subscriber that does not keep up (docs/architecture/cassini-backpressure.md, "Tests,
//! first"): one that stops reading holds its topic back, the publisher is slowed by TCP and its acknowledgements stop,
//! the budget holds no more than its limit, and once the subscriber has been above its high water for the stall bound
//! its connection is ended and the publisher goes on; one that is slow but within the bound receives everything, in
//! order.
mod support;
use cassini_types::MAX_FRAME_BYTES;
use std::time::{Duration, Instant};
use support::{RawConnection, TestClient, with_broker_bounded};

/// The smallest budget a broker takes: one frame. Its high water is an eighth of it, 8 MiB.
const BUDGET: usize = MAX_FRAME_BYTES;
const MESSAGE: usize = 256 * 1024;

/// Polls `value` until it stops changing for `quiet`, and returns it, or the last value at `within`.
async fn settled(within: Duration, quiet: Duration, value: impl Fn() -> usize) -> usize {
    let deadline = Instant::now() + within;
    let mut last = value();
    let mut since = Instant::now();
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let now = value();
        if now != last {
            last = now;
            since = Instant::now();
        } else if since.elapsed() >= quiet {
            break;
        }
    }
    last
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subscriber_that_stops_reading_holds_its_topic_back_until_the_stall_bound_ends_it() {
    stops_reading(MESSAGE).await;
}

/// As above, with messages small enough that the reader reads each before it charges it.
#[tokio::test(flavor = "multi_thread")]
async fn a_subscriber_that_stops_reading_holds_back_small_messages_too() {
    stops_reading(16 * 1024).await;
}

/// A subscriber stops reading while a publisher sends 100 MiB in messages of `size` bytes.
async fn stops_reading(size: usize) {
    let stall = Duration::from_secs(2);
    with_broker_bounded(BUDGET, stall, |broker| async move {
        let budget = broker.budget();

        // Subscribes, then keeps its socket open and reads nothing.
        let mut silent = RawConnection::open(&broker, "silent.test", Some(4096)).await;
        let session = silent.register().await;
        silent.subscribe(&session, "news").await;

        let publisher = TestClient::connect(&broker, "publisher.test").await;
        let count = 100 * 1024 * 1024 / size; // more than the budget and more than the window
        for n in 0..count {
            let mut payload = vec![0u8; size];
            payload[..4].copy_from_slice(&(n as u32).to_be_bytes());
            publisher.publish("news", &payload);
        }

        // The acknowledgements stop short of the end, and the budget holds no more than its limit.
        let began = Instant::now();
        let stopped_at = settled(stall * 3 / 4, Duration::from_millis(400), || {
            publisher.acknowledged("news")
        })
        .await;
        assert!(
            stopped_at < count,
            "the publisher's acknowledgements stop while the subscriber reads nothing ({stopped_at} of {count})"
        );
        assert!(
            began.elapsed() < stall,
            "they stopped before the stall bound"
        );
        let stats = budget.stats();
        assert!(
            stats.peak <= BUDGET && stats.used <= BUDGET,
            "the budget never holds more than its limit: {stats:?}"
        );
        assert!(
            stats.peak >= broker.bounds().high_water,
            "what the subscriber was owed was counted against the budget: {stats:?}"
        );

        // Past the stall bound the subscriber's connection is ended, and the publisher goes on to the end.
        assert!(
            silent.closed_within(stall * 4).await,
            "the broker ends the connection of a subscriber that held its topic back past the stall bound"
        );
        publisher
            .wait_for("every acknowledgement", Duration::from_secs(30), |c| {
                c.acknowledged("news") == count
            })
            .await;
        let stats = budget.stats();
        assert!(stats.slow_disconnections >= 1, "{stats:?}");
        assert!(stats.peak <= BUDGET, "{stats:?}");
        assert!(
            broker.has_session(&session).await,
            "its session is kept for the retention window, as after any lost connection"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_subscriber_within_the_stall_bound_receives_everything_in_order() {
    with_broker_bounded(BUDGET, Duration::from_secs(10), |broker| async move {
        let budget = broker.budget();

        let mut slow = RawConnection::open(&broker, "slow.test", Some(4096)).await;
        let session = slow.register().await;
        slow.subscribe(&session, "news").await;

        let publisher = TestClient::connect(&broker, "publisher.test").await;
        let count = 120u32; // 30 MiB, past the subscriber's high water of 8 MiB
        for n in 0..count {
            let mut payload = vec![0u8; MESSAGE];
            payload[..4].copy_from_slice(&n.to_be_bytes());
            publisher.publish("news", &payload);
        }

        // It reads a message every few milliseconds, and acknowledges each, as a client does.
        let mut received = Vec::new();
        while received.len() < count as usize {
            let delivery = slow
                .next_delivery("news", Duration::from_secs(10))
                .await
                .expect("the slow subscriber is given every message");
            received.push(u32::from_be_bytes(
                delivery.payload[..4].try_into().unwrap(),
            ));
            slow.acknowledge(delivery.delivery_id).await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            received,
            (0..count).collect::<Vec<_>>(),
            "everything, in order"
        );
        publisher
            .wait_for("every acknowledgement", Duration::from_secs(10), |c| {
                c.acknowledged("news") == count as usize
            })
            .await;
        let stats = budget.stats();
        assert_eq!(stats.slow_disconnections, 0, "{stats:?}");
        assert!(stats.peak <= BUDGET, "{stats:?}");
    })
    .await;
}

#[tokio::test]
async fn a_broker_whose_budget_is_below_one_frame_does_not_start_and_says_why() {
    let refused = cassini_broker::budget::Bounds::new(
        MAX_FRAME_BYTES / 2,
        cassini_broker::budget::DEFAULT_STALL,
    )
    .unwrap_err();
    assert!(refused.contains("below one frame"), "{refused}");
    assert!(refused.contains("CASSINI_MEMORY_BUDGET_BYTES"), "{refused}");
}
