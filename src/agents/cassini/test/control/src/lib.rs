use cassini_types::{SessionDetails, SessionMap};
use clap::Parser;
use harness_common::ProducerConfig;
use ractor::ActorRef;
use rkyv::{Archive, Deserialize, Serialize};
use std::collections::HashSet;
use std::process::Stdio;
use tokio::{process::Command, task::AbortHandle, time::Instant};
pub mod service;

/// Our idea of the what state the broker is in over the course of tests. Updated as observations are made about the broker
#[derive(Default)]
pub struct ObservedBrokerState {
    registered: bool,
    sessions: SessionMap,
    topics: HashSet<String>,
}

#[derive(Debug, Clone)]
pub enum Observation {
    Session(SessionDetails),
    Sessions(SessionMap),
    Topics(HashSet<String>),
}

impl ObservedBrokerState {
    fn apply(&mut self, obs: Observation) {
        match obs {
            Observation::Sessions(list) => {
                self.sessions = list;
            }

            Observation::Topics(topics) => {
                self.topics = topics;
            }
            _ => (),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub enum SessionExpectation {
    CountAtLeast(u64),
    ContainsIds(Vec<String>),
}

#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub enum TopicExpectation {
    Exists(Vec<String>),
    CountAtLeast(u64),
}

#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct SubscriptionExpectation {
    pub topic: String,
    pub count_at_least: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct Expectations {
    pub sessions: Option<SessionExpectation>,
    pub topics: Option<TopicExpectation>,
    pub subscriptions: Option<SubscriptionExpectation>,
}

impl Expectations {
    fn is_satisfied(&self, state: &ObservedBrokerState) -> bool {
        if let Some(SessionExpectation::CountAtLeast(n)) = &self.sessions {
            if state.sessions.len() < *n as usize {
                return false;
            }
        }

        if let Some(TopicExpectation::Exists(topics)) = &self.topics {
            for t in topics {
                if !state.topics.contains(t) {
                    return false;
                }
            }
        }

        true
    }
}
pub struct ActiveGate {
    pub deadline: Instant,
    pub expectations: Expectations,
}
#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct Gate {
    pub description: Option<String>,
    #[serde(rename(deserialize = "timeoutSeconds"))]
    pub timeout_seconds: u64,
    pub expect: Expectations,
}

#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct StartProducer {
    pub client_id: String,
    pub producer: ProducerConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct Subscribe {
    pub client_id: String,
    pub topic: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct Unsubscribe {
    pub client_id: String,
    pub topic: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct Disconnect {
    pub client_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub enum Action {
    StartProducer(StartProducer),
    Subscribe(Subscribe),
    Unsubscribe(Unsubscribe),
    Disconnect(Disconnect),
}

#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub enum Step {
    Wait(Gate),
    Do(Action),
}
pub enum ConnectionState {
    NotContacted,
    Registered { client_id: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, Archive, serde::Serialize, serde::Deserialize)]
pub struct TestPlan {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename(deserialize = "timeoutSeconds"))]
    pub timeout_seconds: u64,
    pub steps: Vec<Step>,
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
    let dir = tracing_subscriber::filter::Directive::from(tracing::Level::DEBUG);

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
    // let file = File::open(path).expect("Expected to find a test config at {path}");

    let dhall_str =
        std::fs::read_to_string(path).expect("Expected to find dhall configuration at {path}");

    let config: TestPlan = serde_dhall::from_str(&dhall_str).parse().unwrap();

    return config;
}
