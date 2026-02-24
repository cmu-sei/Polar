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

    return config;
}
