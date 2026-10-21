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

//! Every client delivers its events (messages, acknowledgements, refusals) through ractor `OutputPort`s, as do the
//! builder's agents. ractor's first port kept ten messages per subscriber, and a subscriber that fell further behind was
//! not slowed or sent fewer: its forwarding task ended, and it heard nothing on that port again. Under load a producer
//! stopped hearing its acknowledgements and a subscriber its messages, silently (docs/architecture/policy-enforcement.md,
//! stage 5a). The workspace uses ractor's `output-port-v2`, whose subscribers are never dropped for falling behind.

use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Takes a millisecond over each message, so a burst leaves it far behind.
struct Slow;

#[ractor::async_trait]
impl Actor for Slow {
    type Msg = u32;
    type State = Arc<AtomicUsize>;
    type Arguments = Arc<AtomicUsize>;

    async fn pre_start(
        &self,
        _myself: ActorRef<u32>,
        seen: Arc<AtomicUsize>,
    ) -> Result<Arc<AtomicUsize>, ActorProcessingErr> {
        Ok(seen)
    }

    async fn handle(
        &self,
        _myself: ActorRef<u32>,
        _message: u32,
        seen: &mut Arc<AtomicUsize>,
    ) -> Result<(), ActorProcessingErr> {
        tokio::time::sleep(Duration::from_millis(1)).await;
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn a_subscriber_that_falls_behind_a_burst_still_hears_every_message() {
    const BURST: u32 = 500;
    let port: OutputPort<u32> = OutputPort::default();
    let seen = Arc::new(AtomicUsize::new(0));
    let (slow, _) = Actor::spawn(None, Slow, seen.clone()).await.unwrap();
    port.subscribe(slow, Some);
    tokio::time::sleep(Duration::from_millis(50)).await;
    for n in 0..BURST {
        port.send(n);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while seen.load(Ordering::SeqCst) < BURST as usize && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        seen.load(Ordering::SeqCst),
        BURST as usize,
        "the subscriber was dropped for falling behind"
    );
    // And it still hears what comes after.
    port.send(BURST);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(seen.load(Ordering::SeqCst), BURST as usize + 1);
}
