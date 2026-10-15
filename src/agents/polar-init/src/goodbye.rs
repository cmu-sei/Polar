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

//! Leaving the broker cleanly.
//!
//! polar-init connects to Cassini with the same identity the workload will use, to bootstrap the
//! agent's escrow key. When it is done it must say goodbye, so the broker ends that session at
//! once. Closing the connection without a word looks to the broker like a client that may come
//! back, and it keeps the session, and what it holds for it, for ten minutes: state that belongs
//! to the workload that follows.

use cassini_client::TcpClientMessage;
use ractor::{ActorRef, ActorStatus};
use std::time::Duration;

/// Tells the broker this client is leaving and waits, up to `within`, for the transport to finish
/// saying so. Returns whether it did. A client that does not manage it is no worse off than one
/// that never tried: the broker keeps the session for its retention window.
pub async fn say_goodbye(client: &ActorRef<TcpClientMessage>, within: Duration) -> bool {
    if client
        .cast(TcpClientMessage::Disconnect { trace_ctx: None })
        .is_err()
    {
        return false;
    }
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if matches!(
            client.get_status(),
            ActorStatus::Stopping | ActorStatus::Stopped
        ) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use ractor::{Actor, ActorProcessingErr, async_trait};
    use std::sync::{Arc, Mutex};

    /// A transport that records what it is sent and, like the real one, stops itself on a
    /// disconnect (or, when told to ignore it, does not).
    struct Transport {
        received: Arc<Mutex<Vec<String>>>,
        stops_on_disconnect: bool,
    }

    #[async_trait]
    impl Actor for Transport {
        type Msg = TcpClientMessage;
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
            myself: ActorRef<Self::Msg>,
            message: Self::Msg,
            _state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let TcpClientMessage::Disconnect { .. } = message {
                self.received.lock().unwrap().push("disconnect".to_string());
                if self.stops_on_disconnect {
                    myself.stop(Some("disconnected".to_string()));
                }
            }
            Ok(())
        }
    }

    async fn transport(
        stops_on_disconnect: bool,
    ) -> (ActorRef<TcpClientMessage>, Arc<Mutex<Vec<String>>>) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (actor, _) = Actor::spawn(
            None,
            Transport {
                received: received.clone(),
                stops_on_disconnect,
            },
            (),
        )
        .await
        .unwrap();
        (actor, received)
    }

    #[tokio::test]
    async fn the_goodbye_is_sent_and_the_transport_is_given_time_to_finish_it() {
        let (client, received) = transport(true).await;
        assert!(say_goodbye(&client, Duration::from_secs(2)).await);
        assert_eq!(*received.lock().unwrap(), ["disconnect"]);
    }

    #[tokio::test]
    async fn a_transport_that_does_not_finish_is_not_waited_for_forever() {
        let (client, received) = transport(false).await;
        let started = std::time::Instant::now();
        assert!(!say_goodbye(&client, Duration::from_millis(300)).await);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            *received.lock().unwrap(),
            ["disconnect"],
            "it was still sent"
        );
    }

    #[tokio::test]
    async fn a_transport_that_is_already_gone_is_not_an_error() {
        let (client, _) = transport(true).await;
        client.stop(None);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!say_goodbye(&client, Duration::from_millis(200)).await);
    }
}
