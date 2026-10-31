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
/// A budget that what one subscriber holds back never exhausts: 256 MiB, with a high water of 32 MiB and a window of
/// 64 MiB a connection.
const ROOMY: usize = 4 * MAX_FRAME_BYTES;
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

/// A subscriber stops reading while a publisher sends 160 MiB in messages of `size` bytes: more than the subscriber's
/// high water and the publisher's window together, and less than the budget, so it is the subscriber's high water
/// that holds the topic back, not the budget's alarm.
async fn stops_reading(size: usize) {
    let stall = Duration::from_secs(4);
    with_broker_bounded(ROOMY, stall, |broker| async move {
        let budget = broker.budget();
        let high_water = broker.bounds().high_water;

        // Subscribes, then keeps its socket open and reads nothing.
        let mut silent = RawConnection::open(&broker, "silent.test", Some(4096)).await;
        let session = silent.register().await;
        silent.subscribe(&session, "news").await;

        let publisher = TestClient::connect(&broker, "publisher.test").await;
        let count = 160 * 1024 * 1024 / size;
        for n in 0..count {
            let mut payload = vec![0u8; size];
            payload[..4].copy_from_slice(&(n as u32).to_be_bytes());
            publisher.publish("news", &payload);
        }

        // The acknowledgements stop short of the end: the subscriber is above its high water, and its topic waits for
        // it, with the budget's alarm never raised.
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
        let owed = budget
            .holders()
            .into_iter()
            .find(|b| b.owner().is_some_and(|(_, topic)| topic == "news"))
            .expect("the subscriber's backlog");
        assert!(
            owed.holds_back() && owed.bytes() > high_water,
            "the subscriber is above its high water ({} of {high_water} bytes) and holds its topic back",
            owed.bytes()
        );
        let stats = budget.stats();
        assert_eq!(
            stats.alarms, 0,
            "the subscriber's high water held the topic back, not the budget: {stats:?}"
        );
        assert!(
            stats.peak >= high_water,
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
        assert_eq!(stats.alarms, 0, "{stats:?}");
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
    })
    .await;
}

/// What a subscriber is owed counts each message's whole frame, not only its payload: small messages on a long topic
/// to a subscriber that stops reading take it above its high water as their frames do, though their payloads come to a
/// small part of it (review finding 1).
#[tokio::test(flavor = "multi_thread")]
async fn what_a_subscriber_is_owed_counts_each_message_s_frame_not_only_its_payload() {
    let stall = Duration::from_secs(20);
    with_broker_bounded(BUDGET, stall, |broker| async move {
        let budget = broker.budget();
        let high_water = broker.bounds().high_water;
        // Each publish's frame is some 4 KiB, of which its payload is 16 bytes.
        let topic = "n".repeat(4096);
        let mut silent = RawConnection::open(&broker, "silent.test", Some(4096)).await;
        let session = silent.register().await;
        silent.subscribe(&session, &topic).await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        let count = 2 * high_water / 4096; // twice its high water in frames, an eighth of a MiB in payloads
        for n in 0..count {
            let mut payload = vec![0u8; 16];
            payload[..4].copy_from_slice(&(n as u32).to_be_bytes());
            publisher.publish(&topic, &payload);
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        let held_back = loop {
            let above = budget
                .holders()
                .iter()
                .any(|b| b.owner().is_some_and(|(_, t)| t == topic) && b.holds_back());
            if above || Instant::now() >= deadline {
                break above;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert!(
            held_back,
            "the subscriber's frames take it above its high water: {:?}",
            budget.largest_holders(3)
        );
        assert!(
            publisher.acknowledged(&topic) < count,
            "its topic waits for it"
        );
        assert_eq!(budget.stats().alarms, 0, "{:?}", budget.stats());
        drop(silent);
    })
    .await;
}

/// A subscriber that was away longer than the stall bound while what it is owed passed its high water is not cut when
/// it comes back and reads: the stall clock counts only time spent connected above high water.
#[tokio::test(flavor = "multi_thread")]
async fn a_subscriber_back_from_away_is_given_the_stall_bound_to_catch_up() {
    let stall = Duration::from_secs(2);
    with_broker_bounded(BUDGET, stall, |broker| async move {
        let budget = broker.budget();
        let mut away = RawConnection::open(&broker, "returning.test", None).await;
        let session = away.register().await;
        away.subscribe(&session, "news").await;
        drop(away);
        tokio::time::sleep(Duration::from_millis(300)).await;

        // While it is away, more than its high water (8 MiB) is held for it, and it stays away past the bound.
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        let held = 64u32; // 16 MiB
        for n in 0..held {
            let mut payload = vec![0u8; MESSAGE];
            payload[..4].copy_from_slice(&n.to_be_bytes());
            publisher.publish("news", &payload);
        }
        publisher
            .wait_for(
                "what is held for it accepted",
                Duration::from_secs(10),
                |c| c.acknowledged("news") == held as usize,
            )
            .await;
        tokio::time::sleep(stall + Duration::from_secs(1)).await;

        // It comes back, and more is published at once, so the topic is held back and looks for a stall.
        let mut back = RawConnection::open(&broker, "returning.test", None).await;
        assert_eq!(back.register_resuming(Some(session.clone())).await, session);
        let more = 8u32;
        for n in held..held + more {
            let mut payload = vec![0u8; MESSAGE];
            payload[..4].copy_from_slice(&n.to_be_bytes());
            publisher.publish("news", &payload);
        }
        let mut received = Vec::new();
        while received.len() < (held + more) as usize {
            let delivery = back
                .next_delivery("news", Duration::from_secs(10))
                .await
                .expect("it is given everything, and not cut");
            received.push(u32::from_be_bytes(
                delivery.payload[..4].try_into().unwrap(),
            ));
            back.acknowledge(delivery.delivery_id).await;
            // Steadily, and well within the bound: still above its high water when the topic first looks.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(received, (0..held + more).collect::<Vec<_>>());
        let stats = budget.stats();
        assert_eq!(stats.slow_disconnections, 0, "{stats:?}");
    })
    .await;
}

/// A client that both publishes and subscribes (a processor), sending more than the budget holds to a topic it reads
/// itself: its acknowledgements do not wait behind its publishes, so what it is owed is let go as it reads, and the
/// broker is not left stopped, whether or not the budget's alarm rose on the way
/// (docs/architecture/cassini-backpressure.md, review finding 1).
#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_publishes_and_subscribes_under_an_exhausted_budget_recovers() {
    let stall = Duration::from_secs(2);
    with_broker_bounded(BUDGET, stall, |broker| async move {
        let budget = broker.budget();
        let processor = TestClient::connect(&broker, "processor.test").await;
        processor.subscribe("loop");
        processor
            .wait_for("the subscription", Duration::from_secs(5), |c| {
                c.subscribed("loop")
            })
            .await;
        let count = 400; // 100 MiB, past the 64 MiB budget
        for n in 0..count {
            let mut payload = vec![0u8; MESSAGE];
            payload[..4].copy_from_slice(&(n as u32).to_be_bytes());
            processor.publish("loop", &payload);
        }
        // It is given everything it sent, and every publish is accepted...
        processor
            .wait_for("its own messages", Duration::from_secs(30), |c| {
                c.received_count("loop") == count
            })
            .await;
        processor
            .wait_for("every acknowledgement", Duration::from_secs(10), |c| {
                c.acknowledged("loop") == count
            })
            .await;
        // ... the budget is not left exhausted, and the broker serves a newcomer.
        let deadline = Instant::now() + stall * 6;
        while budget.is_exhausted() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let stats = budget.stats();
        assert!(
            !budget.is_exhausted(),
            "the budget is still exhausted after {:?}: {stats:?}",
            stall * 6
        );
        let probe = TestClient::connect(&broker, "probe.test").await;
        probe.subscribe("probe");
        probe
            .wait_for("the probe's subscription", Duration::from_secs(5), |c| {
                c.subscribed("probe")
            })
            .await;
        probe.publish("probe", b"still serving");
        probe
            .wait_for("the probe's own message", Duration::from_secs(10), |c| {
                c.received_count("probe") == 1
            })
            .await;
    })
    .await;
}

/// Subscribers that each stay below their high water but stop reading together exhaust the budget, with no topic held
/// back and so no stall seen by any topic: the broker ends their connections once they have made no progress for the
/// stall bound, lets go of what they held, and serves again, a publisher that also subscribes included (review
/// finding 1).
#[tokio::test(flavor = "multi_thread")]
async fn subscribers_below_their_high_water_that_stop_reading_do_not_stop_the_broker() {
    let stall = Duration::from_secs(2);
    with_broker_bounded(BUDGET, stall, |broker| async move {
        let budget = broker.budget();
        let topics = 9; // 9 x 7.5 MiB: past the 64 MiB budget, each under its 8 MiB high water
        let mut silent = Vec::new();
        for t in 0..topics {
            let mut connection =
                RawConnection::open(&broker, &format!("silent-{t}.test"), Some(4096)).await;
            let session = connection.register().await;
            connection.subscribe(&session, &format!("t{t}")).await;
            silent.push(connection);
        }
        // An agent's client: what is lost with a cut connection is sent again.
        let agent = support::FacadeClient::connect(&broker, "processor.test").await;
        let processor = &agent.client;
        processor.subscribe("loop");
        processor
            .wait_for("the subscription", Duration::from_secs(5), |c| {
                c.subscribed("loop")
            })
            .await;
        for n in 0..30u32 {
            for t in 0..topics {
                let mut payload = vec![0u8; MESSAGE];
                payload[..4].copy_from_slice(&n.to_be_bytes());
                processor.publish(&format!("t{t}"), &payload);
            }
            processor.publish("loop", &n.to_be_bytes());
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while budget.stats().alarms == 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(budget.stats().alarms > 0, "the budget was exhausted");
        let deadline = Instant::now() + stall * 6;
        while budget.is_exhausted() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let stats = budget.stats();
        assert!(!budget.is_exhausted(), "still exhausted: {stats:?}");
        assert!(stats.slow_disconnections >= 1, "{stats:?}");
        processor
            .wait_for("every publish acknowledged", Duration::from_secs(30), |c| {
                let distinct: std::collections::BTreeSet<_> =
                    c.acknowledged_ids().into_iter().collect();
                distinct.len() == 30 * (topics + 1)
            })
            .await;
        processor
            .wait_for("its own messages", Duration::from_secs(10), |c| {
                c.received_count("loop") >= 30
            })
            .await;
        // Its acknowledgements never waited behind its publishes: it was never taken for a stalled subscriber.
        assert_eq!(
            agent.registrations().len(),
            1,
            "the processor kept its connection"
        );
        drop(silent);
    })
    .await;
}

/// A client owed more than the budget holds, which publishes and then acknowledges what it was given: its
/// acknowledgements are read while its publish waits for the budget, and they make the room the publish waits for. The
/// broker does not wait for the stall bound to cut the client (review finding 1).
#[tokio::test(flavor = "multi_thread")]
async fn acknowledgements_behind_a_publish_that_waits_for_the_budget_are_read() {
    let stall = Duration::from_secs(6);
    with_broker_bounded(BUDGET, stall, |broker| async move {
        let budget = broker.budget();
        let topics = 9; // 9 x 7.5 MiB: past the 64 MiB budget, each under its 8 MiB high water
        let mut client = RawConnection::open(&broker, "processor.test", None).await;
        let session = client.register().await;
        for t in 0..topics {
            client.subscribe(&session, &format!("t{t}")).await;
        }
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        for n in 0..30u32 {
            for t in 0..topics {
                let mut payload = vec![0u8; MESSAGE];
                payload[..4].copy_from_slice(&n.to_be_bytes());
                publisher.publish(&format!("t{t}"), &payload);
            }
        }

        // It reads what it is given and holds on to it, until the budget is exhausted.
        let mut owed = Vec::new();
        let began = Instant::now();
        while budget.stats().alarms == 0 && began.elapsed() < Duration::from_secs(10) {
            if let Some(cassini_types::ClientMessage::PublishResponse {
                delivery_id: Some(delivery_id),
                ..
            }) = tokio::time::timeout(Duration::from_millis(200), client.read())
                .await
                .ok()
                .flatten()
            {
                owed.push(delivery_id);
            }
        }
        assert!(budget.stats().alarms > 0, "the budget was exhausted");

        // It publishes, and then acknowledges everything it was given, in that order on its one connection.
        let asked = Instant::now();
        client
            .send(&cassini_types::ClientMessage::PublishRequest {
                topic: "own".to_string(),
                payload: std::sync::Arc::new(b"after".to_vec()),
                registration_id: session.clone(),
                trace_ctx: None,
                share_identity: false,
                epoch: 1,
                seq: 1,
            })
            .await;
        for delivery_id in &owed {
            client.acknowledge(*delivery_id).await;
        }
        let mut answered = false;
        while !answered && asked.elapsed() < stall {
            match tokio::time::timeout(Duration::from_millis(200), client.read()).await {
                Ok(Some(cassini_types::ClientMessage::PublishRequestAck { topic, .. }))
                    if topic == "own" =>
                {
                    answered = true;
                }
                Ok(None) => break,
                _ => {}
            }
        }
        assert!(
            answered,
            "its publish is accepted once its acknowledgements make room ({} owed), after {:?}: {:?}",
            owed.len(),
            asked.elapsed(),
            budget.stats()
        );
        assert!(
            asked.elapsed() < stall / 2,
            "well within the stall bound, not by it: {:?}",
            asked.elapsed()
        );
        assert_eq!(budget.stats().slow_disconnections, 0, "{:?}", budget.stats());
    })
    .await;
}

/// A subscriber away when the budget is exhausted lets go of what it was owed, all of it as it was counted (its
/// messages' frames, not only their payloads): back, it owes nothing once it has acknowledged what it is given
/// (review finding 1).
#[tokio::test(flavor = "multi_thread")]
async fn a_subscriber_away_when_the_budget_is_exhausted_lets_go_of_all_it_was_owed() {
    let stall = Duration::from_secs(20);
    with_broker_bounded(BUDGET, stall, |broker| async move {
        let budget = broker.budget();
        let topic = "n".repeat(4096);
        let mut away = RawConnection::open(&broker, "away.test", None).await;
        let session = away.register().await;
        away.subscribe(&session, &topic).await;
        drop(away);
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Held for it while it is away, past the budget: each message is 60 KiB of payload in a frame some 4 KiB
        // longer.
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        let count = BUDGET / (60 * 1024) + 64;
        for n in 0..count {
            let mut payload = vec![0u8; 60 * 1024];
            payload[..4].copy_from_slice(&(n as u32).to_be_bytes());
            publisher.publish(&topic, &payload);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while budget.stats().alarms == 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(budget.stats().alarms > 0, "the budget was exhausted");
        settled(Duration::from_secs(10), Duration::from_millis(500), || {
            publisher.acknowledged(&topic)
        })
        .await;

        // Back, it reads and acknowledges what it is given.
        let mut back = RawConnection::open(&broker, "away.test", None).await;
        assert_eq!(back.register_resuming(Some(session.clone())).await, session);
        while let Some(delivery) = back.next_delivery(&topic, Duration::from_secs(1)).await {
            back.acknowledge(delivery.delivery_id).await;
        }
        let owes = || {
            budget
                .holders()
                .into_iter()
                .find(|b| b.owner().is_some_and(|(_, t)| t == topic))
                .map(|b| b.bytes())
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while owes() != Some(0) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(
            owes(),
            Some(0),
            "it owes nothing once it has acknowledged all it was given: {:?}",
            budget.stats()
        );
    })
    .await;
}

/// A processor publishing small frames back to back to the topics it subscribes to, under a budget exhausted by
/// what it and silent subscribers hold, is not cut and loses nothing (second review, finding A).
#[tokio::test(flavor = "multi_thread")]
async fn a_processor_publishing_small_frames_back_to_back_under_an_exhausted_budget_is_not_cut() {
    processor_under_exhaustion(60 * 1024, 130).await;
}

/// As above, with frames too large to read before they are admitted (finding A).
#[tokio::test(flavor = "multi_thread")]
async fn a_processor_publishing_large_frames_under_an_exhausted_budget_is_not_cut() {
    processor_under_exhaustion(MESSAGE, 30).await;
}

/// A processor (an agent's client that subscribes to the topics it publishes to, and reads and acknowledges what it
/// is given) publishes `rounds` messages of `size` bytes to each of nine topics, while a slow subscriber on each reads
/// and acknowledges them at half a MiB a second: together they exhaust the budget for longer than the stall bound
/// (each topic under its high water, and every subscriber making progress), and the processor's own deliveries, the
/// same messages, are among what holds it, its acknowledgements behind its publishes on its one connection. No one is
/// cut, and the processor is given everything it published.
async fn processor_under_exhaustion(size: usize, rounds: u32) {
    let stall = Duration::from_secs(2);
    with_broker_bounded(BUDGET, stall, move |broker| async move {
        let budget = broker.budget();
        let topics = 9;
        let pace = Duration::from_micros(size as u64 * 1_000_000 / (512 * 1024));
        let mut slow = Vec::new();
        for t in 0..topics {
            let mut connection =
                RawConnection::open(&broker, &format!("slow-{t}.test"), Some(4096)).await;
            let session = connection.register().await;
            let topic = format!("t{t}");
            connection.subscribe(&session, &topic).await;
            slow.push(tokio::spawn(async move {
                let mut read = 0;
                while let Some(delivery) = connection
                    .next_delivery(&topic, Duration::from_secs(60))
                    .await
                {
                    connection.acknowledge(delivery.delivery_id).await;
                    read += 1;
                    if read == rounds {
                        break;
                    }
                    tokio::time::sleep(pace).await;
                }
                read
            }));
        }
        let agent = support::FacadeClient::connect(&broker, "processor.test").await;
        let processor = &agent.client;
        for t in 0..topics {
            processor.subscribe(&format!("t{t}"));
        }
        processor
            .wait_for("its subscriptions", Duration::from_secs(5), |c| {
                (0..topics).all(|t| c.subscribed(&format!("t{t}")))
            })
            .await;
        for n in 0..rounds {
            for t in 0..topics {
                let mut payload = vec![0u8; size];
                payload[..4].copy_from_slice(&n.to_be_bytes());
                processor.publish(&format!("t{t}"), &payload);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while budget.stats().alarms == 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(budget.stats().alarms > 0, "the budget was exhausted");
        let distinct = |c: &TestClient, t: usize| {
            c.received(&format!("t{t}"))
                .iter()
                .map(|p| u32::from_be_bytes(p[..4].try_into().unwrap()))
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        };
        processor
            .wait_for("everything it published", Duration::from_secs(60), |c| {
                (0..topics).all(|t| distinct(c, t) == rounds as usize)
            })
            .await;
        processor
            .wait_for("every publish accepted", Duration::from_secs(30), |c| {
                let ids: std::collections::BTreeSet<_> = c.acknowledged_ids().into_iter().collect();
                ids.len() == rounds as usize * topics
            })
            .await;
        let stats = budget.stats();
        assert_eq!(
            agent.registrations().len(),
            1,
            "the processor kept its connection: {stats:?}"
        );
        for subscriber in slow {
            assert_eq!(
                subscriber.await.unwrap(),
                rounds,
                "each slow subscriber is given all"
            );
        }
        assert_eq!(
            budget.stats().slow_disconnections,
            0,
            "no one was cut: {stats:?}"
        );
    })
    .await;
}

/// A processor publishing to the topic it reads faster than the broker takes it, under a budget it cannot exhaust: it
/// writes 160 MiB of publishes before it reads, so its own backlog on the topic goes above its high water, the topic
/// is held back, its window fills and its reader waits; then it reads, and its acknowledgements are written behind
/// its blocked publishes. The topic's stall rule refuses its publishes for now rather than end its connection: the
/// broker reads on, reaches its acknowledgements, and the topic moves (second review, finding A).
#[tokio::test(flavor = "multi_thread")]
async fn a_processor_holding_back_its_own_topic_is_not_cut_for_acknowledgements_the_broker_has_not_read()
 {
    use cassini_types::ClientMessage;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let stall = Duration::from_secs(2);
    with_broker_bounded(ROOMY, stall, |broker| async move {
        let budget = broker.budget();
        let mut connection = RawConnection::open(&broker, "processor.test", Some(4096)).await;
        let session = connection.register().await;
        connection.subscribe(&session, "loop").await;
        let (mut reading, mut writing) = connection.into_split();
        let count = 160 * 1024 * 1024 / MESSAGE as u64; // its high water (32 MiB) and its window (64 MiB) over
        let (acks, mut to_ack) = tokio::sync::mpsc::unbounded_channel::<u64>();
        let writer = tokio::spawn(async move {
            for seq in 1..=count {
                let frame = cassini_client::TcpClientActor::frame(&ClientMessage::PublishRequest {
                    topic: "loop".to_string(),
                    payload: std::sync::Arc::new(vec![0u8; MESSAGE]),
                    registration_id: session.clone(),
                    trace_ctx: None,
                    share_identity: false,
                    epoch: 1,
                    seq,
                })
                .unwrap();
                if writing.write_all(&frame).await.is_err() {
                    return;
                }
            }
            let _ = writing.flush().await;
            while let Some(delivery_id) = to_ack.recv().await {
                let frame = cassini_client::TcpClientActor::frame(&ClientMessage::PublishResponseAck {
                    delivery_id,
                })
                .unwrap();
                if writing.write_all(&frame).await.is_err() || writing.flush().await.is_err() {
                    return;
                }
            }
        });
        // It reads nothing at first, while its backlog passes its high water.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let (mut accepted, mut deferred, mut delivered) = (0u64, 0u64, 0u64);
        let ended = loop {
            if accepted + deferred == count && delivered >= accepted {
                break false;
            }
            let read = tokio::time::timeout(Duration::from_secs(20), async {
                let len = reading.read_u32().await.ok()? as usize;
                let mut body = vec![0u8; len];
                reading.read_exact(&mut body).await.ok()?;
                Some(cassini_client::TcpClientActor::unframe(&body).unwrap())
            })
            .await
            .expect("the broker goes on");
            match read {
                None => break true,
                Some(ClientMessage::PublishRequestAck { .. }) => accepted += 1,
                Some(ClientMessage::PublishResponse {
                    result: Err(_),
                    refusal: Some(cassini_types::PublishRefusal::Unavailable),
                    ..
                }) => deferred += 1,
                Some(ClientMessage::PublishResponse {
                    delivery_id: Some(delivery_id),
                    result: Ok(()),
                    ..
                }) => {
                    delivered += 1;
                    let _ = acks.send(delivery_id);
                }
                Some(_) => {}
            }
        };
        let stats = budget.stats();
        assert!(
            !ended,
            "its connection is not ended ({accepted} accepted, {deferred} deferred, {delivered} given back): {stats:?}"
        );
        assert!(deferred > 0, "some were refused for now: {stats:?}");
        assert_eq!(stats.slow_disconnections, 0, "{stats:?}");
        assert_eq!(stats.alarms, 0, "the budget was never exhausted: {stats:?}");
        drop(acks);
        writer.abort();
    })
    .await;
}

/// The bound on sparing: a client that publishes to the topic it subscribes to, and reads what it is given but never
/// acknowledges it, holds the topic back while its reader waits: it is spared once, its publishes refused for now, and
/// then, having acknowledged nothing since, has its connection ended as any stalled subscriber does (second review,
/// finding A).
#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_publishes_and_never_acknowledges_is_spared_once_and_then_ended() {
    use cassini_types::ClientMessage;
    use tokio::io::AsyncWriteExt;
    let stall = Duration::from_secs(2);
    with_broker_bounded(ROOMY, stall, |broker| async move {
        let budget = broker.budget();
        let mut connection = RawConnection::open(&broker, "deaf.test", Some(4096)).await;
        let session = connection.register().await;
        connection.subscribe(&session, "loop").await;
        let (mut reading, mut writing) = connection.into_split();
        let began = Instant::now();
        let reader = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut sink = vec![0u8; 64 * 1024];
            while let Ok(n) = reading.read(&mut sink).await {
                if n == 0 {
                    break;
                }
            }
        });
        let writer = tokio::spawn(async move {
            for seq in 1.. {
                let frame = cassini_client::TcpClientActor::frame(&ClientMessage::PublishRequest {
                    topic: "loop".to_string(),
                    payload: std::sync::Arc::new(vec![0u8; MESSAGE]),
                    registration_id: session.clone(),
                    trace_ctx: None,
                    share_identity: false,
                    epoch: 1,
                    seq,
                })
                .unwrap();
                if writing.write_all(&frame).await.is_err() {
                    return;
                }
            }
        });
        let ended = tokio::time::timeout(Duration::from_secs(30), writer).await;
        reader.abort();
        let stats = budget.stats();
        assert!(ended.is_ok(), "its connection is ended: {stats:?}");
        assert!(stats.slow_disconnections >= 1, "{stats:?}");
        assert!(
            began.elapsed() > stall,
            "after the stall bound, not before: {:?}",
            began.elapsed()
        );
    })
    .await;
}

/// A client acknowledging steadily what it is given on one topic is making progress, though a delivery on another
/// topic waits behind those writes for longer than the stall bound: progress is its connection's, not one topic's
/// (second review, finding B).
#[tokio::test(flavor = "multi_thread")]
async fn a_client_acknowledging_one_topic_is_not_cut_for_a_delivery_waiting_behind_it_on_another() {
    let stall = Duration::from_secs(2);
    with_broker_bounded(BUDGET, stall, |broker| async move {
        let budget = broker.budget();
        let mut steady = RawConnection::open(&broker, "steady.test", Some(4096)).await;
        let session = steady.register().await;
        steady.subscribe(&session, "a").await;
        steady.subscribe(&session, "b").await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        // Just under its high water on "a", and then one message on "b", written after them.
        let on_a = 28;
        for n in 0..on_a {
            let mut payload = vec![0u8; MESSAGE];
            payload[..4].copy_from_slice(&(n as u32).to_be_bytes());
            publisher.publish("a", &payload);
        }
        publisher
            .wait_for("those on a accepted", Duration::from_secs(10), |c| {
                c.acknowledged("a") == on_a
            })
            .await;
        publisher.publish("b", b"behind");
        publisher
            .wait_for("the one on b accepted", Duration::from_secs(10), |c| {
                c.acknowledged("b") == 1
            })
            .await;
        // Silent subscribers then exhaust the budget.
        let topics = 9;
        let mut silent = Vec::new();
        for t in 0..topics {
            let mut connection =
                RawConnection::open(&broker, &format!("silent-{t}.test"), Some(4096)).await;
            let session = connection.register().await;
            connection.subscribe(&session, &format!("s{t}")).await;
            silent.push(connection);
        }
        for n in 0..30u32 {
            for t in 0..topics {
                let mut payload = vec![0u8; MESSAGE];
                payload[..4].copy_from_slice(&n.to_be_bytes());
                publisher.publish(&format!("s{t}"), &payload);
            }
        }
        // It reads and acknowledges steadily; the one on "b" comes last, past the stall bound.
        let began = Instant::now();
        let (mut a, mut b) = (0, false);
        while a < on_a || !b {
            let message = tokio::time::timeout(Duration::from_secs(10), steady.read())
                .await
                .expect("it is given what it is owed")
                .unwrap_or_else(|| {
                    panic!(
                        "its connection is not ended (after {a} on a, {:?}): {:?}",
                        began.elapsed(),
                        budget.stats()
                    )
                });
            if let cassini_types::ClientMessage::PublishResponse {
                topic,
                delivery_id: Some(delivery_id),
                result: Ok(()),
                ..
            } = message
            {
                match topic.as_str() {
                    "a" => a += 1,
                    "b" => b = true,
                    _ => {}
                }
                steady.acknowledge(delivery_id).await;
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
        }
        assert!(
            began.elapsed() > stall,
            "the one on b waited past the stall bound: {:?}",
            began.elapsed()
        );
        assert!(budget.stats().alarms > 0, "while the budget was exhausted");
        drop(silent);
    })
    .await;
}

/// A delivery its client cannot decode is let go when the client names the frame it came in: it is not held, nor
/// sent again on resume, for a client that will never decode it (second review, finding B).
#[tokio::test(flavor = "multi_thread")]
async fn a_delivery_the_client_could_not_decode_is_let_go_when_it_says_so() {
    with_broker_bounded(BUDGET, Duration::from_secs(10), |broker| async move {
        let budget = broker.budget();
        let mut client = RawConnection::open(&broker, "poisoned.test", None).await;
        let session = client.register().await;
        client.subscribe(&session, "news").await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        publisher.publish("news", b"undecodable");
        publisher.publish("news", b"fine");
        let first = client
            .next_delivery("news", Duration::from_secs(5))
            .await
            .expect("the first");
        // The client could not decode the frame it came in, and says which; the next it acknowledges.
        client
            .send(&cassini_types::ClientMessage::FrameUndecodable {
                frame: client.frames,
            })
            .await;
        let second = client
            .next_delivery("news", Duration::from_secs(5))
            .await
            .expect("the second");
        client.acknowledge(second.delivery_id).await;
        let owes = || {
            budget
                .holders()
                .into_iter()
                .find(|b| b.owner().is_some_and(|(_, t)| t == "news"))
                .map(|b| b.bytes())
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while owes() != Some(0) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(owes(), Some(0), "it owes nothing");
        drop(client);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut back = RawConnection::open(&broker, "poisoned.test", None).await;
        assert_eq!(back.register_resuming(Some(session.clone())).await, session);
        let replayed = back.next_delivery("news", Duration::from_secs(1)).await;
        assert_eq!(
            replayed.map(|d| d.delivery_id),
            None,
            "nothing is sent again (the first was {})",
            first.delivery_id
        );
    })
    .await;
}

/// The pressure manager is the broker's child: one that ends while the broker runs is started again by it, and the
/// stall checks go on (the maintainer's review: no policy outside the supervision tree).
#[tokio::test(flavor = "multi_thread")]
async fn the_pressure_manager_is_started_again_when_it_ends_and_the_checks_go_on() {
    let stall = Duration::from_secs(2);
    with_broker_bounded(BUDGET, stall, |broker| async move {
        let budget = broker.budget();
        let first = ractor::registry::where_is(cassini_broker::PRESSURE_MANAGER_NAME.to_string())
            .expect("the broker runs its pressure manager");
        let first_id = first.get_id();
        first.kill();
        let deadline = Instant::now() + Duration::from_secs(5);
        let again = loop {
            if let Some(cell) =
                ractor::registry::where_is(cassini_broker::PRESSURE_MANAGER_NAME.to_string())
                && cell.get_id() != first_id
            {
                break Some(cell);
            }
            if Instant::now() >= deadline {
                break None;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert!(again.is_some(), "a new pressure manager is started");
        // Its checks go on: a subscriber that stops reading is ended after the stall bound, as before.
        let mut silent = RawConnection::open(&broker, "silent.test", Some(4096)).await;
        let session = silent.register().await;
        silent.subscribe(&session, "news").await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        for n in 0..64u32 {
            let mut payload = vec![0u8; MESSAGE];
            payload[..4].copy_from_slice(&n.to_be_bytes());
            publisher.publish("news", &payload);
        }
        assert!(
            silent.closed_within(stall * 4).await,
            "the new manager's checks end a stalled subscriber: {:?}",
            budget.stats()
        );
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
