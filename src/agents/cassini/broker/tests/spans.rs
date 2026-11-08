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

//! What a publish costs in tracing. Each span the broker opens for a message is below `info`, so the default filters
//! (`info` for the console, `warn` for traces) do not create it. At `info` the publish's span was created, exported and
//! entered in every actor that handled the message, 40% of the broker's CPU (docs/architecture/broker-baseline.md,
//! "What measuring it found", item 7). Its own test binary: the subscriber that records the spans is the process's.

mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::*;
use tracing::{Level, span};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;

/// The name and level of every span the broker opens while `recording` is set.
struct Recorder {
    recording: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<(&'static str, Level)>>>,
}

impl<S: tracing::Subscriber> Layer<S> for Recorder {
    fn on_new_span(&self, attrs: &span::Attributes<'_>, _id: &span::Id, _ctx: Context<'_, S>) {
        let meta = attrs.metadata();
        if self.recording.load(Ordering::SeqCst) && meta.target().starts_with("cassini_broker") {
            self.seen.lock().unwrap().push((meta.name(), *meta.level()));
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_publish_opens_no_span_at_info_or_above() {
    let recording = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::new()));
    // Every level, so a span is seen whatever its level.
    tracing_subscriber::registry()
        .with(Recorder {
            recording: recording.clone(),
            seen: seen.clone(),
        })
        .try_init()
        .expect("this binary's only subscriber");

    let on = recording.clone();
    with_broker(|broker| async move {
        let topic = "spans";
        let subscriber = TestClient::connect(&broker, "subscriber.test").await;
        subscriber.subscribe(topic);
        subscriber
            .wait_for("the subscription", Duration::from_secs(10), |c| {
                c.subscribed(topic)
            })
            .await;
        let publisher = TestClient::connect(&broker, "publisher.test").await;
        on.store(true, Ordering::SeqCst);
        for n in 0..10u64 {
            publisher.publish(topic, &n.to_be_bytes());
        }
        subscriber
            .wait_for("every message", Duration::from_secs(10), |c| {
                c.received_count(topic) >= 10
            })
            .await;
        on.store(false, Ordering::SeqCst);
    })
    .await;

    let seen = seen.lock().unwrap().clone();
    // The publishes went through the broker's spans, so the recorder saw them.
    assert!(
        seen.iter()
            .any(|(name, _)| *name == "cassini.request.publish_request"),
        "the publish's span was not seen: {seen:?}"
    );
    // A level at or above info is one a default filter admits (tracing orders error < warn < info < debug < trace).
    let admitted: Vec<_> = seen
        .iter()
        .filter(|(_, level)| *level <= Level::INFO)
        .collect();
    assert!(
        admitted.is_empty(),
        "spans a publish opens at info or above: {admitted:?}"
    );
}
