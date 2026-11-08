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

//! polar-healthcheck
//!
//! Kubernetes liveness probe binary for Polar agents. The logic is in
//! `polar_health::probe`; this only reads the environment, prints the report
//! and exits.
//!
//! A dead-man's switch, nothing more: it checks that the agent's health actor is
//! still writing its health file, and fails if the file is missing, unreadable or
//! older than the threshold. It does not judge health, connections or certificates;
//! the actor does, and acts on it in an orderly way. See `polar_health::probe`.
//!
//! Exit 0 = the file is fresh, Exit 1 = it is not (or the settings are malformed).
//!
//! Environment variables:
//!   POLAR_HEALTH_FILE         Path to agent health state JSON (default: /tmp/polar-health.json)
//!   POLAR_HEALTH_STALE_SECS   Fail if health file is older than N seconds (default: 90)
// Reading the process environment goes through polar-env (see clippy.toml).
#![deny(clippy::disallowed_methods)]

use polar_health::HealthState;
use polar_health::probe::{self, ProbeConfig};

fn main() {
    let config = match ProbeConfig::from_env() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    };
    let report = probe::run(&config, HealthState::now());
    for line in &report.lines {
        eprintln!("{line}");
    }
    std::process::exit(if report.ok { 0 } else { 1 });
}

// Every Polar binary allocates with Scudo (docs/architecture/broker-baseline.md, "Decided").
#[global_allocator]
static GLOBAL: polar_scudo::Scudo = polar_scudo::Scudo;
