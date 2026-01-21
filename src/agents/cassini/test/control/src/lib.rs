use clap::Parser;
use harness_common::{Expectation, MessagePattern, ProducerConfig};
use ractor::ActorRef;
use rkyv::{Archive, Deserialize, Serialize};
use std::collections::HashSet;
use std::process::Stdio;
use tokio::{process::Command, task::AbortHandle, time::Instant};
pub mod service;

/// A single phase of the test.
/// Phases are time-bounded, not event-gated.
#[derive(Debug, Clone, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct Phase {
    pub name: String,
    pub producers: Vec<ProducerConfig>,
    /// Expectations evaluated by sinks during this phase.
    pub expectations: Vec<Expectation>,
}

/// Entire test plan executed by the harness controller.
/// Phases are executed sequentially.
#[derive(Debug, Clone, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct TestPlan {
    pub name: String,
    pub phases: Vec<Phase>,
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

pub fn init_logging() {
    let dir = tracing_subscriber::filter::Directive::from(tracing::Level::TRACE);

    use std::io::stderr;
    use std::io::IsTerminal;
    use tracing_glog::Glog;
    use tracing_glog::GlogFields;
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::Registry;

    let fmt = tracing_subscriber::fmt::Layer::default()
        .with_ansi(stderr().is_terminal())
        .with_writer(std::io::stderr)
        .event_format(Glog::default().with_timer(tracing_glog::LocalTime::default()))
        .fmt_fields(GlogFields::default().compact());

    let filter = vec![dir]
        .into_iter()
        .fold(EnvFilter::from_default_env(), |filter, directive| {
            filter.add_directive(directive)
        });

    let subscriber = Registry::default().with(filter).with(fmt);
    tracing::subscriber::set_global_default(subscriber).expect("to set global subscriber");
}

pub fn read_test_config(path: &str) -> TestPlan {
    let dhall_str =
        std::fs::read_to_string(path).expect("Expected to find dhall configuration at {path}");

    let config: TestPlan = serde_dhall::from_str(&dhall_str).parse().unwrap();

    return config;
}
