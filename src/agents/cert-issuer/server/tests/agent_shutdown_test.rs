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

//! cert-issuer's end, run whole (docs/architecture/cert-issuer-isolation.md, "1b in detail"): a health verdict ends it
//! through the builder's handshake, its listener drained, and `run` returns non-zero so its Job replaces it.

mod agent_rig;
mod helpers;
use agent_rig::*;
use std::time::Duration;

#[test]
fn a_health_verdict_ends_cert_issuer_through_the_drain_and_run_returns_non_zero() {
    // Its one-hour certificate is under a two-hour expiry floor, so the first tick is a verdict.
    let rig = Rig::start(&[
        ("POLAR_HEALTH_EXPIRY_SECS", "7200"),
        ("POLAR_HEALTH_REJUVENATION_SECS", "7400"),
    ]);
    let status = rig
        .ended
        .recv_timeout(Duration::from_secs(30))
        .expect("cert-issuer ended within the shutdown budget, without the watchdog");
    assert_eq!(
        status, 1,
        "an agent that ends has failed, so its Job replaces it"
    );
    assert!(
        rig.get("/ca").is_err(),
        "its listener was drained: nothing listens"
    );
}
