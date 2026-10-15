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

use clap::Parser;
use harness_common::ProducerConfig;
use rkyv::{Archive, Deserialize, Serialize};
pub mod service;

/// A single phase of the test.
/// Phases are time-bounded, not event-gated.
#[derive(Debug, Clone, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct Test {
    pub name: String,
    pub producer: ProducerConfig,
}

/// Entire test plan executed by the harness controller.
/// Phases are executed sequentially.
#[derive(Debug, Clone, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct TestPlan {
    pub name: String,
    pub tests: Vec<Test>,
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Arguments {
    #[arg(short, long)]
    /// Path to a valid .dhall configuration file. It will be evaluated to generate a TestPlan
    pub config: String,
    #[arg(long, default_value = "./target/debug/cassini-server")]
    pub broker_path: String,
    #[arg(long, default_value = "./target/debug/harness-producer")]
    pub producer_path: String,
    #[arg(long, default_value = "./target/debug/harness-sink")]
    pub sink_path: String,
}

pub fn read_test_config(path: &str) -> TestPlan {
    let dhall_str =
        std::fs::read_to_string(path).expect("Expected to find dhall configuration at {path}");

    let config: TestPlan = serde_dhall::from_str(&dhall_str).parse().unwrap();

    config
}
