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

//! Telling that the other end of a connection is gone.
//!
//! A connection can die without either end hearing about it: a node loses power, a NAT drops its
//! entry, a pod is killed hard. Nothing is sent, so nothing on the other end fails, and a
//! reader waits forever on a connection that will never carry another byte. Two kernel
//! mechanisms cover the two ways it shows:
//!
//! - **Keepalive** probes a connection that has gone quiet, and closes it when the peer stops
//!   answering. It finds a dead peer on an idle connection.
//! - **`TCP_USER_TIMEOUT`** closes a connection whose sent data has gone unacknowledged for too
//!   long. Without it, a connection with data in flight to a dead peer waits out the kernel's
//!   retransmission limit, which is about a quarter of an hour.
//!
//! Both ends of every Cassini connection set them, so a dead peer is found within about a minute
//! and the read or write fails like any other connection error, which the code already handles.
//!
//! `TCP_USER_TIMEOUT` is a Linux option (socket2 offers it on Linux, Android, Fuchsia and
//! Cygwin). Elsewhere, such as a macOS host building and running the code for development, only
//! keepalive is set. The images run on Linux, so a deployment gets both.

use socket2::{SockRef, TcpKeepalive};
use std::os::fd::AsFd;
use std::time::Duration;

/// How long a connection is idle before the first probe.
pub const KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
/// How often an unanswered probe is repeated.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
/// How many unanswered probes mean the peer is gone.
pub const KEEPALIVE_PROBES: u32 = 3;
/// How long sent data may go unacknowledged before the connection is closed. Idle time plus
/// probes (30 + 3 x 10 seconds) is the same order, so both mechanisms find a dead peer in about
/// the same time.
pub const USER_TIMEOUT: Duration = Duration::from_secs(60);

/// Makes the kernel find a dead peer on `stream`, in about a minute, whether the connection is
/// idle or has data in flight (on an idle one only, where `TCP_USER_TIMEOUT` does not exist).
pub fn enable_dead_peer_detection(stream: &impl AsFd) -> std::io::Result<()> {
    let socket = SockRef::from(stream);
    socket.set_tcp_keepalive(
        &TcpKeepalive::new()
            .with_time(KEEPALIVE_IDLE)
            .with_interval(KEEPALIVE_INTERVAL)
            .with_retries(KEEPALIVE_PROBES),
    )?;
    #[cfg(any(
        target_os = "android",
        target_os = "fuchsia",
        target_os = "linux",
        target_os = "cygwin"
    ))]
    socket.set_tcp_user_timeout(Some(USER_TIMEOUT))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    fn connected() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    #[test]
    fn a_connection_gets_keepalive_with_these_timings() {
        let (client, _server) = connected();
        enable_dead_peer_detection(&client).unwrap();
        let socket = SockRef::from(&client);
        assert!(socket.keepalive().unwrap());
        assert_eq!(socket.keepalive_time().unwrap(), KEEPALIVE_IDLE);
        assert_eq!(socket.keepalive_interval().unwrap(), KEEPALIVE_INTERVAL);
        assert_eq!(socket.keepalive_retries().unwrap(), KEEPALIVE_PROBES);
    }

    #[test]
    #[cfg(any(
        target_os = "android",
        target_os = "fuchsia",
        target_os = "linux",
        target_os = "cygwin"
    ))]
    fn sent_data_that_is_never_acknowledged_closes_the_connection() {
        let (client, _server) = connected();
        enable_dead_peer_detection(&client).unwrap();
        assert_eq!(
            SockRef::from(&client).tcp_user_timeout().unwrap(),
            Some(USER_TIMEOUT)
        );
    }

    #[test]
    fn the_settings_are_on_a_connection_that_has_not_been_touched_otherwise() {
        // The accepting end gets them too: a server finds a dead client the same way.
        let (_client, server) = connected();
        assert!(!SockRef::from(&server).keepalive().unwrap());
        enable_dead_peer_detection(&server).unwrap();
        assert!(SockRef::from(&server).keepalive().unwrap());
    }

    #[test]
    fn the_time_to_find_a_dead_peer_is_about_a_minute_either_way() {
        let probing = KEEPALIVE_IDLE + KEEPALIVE_INTERVAL * KEEPALIVE_PROBES;
        assert_eq!(probing, Duration::from_secs(60));
        assert_eq!(probing, USER_TIMEOUT);
    }
}
