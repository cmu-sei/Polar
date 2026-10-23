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

//! A listener that fails outside a shutdown ends cert-issuer (docs/architecture/cert-issuer-isolation.md, "1b in
//! detail"): its supervisor sees the failure as it sees any child's and asks for the shutdown, so a CA front that serves
//! nothing cannot look healthy. Run whole, in its own process.

mod agent_rig;
mod helpers;
use agent_rig::*;
use cert_issuer::listener::ListenerMessage;
use std::time::Duration;

#[test]
fn a_listener_that_fails_outside_a_shutdown_ends_cert_issuer_and_run_returns_non_zero() {
    let rig = Rig::start(&[]);
    assert!(rig.first("/ca").starts_with("HTTP/1.1 200"));

    // Fail it as a dead server would: its serving ended while no drain was asked for.
    let listener: ractor::ActorRef<ListenerMessage> =
        ractor::registry::where_is("cert-issuer.listener".to_string())
            .expect("the listener is registered by its name")
            .into();
    listener
        .send_message(ListenerMessage::Ended("injected by the test".to_string()))
        .unwrap();

    let status = rig
        .ended
        .recv_timeout(Duration::from_secs(30))
        .expect("cert-issuer ended within the shutdown budget, without the watchdog");
    assert_eq!(status, 1);
}
